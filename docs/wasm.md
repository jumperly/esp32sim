# Running the emulator in the browser (WebAssembly)

The whole emulator — both Xtensa cores, the SoC, the boards, the virtual WiFi and subnet —
compiles to a single WebAssembly module and runs inside the page, in a Web Worker. Nothing is
uploaded anywhere: firmware is read from the visitor's disk (or fetched from files you host next
to the page) and executed in the tab.

## Build and try it

```sh
tools/wasm-build.sh                      # -> web/wasm/esp32sim.wasm (needs the wasm32-unknown-unknown target)
tools/fetch-demo-assets.sh               # mask ROM ELFs, xterm.js, the Linux image (--no-linux skips its 16 MB)
python3 -m http.server -d web 8790       # any static server; file:// will not do (workers, fetch)
open 'http://127.0.0.1:8790/run.html?wasm&fw=hello'
```

Every demo in `web/wasm/fw/demos.json` boots from the mask ROM, which is not committed: without the
fetch the page stops at `esp32s3_rev0_rom.elf: 404`. xterm.js is only for the Terminal tab.

`?wasm` switches `web/run.html` from its WebSocket transport to the worker; the page gains a
firmware panel: board, flash/PSRAM size, an optional WiFi spec, function stubs, and file inputs
for the mask-ROM ELF, `bootloader.bin`, `partition-table.bin`, the app image, its ELF (symbols
— needed for stubs) and a script. **Boot** starts it; the rest of the page — console tabs,
display, touch, buttons, knob, audio, camera — is the same UI the native emulator serves.

For your own demos, `?wasm&fw=<name>` loads `web/wasm/fw/<name>.json` and boots it without
clicking (format in `web/wasm/fw/README.md`). Everything in that directory except the manifests
and `public/` (our own demo firmware) is git-ignored: the mask ROM is Espressif's and other firmware
is whoever built it; host them only where you may.

## On GitHub Pages

`.github/workflows/pages.yml` builds the module on every push to `main`, runs
`tools/fetch-demo-assets.sh` (the mask-ROM ELFs from the Apache-2.0 `espressif/esp-rom-elfs` release,
xterm.js, the Linux image), and publishes `web/` — so the page at
**https://mikroverk.github.io/esp32sim/** is the emulator, with the demos in
`web/wasm/fw/demos.json` — hello_world, the Touch-LCD-4B energy panel with its SID player, the Atech
Pocket Synth, and the C3 and C6 demos — one click away and the file inputs for anyone's own firmware. It
also runs `tools/fetch-pocket-tank.sh` for the **pocket-tank** demo ([mediacutlet/pocket-tank](https://github.com/mediacutlet/pocket-tank),
MIT): it checks the committed bootloader, partition table and app in `web/wasm/fw/public/` (the bytes of
the project's browser installer, whose host answers GitHub's runners with something else) and fetches the
7.56 MB model from the repository, all pinned by SHA-256. The workflow also fetches the **Linux-on-esp32-S3** flash image
(GPL-3.0, [svermigo/Linux-on-esp32-S3](https://github.com/svermigo/Linux-on-esp32-S3), release 0.7,
pinned by commit and SHA-256 in `pages.yml`) so the `linux` demos boot it — `linux-term` opens on the
xterm.js terminal tab (manifest `terminal: true`), where `vi`, `top` and colours render properly; the image is never
committed here — the source for everything in it is that repository. On a `github.io` host the page starts in wasm mode without
`?wasm`. Firmware committed under `web/wasm/fw/public/` is ours, except pocket-tank's three MIT parts next to its license; the panel is a
separate build with placeholder `secrets.h` values (checked with `strings` against the real ones).

**Demo data without a rebuild.** The panel firmware has a `demo` data partition (0x610000,
64 KB); when it holds a JSON document the firmware renders that — prices for today and
tomorrow, hourly kWh, tile states, header power, a fixed clock — and never starts WiFi. The
manifest writes `public/energydata.json` there (`flash_at`), so changing the demo is editing a
JSON file; real boards have the partition erased and behave as before. Natively:
`--flash-at 0x610000=web/wasm/fw/public/energydata.json`.

## What it is

`tools/wasm-test.mjs` runs the built module under Node through the same manifests the page
uses and fails on a panic or a missing console line; CI runs it after the goldens.

Every chip is in the one module: `esp32sim_new` takes a board name, and `esp32c3` or `esp32c6`
builds the RISC-V machine instead of the Xtensa one; `esp32` or a classic board name such as
`ttgo-tdisplay` builds the classic ESP32. The C3 has no `WebServer` of its own — it is
console-only — so the wasm layer turns its console into the same `{"t":"serial"}` messages the
S3 sends, and `esp32sim_cpu_hz` tells the worker which clock to pace against (240 MHz vs 160).

```
web/run.html     the UI, unchanged; `link` is either a WebSocket or the worker
web/emu.js       page side: firmware panel, manifest loading, window.EmuLink
web/wasm/worker.js   owns the wasm instance, paces it to wall time, relays the UI protocol
wasm/            the crate: a C ABI over the S3, C3 and C6 machines (esp32sim_new / load / wifi / stub /
                 boot / run / out_* / in_*); no bindgen, no dependencies
```

Inside the module the machine talks to the page through the same `WebServer` the native build
uses, in **queue mode**: every `send_text`/`send_binary` lands in an outbox the worker drains
after each run slice (`docs/web-ui.md` is the protocol on both sides). The worker keeps the
machine at wall time: it computes the cycle count the clock has earned, runs in ≤2 M-cycle
slices, yields every 25 ms so frames and audio flow, and resynchronises instead of bursting if
the tab falls half a second behind. `Date.now()` is passed in for the emulated SNTP server.

## What works, measured (M-series Mac, Chrome)

| firmware | in the tab | notes |
| --- | --- | --- |
| IDF hello_world | real time | ROM → bootloader → app, `esp_restart` reboots through the ROM |
| **Linux 6.11 on the ESP32-S3** (svermigo/Linux-on-esp32-S3) | real time, ~4× under Node | the esp-hosted network adapter on core 0, Linux XIP from flash on core 1, cramfs root, jffs2 overlays; login and a shell typed on the page console (UART0). Needs the `wifi` spec (PHY calibration) and a stub on `nimble_port_init` — no Bluetooth baseband is modelled, so BLE provisioning is skipped |
| Waveshare Touch-LCD-4B energy panel + SID player | **real time**, ~62 Minsn/s | LVGL at 60 fps, touch, the tune plays through WebAudio |
| Atech 14-port synth | real time | ST7735 and WS2812 decoded, buttons/knob, scripted scenario |
| ESP32-C3 hello_world | real time | the other chip: one RV32IMC core, console only — pick board `esp32c3` |
| ESP32-C6 hello_world | real time | the newest chip: one RV32IMAC core, console only — pick board `esp32c6` |
| LilyGO TTGO T-Display spot prices (MicroPython) | real time, ~3× under Node | the classic ESP32: stock MicroPython v1.29.0 booted by the ROM from the combined image, the app from a LittleFS image in the vfs partition, the ST7789 135×240 over VSPI; demo data, no Wi-Fi — board `ttgo-tdisplay` |
| ESP32-C6 802.15.4 energy scanner | real time | the Waveshare ESP32-C6-LCD-1.47: LVGL spectrum on the ST7789 over SPI2+GDMA, WS2812, energy detect from the MAC model's moving 2.4 GHz picture; BOOT on the page — board `waveshare-c6-lcd147` |
| ESP32-C6 Contiki-NG | real time | Contiki-NG as an unmodified IDF app: its own scheduler and 802.15.4 stack over the emulated MAC |
| …two of them on one medium | real time | a manifest with a `nodes` array boots a network through `esp32sim_net_*` instead of `esp32sim_new`: several motes, one medium, no simulator behind them |
| …an RPL/IPv6 network | real time | rpl-udp server (the DAG root) and client: the client joins, then UDP request and reply every 10 s over 6LoWPAN, RPL Lite, CSMA and hardware ACKs |

The browser's Xtensa block scheduler can compile hot integer/branch/memory blocks into
additional WASM modules. After 32 executions, an eligible block is installed in the emulator's
exported function table. Subsequent calls stay inside WASM; JavaScript compiles and retires
modules but does not dispatch each block. Calls have generated WASM paths; window overflow
and illegal calls retain the existing exception handling. A supported prefix ending in a
return can also be compiled: the return uses the existing interpreter helper, preserving
its window and exception semantics. Other unsupported operations keep their block interpreted.
Unaligned, unmapped, read-only and peripheral accesses use the ordinary bus helper.

Compiled WASM blocks can survive decoded-arena turnover. Reuse requires matching every decoded
instruction, the block length and the fast-memory contract. At each arena flush, retention is
limited to the two most recent decoder generations, 16,384 blocks and 64 MiB of emitted WASM
per core. New code can grow beyond those retention limits between flushes; engine-generated
machine code and other browser allocations are additional. The host reports peak live emitted
bytes and module counts to make this tradeoff measurable.

Whole-block calls use a separate path when entry checks prove there can be no register-window
collision and no active loop end within the block. That path omits per-instruction entry,
budget, overflow and loop-end tests. Generated code loads only the block's operand registers
and computes register-window collision state once per entry. Budget cuts, resumptions and
states that fail either guard keep their checked path.

Once a block is hot, the emitter also tries to form a *region*: the block plus the blocks
reachable from it over statically known edges (fallthrough, conditional-branch target, `J`,
and the backedge of a hardware loop set up inside the region), compiled as one function.
Formation starts with at most 8 chunks, 64 instructions and 4 code pages; splitting at
hardware-loop ends can add chunk boundaries without adding instructions. Guest registers stay in WASM locals
across the internal edges; continuing inside the region checks the remaining instruction
allowance and whether a helper or code-page store requires an exit. Anything that
could make a block boundary observable leaves the region instead: interpreter helpers and
stores into one of the region's own code pages make the next chunk head exit, calls, returns
and computed jumps end a chunk, and entry checks cover page versions, probe boundaries, an active hardware loop,
and window and coprocessor state. A region's continuation
exits carry the next PC and are never mid-block cuts, so budget cuts inside a chunk still go
through the chunk's own block module. A dispatch at any chunk head of a live region enters the
region at that chunk, and a head already inside a live region does not get a region of its
own. `ENTRY` may head a region; the window proof is redone after the rotation. The profile
build (`jit-profile`) reports regions formed, entries, generated-entry rejects and instructions retired
per core.

The PIE (coprocessor 3) instructions the TinyDraw tile kernels use are emitted on WASM SIMD:
aligned 128-bit load and store with post-increment, lane compares, the bitwise q-register
operations, 32-bit lane insert and zeroing. So is the dot product of on-device inference
(pocket-tank's 4-bit matmul): signed 8- and 16-bit multiply-accumulate into ACCX, with and
without its load, the ACCX reset, and the RUR of ACCX_0/ACCX_1 that follows each dot product.
The products are widening vector multiplies with 64-bit lane sums, and ACCX is updated with
the interpreter's 40-bit saturation. Q registers stay in CPU memory as `v128` values; generated
functions have one `v128` and one `i64` scratch local. The CP3-disabled check is proved once per
body next to the FP one. Other PIE instructions keep the interpreter, and a block containing
any of them stays interpreted.

Compiled execution uses the same instruction-count timing as the default block interpreter.
Timer budgets, interrupts, loop ends, code-page versions and observer boundaries still bound
execution. This does **not** extend the receipt-based cycle model or establish cycle accuracy.
The earlier `esp32sim_jit_prepare/commit` experiment remains available for its synthetic test;
the page no longer uses it to run firmware.

Custom WASM hosts must provide `host_jit_compile` and `host_jit_release` from
`web/wasm/jit.mjs`, alongside `host_log`. The module exports `__indirect_function_table`.
Append `&jit=0` to the page URL, or configure `esp32sim_set_jit(emu, 0)` before
running, to compare with the interpreter;
`esp32sim_block_jit_insns(emu)` counts retired instructions through compiled blocks, including
memory helpers. `createJitHost` exposes compilation, failure, release and compile-time counters.
`ESP32SIM_NO_WASM_JIT=1 node tools/wasm-test.mjs ...` exercises the interpreter on the same build.

`tools/wasm-jit-test.sh` builds a separate test-enabled module and runs generated-code
comparisons with the interpreter under Node, including budget/resume, timers, register windows,
loops, memory faults and code invalidation. CI runs it alongside the firmware smoke tests.
It does not overwrite `web/wasm/esp32sim.wasm`.

An optional `jit-profile` feature adds statistical block profiling to the WASM build. Its
host must supply `env.host_profile_now`, a monotonic millisecond clock (for example,
`() => performance.now()`). Call `esp32sim_profile_report(emu)` to emit per-core TSV through
`host_log`: sampled PCs, compiled/interpreted status, instruction counts, elapsed samples,
unsupported operations and block operations. Blocks are sampled with probability 1/4096
using a pseudorandom sequence. PC rows describe the first sampled decoded shape; profiles
of self-modifying code may combine multiple shapes at the same PC. Counts include trap
iterations and compiled helper execution, so they are estimates rather than exact opcode
retirement counts. Elapsed samples include lookup and dispatch but exclude the outer SoC
scheduler. Clock-call overhead and quantization can dominate these short intervals; do not
convert their sum into a wall-time breakdown. Use uninstrumented runs to measure speed.
Normal builds contain neither the sampling code nor the clock import.

## Limits

- **No sockets.** The browser has none. With a `wifi=` spec the firmware still associates,
  gets a DHCP lease, resolves names and syncs time against the emulated subnet, but connections
  past the gateway are refused (the `--net none` behaviour) — unless the page carries them itself
  over the [host network](#host-network). The [host Ethernet relay](#host-ethernet-relay) below lets a page
  carry the guest's frames to a host helper instead.
- **No file outputs**: `--wav`, `--tft-png`, register traces — the page is the output.
- **Emulator log lines** (`[emu] …`) that the native build prints to stderr do not exist here,
  except the ones the wasm glue forwards (stubs, resets, load errors) to the console tab and the
  browser console.
- **Memory**: flash + PSRAM + SRAM + ROM plus the block caches; the panel configuration takes
  ~45 MB of wasm memory. The block tables are sized smaller than natively (`block.rs`).
- Audio needs one click on **enable audio** — browsers will not start WebAudio otherwise.

## A network of motes

A manifest with a `nodes` array boots a *network* rather than a single machine: the page calls
`esp32sim_net_new` and, per node, `esp32sim_net_add` (its MAC, board, position and power-on
offset) and `esp32sim_net_load`, then `esp32sim_net_run` advances the whole network to a point in
network time while `esp32sim_net_console_take` and `esp32sim_net_stat` report what each mote did.

```json
"nodes": [ { "mac": "02:00:00:00:00:01", "start_ms": 0,    "x": 0, "y": 0 },
           { "mac": "02:00:00:00:00:02", "start_ms": 1300, "x": 2, "y": 0 } ]
```

The medium and the lock-step are in the module (`esp32c6::net`), not in the worker, for the same
reason the Cooja front end keeps them in Rust: `Machine::run_until_cycle` stops at the instruction
that starts a transmission and `SocBus::radio_receive` puts a frame on the air at its first
preamble byte, so the exactness is already there and a native test can hold it to it
(`esp32c6/tests/net.rs`). The worker only paces network time to the wall clock and relays.

A node may carry its own `files` and `symbols`: an entry of a kind the node names replaces the
shared one, so a root and a client — two images with two `bb_init` addresses — share one
bootloader and partition table and differ only in `app`.

`start_ms` is not cosmetic. Two identical images booted at the same instant are deterministic to
the cycle, so their application timers never drift apart: every broadcast is sent while the other
mote is transmitting, and nothing is ever heard. Real motes are staggered by their power-on; here
it has to be said out loud.

The browser build script defaults to LLVM inline threshold 4000, selected by
[EX154](experiments.md#ex154) on Rust 1.98.1. Set `RUSTFLAGS` explicitly to override
it; `RUSTFLAGS='' tools/wasm-build.sh` uses the compiler's default threshold.
The script leaves the Rust-produced WASM unchanged unless `WASM_OPT=1` is set,
which requests Binaryen `wasm-opt -O3` and requires that tool to be installed.
The measured configuration used no Binaryen postprocessing. `CARGO_ENCODED_RUSTFLAGS`,
when set, takes precedence over `RUSTFLAGS` under Cargo's usual rules.

## Host Ethernet relay

S3, C3 and C6 provide an optional Ethernet transport behind their virtual AP.
Configure the AP with `esp32sim_wifi`, then call `esp32sim_ethernet_relay(e, 1)`
before boot. Association and WPA2 still run in the emulator; the host supplies
the network behind the AP, including DHCP. This mode bypasses the built-in
`VirtualNet`. With relay disabled, the existing virtual subnet remains the default.
Native Rust hosts use `SocBus::set_ethernet_relay`, `take_ethernet_frames` and
`receive_ethernet_frame` for the same operations.

| Export | Contract |
| --- | --- |
| `esp32sim_ethernet_relay(e, enabled)` | Nonzero selects relay, zero restores the built-in network; returns 0 on success, 1 on error. |
| `esp32sim_ethernet_take(e)` | Drain TX in FIFO order; returns the number of frames in the new batch. |
| `esp32sim_ethernet_ptr(e, index)` | Pointer to a drained frame; null for an invalid index. |
| `esp32sim_ethernet_len(e, index)` | Frame length; zero for an invalid index. |
| `esp32sim_ethernet_receive(e, ptr, len)` | Copy an inbound frame; returns 0 if queued, 1 if rejected. Requires an AP and relay mode. |

Frames contain Ethernet headers and payloads without FCS, from 14 through 1518
bytes. Each direction holds at most 64 queued frames in relay mode. Excess TX
frames are dropped and counted in the WiFi link's `tx_dropped` (`StationLink`); a full RX queue rejects injection. Delivery can drop frames
when the guest has no free receive descriptor. Drain after each run slice and
copy bytes before the next `ethernet_take`, which replaces the entire batch.
Recreate JavaScript memory views after calls that may grow WASM memory.

Changing modes clears pending Ethernet queues; selecting the same mode leaves
them intact. Configure transport before starting guest connections. Switching
modes does not migrate DHCP leases or TCP connections. Chip reset clears pending
frames but preserves the AP, network object and relay selection. A drained WASM
batch remains readable until the next `ethernet_take` or emulator deletion.

The [Arduino relay check](evidence/ethernet-c3-2026-10-02/relay.mjs) shows the ABI
with a host DHCP responder.

Camera frames use `esp32sim_in_bin` type 3: `[3, width u16le, height u16le, RGBA...]`.
The entire message must fit 8 MiB, with nonzero dimensions and exact pixel length.
The receiver copies the message before returning and applies it at the next run
boundary. Sensor SCCB settings select output format and geometry. A board without
a camera emits `{"t":"camera-error","message":"no camera on this board"}`.

### Virtual BLE controller (Jumperly patch J23)

The CLI's `--ble` in the browser, on the S3, the C3 and the classic ESP32. `esp32sim_ble_virtual(e, elf, len)` before
boot: the application ELF (its symbols; debug info is not needed) — 0, or 1 for another chip, after
boot, or an ELF without the controller symbols (logged). `esp32sim_ble_virtual_command(e, cmd, len)`: a
command for the virtual peer now — `connect`, `disconnect`, `discover`, `read HANDLE`, `write HANDLE
HEX` (at most 512 bytes; long values as prepared writes, J24), `subscribe CCC_HANDLE`; 0, or 1 (no virtual controller, a bad command: the reason is the
next log line, `error …`). `esp32sim_ble_log_take(e)`: the oldest log line into the buffer (its
length, 0 = none) — `t=<seconds>s <text>`, the lines the CLI prints as `[ble]` (advertising, the
connection and its MTU (`mtu N`, J24), discovered services / characteristics / attributes, reads, writes, notifications, ATT
errors, unsupported HCI opcodes); at most 1024 wait, older ones are dropped.

`esp32sim_ble_nearby(e, ptr, len)` (Jumperly patch J25): the Bluetooth devices around the user,
heard by the guest's scans (not connectable), replacing the ones there were. Entries of `[addr 6,
least significant byte first][addr_type u8: 0 public, 1 random][rssi i8][adv_len u8][adv][rsp_len
u8][rsp]`, the data at most 31 bytes each, at most 64 devices (`len` 0: none). Each scan of the
guest — and a running one when the list changes — gets an LE Advertising Report per device
(ADV_IND; SCAN_RSP too for an active scan, an empty one included). 0, or 1 without the virtual
controller or for bad data.

`esp32sim_ble_air(e, addr)` (Jumperly patch J26): Bluetooth LE between the boards of a project.
`addr` points to the board's public address (6 bytes, least significant first), which READ_BD_ADDR
answers; after `esp32sim_ble_virtual`, before boot; 0, or 1 without the virtual controller. Then
LE Create Connection to an address other than the peer's is the host's to answer: public (another
board) or, Jumperly patch J27, random (a device around the user that the host connects; LE
Connection Complete carries the address type asked for).
`esp32sim_ble_air_take(e)` puts the frames the board sent since the last call into the buffer and
returns their count, each as `[len u16][frame]`; `esp32sim_ble_air_put(e, ptr, len)` gives the board
one frame (0, or 1 for a bad frame, logged). Frames start with their kind:

| Kind | Frame | Meaning |
| --- | --- | --- |
| 1 | `[1][n][adv n][m][scan response m]` | the board advertises (connectable) |
| 2 | `[2]` | it stopped advertising (or got connected) |
| 3 | `[3][addr 6]` | its guest asks to connect to `addr` |
| 4 | `[4][addr 6][central u8]` | to the board: connected to `addr`; `central` 1 for the guest that asked |
| 5 | `[5][addr 6]` | to the board: the connection failed (LE Connection Complete, status 0x02, as after LE Create Connection Cancel) |
| 6 | `[6][cid u16][payload]` | L2CAP frame of the link, either way |
| 7 | `[7]` | the link ended (either way) |

The host routes them: advertising goes into the other boards' `esp32sim_ble_nearby` lists (their
scans hear it); a connect request to an advertising board answers both with kind 4, otherwise the
asker with kind 5; data and the disconnection go to the other board of the link. The log has
`remote connected guest=<central|peripheral> peer=<address>` and `remote connect failed addr=…`.

### C3 full BLE observation

Call `esp32sim_ble_full(emu)` before boot (0 = enabled, 1 = wrong chip or already
booted). This runs the guest link controller without HCI substitution. After each
run slice, call `esp32sim_ble_take(emu)` until it returns zero. Each nonzero return
is the UTF-8 byte length at `esp32sim_ble_ptr(emu)`, valid until the next take or
emulator deletion. `[ble-air]` contains modeled half-microsecond time, channel,
PDU type, AdvA, decoded name/service AD fields and the raw PDU. `[ble-config]`
contains configured scan-response data, not a transmitted packet. The bounded
1024-entry queue reports lost observations as `[ble-observer] dropped=N`.
Queued observations and the dropped count survive guest reboot. Invalid enable
calls log a reason through `host_log`. Use `esp32sim_ble_scan(emu, enabled)` for active scanning,
`esp32sim_ble_command(emu, ptr, len)` for UTF-8 `connect`, `disconnect`,
`central-stop` or `read-uuid SERVICE CHARACTERISTIC`. `central-stop` silences
a connected central. These exports return 0 on
success or 1 for invalid commands/state/chip. Reads use the guest ATT server.
The central models one unencrypted 1M CSA#1 connection with a 30 ms interval,
2 s supervision timeout and ATT MTU 23. See [EX213](evidence/ble-c3-connection/README.md)
for model limits and the production-module reconnect test.

## Host network

Jumperly patch J13. The page can carry the station's traffic itself: the user-mode NAT runs in
the module as natively, but over a `QueueTransport` (`esp-soc/src/nat/queue.rs`) instead of
sockets. The guest's socket operations queue up as records the page takes after every run, and
the page answers with events before the next one — the deferred-bridge pattern of the I2C / SPI
bridges, without threads.

| Export | Contract |
| --- | --- |
| `esp32sim_net_host(e, flags)` | bit 0 host transport, bit 1 the host resolves A / AAAA (`RESOLVE`), bit 2 SNTP answered locally. After `esp32sim_wifi_ap` (a new access point drops it), before or after boot; survives a chip reset. Flags without bit 0 restore the stage-1 subnet. 1 without an access point. |
| `esp32sim_net_take(e)` | The records queued since the last call: their count; bytes in the buffer (`esp32sim_buf_ptr`, `esp32sim_buf_len`). |
| `esp32sim_buf_len(e)` | Length of the buffer of the last `*_take` call. |
| `esp32sim_net_event(e, kind, id, arg, ptr, len)` | One event (below). `TCP_DATA` returns the bytes taken, `UDP_DATA` 1 taken / 0 dropped, the rest 1 applied / 0 not waited for. |
| `esp32sim_wifi_stat(e, which)` | 0 associated (keys installed), 1 DHCP lease given, 2 TCP flows open through the host, 3 bytes to the host, 4 bytes from the host, 5 TCP flows the host could not open, 6 DNS answers from the host, 7 / 8 LAN packets out / in. |

Records, little-endian, each padded to a multiple of 4 bytes:
`u8 op, u8 flags, u16 port, u32 id, u32 len, len bytes`.

| op | Record | Fields |
| --- | --- | --- |
| 1 | TCP_CONNECT | `id` = flow, `port`, data = IPv4[4] |
| 2 | TCP_DATA | the guest's bytes, never more than the host's credit |
| 3 | TCP_SHUTDOWN | the guest finished sending (FIN, every byte before it written) |
| 4 | TCP_CLOSE | the NAT forgot the flow; flags bit 0 = abort (guest RST, reboot) |
| 5 | UDP_OPEN | `port`, data = IPv4[4] |
| 6 | UDP_DATA | one datagram |
| 7 | UDP_CLOSE | |
| 8 | RESOLVE | `id` = query, `port` = qtype (1 A, 28 AAAA), data = the name (UTF-8) |
| 9 | INCOMING_RESULT | J15: `id` = the host's incoming flow, flags bit 0 = the guest accepted it |

| kind | Event | `arg` / data |
| --- | --- | --- |
| 1 | CONNECTED | `arg` = bytes the host takes before its first credit |
| 2 | FAILED | `arg` 1 refused, 2 unreachable / timed out, 3 policy, 4 offline; the guest gets RST\|ACK. On an open flow it is a reset |
| 3 | TCP_DATA | the host's bytes; at most 64 KiB wait per flow, the rest is offered again later |
| 4 | TCP_EOF | the remote side finished sending |
| 5 | TCP_RESET | after the bytes already taken |
| 6 | TX_CREDIT | `arg` more bytes |
| 7 | UDP_DATA | one datagram; 16 wait per flow |
| 8 | RESOLVED | `id` = query, `arg` = TTL, data = n × IPv4[4] (none: an empty answer) |
| 9 | UNRESOLVED | `arg` = rcode 2 SERVFAIL or 3 NXDOMAIN |
| 10 | INCOMING | J15: the host opens flow `id` to the guest's port `arg`; data = the peer's IPv4[4] + port u16 LE |

Limits per board: 16 TCP flows (the 17th SYN is dropped and retried by the guest), 64 KiB waiting
for the guest per TCP flow, 256 KiB of data in untaken records (`tcp_write` blocks, a datagram is
dropped), 16 datagrams per UDP flow. Flow ids grow and are not reused while a flow lives. No
TCP_CLOSE follows FAILED or TCP_RESET: the host has forgotten the flow already. Data for a flow
the guest has forgotten is taken and dropped. With bit 1 the host sees no DNS datagrams: A and AAAA
go as `RESOLVE`, any other query is answered here, empty (NOERROR). With bit 2 SNTP is answered
from `esp32sim_run`'s `unix_ms`; without it the guest's NTP goes to the host as UDP. ICMP echo
is answered locally for any address.

A host loop:

```js
for (;;) {
  for (const ev of dueEvents()) {                                  // in arrival order
    const taken = deliver(ev);                                     // esp32sim_net_event
    if (ev.kind === 3 && taken < ev.bytes.length) keep(ev, taken); // offer the rest next time
  }
  exports.esp32sim_run(e, cycles, Date.now());
  const n = exports.esp32sim_net_take(e);
  const records = new DataView(memory.buffer, exports.esp32sim_buf_ptr(e), exports.esp32sim_buf_len(e));
  forward(records, n);                                             // to the gateway
}
```

`wasm/tests/jumperly.rs` (`wifi_net_reaches_a_scripted_internet_through_the_host`) runs the
`wifi-net` fixture against a scripted host: `dns: example.com -> 93.184.215.14`, SNTP time,
`http GET http://example.com/ -> 200`, then TCP_SHUTDOWN and TCP_CLOSE.

### Connections to the guest

Jumperly patch J15. The host can also open a TCP connection to a server on the board (a page
served by `WebServer`, a local helper polling the device): `INCOMING` with an id of the host's
own, the guest's port in `arg` and the real peer (IPv4[4] + port u16 little-endian) as data. The
id must be nonzero and not a live flow's; the guest's ids grow from 1, so the host takes high
ones (bit 31 set). The NAT sends the station a SYN from the gateway, `10.0.2.2:<port>` (49152 and
up, the peer's address appears only in the log), once it has heard the station; then exactly one
`INCOMING_RESULT` comes:

- accepted (flags bit 0, the guest's SYN-ACK): the flow runs like an outbound one — the host's
  `TCP_DATA` / `TCP_EOF` / `TCP_RESET`, the guest's `TCP_DATA` / `TCP_SHUTDOWN` / `TCP_CLOSE`. The
  guest's side sends up to 64 KiB before the host's first `TX_CREDIT` (there is no `CONNECTED`);
- refused (the guest's RST: nobody listens; no answer within 5 s of device time; the station not
  heard yet; no free flow): the flow is gone, no `TCP_CLOSE` follows.

The host may send `TCP_DATA`, `TCP_EOF` and `TX_CREDIT` right after `INCOMING`; the bytes wait for
the handshake. `TCP_RESET` or `FAILED` before the answer gives the flow up: the guest is reset and
no record follows. `INCOMING` returns 0 for a taken id, port 0, short data, or when 16 TCP flows
(both directions) are open. `wasm/tests/jumperly.rs`
(`the_host_connects_to_a_guest_server_and_a_closed_port_refuses`) connects to `lan-server`'s port
7000 (`ping` → `pong`, the guest prints `lan: client 10.0.2.2`), then TCP_EOF, the guest's
TCP_SHUTDOWN and TCP_CLOSE; port 81 is refused.

## LAN between boards

Jumperly patch J14. Boards of one Wi-Fi network (one emulator each) reach each other at the IP
level: a packet for a peer's address, and a UDP broadcast or multicast other than DHCP
(`10.0.2.255`, `255.255.255.255`, `224.0.0.0/4`), leaves the station's virtual network before the
gateway's services and the NAT see it. The page stamps it with the sender's device time and
hands it to each peer when that peer's clock reaches the stamp plus its latency (Jumperly: 500 µs,
longer than a multi-board slice, so delivery is exact however the run is sliced). Every address
of the subnet resolves to the gateway's MAC (proxy ARP), so the router needs no ARP of its own;
a delivered packet comes from the gateway's MAC to the station's MAC, also a broadcast or
multicast one. Both ends run real lwIP: `WiFiServer` on one board accepts `WiFiClient` of
another. mDNS works over it ([notes](jumperly/mdns-lan-2026-10-07/README.md)).

| Export | Contract |
| --- | --- |
| `esp32sim_set_mac(e, ptr6)` | The station's MAC (efuse BLK1 and the machine's; on the classic ESP32 BLK0 with its CRC8), S3, C3 and the classic ESP32, before boot; 1 otherwise. |
| `esp32sim_net_station(e, ptr4)` | The address DHCP gives the station (default 10.0.2.15). After `esp32sim_wifi_ap`, before boot; 1 otherwise. |
| `esp32sim_lan_peers(e, ptr, count)` | The other boards' addresses (`count` × IPv4[4]); 0 turns the LAN off. After `esp32sim_wifi_ap` (a new access point drops it); 1 without one. |
| `esp32sim_lan_take(e)` | The packets sent to peers since the last call: their count; per packet `f64` device µs (cycles over the CPU MHz), `u32` length, the IPv4 packet padded to 4 bytes. At most 256 wait; more are dropped. |
| `esp32sim_lan_receive(e, ptr, len)` | An IPv4 packet for the station, queued for its receive ring at once: 0 accepted; 1 without a LAN, before the station's first frame, or for a malformed packet, one over 1500 bytes or one for another address. |

A packet for a subnet address that is neither a peer nor a service goes on as before: refused
with RST without the host transport, to the host with it. `wasm/tests/jumperly.rs`
(`two_boards_talk_tcp_and_udp_over_the_lan`) runs `lan-server` (10.0.2.15) and `lan-client`
(10.0.2.16, its names through a scripted host, CPI 3) in lockstep: `lan: pong from 10.0.2.15` by
name and by address, `udp: hello from 10.0.2.16` on the server.

## Octal flash (ESP32-S3-WROOM-2)

Jumperly patch J16. A module with octal SPI flash (ESP32-S3-WROOM-2: N16R8V, N32R8V, N32R16V)
runs its firmware (core `FlashMode=opi`, `PSRAM=opi`) on the octal path of real silicon: efuse
FLASH_TYPE says octal, so the ROM resets the flash out of OPI mode at every boot ("Octal Flash
Mode Enabled"), the bootloader and the app find a Macronix octal chip (JEDEC C2 80, log2(size) +
0x20) and switch it to DTR OPI with WRCR2; from then on the flash takes the 16-bit commands
(8DTRD, PP4B, SE4B, BE4B, RDSR, RDCR2, ...). The octal PSRAM's MR2 reports the PSRAM size, so a
16 MB module maps 16 MB (ESP-IDF sizes it from MR2 alone).

| Export | Contract |
| --- | --- |
| `esp32sim_flash_octal(e, on)` | The flash is octal (`on` nonzero) or quad (0, the default). Before boot, ESP32-S3 only; 1 otherwise. It survives a chip reset. |

The same firmware on the quad model still runs: ESP-IDF's flash mode auto-detection falls back to
DIO when efuse FLASH_TYPE is quad. `wasm/tests/jumperly.rs` (`octal_flash_modules_run_opi_firmware`)
runs the `octal16` / `octal32` fixtures on 16 MB flash + 8 MB PSRAM and 32 MB + 16 MB.

## I2S receive (microphones)

Jumperly patch J17, on upstream's receiver (`esp-periph/src/i2s/rx.rs`: PCM at the DMA boundary,
the RX clock tree for the frame rate, the RX registers for the data bits, received slots and byte
layout, the samples written into the GDMA IN ring at the frame rate with IN_SUC_EOF every
RX_EOF_NUM bytes, so each DMA buffer reaches `i2s_channel_read` through the driver's EOF
callback). Without a host a receiver reads what upstream gives it: silence, `--i2s-tone` or the
pin-routed PCM sources. With one, the host supplies the slot words of the frames before each run:
the emulator does not shift SD bit by bit.

| Export | Contract |
| --- | --- |
| `esp32sim_i2s_host(e, on)` | The host supplies what the receivers get (`on` nonzero); a frame falling due with no word queued is silence and ends the slice once, until the host pushes again. Survives a chip reset. ESP32-S3 and the classic ESP32 (Jumperly patch J28); 1 otherwise. |
| `esp32sim_i2s_rx_need(e, port, cycles)` | Frames controller `port` (0, 1) should get now so the next `cycles` find them queued (those falling due, less the queued); 0 while its receiver is stopped or in a mode the model does not receive. The buffer: eight u32 LE — frame rate Hz, BCK per slot, data bits, received slot mask, slots per frame, flags (bit 0 running, bit 1 Philips), queued frames, BCK Hz. |
| `esp32sim_i2s_rx_push(e, port, ptr, frames)` | Queue `frames` × 8 bytes: the left (WS low) and right (WS high) slot word, u32 LE, MSB-aligned (bit 31 = the first bit after the WS edge and the one-BCK delay). Returns how many it took (at most 65536 wait). |

The receiver takes RX_BITS_MOD + 1 (16, 24 or 32) bits from the top of each slot word into 2, 3
(4 with RX_24_FILL_EN) or 4 little-endian bytes, one or two slots per frame; the modes upstream
does not model (slave clocks, right alignment, big-endian, LSB first, more than two slots)
receive nothing. Turning the host off drops its queue. The web simulator's adapter routes the
request to the part on the pads of the RX BCK / WS outputs and the SD input.
`wasm/tests/jumperly.rs` (`i2s_mic_reports_the_level_of_the_host_sine_on_the_left_slot`) runs the
`i2s-mic` fixture (16 kHz, 32-bit mono left) on a −26 dBFS host sine: the sketch reports −26.0 dBFS
and the peak 420 426.

## UART pads

Jumperly patch J19. The UARTs move bytes straight between the console and the FIFOs; which pads
carry them is the host's business (the web simulator routes the bytes along the wires of the pads).
A UART reaches its pads either through the GPIO matrix (any pad: TXD_OUT on the pad's output, RXD_IN
reading it) or on its own IO_MUX pads with the matrix bypassed (S3: UART0 on 43 / 44, UART1 on
17 / 18; C3: UART0 on 21 / 20; ESP-IDF picks them when the sketch asks for exactly those pins).

| Export | Contract |
| --- | --- |
| `esp32sim_uart_pins(e, port)` | The pads of UART `port` as `tx \| rx << 8`, 255 for none: TX is the pad routed to TXD_OUT through the matrix, else the IO_MUX TX pad in its UART function; RX the pad the matrix routes to RXD_IN, else (matrix bypassed) the IO_MUX RX pad in its UART function with input enabled. S3: ports 0..2; C3: 0, 1; the classic ESP32: 0..2 (UART0 on 1 / 3). |
| `esp32sim_wifi_beacons(e, ptr, len)` | Jumperly patch J22: the networks around (beacon-only, not joinable), replacing the ones there were. Entries `[ssid_len u8][ssid][channel u8][rssi i8][secure u8][bssid 6]`, SSID 1..=32 bytes of UTF-8, channel 1..=14, at most 32; `len` 0 clears them. They answer wildcard or matching probe requests from their own BSSID with their channel, signal and security; a network with the access point's SSID is left out. After `esp32sim_wifi_ap` (a new access point drops them), before or after boot. 0 ok; 1 without an access point or for bad data (logged). |
| `esp32sim_uart_baud(e, port)` | Jumperly patch J21: the baud UART `port` is set to (its clock source and dividers, upstream's `uart_route`), 0 when not configured or no such port. |
| `esp32sim_gpio_in_pin(e, signal)` | The pad a matrix input signal reads, 255 if none. On the C3 it now decodes the C3 layout (SIG_IN_SEL at bit 6, pad in bits 0..4; the S3: bit 7, 0..5). On the classic ESP32 (J28) also a pad that carries the signal on its IO_MUX function with the matrix bypassed; the pad needs FUN_IE. |

`wasm/tests/jumperly.rs` (`serial1_and_serial2_on_matrix_pins`, `c3_serial1_on_matrix_pins`) runs
the `serial12` fixture (Serial1 RX 16 / TX 17, Serial2 RX 4 / TX 5) and `c3-serial1` (Serial1 RX 3 /
TX 4): the bytes come out of console ports 1 and 2, the pads are found, and lines fed to the ports
come back on UART0.

## Classic ESP32 in the browser

Jumperly patch J28. `esp32sim_new("esp32", …)` builds the classic ESP32 (ESP32-D0WDQ6, two Xtensa
LX6 cores at 240 MHz) of a NodeMCU-32S: always 4 MB of flash, the module's (a larger `flash_mb` is
logged and ignored), no PSRAM. It boots through the mask ROM (`esp32_rev300_rom.elf`, the ECO3
ROM upstream fetches) from a whole-flash image whose second-stage bootloader sits at **0x1000**,
not 0x0 as on the S3 and the C3; the partition table stays at 0x8000 and the app at 0x10000.
`esp32sim_boot(e, 1)` (the app without the ROM) is upstream's error for this chip.

The Jumperly ABI behind the `JumperlyChip` trait (`wasm/src/chip.rs`, `wasm/src/chips/esp32.rs`)
works as on the S3: pins GPIO0..39 (34..39 input only: no drive, no pulls in the state word),
console ports 0..2 (no USB-Serial/JTAG: port 3 takes nothing and has nothing), RMT frames
(RMT_SIG_OUT0 = 87), PWM outputs from LEDC (signals 71..86; Arduino's channels 0..7 are the
high-speed group 71..78; ESP32Servo and `tone()` use LEDC on this chip), the linear ADC on ADC1
(GPIO32..39) and ADC2 (GPIO0, 2, 4, 12..15, 25..27; the Arduino core itself returns 0 for ADC2
while Wi-Fi runs), the I2C bridge on both controllers, the SPI bridge on HSPI (host 2) and VSPI
(host 3, Arduino's default `SPI`), the I2S host on ports 0 and 1, BLE, Wi-Fi, the LAN and
`esp32sim_reset`. Every reset takes the strapping byte from the circuit: a pad the host drives
(`esp32sim_gpio_set_input` level 0 or 1) gives its level, a free one — never set, or floating
(level 2) — its reset pull (GPIO0, 5, 15 up; 2, 4, 12 down), so an empty circuit boots
`boot:0x13 (SPI_FAST_FLASH_BOOT)` and IO0 held low at the reset
`boot:0x3 (DOWNLOAD_BOOT(UART0/UART1/SDIO_REI_REO_V2))`, where the ROM prints `waiting for
download` and waits (the SDIO slave DMA block reads its links parked, the rest of the SDIO slave
is not modelled). The power-on reset too: until the first instruction runs (after `esp32sim_boot`,
before the first `esp32sim_run`), each `esp32sim_gpio_set_input` latches the strap again, so a host
that hands the chip its circuit before the first slice boots IO0-low into download mode. The
host's reset is the board's EN: the host lets go of the chip's pads first (their nets float unless
the circuit drives them) and hands the chip the circuit's levels, so the strap never sees the
chip's own last outputs (an LED on GPIO2 that was lit). As a power-on reset it also clears the
RTC domain's DAC power (below); a guest's software reset (`esp_restart`) keeps the RTC domain.

| Export | Contract |
| --- | --- |
| `esp32sim_gpio_set_input(e, pin, level)` | The level the circuit puts on `pin`: 0 low, 1 high, 2 floating (nothing drives the net). A floating pad reads low (or its own IO_MUX pull) as level 0 did before, but on the classic it is no driven pad: the strap at a reset takes its reset pull. It stays floating across a chip reset. On the S3 and the C3, 2 is level 0. 1 for another level, a pin past the chip's or a chip without the ABI. |

| Export | Contract |
| --- | --- |
| `esp32sim_dac_take(e)` | The DAC output changes since the last call: their count; per change `u32` GPIO (25 = DAC1, 26 = DAC2) and `u32` millivolts LE, `(code * 3300 + 127) / 255` (code 128 → 1656, 255 → 3300); `u32::MAX` once when an output turns off (`dacDisable`, or `esp32sim_reset`: the host's reset is a power-on reset and powers both DACs down; a guest's `esp_restart` keeps them). A change written to PAD_DAC1/2 or SAR_DAC_CTRL2 ends the slice in Jumperly mode. 0 and an empty buffer for a chip without a DAC (S3, C3). |
| `esp32sim_touch_set(e, gpio, touched)` | A finger on touch pad `gpio` (`touched` nonzero) or not: T0..T9 = GPIO 4, 0, 2, 15, 13, 12, 14, 27, 33, 32; `touchRead` counts drop while it is touched. 0 ok; 1 for another pin or another chip (S3, C3, C6). Survives a chip reset (RTC domain); Jumperly mode is not needed. |

Limits: the ROM is the ECO3 one (banner `ets Jul 29 2019 12:21:46`); a board with an older silicon
revision (v1.x prints `ets Jun  8 2016 00:22:57`) runs the same sketches, but its ROM banner and
`esp_chip_info` revision differ from the emulator's. Download mode stops at `waiting for download`:
the ROM's serial loader protocol (esptool's SYNC and the commands after it) is not exercised.
Octal flash is refused (`esp32sim_flash_octal` → 1); MCPWM has no PWM tap on the classic.
Deep sleep is not modelled: `esp_deep_sleep_start()` stops the chip (the ROM waits for the RTC
memory checksum forever, so the guest never wakes), and the host log gets one line per boot,
`[emu] deep sleep is not modelled: the chip stops here`, at the first write of RTC_MEM_CONF
RTC_MEM_CRC_START (the sleep path's first step) or RTC_CNTL_STATE0 SLEEP_EN.

`wasm/tests/jumperly.rs` (`esp32_*`) runs the `esp32-*` fixtures (`esp32:esp32:nodemcu-32s`, see
`emulator/fixtures/README.md` of the Jumperly monorepo): blink on GPIO2, Serial2 on RX 16 / TX 17
at 9600, `analogRead` on GPIO34 and GPIO4, LEDC and `tone()`, ESP32Servo at 50 Hz, a NeoPixel
frame on GPIO4, the I2C scan and the VSPI transfer through the host bridges, the INMP441 sketch
on a −26 dBFS host sine, the Wi-Fi scan and join, the BLE server through the virtual peer, the
DAC, touch, the host reset and download mode with IO0 held, floating strapping pins at a host
reset, IO0 low before the first run, the DAC off after a host reset and the deep-sleep line.
