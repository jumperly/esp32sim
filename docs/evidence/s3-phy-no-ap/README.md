# Complete S3 PHY bring-up without a virtual AP

## Rule and scope

The S3 FE result read at 0x60006174 exposes bit 16 regardless of whether a
virtual AP is configured. This removes the dependency on AP presence from the
existing synchronous ideal-radio completion model; it does not associate a
station, fabricate frames, or change StationLink. The "IQ estimation done"
meaning is inferred from the closed PHY driver's polling and the pre-existing
model, not a documented ESP-IDF register or a silicon timing measurement.
ESP-IDF v5.5.4 `components/soc/esp32s3/include/soc/reg_base.h:11-13`
places the FE block at 0x60006000. Its [fe_reg.h:23-31](https://github.com/espressif/esp-idf/blob/v5.5.4/components/soc/esp32s3/include/soc/fe_reg.h#L23-L31) names FE_GEN_CTRL at
+0x90 and the IQ_EST control bits there, not the result at +0x174. The IQ
naming is anchored to those FE_GEN_CTRL bits; the +0x174 completion-bit
meaning remains inferred from the driver's polling, not a named header field.
No calibration latency/start protocol is added in this correctness fix.

Base: ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea (upstream main).

## Regression and firmware controls

`cargo +1.99.0 test --release -j2 -p esp32s3 --test wifi_phy`:
new tests fail semantically on base (0 vs 65536 without AP; 4660 vs 70196
following AP removal) and pass with the fix. Writes retain all existing word
bits. Idle/associated AP state is unchanged; reads neither create AP/network
nor deliver frames. Restoring the AP gate kills both peripheral tests and the
external no-AP firmware test; restoring the fix passes again. The cleanup
re-ran this mutation by restoring AP-gated state: both peripheral regressions
fail semantically and the optional no-AP CLI test fails (exit 101, no compile
error). Restored cleanup: both peripheral tests and all three optional CLI
controls pass with the exact input hashes below.

`ROBOT_FACE_FW_DIR=<inputs> cargo +1.99.0 test --release -j2 -p esp32sim
--test wifi_phy -- --include-ignored`: three tests passed in the original
qualification. The Robot Face starter build is a **private artefact**, built
with Arduino-ESP32 3.3.9 / IDF 5.5.4. No public source URL, pinned source commit
or complete build recipe is available for these exact bytes; the hashes below
identify that historical input, not a publicly reproducible fixture:

| Input | SHA-256 |
| --- | --- |
| firmware.bin | 8fc10c5ca0ddea496b733292de57a59402cdd4df8bfdfd06a625d595f7ffeda0 |
| bootloader.bin | 3a06e81d78e928687d7acd8abcd069543a4413de4398e2e2940a97b3fc739fd8 |
| partitions.bin | 1d9cca96de0fe07ad7fc0648b9878ddecd9ce565e38b589ad20fea698ed4c80c |
| esp32s3_rev0_rom.elf | c0ce0f338d1de1bdc6efbef1591779a2a42c1ab7d759d3c6ae8ae63a7dd34cfd |

No firmware files are added. These ignored `external_` tests are **optional**
and require the matching privately supplied artefacts; without them, leave
ROBOT_FACE_FW_DIR unset and skip `external_` tests as CI does. The two peripheral
regressions and public demo smoke tests do not depend on this build. Rebuilds
may produce different bytes. SSID/password placeholders are test literals,
not credentials.

Native CLI: `--chip s3 --boot rom --rom <rom> --bootloader <bootloader>
--ptable <partitions> --app <firmware> --flash-mb 8 --psram-mb 0 --board none
--net none --max-seconds <budget>`. Functional results, not host timing:

| AP | Base | Fix |
| --- | --- | --- |
| none | TFT-only at 10 nominal seconds | Connecting within 960,000,000 cycles (4 nominal seconds); at 20 seconds prints offline timeout and Setup complete, no connected marker, 0 received frames |
| matching public placeholders, channel 6 | Connecting, connected IP, Setup complete at 10 seconds | identical serial and guest instruction/exception/interrupt totals; AP Associated, 1 DHCP lease |
| mismatched SSID, channel 6 | Connecting and timeout dots at 10 seconds, AP Idle, no connection | identical serial and guest instruction/exception/interrupt totals; no fake link |

At the same 2,400,000,000-cycle budget, no-AP FE result reads fall from
196077903 (last value 0) to 87 (last value 0x10000). This is a register-access
count demonstrating exit from the poll, not a host CPU benchmark. The sketch
continues offline after its own association timeout, so Setup complete alone
must not be treated as evidence of connection.

These are measured register-access counts, not personal identifiers; privacy
normalization must preserve their exact digits. The raw `--regstat` rows are
`196077903 rd FE+0x174 0x60006174 0x00000000 0x42095f03` on base and
`87 rd FE+0x174 0x60006174 0x00010000 0x42095f03` with the fix. The receipt's
SHA-256 before the measurement-preservation clarification was
`5bd1ad4262a89e8ad5dd950b59f075fddba93e2127e0e79025bf2261a881fa95`.
Any masked rendering of that count is not the stored measurement. The evidence
privacy checker is read-only; it reports patterns without rewriting samples.
Input hashes and the reproduction command above are unchanged.

## Other chips

No shared AP-gated completion path: C3 `FeIq` in esp32c3/src/wifi.rs completes
80 APB edges after its start write, without AP state; C6 `ModemBb` completes
while the IQ start bit is set; classic `FrontEnd` in esp32/src/wifi.rs uses its
own start-bit completion. None are modified. No follow-up correctness bug is
asserted for these independent paths.

## Static cost

The initial AP-gate fix (before the dead-state cleanup): same release profile
and toolchain; disassemble both native CLI binaries with
`xcrun llvm-objdump -d --demangle --no-show-raw-insn` and compare using address,
branch-target, alignment-padding and constant-pool normalization. All 2200
common functions are accounted for: 2198 identical, no missing functions.
Only S3 `periph_read` (808 -> 803 normalized instructions) and
`periph_write_inner` (1742 -> 1740) change. On the FE arm, the AP option load /
comparison is replaced by constant true; the result read ORs immediate 0x10000.
No added tick/deadline work, struct fields, link handling or JIT change. Raw
addresses/layout are not claimed identical. All other functions, including
scheduler/device tick/IRQ functions, have identical normalized instruction bodies.

### Dead-state cleanup

`FeIq.done` and the FE arm in `pre_access` are removed; `read` ORs bit 16
directly. The firmware-visible word and synchronous completion rule are
unchanged. Three independently built native release binaries (main, reviewed
PR head, cleanup) use Rust 1.99.0 and separate target directories.
[Complete static comparison and layout probe](static-cost.json);
[comparison script](compare-disassembly.py). Reproduce with:

```sh
xcrun llvm-objdump -d --demangle --no-show-raw-insn <main-cli> > main.dis
xcrun llvm-objdump -d --demangle --no-show-raw-insn <reviewed-cli> > previous.dis
xcrun llvm-objdump -d --demangle --no-show-raw-insn <cleanup-cli> > candidate.dis
python3 docs/evidence/s3-phy-no-ap/compare-disassembly.py main.dis candidate.dis
python3 docs/evidence/s3-phy-no-ap/compare-disassembly.py previous.dis candidate.dis
```

| S3 function | main | Reviewed head | Cleanup |
| --- | ---: | ---: | ---: |
| periph_read | 808 | 803 | 795 |
| periph_write_inner | 1742 | 1740 | 1764 |
| tick_impl | 3686 | 3686 | 3686 |
| after_round | 114 | 114 | 116 |
| step_core | 990 | 990 | 992 |
| run_unmodeled (unobserved) | 3311 | 3311 | 3306 |

Sizes above are normalized instruction counts, not bytes or timings. In each
comparison all 2200 common functions are accounted for: 2118 identical,
82 changed, none missing. The changed functions are S3-specific plus the
shared CLI entry; other-chip bodies are identical after constant-pool
normalization (including omitted zero offsets).

The removal changes Rust layout: FeIq 8 -> 4 bytes, Peripherals 9360 -> 9352,
SocBus 12408 -> 12400. FE moves from peripheral offset 9096 to 9344; bus tail
fields move by -8 bytes. This propagates field offsets and address generation
through S3 memory, tick, IRQ and scheduler functions. `after_round` and
`step_core` gain two instructions where last-status addressing uses two scalar
loads instead of a paired load plus another address materialization. The
peripheral dispatch bodies also change block ordering/register allocation;
`periph_write_inner` grows despite the removed FE pre-access arm. These are
explained code-generation differences, **not** identical hot-path disassembly
or a measured CPU clearance of the cleanup. No source-level tick/deadline
rule, link handling or JIT implementation is added.

## Required gates

The dead-state cleanup re-ran the commands below: 940 CI-policy tests,
912 plain tests with 49 ignored, both Clippy gates, production WASM and all
eight demos passed. The original qualification had the same test totals.

All commands use `nice -n 10`, cargo +1.99.0, `-j2`, and an isolated target
outside the control checkout. Native Clippy `--workspace --all-targets --
-D warnings` and WASM Clippy `--release --target wasm32-unknown-unknown -p
esp32sim-wasm --features jit-tests -- -D warnings` exit 0.

With HOME pointing at an empty directory and only ESP32SIM_ROM_DIR selecting
`web/wasm/fw` (RUSTUP_HOME/CARGO_HOME retained solely for toolchain access),
`cargo +1.99.0 test --release -j2 --workspace -- --include-ignored --skip
external_` passes 940 tests with zero failures/ignored. The same empty HOME
without ESP32SIM_ROM_DIR, `cargo +1.99.0 test --release -j2 --workspace`,
passes 912 tests with 49 ignored and zero failures.

`RUSTUP_TOOLCHAIN=1.99.0 CARGO_BUILD_JOBS=2 tools/wasm-build.sh` exits 0;
`node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki
c6-contiki-net c6-rpl-net panel` passes all eight demos. The build script's
hardcoded `target/` copy uses a worktree symlink to the isolated target.
JIT-specific gate is not applicable: no JIT source changes. Evidence privacy
check passes: 1811 tracked evidence files / 20 gzip files, supplemented by manual
review. No golden files changed.

## CPU comparison

Centrally measured by joakimeriksson on October 14, 2026, for the reviewed
head before the dead-state cleanup, versus upstream main. [Original run and
method](https://github.com/mikroverk/esp32sim/pull/223#issuecomment-6177864094).
User CPU on Apple silicon, 1-minute load 5–7 throughout, seven alternating
pairs, identical instruction counts and console hashes in every pair. The
comment does not specify a warmup; no warmup is inferred here.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| S3 hello (3000 s) | 25.67 | 26.04 | +1.4% | 5 of 7 |
| Pocket Tank (30 s, S3 PSRAM + AMOLED) | 40.96 | 40.95 | −0.0% | 4 of 7 |

The Pocket Tank row is credited to joakimeriksson's
[October 14, 2026 follow-up](https://github.com/mikroverk/esp32sim/pull/223#issuecomment-6178293944),
using seven alternating pairs with identical instructions and console hashes.
That follow-up states load 6–7 for S3 hello. Neither row has a slower >=6/7
flag. The aggregate eight-PR scratch-branch results in the original comment
are not single-PR measurements and are not included here.
This table measures the pre-cleanup head, not the dead-state cleanup; no new
CPU timing run was performed for the dead-state cleanup.

## Limits

This is direct native firmware/peripheral validation plus the standard WASM
smoke suite, not browser-worker, display, add-on ABI or deployment acceptance.
No production data, user firmware, session replays or hardware were used.
The Robot Face controls use the private starter artefacts labelled above.
Existing golden files remain unchanged; EX210 now links this receipt.
