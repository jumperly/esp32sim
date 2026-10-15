# Classic Wi-Fi authentication TX lengths (EX226 follow-up)

Base: `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea` (merged upstream main).
Reviewed head: `5d760b85bf27899251ccae2a47894e64f812f45d` (#222).
Maintainer-review follow-up: [receipt](review-follow-up.md).

## Rule and limits

The classic TX adapter follows libpp's general TX convention: descriptor
`length` is the TX byte count, independently of `size`. ESP-IDF v5.5.2
`soc/include/soc/lldesc.h:32` states "Size (or length for TX)";
`hal/dma_types.h:23-24` agrees. This is a public DMA-header rule, not a
special case for any firmware or a new public MAC-register definition.
Ownership, the complete
12-byte descriptor extent and the entire frame's mapped-DRAM extent remain
mandatory. RX capacity checks are unchanged. Multi-descriptor frames, PSRAM DMA
and RF timing are outside this change.

The regression submits a real 30-byte open-system authentication request
through the classic MAC to the shared access point. For capacities 28, 30 and
32 it pins completion, queue control, TX count and the AP's successful
sequence-2 response. Separate negatives exercise CPU-owned descriptors,
length crossing DRAM's upper bound despite size fitting, below-DRAM and
near-u32-limit buffers, and a truncated descriptor. Invalid TX must leave
completion and queue control unpublished. No shared link, bus layout or
other-chip source is changed.

## Checks

These are the original qualification results for `5d760b85`; the
[follow-up receipt](review-follow-up.md) records the rerun for this documentation change.
Rust 1.99.0, serial builds with `-j2` and `nice -n 10`:

- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings`: pass.
- Empty HOME, only `ESP32SIM_ROM_DIR` as emulator input:
  `cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_`:
  939 passed, 0 failed, 0 ignored (includes doc tests).
- Empty HOME, no `ESP32SIM_*` input:
  `cargo +1.99.0 test --release --workspace`: 911 passed, 0 failed, 46 ignored.
- `RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh`: pass.
- `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel`: all eight pass.
- `node tools/check-evidence-privacy.mjs`: pass.
- `cargo +1.99.0 test --release -p esp32 --lib wifi::tests`: six pass.
- Existing goldens and `docs/experiments.md`: unchanged. JIT implementation
  unchanged; the separate JIT-change suite is not applicable.

Fetch inputs in the worktree with `tools/fetch-demo-assets.sh --no-linux`.
Keep `CARGO_HOME` and `RUSTUP_HOME` pointing to installed toolchains when
changing HOME; no firmware input is discovered from the normal home directory.
`tools/wasm-build.sh` expects `target/`, so when setting `CARGO_TARGET_DIR`
use a `target` symlink to that dedicated build directory.

## Mutations

Run `CARGO_TARGET_DIR=<build> python3 docs/evidence/classic-wifi-auth/mutate.py <results>`.
All eight compiled and failed at runtime, rather than failing compilation:

| Replacement | Killing test |
| --- | --- |
| Restore length > size rejection | authentication_tx_reads_full_length_not_buffer_capacity |
| Read size bytes, not length | authentication_tx_reads_full_length_not_buffer_capacity |
| Ignore DMA ownership | invalid_tx_descriptors_never_publish_completion |
| Bound size bytes, not length | invalid_tx_descriptors_never_publish_completion |
| Skip descriptor extent check | invalid_tx_descriptors_never_publish_completion |
| Complete before validating buffer extent | invalid_tx_descriptors_never_publish_completion |
| Omit station delivery | authentication_tx_reads_full_length_not_buffer_capacity |
| Omit TX completion | authentication_tx_reads_full_length_not_buffer_capacity |

## Static comparison

Build both frozen revisions with
`cargo +1.99.0 build --release -j2 -p esp32sim --bins`, using separate target
directories (a shared output path can retain another worktree's top-level
binary even when Cargo says fresh). Disassemble with
`xcrun llvm-objdump -d --demangle --no-show-raw-insn <esp32sim>`.

After normalizing addresses, alignment nops, data-page relocations and
identical-body linker aliases, 2,199 common functions match; one changes:
classic `SocBus::tick`, which inlines `wifi_step`, shrinks from 1,854 to 1,851
instructions. Its first 1,262 instructions match. The active TX descriptor
block loses size extraction and the length/capacity conditional comparison;
remaining differences are register allocation for the frame offset/length.
All 119 selected S3/C3/C6/core/peripheral hot functions match, with none
missing. No fields, tick calls or idle gates are added. This is a static
comparison, not a measured CPU clearance. Artifact identities are in
`static.json`.

## CPU comparison

Measured centrally by joakimeriksson on 2026-10-14, main `ab89dc54` versus
head `5d760b85`, classic hello for 30 s. User CPU on Apple silicon,
seven alternating pairs, 1-minute load 5–7 throughout; instruction counts
and console hashes identical in every pair. The comment does not specify
a warmup for this run. This is the maintainer's measurement, not a local rerun.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| classic hello (30 s) | 1.85 | 1.92 | +3.8% | 4 of 7 |

No slower-in-at-least-six-pairs flag. The load range differs from the
preferred start-below-5 gate; retain that limitation rather than relabeling
this as a quiet-machine run. [Source review and table](https://github.com/mikroverk/esp32sim/pull/222#issuecomment-6177863678).
