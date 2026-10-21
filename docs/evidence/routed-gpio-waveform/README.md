**CPU table pending.** Our timing run (user CPU, 1 warmup + 7 alternating pairs against main, identical instruction counts and console hashes) needs a quiet machine and hasn't run yet. The table will be added to the receipt and posted here as soon as it is measured.

# Routed software GPIO waveform observation (EX218 follow-up)

Base: `77e43c20a70054ac84d1724d757fa9ce12668151`.

## What and why

A software-driven waveform can change when firmware changes a pad's routing,
mux or output enable without changing its output latch. Opted-in boards now
observe the programmed drive after those writes and after attachment/reset,
instead of relying on raw latch transitions alone.

## Contract and limits

`BoardModel::gpio_waveform_at(cycle, PinRoutes)` is enabled by the existing
`uses_gpio_waveform()` flag. ESP32, S3, C3 and C6 deliver the post-write routes
after GPIO and IO_MUX register writes. Attachment also delivers the current
state, including the reset peripheral state for a persistent board. Existing
raw `gpio_output_at`, input/IRQ resolution and RMT callbacks are unchanged;
there is no second decoder or scheduling mechanism.

`PinRoutes::software_output(pin)` returns the programmed software-latch drive:
valid pad, GPIO matrix function, software selector, enable and its inversion,
and output inversion. Peripheral-selected or disabled pads return `None`.
This is not electrical pad resolution: RTC ownership, pulls, open-drain
contention and peripheral-generated waveforms are outside the query. Classic
uses the existing translated pad map and tests both APB and AHB aliases.

Field positions cite ESP-IDF v5.5.4
`components/soc/<chip>/register/soc/gpio_reg.h`: ESP32 lines 7065–7092,
S3 7790–7813, C3 3883–3906, C6 4992–5023. C6 names selector 128 as GPIO_OUT;
S3/C3 reset selectors are 256/128. Using GPIO_ENABLE for the software selector
regardless of OEN_SEL is inferred model behavior consistent with the classic
adapter, not hardware calibration. Both OEN_SEL states are tested. No firmware
library or other ESP-IDF version is used to qualify these generated fixtures.

## Port and overlap

The Rust implementation/test diff is unchanged from the accepted routed-drive
change. Main's RTC CRC write hook, classic attachment IRQ invalidation and
merged PCM query are preserved. The experiment catalog retains all 229 main
IDs and their order, and all main row text except the accepted EX218 suffix;
no new experiment is claimed. `port.json` records these invariants.

Open PRs checked on 2026-10-21: #239 touches the C6 bus, #243 and #246 touch
classic bus/peripherals, and #258 touches the S3 bus. This adds no competing
register block, device-set entry or shared routing owner. It builds directly
on the base above; no unmerged feature dependency is required.

## Regression fixtures and mutations

Four bus tests cover mux-only connection/disconnection, route-only peripheral
selection, output and enable inversion, disabled drive, opt-out, reset and
classic aliases. The shared query test covers invalid pins, S3 GPIO48 and both
OEN_SEL states. The generated waveform fixture retains exact nine-byte RGB
output, 145 edges, pulse widths, quanta 1/64/256/1024, native JIT/interpreter
and S3 peer-core checks. These are generated instruction/register fixtures,
not Arduino firmware or hardware-calibrated timing.

Run the rule-removal harness in a disposable worktree:

```
CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$TARGET" \
  nice -n 10 python3 docs/evidence/routed-gpio-waveform/mutate.py --out "$MUTATIONS"
```

The harness independently omits write/reset notification and bypasses opt-out
on each chip, then removes mux, route, enable, output-inversion and
enable-inversion rules from the shared query. It requires an assertion-test
failure, not a compilation error, for every kill and restores source in a
`finally` block. Normal-source focused release tests are run afterward.
All 17 semantic mutations were killed with exit 101 and no compiler errors;
all five restored-source focused release tests passed. `mutations.json` names
the killing test for every removal. Real WASM passed 111126 differential cases
(99679 compiled modules released) and 24 waveform cases (738 modules released).

| Removed rule (mutation names) | Killing test |
| --- | --- |
| `classic-write`, `classic-reset`, `classic-optout` | `classic_routed_waveform_and_alias` |
| `s3-write`, `s3-reset`, `s3-optout` | `s3_routed_waveform` |
| `c3-write`, `c3-reset`, `c3-optout` | `c3_routed_waveform` |
| `c6-write`, `c6-reset`, `c6-optout` | `c6_routed_waveform` |
| `mux`, `route`, `enable`, `output-inversion`, `enable-inversion` (each independently) | `software_drive_respects_mux_route_enable_and_inversion` |

## Required checks

Rust 1.99.0, `nice -n 10`, `CARGO_BUILD_JOBS=4`, serial builds. Tests used an
empty HOME, explicit installed CARGO_HOME/RUSTUP_HOME, and only the fetched ROM
directory for the CI-policy run; the plain run had no ROM environment setting.
`tools/fetch-demo-assets.sh --no-linux` ran inside this worktree; no firmware
was copied from another checkout. `inputs.json` pins the resulting inputs.
Build output was external via CARGO_TARGET_DIR. A worktree `target` symlink to
that directory accommodates the existing WASM scripts' relative artifact paths.
`RUSTUP_TOOLCHAIN=1.99.0` was set for both WASM scripts.

| Command | Result |
| --- | --- |
| `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings` | PASS |
| `ESP32SIM_ROM_DIR="$ROOT/web/wasm/fw" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_` | PASS: 1015 passed, 0 failed, 0 ignored |
| `cargo +1.99.0 test --release --workspace` | PASS: 987 passed, 0 failed, 50 ignored |
| `tools/wasm-build.sh` | PASS |
| `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel` | PASS: eight manifests |
| `tools/wasm-jit-test.sh` | PASS: 111126 differential and 24 waveform cases |
| `cargo +1.99.0 test --release -p esp32sim -p esp-soc --test routed_waveform` | PASS: five tests after mutation restoration |
| `cargo +1.99.0 build --release --bins` | PASS: candidate and independent base worktree, serial |
| `node tools/check-evidence-privacy.mjs` | PASS; compact evidence also manually reviewed |

All 55 tracked `tests/golden` files are byte-identical to the base. The full
CI-policy suite exercised console, stop/interrupt and audio oracles without
UPDATE_GOLDENS. JIT source is unchanged, but the full real-WASM suite was run
to exercise the routed fixture through generated modules.

## Hot-path disassembly

Final `--release --bins` binaries were disassembled with LLVM 23.1.3:

```
llvm-objdump --disassemble --demangle --no-show-raw-insn <binary>
python3 docs/evidence/routed-gpio-waveform/compare.py main.dis candidate.dis --out static.json
python3 docs/evidence/routed-gpio-waveform/literal-proof.py \
  --main-dis main.dis --candidate-dis candidate.dis \
  --main-bin main-esp32sim --candidate-bin candidate-esp32sim --out literals.json
```

The existing `classic-core/asmcmp.py` normalizer compares selected four-chip
scheduling, execution, tick/deadline, read/write and MMIO/store functions.
125 selected functions: 119 identical, six different, zero missing.
`static.json` records every function and instruction count. The comparator was
not weakened to absorb the two literal-pool differences below.

| Function | Base → candidate instructions | Explanation |
| --- | --- | --- |
| ESP32 `store` | 2722 → 2747 | Inlined GPIO/mux block guard, opt-in and routed callback; register allocation/spill-slot changes throughout the inlined MMIO body. The existing RTC CRC branch is retained; total stack frame stays 0xc70. |
| C3 `periph_write` | 1734 → 1739 | Reuses block classification; new guarded calls change allocation, spills and block order. Frame stays 0x120. |
| C6 `periph_write` | 1872 → 1901 | New block guard and opt-in/callback calls; allocation/spill changes grow this MMIO frame from 0x120 to 0x130. |
| S3 `periph_write_inner` | 1764 → 1792 | New GPIO/mux guard and opt-in/callback path with register allocation and block-order changes. Frame stays 0x190. |
| C3 `refresh_irq` | 341 → 341 | One literal-load spelling changes from `[x9, #PAGEOFF]` to `[x9]`: the literal moved onto a page boundary. All other normalized instructions match. |
| C6 `refresh_irq` | 446 → 446 | The analogous single `[x8, #PAGEOFF]` → `[x8]` literal-load change, with all other normalized instructions matching. |

`literal-proof.py` reads the final Mach-O file-backed segments and proves the
two IRQ loads still fetch the same 16 bytes,
`00100000002000000040000000800000`, from base address `0x1001eae10` versus
candidate `0x1001eb000`. `literals.json` retains the exact instructions and
bytes. These are relocation-only differences, not new IRQ work. Selected
scheduler, tick, fetch, read and execution functions are otherwise identical.
`artifacts.json` pins final binary/disassembly hashes and retained compressed
binary names. Raw final binaries/disassemblies and command logs are retained
with the local qualification evidence, not published; the commands above
reproduce the comparison from the base and this change.

No struct fields or per-tick services are added. The trait method is appended,
retaining existing callback slots. GPIO/mux writes have a new block guard,
including on opt-out boards; selected writes call the opt-in query and construct
routes/deliver only when enabled. Attachment/reset has analogous opt-in work
outside ordinary execution. MMIO/store code is not claimed identical or free.

## CPU comparison

PENDING

No timing benchmark was run. Diagnostic throughput text from functional runners
is not CPU qualification. No performance or hardware timing claim is made.
