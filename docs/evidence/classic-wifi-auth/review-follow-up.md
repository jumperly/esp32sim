# EX226 / #222 maintainer-review follow-up

Previous head: `5d760b85bf27899251ccae2a47894e64f812f45d`.
Base remains `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea`; no rebase.

## Rule and review evidence

Only the TX comment and documentation change. libpp's general TX convention
uses `length` as the TX byte count, independently of `size`:
ESP-IDF v5.5.2 `soc/include/soc/lldesc.h:32` ("Size (or length for TX)")
and `hal/dma_types.h:23-24`. Ownership and descriptor/frame DRAM bounds,
RX capacity validation, and the test-module layout remain unchanged.

[joakimeriksson's review, 2026-10-14](https://github.com/mikroverk/esp32sim/pull/222#issuecomment-6177863678)
reports official MicroPython v1.29.0 (IDF 5.5.2 blob) validation on classic
ESP32 with #228 supplying CLI Wi-Fi attachment. Descriptor length/size:
auth 34/32, null 28/24, ARP 80/78, DHCP 388/386, keep-alive 92/90.
Main sends probes without authenticating; #222 reaches authentication,
association, verified four-way handshake, DHCP, NTP and HTTP 200 through NAT.
The open-AP variant joins too; S3 joins on unpatched main. These are the
maintainer's results, not a local firmware rerun or silicon timing validation.

The original 30-byte authentication regression remains useful as a compact
frame fixture, not as a firmware-specific TX rule. EX226 retains its earlier
results and now links this follow-up. The fork-only implementation SHA is
removed from the README and proposed description.

## CPU comparison

Measured centrally by joakimeriksson, classic hello 30 s, main `ab89dc54`
versus previous head `5d760b85`, user CPU on Apple silicon, seven alternating
pairs, 1-minute load 5–7 throughout, identical instruction counts and console
hashes in every pair. No warmup count is specified in the source comment.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| classic hello (30 s) | 1.85 | 1.92 | +3.8% | 4 of 7 |

No slower-in-at-least-six-pairs flag. Preserve the measured load range;
this is not claimed as a start-below-5 quiet-machine run. The table measures
the previous head; the follow-up changes no executable Rust or test code.

## Follow-up verification

Commands use Rust 1.99.0, `nice -n 10`, two build jobs and dedicated,
separate target directories for the previous and follow-up revisions.
Fetch firmware inside each worktree with `tools/fetch-demo-assets.sh --no-linux`.
For empty-HOME tests, preserve installed `CARGO_HOME` and `RUSTUP_HOME`.

- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings`: pass.
- Empty HOME, only `ESP32SIM_ROM_DIR=<worktree>/web/wasm/fw` as emulator input:
  `cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_`:
  939 passed, 0 failed, 0 ignored, including doc tests.
- Empty HOME, no `ESP32SIM_*` inputs:
  `cargo +1.99.0 test --release --workspace`: 911 passed, 0 failed, 46 ignored.
- `RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh`: pass.
- `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel`: eight pass.
- `CARGO_TARGET_DIR=<build> python3 docs/evidence/classic-wifi-auth/mutate.py <results>`:
  all eight compiled mutations killed at runtime by the designated tests in the original receipt.
- Restored source: `cargo +1.99.0 test --release -p esp32 --lib wifi::tests`: six pass.
- `node tools/check-evidence-privacy.mjs` and `git diff --check`: pass.
- Existing golden files remain byte-identical; only EX226 changes in the experiment catalog.
  No JIT implementation changes; the JIT-change suite is not applicable.

## Previous-head static comparison

Build both trees with `cargo +1.99.0 build --release -j2 -p esp32sim --bins`
in separate target directories. Run
`xcrun llvm-objdump -d --demangle --no-show-raw-insn <esp32sim>` for each,
then `python3 docs/evidence/classic-wifi-auth/compare.py <previous.dis> <follow-up.dis>`.
Result: `identical=2200 different=0 only_main=0 only_branch=0`.
All common functions, including every hot path, match the previous head.
The original comparison against main remains in the original receipt and `static.json`;
no new comparison against a moving main or new CPU run is claimed.
