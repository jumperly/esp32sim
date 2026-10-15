# Classic event-driven IRQ presentation (EX028 follow-up)

Stacks on `ac1dd23aec18ad4428f69c7d0bd189dc13ba4350` (classic GPIO query diet).
Comparison base: main `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea`.
Implementation: `6a17af06b36d0952e47d6a6eebb8be756cb7574b`.

## Contract

Classic previously requested interrupt presentation after every bus tick, even
when no source or matrix changed. Presentation scans all device sources and
routes them to both CPUs. This change removes that unconditional request:

- Dispatch's existing changed-source result propagates to the classic bus.
- Manually APB-scaled timer, LEDC and RMT advancement compares those devices'
  source levels before and after the existing tick. The existing clock and
  tick semantics, including synchronous classic I2C, are not changed.
- The maintained GPIO raw/pending word is compared around peripheral output
  synchronization. This catches routed output level changes even when the
  peripheral's interrupt is disabled. Neither query scans pins.
- Existing input, board UART, Wi-Fi TX/RX and I2S DMA event invalidation remains
  live. Board attachment/restoration invalidates presentation as well.
- Actual scheduler core reset invalidates classic presentation through the
  inlined `Soc::reset_core_with_bus` specialization, in both unmodeled and
  modeled scheduling. The bus dirty bit survives until the existing next
  presentation, after reset has erased core-local levels and edge history.
  Host/script releases before a round and guest releases during a round use
  the same reset path. No write-round latch or per-tick core-state polling is
  added. DPORT meanings reuse the adapter's ESP-IDF v5.5.4
  `register/soc/dport_reg.h:118-141` reference.

The earlier control-write latch was insufficient: a guest `s32i.n` clearing
APP reset during round N consumed the latch at the end of N; the actual APP
reset at N+1 then lost unchanged asserted level/edge inputs. The two public
guest-release tests failed (0 instead of 512/1024) before this correction.
The old bus-only control-write probe did not exercise that ordering and is
not evidence for APP release correctness. It is replaced by Machine tests.

No new source-summary cache or register interpretation is introduced. MMIO
writes conservatively invalidate as before; full live source/matrix queries run
on events, not the clean polling path. DPORT status reads remain live. FIFO
reads preserve the existing sticky UART interrupt until INT_CLR; no new read
invalidation or read-path work was retained. Direct host mutation of internal
peripheral fields requires the existing `Machine::sync_irq()` contract.

This is a classic follow-up to EX028, related to EX190's precise device refresh,
not a repeat of the earlier S3 summary-cache timing experiments. Shared machine
reset call sites use the new default-inlined hook: `esp-soc` changes in three
files (+7/-3). The S3/C3/C6 chip crates are untouched and all 51 selected hot
functions are identical. Their default hook only calls the existing reset;
it does not imply unconditional IRQ presentation. The maintainer's S3 held-release
probe fails on this head and main; an S3 invalidation specialization is a separate
follow-up, not included here. No extra clean-round branch, new shared state or JIT
change is introduced.

## Verification

Rust 1.99.0; commands prefixed with `nice -n 10`; separate build targets.
Required checks on the implementation all pass:

```
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
HOME=<empty> ESP32SIM_ROM_DIR=<repo>/web/wasm/fw cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
HOME=<empty> cargo +1.99.0 test --release --workspace
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
cargo +1.99.0 test --release -p esp32 irq_events -- --nocapture
```

Empty-HOME runs keep CARGO_HOME/RUSTUP_HOME only for the installed toolchain and
cache. The plain run unsets ESP32SIM_ROM_DIR. ROM-policy suite: 957 passed,
zero failed or ignored. Plain suite: 929 passed, zero failed, 46 ignored.
Both Clippy steps, WASM build and eight demos pass. The build script's hardcoded
`target/` path uses a worktree-local symlink to CARGO_TARGET_DIR. No JIT code
changed; the conditional JIT-change suite does not apply. All goldens remain
byte-identical; none were regenerated.

Thirteen focused event probes cover clean idle/MMIO polling, input/write dirtiness,
both CPU timer lines, rising/falling timer events, unchanged asserted sources,
all UART ports/APB-AHB aliases with sticky FIFO semantics, RTC watchdog dispatch,
manual LEDC/RMT source events, routed GPIO without a peripheral IRQ and board/reset
invalidation. Six additional tests include real Machine-level level/edge guest
release, host-before-round release, repeated resets through clock holds/stalls,
script-before-round release and edge acknowledgement stability. Guest release
and repeated control tests exercise unmodeled, unit-cost modeled, approximate
batch and approximate-frontier schedulers. A seeded 10,000-operation event trace
is included. Eleven mutations are assertion-killed in
debug builds; `mutations.json` lists the exact killing tests. Compilation
failures are rejected as kills. Reproduce in a separate worktree:

```
python3 docs/evidence/classic-irq-events/mutate.py <worktree> <target> <output>
```

The core-release mutation removes classic reset invalidation. Separate
unmodeled-reset-order and modeled-reset-order mutations bypass the bus-aware
hook at the actual scheduler reset. Machine guest-release tests kill both,
including the next-round ordering missed by the old bus probe. Stall-only
release keeps acknowledged edges clear and does not reset the core.

## Public equal-work fixtures

`compare.py` uses classic hello and the inherited reproducible synthetic GPIO
polling/busy-I2C assembly and harness. Build the ELF inputs using the recipe in
`../classic-gpio-irq/README.md` (GCC 8.4.0, esp-2021r2-patch5). Both binaries run
from the same repository/ROM inputs. No timing is collected by this harness.
`inputs.json` hashes inputs; `fixtures.json` retains both results:

- Classic hello, reset to three guest seconds: core0 4,610,036 + core1 668,561
  instructions; 3,174 exceptions and 597 interrupts. Console hash and complete
  per-core/per-line interrupt counts and mapped-source reports match main.
- GPIO polling: 800,000 instructions, 2,400,000 cycles, zero taken interrupts.
- Busy-I2C byte sink: 800,000 instructions, 2,400,000 cycles, 42,105 transactions,
  zero taken interrupts. This is not an OLED controller. Synthetic CPU IRQ
  delivery is masked; source presentation remains enabled.

Both synthetic console/output hashes and all counts match main. These are
bounded correctness workloads, not proofs of speedup or hardware equivalence.

```
python3 docs/evidence/classic-irq-events/compare.py <base-target> <candidate-target> <fixture-dir> <output>
```

## Hot-path disassembly

`codegen.py` uses `docs/evidence/classic-core/asmcmp.py` (EX230), retaining
opcodes, registers, field offsets and instruction order after address
normalization. Its explicit selectors and binary/disassembly hashes are in
`codegen-other.json` and `codegen-classic.json`.

All 51 selected S3/C3/C6 hot functions match main, including bus tick/access,
machine execution/presentation and CPU execution specializations. This is a
bounded selector, not whole-binary identity.

Classic: 21 selected, five identical, 16 differing/one-sided. The inherited GPIO
query diet explains GPIO read/write and summary extraction changes. Classic
bus tick grows 1,854 to 2,308 static instructions: fixed-size before/after source
comparisons and dirty propagation replace the unconditional presentation
request; GPIO words use constant-work cached queries. The insufficient write
latch has been removed. Classic after_round grows 81 to 84 instructions;
present_irqs is 207 to 234 (the parent's one common scan for two cores).
Classic run_unmodeled grows 1,707 to 1,708 instructions: one dirty-bit store in
the already-conditional secondary-core reset branch, not on clean rounds.
run_modeled retains 3,445 instructions; its inlined reset path now invalidates
classic presentation. Other classic execution differences
include inherited field-offset/layout changes. Static lengths are not executed-work
counts or CPU measurements.

```
python3 docs/evidence/classic-irq-events/codegen.py <base-esp32sim> <candidate-esp32sim> <output>
```

## CPU comparison

Measured by joakimeriksson on Apple silicon, user CPU, alternating pairs with
identical instruction counts and console hashes: PR #230 head `d8003ea5` versus
main `ab89dc54`. Source: [maintainer run, October 15, 2026](https://github.com/mikroverk/esp32sim/pull/230#issuecomment-6186021541).
These are the maintainer's measurements of the complete cache + event change,
not a new run or an isolated cache measurement. Unreported medians and pair counts
are left as dashes; raw samples and load observations were not provided.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| classic hello (30 s) | 1.830 | 0.390 | −78.7% | 0 of 7 |
| busy GPIO polling fixture | 64.94 | 39.74 | −38.8% | 0 of 5 |
| busy I2C fixture | 61.13 | 44.40 | −27.4% | 0 of 5 |
| MicroPython demo (45 s) | 4.86 | 1.79 | −63% | — |
| C3 hello (control; per-binary layout effect, flagged) | — | — | +2.4% / +2.8% | 6 of 7, 7 of 7 (two runs) |
| S3 hello (control) | — | — | +2.0% | 4 of 7 |

C3 is slower in 13 of 14 pairs, not parity: `esp32sim-c3` links the classic
crate, so the maintainer attributes this to a per-binary layout effect, as in
the #224 bisect. The selected C3 hot functions are byte-identical; this is not
a whole-binary identity claim. S3's +2.0% in 4/7 is reported without claiming parity.

## Overlap and limits

PR #224 (`626c8b2fcea3da609f0a8c8ae265576033750f38`) gates classic idle adapters.
This follow-up shares the bus/peripheral tick sites: a trial merge reports
content conflicts in `esp32/src/bus.rs` and `esp32/src/periph.rs`. It needs an
explicit integration/requalification, not an assumed clean merge. The parent
GPIO change is included by ancestry. No push or publication was performed.

Register writes remain conservative; unchanged-value writes may still scan
sources. Clocked-device change detection adds bounded work to classic tick;
the maintainer's complete-PR timing above measures the tradeoff. No electrical timing, silicon,
browser speed, OLED routing or SDK behavior change is claimed. Committed
material contains only public/synthetic recipes, hashes, counts and outcomes;
raw logs and disassembly remain outside Git.
