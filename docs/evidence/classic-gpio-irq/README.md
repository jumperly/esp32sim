# Classic GPIO IRQ query diet (EX028)

Base: upstream main `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea`.
This is the classic LX6 retry of EX028, not another S3 summary-cache screen.
Earlier EX028 negative results are retained in the catalog. The material difference
is classic's adapter: each IRQ query scanned 40 pins for both the resolved level
and each core's routing. The CPU table below measures the complete PR #230,
including event-driven presentation, not the cache alone.

## Contract and scope

`ClassicGpio` maintains a mask of level-configured pins, their resolved status,
and independent APP/APP_NMI/PRO/PRO_NMI route masks. IRQ queries combine the live
software/edge-pending word with the maintained level word, then AND the selected
route. No enabled route therefore needs no pin scan. INT_TYPE zero still routes
software-pending bits; the fifth enable bit retains main's behavior. The model's
edge behavior, level reassertion, status W1TC and matrix mapping are unchanged.

Level status is refreshed on PINn and IO_MUX writes, GPIO output/enable writes
(the shared fallback input can change without a driven-pad change), host input
and release, changed driven signals, RTC pad takeover and RTC restoration.
Status-only writes and unchanged peripheral output observations do not refresh it.
A test-only counter qualifies this negative-work contract; it does not exist in
production builds. New production fields are appended to the GPIO adapter.

Only GPIO is event-cached. Other device summaries deliberately remain live:
host UART input and device timer transitions do not participate in GPIO
invalidation. IRQ presentation samples their common summary once, then adds
GPIO's per-core sources and applies each interrupt matrix. The complete PR changes
the shared `esp-soc` scheduler in three files (+7/-3) for the bus-aware reset hook;
S3/C3/C6 chip crates are untouched and all 51 selected hot functions are identical.
No guest-clock, I2C control/routing, JIT or scheduling semantics change is intended.
Active classic I2C byte deadlines participate in dispatch; their incremental
slicing cost is unquantified. Busy polling is not an emulator correctness bug.

Register references checked: ESP-IDF v5.5.4
`components/soc/esp32/register/soc/gpio_reg.h:303,322` and v4.4.7
`components/soc/esp32/include/soc/gpio_reg.h:311,330` agree on INT_ENA shift 13
and INT_TYPE shift 7. Arduino 2.x uses the 4.4 family; Arduino 3.x uses IDF 5.x.
The change preserves the existing adapter's interpretation, not a new silicon claim.

## Verification

All commands run with Rust 1.99.0, `nice -n 10` and an isolated `CARGO_TARGET_DIR`.
No goldens were regenerated. Required native and WASM checks passed:

```
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
HOME=<empty> ESP32SIM_ROM_DIR=<repo>/web/wasm/fw cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
HOME=<empty> cargo +1.99.0 test --release --workspace
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
cargo +1.99.0 test --release -p esp32 gpio_irq -- --nocapture
```

The empty-HOME commands retain only CARGO_HOME/RUSTUP_HOME to locate the installed
toolchain and dependency cache. The plain run unsets ESP32SIM_ROM_DIR.
CI-policy run: 944 passed, zero failed, zero ignored.
Plain run: 916 passed, zero failed, 46 ignored.
Both Clippy checks, all eight WASM demos and privacy check pass.
No JIT implementation changed, so the conditional JIT suite is not required.

Six focused tests compare against the pre-change uncached query algorithm:
all eight interrupt types, all 32 enables, both cores/NMI, bank boundary pins
0/4/31/32/39, software pending, W1TS/W1TC/direct status, interrupt matrix,
GPIO outputs and fallback inputs, IO_MUX input/pulls/direct functions, peripheral
signal value/OE, RTC takeover/release/restore, and a seeded 12,000-operation trace.
The trace constrains synthetic output selectors to valid IDs 0..256.
UART host input and timer expiry/clear explicitly verify live non-GPIO summaries.

All 25 mutations are assertion-killed, recorded in `mutations.json`;
`mutate.py` changes one rule at a time and rejects compilation errors as kills.
The eight `invalidation-N` mutations remove, in source order: changed-output,
host-input, release, IO_MUX, output/enable, PINn, RTC restore and RTC MMIO refresh.
The RTC MMIO mutation survives a weaker inactive-level probe; the qualifying
case uses a live high input with no driven-output change, then takeover/release.
That negative coverage result is retained with the final RTC kill. Re-run:

```
python3 docs/evidence/classic-gpio-irq/mutate.py --workspace "$PWD" --target "$CARGO_TARGET_DIR" --output <mutation-output>
```

## Equal-work fixtures

`inputs.json` hashes all public/synthetic inputs; `fixtures.json` retains the
main/candidate equality results. No user project or private sketch is used.

- Public classic hello, reset to three guest seconds: core0 4,610,036 and core1
  668,561 instructions; 720,000,000 cycles, 3,174 exceptions, 597 interrupts.
- Synthetic GPIO IRQ-enabled polling: 800,000 instructions and 2,400,000 cycles.
- Synthetic busy I2C byte-sink starter: 800,000 instructions, 2,400,000 cycles,
  42,105 transactions. This is a display-like byte sink, not an OLED controller.

All instruction counts, console bytes/hashes and these stop/interrupt totals
match main exactly. The synthetic cases mask CPU delivery (no fake ISR); their
source presentation still runs. The focused tests qualify routing and semantics.

Build the synthetic fixtures with caller-supplied `XTENSA_PREFIX` (including the
full `xtensa-esp32-elf` executable prefix); measured compiler is GCC 8.4.0,
crosstool-NG esp-2021r2-patch5. No SDK or external firmware source is linked:

```
"$XTENSA_PREFIX-gcc" -nostdlib -mtext-section-literals -Wl,-Ttext=0x40080000,-e,_start -DGPIO_FIXTURE docs/evidence/classic-gpio-irq/fixture.S -o <output>/gpio.elf
"$XTENSA_PREFIX-gcc" -nostdlib -mtext-section-literals -Wl,-Ttext=0x40080000,-e,_start docs/evidence/classic-gpio-irq/fixture.S -o <output>/i2c.elf
cargo +1.99.0 run --release -p esp32 --example gpio_irq_fixture -- gpio <output>/gpio.elf
cargo +1.99.0 run --release -p esp32 --example gpio_irq_fixture -- i2c <output>/i2c.elf
```

Use the same example and assembly on a detached worktree at the named base.
For classic hello, fetch with `tools/fetch-demo-assets.sh --no-linux`, then run:

```
esp32sim --chip esp32 --rom web/wasm/fw/esp32_rev300_rom.elf --board esp32dev --boot rom --no-dump --console uart0 --bootloader web/wasm/fw/public/classic-hello-bootloader.bin --ptable web/wasm/fw/public/classic-hello-ptable.bin --app web/wasm/fw/public/classic-hello_world.bin --max-seconds 3
```

## Static hot-path comparison

`codegen.py` reuses EX230's address normalizer and preserves opcodes, field
offsets, registers and instruction order. ARM64 Rust 1.99.0 fat-LTO release CLI
binaries: 99 selected symbols, 78 identical, 14 differing common symbols and
seven one-sided symbols due to changed inlining/extraction boundaries. Binary,
disassembly and per-function hashes/counts are retained in `codegen.json`.

- Classic `raw_status`'s 71-instruction scan disappears into constant-work
  loads/ORs/ANDs in its callers. A new 91-instruction `refresh_level_status`
  walks only configured level pins on invalidation, outside the IRQ query.
- The old 323-instruction core-specific `source_status` is inlined; the common
  live-device `Dispatch::source_status` is now a 288-instruction function.
  `present_irqs` changes 207 to 234 static instructions: one common scan and
  per-core GPIO mask operations replace two scans and four routed-pin loops.
  Static function length alone is not executed work or a speed measurement.
- GPIO write grows 489 to 556 instructions for invalidation and mask updates.
  GPIO read shrinks 381 to 251 instructions with constant-work IRQ masks;
  peripheral read32 grows 596 to 624 with inlined per-core source handling.
  Classic bus tick grows 1,854 to 1,927 instructions: the compiler now inlines
  LEDC/RMT synchronization (previously 164/167-instruction functions) and outlines
  the 81-instruction set_output_signal helper, alongside register allocation and
  field-offset changes. That helper's changed-pad test guards the tail call to
  refresh_level_status. Unchanged output observations do not refresh level status.
- Classic fetch/read32/after_round/run_modeled/run_unmodeled/step_core and
  Xtensa classic run/step_extra keep instruction counts; field offsets move
  by 48 bytes because the appended cache grows the embedded GPIO adapter.
  Classic next_deadline keeps 817 instructions, with field-offset and compiler
  block/register-order differences; no deadline or I2C rule is changed.
- The other selected hot functions, including other-chip execution, match.
  This is a bounded selection, not a whole-binary identity claim.

```
python3 docs/evidence/classic-gpio-irq/codegen.py <base-esp32sim> <candidate-esp32sim> <output>
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

`esp32/src/periph.rs` overlaps PR #224's adapter gating at head
`626c8b2fcea3da609f0a8c8ae265576033750f38`; changes are separate from its clock,
reset and tick gates. The local handoff records the throwaway trial-merge result.
No publication, hardware validation, electrical timing or browser-speed claim.
Only curated hashes/counts/outcomes are committed; raw logs/disassembly remain
outside Git. No personal, host, process, or production-user data is retained here.
