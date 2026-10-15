# EX233: a pluggable host transport for the user-mode NAT

Base: upstream/main `9890ac55`. The final source is the commit containing this receipt.
Inputs, hashes and the exact commands are in [inputs.json](inputs.json); the checks that ran
and their results in [checks.json](checks.json).

`Nat` terminated guest flows directly on `std::net` sockets, so only a host with sockets could
carry the guest's traffic. This puts the host side behind a trait, with nothing else changing
for `--net nat`:

- `NatTransport` (`esp-soc/src/nat/transport.rs`): `tcp_connect` / `tcp_state` (`Connect::Pending`,
  `Open`, `Failed(ErrorKind)`) / `tcp_read` / `tcp_write` / `tcp_shutdown` / `tcp_close(abort)`,
  `udp_open` / `udp_send` / `udp_recv` / `udp_close`, `resolver()`, and the optional `resolve` /
  `resolved` (`Resolve::Addrs(addrs, ttl)` or `Failed(rcode)`) for a transport that looks names up
  itself. Reads and writes follow non-blocking `std::io` rules.
- `StdTransport` (`nat/std_transport.rs`) is the old code moved as is: `std::net` sockets, a
  connect worker thread per SYN under the process-wide `ConnectPermit` limit of 8, the resolver
  from `/etc/resolv.conf` (1.1.1.1 without one). `Nat::new(log)` is
  `Nat::with_transport(log, Box::new(StdTransport::new()))`.
- The flow's private socket enum became `FlowState { Connecting(id), Connected(id), TimeWait,
  Closed }`. The NAT calls `tcp_close` exactly once per flow (`abort` on a guest RST, a failed
  connect, a host read / write error, `abort_all`; otherwise on TIME_WAIT entry or idle expiry)
  and `udp_close` once per UDP flow (eviction or idle expiry).
- `Nat::dns_query` hands a guest A / AAAA query to a transport that resolves names itself (at
  most 32 waiting, the oldest gives way, 30 s expiry) and answers from the address the guest
  asked, with the query's transaction id; `false` (the std transport) keeps relaying the datagram
  to `resolver()`. The answer is built by `net::packet::dns_reply`, moved out of
  `VirtualNet::dns`, which uses it too (`dns_question` parses the question for both).
- `Nat::abort_all` drops every flow, resetting the transport's; `StationLink::surviving_reboot`
  calls it, since the rebooted guest's sockets are gone.
- `VirtualNet::local_ntp` (default false) answers SNTP from the host clock even with a NAT.

The one visible change on the native path: a failed connect logs the error kind
(`connection refused`, `address not available`) instead of the OS message. Nothing runs per
tick: `Nat::poll` runs only while a NAT is attached (`--net nat` with `--wifi`), which the hello
demos and goldens never do. Goldens are unchanged.

## Tests

`esp-soc/src/nat/tests.rs`: the existing socket tests run unchanged on `StdTransport` (SYN /
data / FIN retransmission and cumulative ACK, partial and wrapping ACKs, in-order and idempotent
FIN, bounded guest buffer, short writes and WouldBlock, FIN after host write drain, SYN-ACK
resend, host reads bounded by the window, zero-window probe, connect permits, TIME_WAIT eviction,
UDP capacity, malformed TCP headers). New, against a scripted `MockTransport`: a SYN opens a
transport flow and guest data is written to it; WouldBlock closes the guest window without
losing bytes; transport EOF becomes a guest FIN and the close is graceful; a refused connect
resets the guest and forgets the flow; a full transport drops the SYN until its retransmission;
a guest RST aborts the transport flow; `abort_all` resets every flow; a guest reboot aborts the
NAT's flows; a resolving transport answers DNS from the guest's resolver address; a failed
lookup is NXDOMAIN and an empty one has no records; DNS is relayed when the transport does not
resolve; at most 32 lookups wait and the oldest gives way; `local_ntp` answers SNTP with a NAT.

## Firmware

[wifi-net-probe.ino](wifi-net-probe.ino), Arduino-ESP32 3.3.12: scan, join the open network
`Jumperly`, DHCP, `WiFi.hostByName("example.com")`, SNTP from `pool.ntp.org`, then
`GET http://example.com/`. Run with `--wifi ssid=Jumperly --net nat --debug net` on main
`9890ac55` and on this branch, on a desktop with internet access (commands in inputs.json):

| Build | Console | NAT log |
| --- | --- | --- |
| main | `ip=10.0.2.15 gw=10.0.2.2 dns=10.0.2.3`, `dns: example.com -> 0.0.0.0`, `time: 2026-10-15 15:55:01 UTC`, `http GET http://example.com/ -> -1 (connection refused)` | 2 + 1 UDP queries to the host resolver:53 (29 and 30 bytes), one 94-byte reply (pool.ntp.org, 4 A records), one 48-byte UDP exchange with an NTP server on port 123, `TCP … -> 0.0.0.0:80 connecting`, `failed: … (os error 10049)` |
| this branch | the same lines (`time: 2026-10-15 15:53:34 UTC` and `15:54:13 UTC` in two runs) | the same counts; `failed: address not available` |

SNTP went through the transport to the real network on both builds (the time is the host's
wall clock). `example.com` does not resolve on this host at all: a direct Python query to the
same resolver times out for its A and AAAA records while `example.org`, `github.com` and
`pool.ntp.org` answer in ~50 ms, so lwIP got no answer, `hostByName` returned 0.0.0.0 and the
HTTP connect went to 0.0.0.0:80. That is the host's network, identical on both builds. With no
resolver file (the 1.1.1.1 fallback) both builds were byte-identical too: UDP to 1.1.1.1 is
blocked on this host, no reply at all, same console, same instruction counts
(87 418 016 + 57 211 034). With a resolver file the counts differ run to run by the SNTP
timing (87 007 258 / 87 007 931 + 52 815 099 / 52 831 491).

## CPU comparison

Against main `9890ac55` (the same tree, the same Rust 1.99.0 release build in a separate
worktree), the hello demos as CONTRIBUTING.md asks: S3 hello 3000 emulated seconds (board
none), C3 and C6 hello 30 seconds; child user CPU seconds via `GetProcessTimes` (Windows,
15.625 ms timer resolution); one warmup pair B→M, then seven measured pairs alternating M→B,
B→M; change is the ratio of medians. Every run of a workload retired the same instruction
count and produced the same console SHA-256 on both builds (`identical_work_and_console` in
[cpu.json](cpu.json)). No load gate. Pocket Tank was not run: the change touches neither the
S3 core nor its bus.

None of this change's code runs on these workloads: without `--wifi` there is no virtual
network and no NAT, so `Nat::poll` is never called; the only mechanism left for a difference is
code placement in the binary. The measurement nevertheless shows S3 hello slower in 5/7 pairs
twice: run 1 (the three demos, with light git activity on the desktop) +3.13%, run 2 (S3 hello
alone, idle desktop) +4.37%, both with overlapping ranges and a ~34 s outlier on each side;
C3 hello −14.39% (2/7) and C6 hello −10.00% (2/7). By the receipt rules' flags (slower in at
least 6/7 pairs, or non-overlapping ranges) nothing is flagged, and the S3 result is reported
as is rather than explained away: a maintainer measurement on the usual Apple-silicon setup,
and a static hot-function comparison (not possible on this MSVC build, which has no symbols for
`llvm-objdump`), would settle whether it is placement or noise; the A/A control below says noise.

### A/A control, started 2026-10-15T18:22:02

The same protocol with main's binary against a byte-identical copy of itself (SHA-256 equal), S3 hello only, idle desktop: "main" median 31.8281 s (27.8438–32.6875), "copy" median 32.3594 s (28.9531–33.3906), change +1.67%, the copy slower in 5/7 pairs; identical instructions and console hashes. Pairs (main / copy, warmup first): 31.781 / 32.828; 31.906 / 32.359; 31.828 / 33.391; 32.688 / 31.906; 32.281 / 32.828; 29.953 / 29.141; 27.844 / 28.953; 29.766 / 32.859. So this setup produces a 5/7 asymmetry and a few percent between identical binaries; the +3–4% above is within that.

### run 1, started 2026-10-15T17:57:06

| Workload | Main median (range) | PR median (range) | Change | PR slower in N/7 | Instructions | Console SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| s3-hello | 32.9531 (29.8750–36.2969) | 33.9844 (28.0938–34.9844) | +3.13% | 5/7 | 1134713226 + 655106431 | `98de90e19cbd…` |
| c3-hello | 2.0625 (1.5625–2.3906) | 1.7656 (1.2500–2.2031) | -14.39% | 2/7 | 4800000000 | `3b9d8d3eb053…` |
| c6-hello | 2.8125 (2.4844–2.9531) | 2.5312 (2.0781–2.8438) | -10.00% | 2/7 | 4800000000 | `bd69fbb9ac5c…` |

#### s3-hello

| Pair | Order | Main s | PR s |
| --- | --- | ---: | ---: |
| warmup | B→M | 34.156 | 33.344 |
| pair1 | M→B | 36.297 | 28.094 |
| pair2 | B→M | 31.766 | 33.875 |
| pair3 | M→B | 32.953 | 34.984 |
| pair4 | B→M | 29.875 | 34.594 |
| pair5 | M→B | 33.062 | 32.562 |
| pair6 | B→M | 33.203 | 34.750 |
| pair7 | M→B | 31.734 | 33.984 |

#### c3-hello

| Pair | Order | Main s | PR s |
| --- | --- | ---: | ---: |
| warmup | B→M | 1.531 | 1.719 |
| pair1 | M→B | 2.391 | 2.203 |
| pair2 | B→M | 1.812 | 1.969 |
| pair3 | M→B | 2.062 | 1.250 |
| pair4 | B→M | 1.562 | 1.766 |
| pair5 | M→B | 2.141 | 2.047 |
| pair6 | B→M | 2.250 | 1.562 |
| pair7 | M→B | 1.766 | 1.641 |

#### c6-hello

| Pair | Order | Main s | PR s |
| --- | --- | ---: | ---: |
| warmup | B→M | 2.812 | 2.594 |
| pair1 | M→B | 2.672 | 2.438 |
| pair2 | B→M | 2.531 | 2.578 |
| pair3 | M→B | 2.953 | 2.438 |
| pair4 | B→M | 2.484 | 2.078 |
| pair5 | M→B | 2.859 | 2.531 |
| pair6 | B→M | 2.812 | 2.562 |
| pair7 | M→B | 2.812 | 2.844 |

### run 2 (S3 hello only), started 2026-10-15T18:08:52

| Workload | Main median (range) | PR median (range) | Change | PR slower in N/7 | Instructions | Console SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| s3-hello | 26.1250 (25.4688–33.9375) | 27.2656 (25.7031–34.1250) | +4.37% | 5/7 | 1134713226 + 655106431 | `98de90e19cbd…` |

#### s3-hello

| Pair | Order | Main s | PR s |
| --- | --- | ---: | ---: |
| warmup | B→M | 30.484 | 33.938 |
| pair1 | M→B | 33.938 | 32.453 |
| pair2 | B→M | 29.953 | 34.125 |
| pair3 | M→B | 26.062 | 25.703 |
| pair4 | B→M | 25.688 | 27.266 |
| pair5 | M→B | 26.125 | 26.375 |
| pair6 | B→M | 25.469 | 26.922 |
| pair7 | M→B | 26.469 | 28.391 |

## Limits

- `StdTransport` has no portable abortive close: `tcp_close(id, true)` closes like a graceful
  close (as before).
- The resolver file lookup is unchanged (`/etc/resolv.conf`, else 1.1.1.1); on Windows the
  firmware run above supplied one at the drive root for the duration of the run.
- The self-resolving and `local_ntp` paths are exercised by the mock transport only; no
  transport other than `StdTransport` is in this change.
- Pure host-side code: no register or silicon claim.
