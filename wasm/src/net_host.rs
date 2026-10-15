//! Jumperly patch J13: the host's network for the station behind the virtual access point. The
//! guest's sockets go out through the user-mode NAT over a `QueueTransport`: the host takes their
//! operations after every run (`esp32sim_net_take`) and answers with events
//! (`esp32sim_net_event`) before the next one. The host resolves the guest's names; SNTP may stay
//! local. Without `esp32sim_net_host` nothing changes (the stage-1 subnet: local DNS, TCP refused).
use super::{bytes, log, Emu};
use crate::machine::MachineKind;
use esp_soc::nat::{HostEvent, Nat, QueueTransport};
use esp_soc::wifi::{StaState, StationLink, WpaState};

/// The station's link (S3 and C3), with the bus's deferred device time delivered first.
pub(crate) fn link(e: &mut Emu) -> Option<&mut StationLink> {
    match &mut e.m {
        MachineKind::S3(m) => { m.bus.flush_ticks(); Some(&mut m.bus.periph.wifi.link) }
        MachineKind::C3(m) => { esp_soc::SocBus::flush_ticks(&mut m.bus); Some(&mut m.bus.periph.wifi.link) }
        MachineKind::C6(_) | MachineKind::Esp32(_) => None,
    }
}

/// Carry the station's traffic over the host. `flags` bit 0: the host transport (TCP and UDP past
/// the gateway become operations for the host); bit 1: the host resolves the guest's A / AAAA
/// queries (`RESOLVE`; without it DNS datagrams go to the host as UDP to 10.0.2.3:53); bit 2: SNTP
/// is answered locally from the host clock (`esp32sim_run`'s `unix_ms`). Flags without bit 0 go back
/// to the stage-1 subnet. Call it after `esp32sim_wifi_ap` (again after every one: a new access
/// point brings a new network), before or after boot; it survives a chip reset, which aborts the
/// open flows (`TCP_CLOSE` with abort). A second call starts a new queue: the old flows are
/// forgotten without records. Returns 1 without an access point.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_host(e: *mut Emu, flags: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(net) = link(e).and_then(StationLink::net_mut) else {
        log("[emu] net host: no network; attach the access point first (esp32sim_wifi_ap)");
        return 1;
    };
    if flags & 1 == 0 {
        net.nat = None;
        net.local_ntp = false;
        e.net = None;
        return 0;
    }
    let (transport, handle) = QueueTransport::with_dns(flags & 2 != 0);
    net.nat = Some(Nat::with_transport(net.log, Box::new(transport)));
    net.local_ntp = flags & 4 != 0;
    e.net = Some(handle);
    0
}

/// Take the operations the guest's sockets queued since the last call: their count; the records
/// are in the buffer (`esp32sim_buf_ptr`, `esp32sim_buf_len`), little-endian, each padded to a
/// multiple of 4 bytes: `u8 op, u8 flags, u16 port, u32 id, u32 len, len bytes`. op 1 TCP_CONNECT
/// (port, data = IPv4[4]), 2 TCP_DATA, 3 TCP_SHUTDOWN, 4 TCP_CLOSE (flags bit 0 abort), 5 UDP_OPEN
/// (port, IPv4[4]), 6 UDP_DATA, 7 UDP_CLOSE, 8 RESOLVE (id = query id, port = qtype 1 A / 28 AAAA,
/// data = the name in UTF-8), 9 INCOMING_RESULT (Jumperly patch J15: id = the host's incoming
/// flow, flags bit 0 = the guest accepted it; port 0, no data). 0 without `esp32sim_net_host`.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    match &e.net {
        Some(h) => h.take_ops(&mut e.buf),
        None => { e.buf.clear(); 0 }
    }
}

/// Deliver one host event. `kind` 1 CONNECTED (`arg` = bytes the host takes before its first
/// TX_CREDIT), 2 FAILED (`arg` 1 refused, 2 unreachable or timed out, 3 policy, 4 offline: the
/// guest gets RST; a flow already open is reset), 3 TCP_DATA → the bytes taken (the rest is offered
/// again later; all of them for a flow the guest has forgotten), 4 TCP_EOF, 5 TCP_RESET (after the
/// bytes already taken), 6 TX_CREDIT (`arg` bytes), 7 UDP_DATA → 1 taken, 0 dropped (16 waiting per
/// flow), 8 RESOLVED (`id` = query id, `arg` = TTL, data = n × IPv4[4], none for an empty answer),
/// 9 UNRESOLVED (`arg` = rcode 2 SERVFAIL or 3 NXDOMAIN), 10 INCOMING (Jumperly patch J15: the host
/// opens TCP flow `id` to the guest's port `arg`; data = the real peer's IPv4[4] + port u16
/// little-endian, for the log: the guest sees 10.0.2.2 and a port of the NAT's; `id` is the host's
/// choice, nonzero and not a live flow's (high ids, e.g. bit 31 set, never meet the guest's, which
/// grow from 1); 0 when the id is taken, the port is 0, the data is short or 16 TCP flows are open.
/// The host may send TCP_DATA, TCP_EOF and TX_CREDIT at once; the guest's side sends it up to
/// 64 KiB before its first TX_CREDIT. One INCOMING_RESULT record answers it: accepted, the flow
/// runs like an outbound one; refused, it is gone (no TCP_CLOSE). TCP_RESET or FAILED before the
/// answer gives it up: the guest is reset, no record follows.) The others return 1 when applied,
/// 0 for a flow or query that is not waiting for it. No TCP_CLOSE follows FAILED or TCP_RESET.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_event(e: *mut Emu, kind: u32, id: u32, arg: u32, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let data = unsafe { bytes(ptr, len) };
    let (Some(h), Some(ev)) = (&e.net, HostEvent::from_abi(kind, id, arg, data)) else { return 0 };
    h.event(ev)
}

/// One number about the station's network: 0 associated with the access point (keys installed on
/// a WPA2 network) 1/0, 1 DHCP lease given 1/0, 2 TCP flows through the host open, 3 bytes to the
/// host, 4 bytes from the host, 5 TCP flows the host could not open, 6 DNS answers from the host,
/// 7 LAN packets sent to other boards, 8 LAN packets received from them. 0 for anything unknown.
/// (Named `wifi_stat`: `esp32sim_net_stat` is the 802.15.4 network's.)
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_stat(e: *mut Emu, which: u32) -> f64 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let host = e.net.as_ref().map(|h| (h.stats(), h.tcp_open()));
    let Some(link) = link(e) else { return 0.0 };
    let joined = link.ap().is_some_and(|ap| ap.state == StaState::Associated && (ap.cfg.psk.is_none() || ap.wpa.state == WpaState::Installed));
    let net = link.net();
    let (stats, open) = host.unwrap_or_default();
    match which {
        0 => f64::from(u8::from(joined)),
        1 => f64::from(u8::from(net.is_some_and(|n| n.dhcp_acks > 0))),
        2 => open as f64,
        3 => stats.bytes_to_host as f64,
        4 => stats.bytes_to_guest as f64,
        5 => stats.tcp_failed as f64,
        6 => stats.dns_answers as f64,
        7 => net.and_then(|n| n.lan.as_ref()).map_or(0.0, |l| l.sent as f64),
        8 => net.and_then(|n| n.lan.as_ref()).map_or(0.0, |l| l.received as f64),
        _ => 0.0,
    }
}

// ------------------------------------------------------------------ Jumperly patch J14: the station and the LAN

/// Jumperly patch J14: the station's MAC (efuse BLK1 and the machine's), so boards of one network
/// differ. Before boot only; S3 and C3. Returns 1 otherwise.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; `mac` must be
/// readable for 6 bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_set_mac(e: *mut Emu, mac: *const u8) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted || mac.is_null() { return 1; }
    // SAFETY: the caller provides six readable bytes.
    let mac: [u8; 6] = unsafe { bytes(mac, 6) }.try_into().unwrap();
    let (efuse, machine_mac) = match &mut e.m {
        MachineKind::S3(m) => (&mut m.bus.periph.efuse, &mut m.mac),
        MachineKind::C3(m) => (&mut m.bus.periph.efuse, &mut m.mac),
        MachineKind::C6(_) | MachineKind::Esp32(_) => { log("[emu] set mac: ESP32-S3 and ESP32-C3 only"); return 1; }
    };
    *machine_mac = mac;
    // BLK1 words 0 and 1, as `Efuse::new` lays them out; the rest of word 1 is the chip's
    efuse.write(0x44, u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]));
    let high = efuse.read(0x48) & !0xffff;
    efuse.write(0x48, high | (u32::from(mac[0]) << 8) | u32::from(mac[1]));
    0
}

/// Jumperly patch J14: the IPv4 address DHCP gives the station (default 10.0.2.15), so boards of
/// one network differ. After `esp32sim_wifi_ap`, before boot. Returns 1 otherwise.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; `ip` must be
/// readable for 4 bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_net_station(e: *mut Emu, ip: *const u8) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted || ip.is_null() { return 1; }
    // SAFETY: the caller provides four readable bytes.
    let ip: [u8; 4] = unsafe { bytes(ip, 4) }.try_into().unwrap();
    let Some(net) = link(e).and_then(StationLink::net_mut) else { log("[emu] net station: attach the access point first"); return 1 };
    net.sta_ip = ip;
    0
}

/// Jumperly patch J14: the other boards of the station's network (`count` × IPv4[4]). Packets for
/// them, and UDP broadcasts and multicasts other than DHCP, leave through `esp32sim_lan_take`
/// instead of the gateway. `count` 0 turns the LAN off. After `esp32sim_wifi_ap` (a new access
/// point drops it), before or after boot. Returns 1 without an access point.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero
/// `count`, `ptr` must be readable for `count * 4` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_lan_peers(e: *mut Emu, ptr: *const u8, count: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides `count * 4` readable bytes.
    let data = unsafe { bytes(ptr, count as usize * 4) };
    let Some(net) = link(e).and_then(StationLink::net_mut) else { log("[emu] lan peers: attach the access point first"); return 1 };
    let peers: Vec<[u8; 4]> = data.as_chunks::<4>().0.to_vec();
    match (&mut net.lan, peers.is_empty()) {
        (_, true) => net.lan = None,
        (Some(lan), false) => lan.peers = peers,
        (None, false) => net.lan = Some(esp_soc::net::Lan::new(peers)),
    }
    0
}

/// Jumperly patch J14: take the packets the station sent to other boards since the last call:
/// their count; the buffer holds, per packet, `f64` device time in µs (the emulated cycles over
/// the CPU clock in MHz, as the station sent it), `u32` length, then the IPv4 packet padded to a
/// multiple of 4 bytes. At most 256 wait; more are dropped.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_lan_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let packets = match link(e).and_then(StationLink::net_mut).and_then(|n| n.lan.as_mut()) {
        Some(lan) => std::mem::take(&mut lan.out),
        None => Vec::new(),
    };
    e.buf.clear();
    for (t_us, packet) in &packets {
        e.buf.extend_from_slice(&(*t_us as f64).to_le_bytes());
        e.buf.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        e.buf.extend_from_slice(packet);
        e.buf.resize(e.buf.len().next_multiple_of(4), 0);
    }
    packets.len() as u32
}

/// Jumperly patch J14: an IPv4 packet from another board for the station, delivered at once (the
/// host calls it when the packet is due by the station's device time). Returns 0 when accepted;
/// 1 without a LAN, before the station sent its first frame, or for a packet that is malformed,
/// longer than 1500 bytes, or not for the station's address, a broadcast or a multicast.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_lan_receive(e: *mut Emu, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let packet = unsafe { bytes(ptr, len) };
    let Some(link) = link(e) else { return 1 };
    u32::from(link.receive_lan_packet(packet).is_err())
}
