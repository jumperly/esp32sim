//! Jumperly patches J13 to J15: the fork's side of the station's network behind the virtual access
//! point. The NAT carried by the page is upstream's (`nat.rs`: `esp32sim_nat_relay`,
//! `esp32sim_nat_take`, `esp32sim_nat_ptr`, `esp32sim_nat_event`); here are the numbers about it
//! (`esp32sim_wifi_stat`) and the LAN between the boards of one network (J14).
//!
//! Jumperly patch J15 rides upstream's exports. `esp32sim_nat_event` also takes kind 10 INCOMING:
//! the host opens TCP flow `id` to the guest's port `arg`; data = the real peer's IPv4[4] + port
//! u16 little-endian, for the log: the guest sees 10.0.2.2 and a port of the NAT's. `id` is the
//! host's choice, nonzero and not a live flow's (high ids, e.g. bit 31 set, never meet the guest's,
//! which grow from 1); 0 when the id is taken, the port is 0, the data is short or 16 TCP flows are
//! open. The host may send TCP_DATA, TCP_EOF and TX_CREDIT at once; the guest's side sends it up to
//! 64 KiB before its first TX_CREDIT. `esp32sim_nat_take` answers with one record 9
//! INCOMING_RESULT (`id` = the host's flow, flags bit 0 = the guest accepted it; port 0, no data):
//! accepted, the flow runs like an outbound one; refused, it is gone (no TCP_CLOSE). TCP_RESET or
//! FAILED before the answer gives it up: the guest is reset, no record follows.
use super::{bytes, log, Emu};
use esp_soc::wifi::{StaState, StationLink, WpaState};

/// The station's link (S3, C3 and the classic ESP32; none on the ESP8266, J29), with the bus's
/// deferred device time delivered first.
pub(crate) fn link(e: &mut Emu) -> Option<&mut StationLink> {
    let c = e.m.chip_mut()?;   // Jumperly patch J28
    c.flush_ticks();
    c.wifi_link()
}

/// One number about the station's network: 0 associated with the access point (keys installed on
/// a WPA2 network) 1/0, 1 DHCP lease given 1/0, 2 TCP flows through the page's relay
/// (`esp32sim_nat_relay`) open, 3 bytes to the page, 4 bytes from the page, 5 TCP flows the page
/// could not open, 6 DNS answers from the page,
/// 7 LAN packets sent to other boards, 8 LAN packets received from them. 0 for anything unknown.
/// (Named `wifi_stat`: `esp32sim_net_stat` is the 802.15.4 network's.)
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_stat(e: *mut Emu, which: u32) -> f64 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let host = e.nat.as_ref().map(|h| (h.stats(), h.tcp_open()));
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
/// differ. Before boot only; S3, C3 and (J28) the classic ESP32 (eFuse BLK0 with its CRC8).
/// Returns 1 otherwise.
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
    // Jumperly patch J28: the chip writes it (`chip::write_efuse_mac` on the S3 and the C3,
    // `ClassicEfuse::new` on the classic)
    if !e.m.chip_mut().is_some_and(|c| c.set_mac(mac)) { log("[emu] set mac: ESP32-S3, ESP32-C3, ESP32 and ESP8266 only"); return 1; }
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
