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
        MachineKind::C6(_) => None,
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
/// data = the name in UTF-8). 0 without `esp32sim_net_host`.
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
/// 9 UNRESOLVED (`arg` = rcode 2 SERVFAIL or 3 NXDOMAIN). The others return 1 when applied, 0 for a
/// flow or query that is not waiting for it. No TCP_CLOSE follows FAILED or TCP_RESET.
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
        _ => 0.0,
    }
}
