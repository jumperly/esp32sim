# mDNS between two boards on the J14 LAN

Jumperly patch J14 routes packets between the boards of one Wi-Fi network at the IP level: a
UDP multicast leaves the station like a broadcast and reaches every peer as an Ethernet frame
unicast to that station's MAC (the access point does not model group addressing). The question
was whether ESPmDNS works over that. It does.

## Run

Source: esp32sim `jumperly` after J14 (2026-10-07). Firmware: `mdns-a.ino` (responder
`mcu1.local`, service `_lan._tcp` on 7000) and `mdns-b.ino` (querier `mcu2.local`), built with
`jumperly/build-worker:3.2.1-4` (arduino-cli, esp32 core 3.2.1, FQBN `esp32:esp32:esp32s3`).
Harness: the `run_lan` lockstep of `wasm/tests/jumperly.rs` (two `Emu`s, 250 µs slices, a packet
due at its send time + 500 µs by the receiver's clock), the two images in place of `lan-server`
and `lan-client`; native debug build, about 35 s for 25 s of device time.

| Board | Serial | Result |
| --- | --- | --- |
| A, 10.0.2.15 | `mdns: responder mcu1.local` | Pass |
| B, 10.0.2.16 | `mdns: queryHost mcu1 -> 10.0.2.15` | Pass |
| B | `mdns: _lan._tcp 1`, `  mcu1 10.0.2.15:7000` | Pass |
| B | `mdns: hostByName mcu1.local -> 10.0.2.15` (lwIP's `.local` path) | Pass |

The same run with B's sockets on the host transport (`esp32sim_net_host(0b111)`) gives the same
lines and no operation for the host: `.local` names never become `RESOLVE` records. lwIP accepts
the multicast datagrams because the station joined 224.0.0.251 (its IGMP report is ignored by
the virtual network, which needs none); the L2 destination is the station's own MAC.

Limits: one responder and one querier; no probing conflict (two boards claiming one name) was
tried. The sketches are not fixtures and no automated test runs them.
