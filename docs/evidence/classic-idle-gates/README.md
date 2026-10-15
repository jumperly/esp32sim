# Classic idle adapter gates

Base: `ab89dc543bd8aa23b2d39f2cc48348ea5a0793ea` (upstream main after #212).
The per-part counters below are deterministic work attribution, not CPU percentages.
The CPU comparison separately credits the maintainer's central timing run.
The workload is the committed classic hello fixture, ECO3 ROM, esp32dev, UART0,
30 emulated seconds including two resets, no Wi-Fi or BLE configuration.
Input hashes and build/disassembly hashes are in `manifest.json`.

## Integration after #230 (October 15, 2026)

Replayed the three reviewed #224 commits onto #230's accepted
`377716b8e39103711bf9ba27e3e33722410461e1`, then integrated main `f5a038bf`
(including #227, #228 and #229). The two tick-site conflicts retain both behaviors:
manual LEDC/RMT advancement remains gated by clock/pending work, while #230's
before/after source comparisons and routed-GPIO comparison remain live. The
Wi-Fi call stays gated, and the bus returns #230's `irq_dirty` result rather than
requesting unconditional interrupt presentation. The Rust delta of the replayed
commits is identical to reviewed #224 except for two redundant clock comments;
#230's source-transition comments are retained instead.

Main's EX223–EX229 table move is preserved. EX028 retains the complete #230
history and maintainer table; EX229 retains this PR's attribution and CPU table.
All other catalog rows match main byte-for-byte. Historical measurements below
still refer to their stated revisions; this integration has no new CPU timing.
In particular, the 9,639,235-call attribution is the original instrumented run,
not a fresh count on the event-driven stack.

Fresh Rust 1.99.0 verification of the integrated tree: both Clippy gates pass;
empty-HOME CI-policy workspace tests pass 963 (18 filtered), plain empty-HOME
workspace tests pass 935 (46 ignored); production WASM, all eight standard demos
and the JIT shared-memory handoff pass. No golden, fixture or JIT source changes.
All eight gate mutations are assertion-killed after integration; all 25 parent GPIO
and 11 parent event mutations are also assertion-killed, with no compiler failures
counted. The restored classic suite passes 137 tests. Native classic
hello still matches exact #230 at core0 16,672,061 / core1 4,515,255 instructions,
27,408 exceptions, 6,016 interrupts and console SHA-256
`13aadb96ac2d9e7d827e9daea68ec3f5dd70de46bb0c2eb60ee2eaad4f7539ec`.

Fresh release disassembly versus exact #230 selects 27 classic functions: 19
identical, the gated bus tick 2,308 → 2,310 static instructions, five Machine
functions differing only by +16-byte field-address immediates (same opcodes,
registers and order), and two newly out-of-line ClassicSpi read/write functions
from main's #227 fix. LEDC/RMT engine and GPIO cache bodies remain identical.
The shared/S3/C3/C6 selection is 118/119 identical: C3 periph_write has two byte
loads at offsets +1 because main #227 adds a SpiMem field. A separate release
build of exact #230 merged with `f5a038bf`, without #224, reproduces those C3
changes; all 119 functions match that integrated control. These are structural
comparisons, not timing measurements, and do not establish CPU parity of the
new stack. Original CPU attribution and measured tables remain unchanged.

## Observed per-part attribution

Denominator: 9,639,235 calls of the classic bus tick, identical on base and candidate.
The temporary counters count adapter entries separately from entries into the actual
clocked-work body. Instrumentation is restored before normal builds and tests.
`base-attribution.json` and `candidate-attribution.json` retain all counters.

| Part | Base adapter calls | Candidate adapter calls | Clocked-work entries, both | Observed gates |
| --- | ---: | ---: | ---: | --- |
| Wi-Fi | 9,639,235 | 0 | 0 | Combined WIFI_CLK_EN / MAC-reset predicate enabled on 0 ticks |
| LEDC | 9,639,235 | 0 | 0 | Cached DPORT clock/reset predicate enabled on 0 ticks |
| RMT | 9,639,235 | 0 | 0 | Cached DPORT clock/reset predicate enabled on 0 ticks |
| I2S (two-port inline loop) | 9,639,235 | 9,639,235 | 0 port-work entries | I2S0 clock on for 94,288 ticks; I2S1 clock on for 0; each reset asserted for 4,717 ticks; no DMA running |
| ADC | 0 tick entries | 0 tick entries | 0 conversion calls | RTC/SENS access-driven, not a DPORT tick device |
| DAC | 0 tick entries | 0 tick entries | 0 output queries | RTC_IO power/mux access-driven, not a DPORT tick device |
| Touch | 0 tick entries | 0 tick entries | 0 measurement calls | RTC/SENS access-driven, not a DPORT tick device |
| VHCI glue | 0 intercept entries | 0 intercept entries | 0 | Opt-in controller glue; no bus tick hook or DPORT gate |

ADC/DAC/touch/VHCI entries are counted at their real access/intercept entry points,
not inferred by assigning every new adapter a bus tick. The ADC SENS device is an
alias in the dispatch table and has no clock() implementation.

## Explanation and scope

Wi-Fi and LEDC/RMT already returned internally when gated off. They were not doing
frame processing, timer advancement or channel advancement in this workload.
This gating is a tidy-up with a small measured gain, not the regression mechanism
behind the classic hello slowdown (see the maintainer's CPU comparison below).
Wi-Fi read the DPORT register map once
per idle tick; I2S read it for both ports even with no DMA pending. LEDC/RMT were
called after Dispatch on every tick regardless of their clock()/pending-work result.

The fix caches Wi-Fi and I2S gate predicates only on DPORT writes, leaves the existing
LEDC/RMT gate cache intact, and uses the adapters' clock() predicate for manual APB
delivery. The idle workload avoids 28,917,705 Wi-Fi/LEDC/RMT adapter calls. The inline
I2S loop remains, but checks cached booleans instead of reading the register map;
its work body requires both an enabled, non-reset port and a running DMA channel.
This also fixes the missing I2S reset check. Public bit definitions:
ESP-IDF v5.5.4 `components/soc/esp32/register/soc/dport_reg.h`, clock bits at
947/965, reset bits at 982/1000, Wi-Fi mask at 1040-1043 and MAC reset at 1064/1069.
LEDC/RMT output synchronization and completed-output delivery are retained even with
clocks off, so already queued GPIO/RMT changes are not lost.

The call counts alone cannot apportion or establish recovery of the slowdown.
The centrally measured table below finds only a small gain. DMA reach rules are
unchanged. No hardware equivalence beyond the existing model is claimed.

I2S freezes rather than resets its DMA state while DPORT RST is asserted. That
state-retention behavior is pre-existing; this patch adds the missing reset gate,
not a DMA reset implementation. As noted in the maintainer review, IDF asserts
I2S reset only at channel allocation. No DMA-state behavior change is made here.

## Guest equivalence

Uninstrumented native classic hello at 30 seconds, both revisions:
core0 16,672,061 instructions; core1 4,515,255 instructions;
27,408 exceptions; 6,016 interrupts; console SHA-256
`13aadb96ac2d9e7d827e9daea68ec3f5dd70de46bb0c2eb60ee2eaad4f7539ec`.
The counter-instrumented runs give these exact same counts and hash.

`demo-comparison.json` records exact instruction counts and console hashes for the
entire standard WASM set: hello, c3-hello, c6-hello, c6-energy-scan, c6-contiki,
c6-contiki-net, c6-rpl-net and panel. All eight match main; the two network manifests
include each node's instruction count and console hash. Only the evidence build
adds a read-only net_stat index 7 to expose existing instruction counts; it is not
part of the patch or the shipped ABI. Binary frame counts and network TX/RX also match.
All native golden tests pass against unchanged committed goldens, with no UPDATE_GOLDENS.

## Mutation checks

`mutate.py` changes one gate, runs its named test, requires an assertion failure
(not a compiler failure), then restores the source. All eight mutations were killed.
The restored esp32 suite passes: 94 unit tests, 15 peripheral tests and 8 TLS tests.

| Mutation | Killing test |
| --- | --- |
| Remove LEDC call-site clock/pending predicate | classic_idle_gates_skip_adapter_calls |
| Remove RMT call-site clock/pending predicate | classic_idle_gates_skip_adapter_calls |
| Remove Wi-Fi bus call-site gate | classic_wifi_clock_and_reset_skip_step |
| Ignore Wi-Fi clock mask | classic_wifi_clock_and_reset_skip_step |
| Ignore Wi-Fi MAC reset | classic_wifi_clock_and_reset_skip_step |
| Ignore I2S port clock bit | classic_i2s_clock_reset_and_pending_gates |
| Ignore I2S port reset bit | classic_i2s_clock_reset_and_pending_gates |
| Ignore I2S pending-DMA predicate | classic_i2s_clock_reset_and_pending_gates |

Counters in adapter structs are cfg(test)-only: no production counter fields or increments.

## Checks

All use Rust 1.99.0 and nice -n 10 with CARGO_TARGET_DIR outside the control checkout.

- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings`: PASS.
- Empty HOME, only ESP32SIM_ROM_DIR for fixture discovery, `cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_`: 941 passed, 18 filtered, no failures.
- Empty HOME and no ESP32SIM_ROM_DIR, `cargo +1.99.0 test --release --workspace`: 913 passed, 46 ignored, no failures.
- `RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh` and `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel`: PASS, including JIT handoff.
- `node tools/check-evidence-privacy.mjs`: PASS; curated evidence manually checked too.
- `git diff --check`: PASS. No JIT source change; separate JIT differential suite not required.

The WASM build script assumes ./target for its copy; ./target is a symlink to the
external CARGO_TARGET_DIR. No build occurs in the control checkout.

## Hot-path disassembly

Build both revisions with `cargo +1.99.0 build --release --bins`, then
`llvm-objdump --disassemble --demangle --no-show-raw-insn` on each esp32sim binary.
Use the committed classic-core asmcmp.py normalization through `disasm.py`.
`classic-hot.json` lists 27 selected functions: 21 identical, six changed.
The 119-function S3/C3/C6/shared selection from classic-core/hot.py is entirely
identical (119 same, 0 different, 0 missing).

Classic bus tick is 1,854 static instructions on both revisions, but not identical:
manual LEDC/RMT calls are now conditional on clock()/pending work, and the inlined
Wi-Fi/I2S paths use cached booleans instead of DPORT map lookups. Register allocation
changes around those predicates. Static function size does not measure the executed
idle path; the call counters establish which engines are actually entered.
The other five changed functions (after_round, run_modeled, run_unmodeled,
settle_modeled_time and step_core) preserve every opcode/register/order and differ
only by +16-byte field offsets after the enlarged two-port I2S adapter array.
The cached flags are added to adapter tails, not ahead of core fields; Rust's layout
still enlarges the enclosing peripheral block. LEDC/RMT engine and output-sync
function bodies remain identical. Full raw disassemblies/diffs are retained outside
Git; their hashes are in manifest.json, and the scripts reproduce the comparison.

## Review requalification (October 14, 2026)

The correction to the attribution and catalog is documentation-only on top of
`626c8b2fcea3da609f0a8c8ae265576033750f38`; no Rust, golden, fixture or dependency
changes. The required checks above were rerun: both Clippy commands pass,
empty-HOME CI-policy workspace tests pass 941 (18 filtered), plain empty-HOME
workspace tests pass 913 (46 ignored), production WASM and all eight demos
including JIT handoff pass, all eight gate mutations are assertion-killed, and
restored-source classic tests pass 94 unit + 15 peripheral + 8 review-core.
Privacy and whitespace checks pass. No JIT source changed.

Fresh `cargo +1.99.0 build --release --bins` builds of the previous head and the
documentation-corrected tree, followed by the objdump command above, give
identical complete instruction listings: only objdump's input-path header differs.
`disasm.py` finds 27/27 classic functions identical; classic-core/hot.py finds
119/119 shared/S3/C3/C6 functions identical, none missing. This is a comparison
against the previous PR head, not a replacement of the original main comparison.
`review-codegen.json` records artifact hashes; binaries can differ in embedded
build-path data without differing in these instruction listings.

## Reproduction

Set EVIDENCE_OUT to a writable external evidence directory. Scripts are sequential:
they temporarily instrument/restore this worktree and must not overlap builds here.
Run attribute.py WORKTREE base, then attribute.py WORKTREE candidate; run mutate.py
WORKTREE and compare_demos.py WORKTREE. compare_native.py expects uninstrumented
binaries under EVIDENCE_OUT/target-base/release and EVIDENCE_OUT/target/release.
disasm.py expects base.dis and candidate.dis under EVIDENCE_OUT. Fetch assets with
`tools/fetch-demo-assets.sh --no-linux` in each worktree before running firmware.

## CPU comparison

Measured centrally by joakimeriksson in the [October 14, 2026 review of
`626c8b2f`](https://github.com/mikroverk/esp32sim/pull/224#issuecomment-6177864525),
against main `ab89dc54`. These are his measurements, not a new local timing run.
User CPU on Apple silicon, 1-minute load 5–7 throughout, seven alternating pairs,
identical instruction counts and console hashes in every pair. The review does
not specify a warmup; no warmup or stricter load-start condition is inferred.

| Workload | main median (s) | PR median (s) | Change | PR slower in |
| --- | ---: | ---: | ---: | ---: |
| classic hello (30 s) | 1.89 | 1.86 | -1.6% | 2 of 7 |
| C3 hello (30 s) | 2.64 | 2.65 | +0.4% | 4 of 7 |

Neither workload is slower in six or more pairs. His separate scratch merge of
all eight open PRs against the same main, with the same method, measured classic
hello +0.6% (3/7 slower), C3 hello 0.0% (3/7), C6 hello -0.6% (2/7), S3 hello
-1.1% (3/7), Pocket Tank +0.2% (3/7). Those combined results do not isolate this PR.

### Regression attribution and follow-up

The maintainer reproduced the earlier slowdown on pristine #206 `bb2b33d5`
versus pristine main: 1.730 → 1.920 s, +11.0%, slower 5/5. This gating recovers
about a seventh of that increase, not the regression as a whole.

His `sample` profiles on #206, main and this head put `SocBus::tick` at about 4%
of samples and `ClassicGpio::raw_status` at about 80%. The latter scans 40 pins
through pad-level/driven resolution on every round via
`present_irqs → source_status → cpu_lines`.

His per-merge bisect (median of five, identical instruction counts) was
non-monotonic: #206 1.70 s, #207 1.79 s, #208 1.73 s, #209 1.80 s, #210 1.91 s,
#211 2.13 s, #212 1.85 s. #211 adds the BLE adapter with no tick hook. Together
with the profile, this supports code-layout-sensitive cost of the hot GPIO scan,
not attribution to unconditional idle adapter entry.

Caching the GPIO interrupt status, invalidated on GPIO/IO_MUX/RTC-pad writes and
input edges, is the real recovery identified by the maintainer and is the separate
follow-up [#230](https://github.com/mikroverk/esp32sim/pull/230). This PR does not
implement that cache. The historical browser
GPIO-summary attempt in EX028 is not evidence against this native classic finding;
the workload, adapter and measurement contract differ.
