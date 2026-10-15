# Classic I2C pin routing at START

Base: upstream `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea`.

The classic MMIO START boundary now supplies SDA/SCL through the existing
`ChipPins::ESP32` route decoder and `I2c::set_pins`. It retains the existing
line-high timeout check and shared address matching. No fields, tick work,
command timing, runtime address or multicast rules are added or replaced.

Classic control bit 10 is not interpreted as the modern FSM reset. ESP-IDF
v5.5.4 `components/soc/esp32/register/soc/i2c_reg.h` defines the classic CTR
register but no FSM_RST field; the ESP32-S3 file at the corresponding path
has FSM_RST at lines 111–117. The classic `components/hal/esp32/include/hal/i2c_ll.h:625–628`
`i2c_ll_master_fsm_rst` is empty; nothing in IDF 5.5.4 / Arduino 3.x writes
bit 10. This control-layout correction cannot have contributed to the SSD1306
result: pin routing at START is the fix. Preserving unmodeled classic bit 10 in register
RAM is an inferred emulator readback policy, not a reserved-bit silicon claim.
`components/soc/esp32/include/soc/gpio_sig_map.h:73–76,200–203` defines
classic I2CEXT0/1 input/output SCL/SDA signal IDs. No hardware validation or
IDF 4.4 validation is claimed.

## Reproduction

`cli/tests/classic_i2c_routes.rs` tests both controllers, APB/AHB aliases,
same-address pin-specific devices, rerouting, either wrong wire, missing
output route, classic END-recipient preservation, and
modern pending-transfer reset cancellation.

The two classic hermetic tests and the modern reset-control test run in CI.
The external Arduino test is ignored without its firmware inputs. Run the focused suite:

```
cargo +1.99.0 test --release -j2 -p esp32sim --test classic_i2c_routes
```

With a dedicated `CARGO_TARGET_DIR` and no other build running:

```
python3 docs/evidence/classic-i2c-routes/mutate.py
```

The executable table individually removes routes, reverses wires, uses port-0
signals for port 1, restores classic reset, and removes
modern reset. A kill requires exit 101 and the named assertion test failing;
a compile error is not a kill. Original sources are restored on exit.

The new bounded Arduino Wire fixture uses the same compiled firmware for
both classic controllers and tests two correct pin pairs plus a cross-wired
pair. It is a routing probe, not a full sensor/OLED/camera model qualification.
The linked PlatformIO firmware build is not in the repository. Build the
committed pinned recipe, then run the ignored external test (the build directory
is `docs/evidence/classic-i2c-routes/firmware/.pio/build/classic`):

```
pio run -j 2 -d docs/evidence/classic-i2c-routes/firmware
CLASSIC_I2C_ROM=<ESP32-ECO3-ROM.elf> \
CLASSIC_I2C_FIRMWARE=<PlatformIO-classic-build-directory> \
cargo +1.99.0 test --release -j2 -p esp32sim --test classic_i2c_routes \
  external_arduino_classic_i2c_routes -- --ignored --exact --nocapture
```

The expected six lines are ACK/read-21, ACK/read-18, transfer-error-4/no-byte
on each port, followed by `ROUTE:done`, without reboot. The pinned Arduino
3.3.9/IDF 5.5.4 driver reports ESP_ERR_INVALID_STATE on the wrong pair and
maps it to Wire error 4, not error 2; the register-driven test separately pins
INT_NACK. No claim of silicon Wire error-code parity is made. The exact same
binary must be used
for any baseline comparison. Firmware input hashes and observed execution
results belong in the final qualification receipt.

## Executed qualification

Original qualification: PR #226 head `846990345d70be6d3581d9ce06617713ec88083b`.
Against the frozen base above, under Rust 1.99.0 with serialized `-j2`:

- Native workspace/all-target Clippy and release WASM jit-tests Clippy with
  `-D warnings`: exit 0.
- Empty-HOME workspace CI policy (only the absolute ROM input set):
  941 passed, 0 failed. Plain empty-HOME workspace run without ROM setting:
  913 passed, 47 ignored, 0 failed.
- `RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh` and
  `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel`:
  exit 0. Existing golden files are unchanged. No JIT implementation changed.
- `node tools/check-evidence-privacy.mjs`: exit 0, supplemented by manual review.
- Three focused register tests pass. The original six executable mutations were killed
  by the named assertion failures (exit 101), not compilation failures. The review
  follow-up removes reserved-bit readback assertions and their clearing mutation;
  five semantic mutations remain. Reserved-bit readback is not a test contract.
- `pio run -j 2` builds the fixture with Arduino 3.3.9/IDF 5.5.4, platform
  tag 55.03.39 (`cbc3349061987c28bc1b48d43d473e70c5ae04ed`). The ignored
  Arduino test passes all seven output lines and zero reboots. The exact same
  firmware fails the routing assertions on the frozen base: all six probes
  report error 4 and no byte, including the four correctly wired probes.

Firmware SHA-256 (linked binaries remain external inputs, not committed here):

- bootloader.bin: `83e9239d016f18e0b90fb0cac8cbe91cd506fb14852e1b618bc2844579c2a993`
- partitions.bin: `148b959cbff1c38aa8e1d5c0ba9d612c54997b945e56a63f41223eef650653a1`
- firmware.bin: `e0671ff1c67f7acf3fc6cd363aea73b501b86a2b09de514b695ea4d0cf3a0421`

Static comparison builds `cargo +1.99.0 build --release -j2 -p esp32sim --bin esp32sim`
on both revisions and disassembles with
`llvm-objdump -d --demangle --no-show-raw-insn`. Address/alignment and linker-alias
normalization yields 2194 equal functions and six differences, none missing.
Four differences (classic tick, C3/C6 refresh_irq, C3 refresh_pin_lines) are
only constant-pool zero-offset spelling versus an explicit offset: the 16-byte
Mach-O constant payloads were checked equal, with unchanged instruction counts,
opcodes and registers. All 45 selected run/step/tick/deadline functions are
therefore identical apart from verified layout-only constant references.
The two semantic differences are the intended classic I2c<16>::write control
arm and classic SocBus::store, with 37 additional static instructions and
register-allocation changes around its START route selection. MMIO stores are
not claimed instruction-identical. No fields or tick operations were added.
This is static qualification, not a CPU timing result.

Limits: this probe does not qualify complete sensor, RTC/OLED or camera/SCCB
models or establish hardware error-code parity. Modern shared addressing and
transfer timing remain covered by the unchanged workspace regressions.

## Review follow-up qualification

The test/documentation follow-up to reviewed head `84699034` leaves all runtime
Rust unchanged. Native and release WASM Clippy pass with warnings denied;
empty-HOME CI-policy tests pass 941/0/0 and plain empty-HOME tests pass
913/0/47. Production WASM packaging and all eight standard demos pass;
tracked golden inputs/outputs are unchanged. Evidence privacy and diff checks pass.
The restored focused suite passes three hermetic tests (one external ignored),
and all five remaining mutations are killed by named assertion failures.
Separately built release CLI disassemblies versus `84699034` are byte-identical
after removing only the object-path header (2,327 symbol labels); the SHA-256
of each listing is `ac2c427bce8247b633ca7a0da31f68bf65910b2a536e1fe5497f2a481d9452ab`.
Instruction addresses, operands and symbol names are retained in this comparison.

Rebuilding the pinned PlatformIO recipe and running the external routing test
passes its seven expected lines with zero reboots. Bootloader and partition
hashes match the original qualification above. The rebuilt firmware SHA-256 is
`8bfda378d457fd0daa9c884326a1c939bfa2688ccdcc76fd851bcac72427e618`;
compile-time metadata makes this a distinct binary, not a new frozen-input
baseline comparison. The earlier baseline and SSD1306 observations are retained,
not remeasured by this test/documentation follow-up.

## CPU comparison

Maintainer joakimeriksson measured main `ab89dc54` versus PR #226 head
`84699034` on October 14, 2026 ([review and table](https://github.com/mikroverk/esp32sim/pull/226#issuecomment-6178427981)).
User CPU, Apple silicon, 1-minute load 5–7 throughout, seven alternating pairs,
identical instruction counts and console hashes in every pair. No new local
timing is claimed for this test/documentation follow-up; runtime Rust is unchanged.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| classic hello (30 s) | 1.83 | 1.83 | +0.0% | 3 of 7 |
| C3 hello (30 s) | 2.55 | 2.52 | -1.2% | 2 of 7 |
| C6 hello (30 s) | 3.28 | 3.28 | +0.0% | 2 of 7 |
| S3 hello (3000 s) | 25.74 | 25.86 | +0.5% | 5 of 7 |

None is slower in six or more pairs. His eight-PR scratch merge versus the same
main, using the same method, measured classic +0.6% (3/7), C3 0.0% (3/7),
C6 -0.6% (2/7), S3 -1.1% (3/7), Pocket Tank +0.2% (3/7); none flagged.
These are separate combined-stack results, not measurements of this PR alone.
Publication still requires independent exact-head review and green exact-head CI.

Classic `set_pins` remains unconditional at START: it is outside tick/step paths,
harmless without pinned devices, and retaining it avoids runtime/codegen changes
to the measured head. S3/C3 keep their existing `has_pinned_devices` guards.
