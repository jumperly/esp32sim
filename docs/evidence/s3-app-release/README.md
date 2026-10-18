# S3 APP release: republish an asserted interrupt after core reset

## What and why

Base: upstream main `f5a038bf917ccb9787714dc1d20c7ae33482337c`.
This is an independent fix, not stacked on #230. It addresses the S3 case
reported by joakimeriksson in #230 on October 15, 2026.

The scheduler presents pending interrupts before it notices APP's release from
reset. Reset then clears that core's external levels and edge history. S3's
source/routing-change filter sees no new peripheral transition, so the asserted
interrupt can disappear until another device event causes presentation.

Reset now invalidates both the bus refresh gate (`irq_dirty`) and the existing
routing-change gate (`intmatrix_dirty`). The latter is necessary: setting only
`irq_dirty` still loses the interrupt because `refresh_irq` suppresses unchanged
source levels. The flags are consumed by existing refresh boundaries; there is
no new field, periodic service or unconditional interrupt presentation.

Main does not yet have `Soc::reset_core_with_bus`. The default hook and its three
call sites (chip reboot, unmodeled release and modeled release) match #230's
shared plumbing exactly. This branch includes that plumbing and only the S3
specialization, not the classic GPIO/event optimization. When either PR lands,
keep a single copy of the hook. The classic and RISC-V specializations retain
the default reset behavior here.

## Reproduction and regression coverage

Before changing production code, a fresh S3 held-release test on the base failed
at the post-release assertion: line 9 expected 512, got 0 (test process exit 101).
The final three-test suite also fails on the base, at the same lost-line
assertion in each test, and passes on this branch.

`esp32s3/tests/irq_release.rs` needs neither ROM nor external firmware. It covers:

- held APP release with an already asserted level (line 9) and edge (line 10);
- release caused by a PRO store, followed by APP reset on the next round;
- three repeated resets with clock hold, stall and script-driven release;
- acknowledged edges staying clear on clean rounds and stall-only release;
- source deassertion and chip reboot clearing the delivered levels;
- unmodeled execution, a unit-cost model, approximate timing and approximate
  JIT frontiers (four modes).

The control bits and register offsets cite ESP-IDF v5.5.5 in the test. These
are synthetic scheduler regressions, not hardware timing measurements.
Related catalog mechanisms: EX090 interrupt refresh boundaries and EX190 precise
source refresh. This is a correctness repair of reset invalidation, not a retry
of an execution-speed experiment; no new experiment ID is assigned.

## Mutation checks

Run from the repository root:

```sh
export CARGO_TARGET_DIR="$TARGET"
python3 docs/evidence/s3-app-release/mutate.py "$OUT/mutations"
```

All four mutations compile and are killed by runtime assertions (exit 101), not
by build failures. The original files are restored after each mutation.
`mutations.json` records the individual failing tests.

| Removed rule | Test killing the mutation |
| --- | --- |
| Bus refresh invalidation after reset | held_release_republishes_asserted_level_and_edge |
| Routing-filter invalidation after reset | held_release_republishes_asserted_level_and_edge |
| Unmodeled release calls the bus-aware hook | held_release_republishes_asserted_level_and_edge |
| Modeled release calls the bus-aware hook | held_release_republishes_asserted_level_and_edge |

Guest and repeated-reset tests kill each mutation too. The modeled-hook mutation
passes the initial unmodeled case and fails when the test reaches modeled mode.

## Required checks

Rust 1.99.0, native aarch64-apple-darwin and wasm32-unknown-unknown.
All commands below exit 0. `checks.json` contains the compact results.
The native tests use an empty HOME, explicit Rust/Cargo installation paths,
and no inherited emulator settings; the CI-policy run adds only
`ESP32SIM_ROM_DIR` for emulator inputs. Plain tests leave that variable unset.

```sh
tools/fetch-demo-assets.sh --no-linux
nice -n 10 cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
nice -n 10 cargo +1.99.0 clippy --release --target wasm32-unknown-unknown \
  -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$PWD/web/wasm/fw" nice -n 10 cargo +1.99.0 test \
  --release --workspace -- --include-ignored --skip external_
nice -n 10 cargo +1.99.0 test --release --workspace
RUSTUP_TOOLCHAIN=1.99.0 nice -n 10 tools/wasm-build.sh
nice -n 10 node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan \
  c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
nice -n 10 cargo +1.99.0 build --release -p esp32sim
```

CI-policy: 944 passed, 0 failed. Plain: 916 passed, 0 failed, 46 ignored.
Both lint gates and all eight WASM demos pass, including the shared-memory JIT
handoff check. No JIT code changed, so the separate JIT differential suite was
not required. No golden output files changed.

For isolated builds set `CARGO_TARGET_DIR` outside the worktree. The WASM build
script copies from `target/`; a worktree-local `target` symlink to the isolated
target directory supplies that path. Demo assets are fetched within the worktree,
not copied from another checkout.

## Static hot-path comparison

Build the base and candidate CLIs in separate target directories, then:

```sh
python3 docs/evidence/s3-app-release/codegen.py \
  "$BASE_TARGET/release/esp32sim" "$TARGET/release/esp32sim" "$OUT/codegen"
```

`codegen.json` records binary/disassembly SHA-256s, the selector and each result.
The comparator reuses EX230's committed `classic-core/asmcmp.py`: relocation,
page-address and alignment normalization, preserving opcodes, register allocation,
field offsets and stack offsets. This is native static evidence, not timing.

71 of 72 selected functions match main: 23/24 S3, 21/21 classic, 14/14 C3,
13/13 C6. There are no missing selected functions. Selected bus tick/read/write/
fetch/deadline paths and the selected core execution functions are unchanged.
The one difference is S3 `run_unmodeled::<false>` (3311 to 3358 normalized
instructions): reset's existing taken-release arm adds the two dirty stores;
LLVM also changes register allocation, block placement and spills, increasing
its stack frame from 0x2a0 to 0x2b0. The full function is therefore NOT identical,
and the difference is not claimed to be purely cold or cost-free. The comparison
script retains its normalized diff for inspection. No source-level work was
added to clean rounds. The generated spill/layout changes are covered by the
CPU comparison below, which does not establish exact CPU parity.

## Public firmware equality (not timing)

```sh
python3 docs/evidence/s3-app-release/compare.py \
  "$BASE_TARGET/release/esp32sim" "$TARGET/release/esp32sim" "$OUT/fixtures"
```

Both arms run identical committed public images and ROMs for 3 emulated seconds.
`inputs.json` pins the input hashes, `fixtures.json` the complete arguments,
per-core instructions, console SHA-256 and exception/IRQ reports. All four
pairs match exactly:

| Workload | Instructions, base = candidate | Exceptions / IRQs |
| --- | --- | --- |
| S3 hello | core0 2,949,934; core1 1,345,182 | 3255 / 602 |
| Pocket Tank startup | core0 388,739,230; core1 4,458,656 | 107492 / 9637 |
| C3 hello | 480,000,000 | 0 / 302 |
| C6 hello | 480,000,000 | 0 / 303 |

Pocket Tank is a bounded startup control with its committed bootloader/table/app,
16 MB flash and 8 MB PSRAM, without the separately fetched language model. It
does not qualify inference, full gameplay or long-run output. No RF, silicon,
production-cohort or hardware-timing claim is made.

## CPU comparison

Measured October 15, 2026: candidate
`bedf0dbd4c5750079bcdf918ea55838e3b50499f` against upstream main
`f5a038bf917ccb9787714dc1d20c7ae33482337c`, using the exact retained release
binaries and public inputs from the comparisons above, without a rebuild.

User CPU seconds (`wait4` user resource usage), not wall time: one warmup pair
and seven measured pairs per fixture, alternating base/candidate order, with
`nice -n 10`. Instruction counts and console SHA-256s match the public firmware
equality table above in all 64 accepted runs (8 warmups and 56 measured runs).
There were no rejected runs or retries.

| Workload | Main median user s | Candidate median user s | Change (ratio of medians) | Candidate slower |
| --- | --- | --- | --- | --- |
| S3 hello | 0.049509 | 0.048313 | -2.416% | 5/7 |
| Pocket Tank startup | 1.069807 | 1.073883 | +0.381% | 4/7 |
| C3 hello | 0.296777 | 0.301625 | +1.634% | 5/7 |
| C6 hello | 0.387649 | 0.385845 | -0.465% | 2/7 |

The run window was 12:35:40–12:36:16 UTC. All 140 load samples met the quiet
machine gates: initial 1-minute load 2.088379, range 2.000977–2.783203, each run
starting below 5 and none above the redo threshold of 7; no builds were observed.
No fixture reaches the flag threshold of slower in at least 6/7 pairs.

This is not a claim of exact CPU parity or a speedup. S3 hello is short
(about 0.05 user CPU seconds) and noisy. Pocket Tank pair 7 varies but is
retained because it met the predefined load/build gates. The comparison covers
the bounded workloads above, not full gameplay or other execution modes.
The independently verified central artifact manifest has SHA-256
`c9f467ba276678ef5d138269ad25e82f7531d1d155a1677cd74993df1c313834`.
