//! A minimal virtual network behind the virtual access point: DHCP, ARP and ICMP echo, enough for
//! firmware to get an IP address and see the link as usable. Frames in and out are Ethernet II.
//!
//! Default layout (QEMU's user-mode numbering, so it looks familiar):
//!   10.0.2.2  gateway / DHCP server  (MAC 02:53:49:4d:00:02)
//!   10.0.2.3  DNS
//!   10.0.2.15 the station
//! Outbound traffic to the real world needs a NAT backend (docs/networking-plan.md); until then this
//! answers everything on the local subnet.

pub(crate) mod packet;
use packet::{checksum, dns_question, dns_reply, ethernet, ip_packet, transport_checksum, udp_packet, GATEWAY_MAC};

pub struct VirtualNet {
    pub gw_mac: [u8; 6],
    pub gw_ip: [u8; 4],
    pub dns_ip: [u8; 4],
    pub sta_ip: [u8; 4],
    pub mask: [u8; 4],
    pub log: bool,
    /// user-mode NAT to the host's network; None keeps everything inside the emulated subnet
    pub nat: Option<crate::nat::Nat>,
    /// Jumperly patch J12: answer SNTP here even with a NAT (the host's clock), instead of relaying it
    pub local_ntp: bool,
    /// Jumperly patch J14: the other boards of the same network, reached at the IP level
    pub lan: Option<Lan>,
    /// Jumperly patch J14: the station's MAC, learned from what it sends (None before its first frame)
    pub sta_mac: Option<[u8; 6]>,
    pub dhcp_acks: u64,
    pub dns_answers: u64,
    pub ntp_answers: u64,
    pub tcp_rejects: u64,
    pub arp_replies: u64,
    pub pings: u64,
    pub unhandled: u64,
    now_us: u64,
}

/// Jumperly patch J14: IPv4 packets between the boards of one network. A packet for a peer, and a
/// UDP broadcast or multicast (not DHCP), leave as they are with the device time they were sent;
/// the host delivers them to the peers (`VirtualNet::receive_lan`). The station resolves every
/// address to the gateway's MAC (proxy ARP), so the router needs no ARP of its own.
pub struct Lan {
    /// the other boards' addresses
    pub peers: Vec<[u8; 4]>,
    /// (device time in µs, IPv4 packet) for the host, oldest first; at most `LAN_OUT_MAX`
    pub out: Vec<(u64, Vec<u8>)>,
    /// packets sent and received; packets dropped because the host did not take `out`
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
}

/// Packets `Lan::out` holds before the host takes them; more are dropped.
pub const LAN_OUT_MAX: usize = 256;
/// The longest IPv4 packet a peer may deliver: the Wi-Fi MTU.
const LAN_MTU: usize = 1500;

impl Lan {
    pub fn new(peers: Vec<[u8; 4]>) -> Self { Lan { peers, out: Vec::new(), sent: 0, received: 0, dropped: 0 } }
}

fn be16(b: &[u8]) -> u16 { u16::from_be_bytes([b[0], b[1]]) }
fn ntp_fraction(nanos: u32) -> u32 {
    (((nanos as u64) << 32) / 1_000_000_000) as u32
}

impl VirtualNet {
    pub fn new(log: bool) -> Self {
        VirtualNet { gw_mac: GATEWAY_MAC, gw_ip: [10, 0, 2, 2], dns_ip: [10, 0, 2, 3],
                     sta_ip: [10, 0, 2, 15], mask: [255, 255, 255, 0],
                     log, nat: None, local_ntp: false, lan: None, sta_mac: None, dhcp_acks: 0, dns_answers: 0, ntp_answers: 0, tcp_rejects: 0, arp_replies: 0, pings: 0, unhandled: 0, now_us: 0 }
    }

    /// Handle one Ethernet frame from the station; returns frames to send back to it.
    pub fn handle(&mut self, eth: &[u8], now_us: u64) -> Vec<Vec<u8>> {
        if eth.len() < 14 { return Vec::new(); }
        self.now_us = now_us;
        let mut src = [0u8; 6]; src.copy_from_slice(&eth[6..12]);
        self.sta_mac = Some(src);
        match be16(&eth[12..14]) {
            0x0806 => self.arp(&eth[14..], &src),
            0x0800 => self.ipv4(&eth[14..], &src),
            et => { self.unhandled += 1; if self.log { eprintln!("[net] ignoring ethertype {:#06x} ({} bytes)", et, eth.len()); } Vec::new() }
        }
    }

    fn frame(&self, dst: &[u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        ethernet(dst, &self.gw_mac, ethertype, payload)
    }

    fn arp(&mut self, p: &[u8], src: &[u8; 6]) -> Vec<Vec<u8>> {
        if p.len() < 28 || be16(&p[6..8]) != 1 { return Vec::new(); }          // only requests
        let target = &p[24..28];
        if target == self.sta_ip { return Vec::new(); }                          // not ours to answer
        let mut r = Vec::with_capacity(28);
        r.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]);                          // ethernet/ipv4, reply
        r.extend_from_slice(&self.gw_mac); r.extend_from_slice(target);           // sender = the address asked for
        r.extend_from_slice(src); r.extend_from_slice(&p[14..18]);                // target = the station
        self.arp_replies += 1;
        if self.log { eprintln!("[net] ARP who-has {}.{}.{}.{} -> {}", target[0], target[1], target[2], target[3], crate::wifi::mac_str(&self.gw_mac)); }
        vec![self.frame(src, 0x0806, &r)]
    }

    fn invalid_ipv4(&mut self) -> Vec<Vec<u8>> {
        self.unhandled += 1;
        if self.log { eprintln!("[net] ignoring malformed or fragmented IPv4 packet"); }
        Vec::new()
    }

    fn ipv4(&mut self, p: &[u8], src: &[u8; 6]) -> Vec<Vec<u8>> {
        if p.len() < 20 { return self.invalid_ipv4(); }
        let ihl = ((p[0] & 0xf) as usize) * 4;
        if p[0] >> 4 != 4 || ihl < 20 || p.len() < ihl { return self.invalid_ipv4(); }
        // Trust the header's total length: the frame may carry padding or a trailing FCS.
        let total = u16::from_be_bytes([p[2], p[3]]) as usize;
        if total < ihl || total > p.len() || be16(&p[6..8]) & 0x3fff != 0 { return self.invalid_ipv4(); }
        let (proto, mut body) = (p[9], &p[ihl..total]);
        if proto == 17 {
            if body.len() < 8 { return self.invalid_ipv4(); }
            let len = be16(&body[4..6]) as usize;
            if len < 8 || len > body.len() { return self.invalid_ipv4(); }
            body = &body[..len];
        }
        let mut sip = [0u8; 4]; sip.copy_from_slice(&p[12..16]);
        let mut dip = [0u8; 4]; dip.copy_from_slice(&p[16..20]);
        // Jumperly patch J14: another board's packet leaves before the gateway's services and the NAT see it
        if self.lan_out(proto, body, &dip, &p[..total]) { return Vec::new(); }
        match proto {
            17 if body.len() >= 8 && be16(&body[2..4]) == 67 => self.dhcp(&body[8..], src),
            17 if body.len() >= 8 && self.local_ntp && be16(&body[2..4]) == 123 => self.ntp(&body[8..], src, &sip, &dip, be16(&body[0..2])),
            // with NAT the flow goes out through a host socket; DNS is redirected to the host's own
            // resolver but still looks like it came from the emulated one
            17 if body.len() >= 8 && self.nat.is_some() => {
                let (sport, dport) = (be16(&body[0..2]), be16(&body[2..4]));
                let now = self.now_us;
                // Jumperly patch J12: a transport that resolves names itself answers DNS
                if dport == 53 && self.nat.as_mut().unwrap().dns_query(src, &sip, sport, &dip, &body[8..], now) { return Vec::new(); }
                let (host_dst, reply_src) = if dport == 53 { (self.nat.as_ref().unwrap().resolver, dip) } else { (dip, dip) };
                self.nat.as_mut().unwrap().udp_out(src, &sip, sport, &host_dst, &reply_src, dport, &body[8..], now);
                Vec::new()
            }
            6 if self.nat.is_some() => {
                let now = self.now_us;
                self.nat.as_mut().unwrap().tcp_in(src, &sip, &dip, body, now)
            }
            17 if body.len() >= 8 && be16(&body[2..4]) == 53 => self.dns(&body[8..], src, &sip, &dip, be16(&body[0..2])),
            17 if body.len() >= 8 && be16(&body[2..4]) == 123 => self.ntp(&body[8..], src, &sip, &dip, be16(&body[0..2])),
            6 if body.len() >= 20 && body[13] & 0x02 != 0 && body[13] & 0x10 == 0 => self.tcp_reject(body, src, &sip, &dip),
            1 if body.len() >= 8 && body[0] == 8 => {                              // ICMP echo request
                let mut icmp = body.to_vec(); icmp[0] = 0; icmp[2] = 0; icmp[3] = 0;
                let c = checksum(&icmp, 0).to_be_bytes(); icmp[2] = c[0]; icmp[3] = c[1];
                self.pings += 1;
                let mut dst_ip = [0u8; 4]; dst_ip.copy_from_slice(&p[12..16]);
                let mut src_ip = [0u8; 4]; src_ip.copy_from_slice(&p[16..20]);
                if self.log { eprintln!("[net] ICMP echo request -> reply ({} bytes)", icmp.len()); }
                vec![self.frame(src, 0x0800, &ip_packet(1, &src_ip, &dst_ip, &icmp))]
            }
            _ => { self.unhandled += 1; if self.log { eprintln!("[net] ignoring IPv4 proto {} ({} bytes)", proto, p.len()); } Vec::new() }
        }
    }

    /// BOOTP/DHCP: answer DISCOVER with OFFER and REQUEST with ACK.
    fn dhcp(&mut self, d: &[u8], src: &[u8; 6]) -> Vec<Vec<u8>> {
        if d.len() < 240 || d[0] != 1 || d[236..240] != [0x63, 0x82, 0x53, 0x63] { return Vec::new(); }
        let mut msg_type = 0u8;
        let mut i = 240;
        while i + 1 < d.len() {
            let (opt, len) = (d[i], d[i + 1] as usize);
            if opt == 255 { break; }
            if opt == 0 { i += 1; continue; }
            if i + 2 + len > d.len() { break; }
            if opt == 53 && len == 1 { msg_type = d[i + 2]; }
            i += 2 + len;
        }
        let reply_type = match msg_type { 1 => 2, 3 => 5, _ => return Vec::new() };   // DISCOVER->OFFER, REQUEST->ACK

        let mut b = vec![0u8; 240];
        b[0] = 2; b[1] = 1; b[2] = 6;                                   // BOOTREPLY, ethernet, 6-byte MAC
        b[4..8].copy_from_slice(&d[4..8]);                              // xid
        b[10..12].copy_from_slice(&d[10..12]);                          // flags
        b[16..20].copy_from_slice(&self.sta_ip);                        // yiaddr
        b[20..24].copy_from_slice(&self.gw_ip);                         // siaddr
        b[28..34].copy_from_slice(src);                                 // chaddr
        b[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        b.extend_from_slice(&[53, 1, reply_type]);
        b.extend_from_slice(&[54, 4]); b.extend_from_slice(&self.gw_ip);         // server id
        b.extend_from_slice(&[51, 4, 0, 1, 0x51, 0x80]);                          // lease 86400 s
        b.extend_from_slice(&[1, 4]); b.extend_from_slice(&self.mask);
        b.extend_from_slice(&[3, 4]); b.extend_from_slice(&self.gw_ip);           // router
        b.extend_from_slice(&[6, 4]); b.extend_from_slice(&self.dns_ip);          // DNS
        b.push(255);
        while b.len() < 300 { b.push(0); }

        let mut udp = Vec::with_capacity(8 + b.len());
        udp.extend_from_slice(&67u16.to_be_bytes()); udp.extend_from_slice(&68u16.to_be_bytes());
        udp.extend_from_slice(&((8 + b.len()) as u16).to_be_bytes()); udp.extend_from_slice(&[0, 0]);   // checksum optional in IPv4
        udp.extend_from_slice(&b);

        if reply_type == 5 { self.dhcp_acks += 1; }
        if self.log { eprintln!("[net] DHCP {} -> {} for {}.{}.{}.{}", if msg_type == 1 { "DISCOVER" } else { "REQUEST" },
                                if reply_type == 2 { "OFFER" } else { "ACK" },
                                self.sta_ip[0], self.sta_ip[1], self.sta_ip[2], self.sta_ip[3]); }
        // Unicast the reply unless the client asked for a broadcast one (BOOTP flags bit 15): a
        // unicast frame travels under the pairwise key, which keeps the group key out of the picture.
        let want_bcast = d[10] & 0x80 != 0;
        let (l2, l3) = if want_bcast { ([0xffu8; 6], [255u8, 255, 255, 255]) } else { (*src, self.sta_ip) };
        let ip = ip_packet(17, &self.gw_ip, &l3, &udp);
        vec![self.frame(&l2, 0x0800, &ip)]
    }
}

impl VirtualNet {
    /// Jumperly patch J14: a broadcast or multicast address of the station's subnet.
    fn is_group(&self, ip: &[u8; 4]) -> bool {
        let bcast: Vec<u8> = self.sta_ip.iter().zip(&self.mask).map(|(a, m)| a | !m).collect();
        *ip == [255; 4] || ip[..] == bcast[..] || ip[0] & 0xf0 == 224
    }

    /// Jumperly patch J14: queue `packet` for the peers when it is theirs (to a peer, or UDP to a
    /// broadcast or multicast address other than DHCP's). False leaves it to the gateway.
    fn lan_out(&mut self, proto: u8, body: &[u8], dip: &[u8; 4], packet: &[u8]) -> bool {
        let Some(lan) = &self.lan else { return false };
        let to_peer = *dip != self.sta_ip && lan.peers.contains(dip);
        let group = proto == 17 && body.len() >= 8 && !matches!(be16(&body[2..4]), 67 | 68) && self.is_group(dip);
        if !to_peer && !group { return false; }
        let now_us = self.now_us;
        let lan = self.lan.as_mut().unwrap();
        if lan.out.len() >= LAN_OUT_MAX { lan.dropped += 1; return true; }
        lan.out.push((now_us, packet.to_vec()));
        lan.sent += 1;
        if self.log { eprintln!("[net] LAN {} bytes -> {}.{}.{}.{} at {} us", packet.len(), dip[0], dip[1], dip[2], dip[3], now_us); }
        true
    }

    /// Jumperly patch J14: an IPv4 packet from a peer, as the Ethernet frame for the station (from
    /// the gateway's MAC; a broadcast or multicast one is unicast to the station). Refused without
    /// a LAN, before the station has sent anything (its MAC is unknown), and for a malformed or
    /// oversized packet or one for another address.
    pub fn receive_lan(&mut self, packet: &[u8]) -> Result<Vec<u8>, String> {
        let Some(mac) = self.sta_mac else { return Err("the station has not sent anything yet".into()) };
        if self.lan.is_none() { return Err("no LAN (esp32sim_lan_peers)".into()); }
        if packet.len() < 20 || packet[0] >> 4 != 4 || ((packet[0] & 0xf) as usize) * 4 < 20 { return Err("not an IPv4 packet".into()); }
        let total = be16(&packet[2..4]) as usize;
        if total < ((packet[0] & 0xf) as usize) * 4 || total > packet.len() || total > LAN_MTU { return Err(format!("bad IPv4 length {total}")); }
        let mut dip = [0u8; 4]; dip.copy_from_slice(&packet[16..20]);
        if dip != self.sta_ip && !self.is_group(&dip) { return Err(format!("{}.{}.{}.{} is not this station", dip[0], dip[1], dip[2], dip[3])); }
        self.lan.as_mut().unwrap().received += 1;
        Ok(self.frame(&mac, 0x0800, &packet[..total]))
    }

    /// Pump the NAT's host sockets; returns frames for the guest.
    pub fn poll(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        self.now_us = now_us;
        match &mut self.nat { Some(n) => n.poll(now_us), None => Vec::new() }
    }

    /// Answer A queries with the local resolver address, so name lookups resolve to something that
    /// exists in the emulated network (NTP in particular). AAAA is answered empty so IPv6 is skipped.
    fn dns(&mut self, q: &[u8], src: &[u8; 6], sip: &[u8; 4], dip: &[u8; 4], sport: u16) -> Vec<Vec<u8>> {
        let Some(question) = dns_question(q) else { return Vec::new() };
        let qtype = question.qtype;
        let r = dns_reply(&q[..question.end], qtype, &[self.dns_ip], 60, 0);
        self.dns_answers += 1;
        if self.log { eprintln!("[net] DNS {} {} -> {}", question.name, if qtype == 1 { "A" } else { "AAAA" },
                                if qtype == 1 { format!("{}.{}.{}.{}", self.dns_ip[0], self.dns_ip[1], self.dns_ip[2], self.dns_ip[3]) } else { "(none)".into() }); }
        let udp = udp_packet(dip, sip, 53, sport, &r);
        vec![self.frame(src, 0x0800, &ip_packet(17, dip, sip, &udp))]
    }

    /// SNTP: hand out the host's clock, so firmware waiting for time gets it.
    #[allow(clippy::identity_op, reason = "the zero leap-indicator field remains visible in the packed header")]
    fn ntp(&mut self, q: &[u8], src: &[u8; 6], sip: &[u8; 4], dip: &[u8; 4], sport: u16) -> Vec<Vec<u8>> {
        if q.len() < 48 { return Vec::new(); }
        let now = std::time::Duration::from_millis(crate::host::unix_time_ms());
        let secs = (now.as_secs() + 2_208_988_800) as u32;               // seconds since 1900
        let frac = ntp_fraction(now.subsec_nanos());
        let mut r = vec![0u8; 48];
        r[0] = (0 << 6) | (4 << 3) | 4;                                  // no warning, version 4, server
        r[1] = 1;                                                        // stratum 1
        r[2] = q[2]; r[3] = 0xec;                                        // poll, precision
        r[12..16].copy_from_slice(b"LOCL");
        for off in [16usize, 32, 40] {                                   // reference, receive, transmit
            r[off..off + 4].copy_from_slice(&secs.to_be_bytes());
            r[off + 4..off + 8].copy_from_slice(&frac.to_be_bytes());
        }
        r[24..32].copy_from_slice(&q[40..48]);                           // originate = client transmit
        self.ntp_answers += 1;
        if self.log { eprintln!("[net] NTP request -> host time ({} s since 1900)", secs); }
        let udp = udp_packet(dip, sip, 123, sport, &r);
        vec![self.frame(src, 0x0800, &ip_packet(17, dip, sip, &udp))]
    }

    /// Nothing here speaks TCP yet, so refuse connections immediately instead of letting firmware
    /// sit in a 30-second connect timeout. A NAT backend is what will make these work.
    fn tcp_reject(&mut self, t: &[u8], src: &[u8; 6], sip: &[u8; 4], dip: &[u8; 4]) -> Vec<Vec<u8>> {
        let (sport, dport) = (be16(&t[0..2]), be16(&t[2..4]));
        let seq = u32::from_be_bytes([t[4], t[5], t[6], t[7]]);
        let mut r = Vec::with_capacity(20);
        r.extend_from_slice(&dport.to_be_bytes()); r.extend_from_slice(&sport.to_be_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);                              // seq 0
        r.extend_from_slice(&seq.wrapping_add(1).to_be_bytes());          // ack the SYN
        r.extend_from_slice(&[0x50, 0x14, 0, 0, 0, 0, 0, 0]);            // RST|ACK
        let c = transport_checksum(dip, sip, 6, &r).to_be_bytes(); r[16] = c[0]; r[17] = c[1];
        self.tcp_rejects += 1;
        if self.log { eprintln!("[net] TCP {}.{}.{}.{}:{} -> refused (no NAT backend yet)", dip[0], dip[1], dip[2], dip[3], dport); }
        vec![self.frame(src, 0x0800, &ip_packet(6, dip, sip, &r))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STA_MAC: [u8; 6] = [0x44, 0x1b, 0xf6, 0x75, 0xdc, 0xe0];
    const PEER: [u8; 4] = [10, 0, 2, 16];

    /// A station at .15 with one peer at .16, its MAC learned from an ARP request it sent.
    fn lan_net() -> VirtualNet {
        let mut net = VirtualNet::new(false);
        net.lan = Some(Lan::new(vec![PEER]));
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&STA_MAC); arp.extend_from_slice(&net.sta_ip);
        arp.extend_from_slice(&[0; 6]); arp.extend_from_slice(&PEER);
        let reply = net.handle(&ethernet(&[0xff; 6], &STA_MAC, 0x0806, &arp), 0);
        assert_eq!(&reply[0][14 + 8..14 + 14], &GATEWAY_MAC, "a peer's address is the gateway's MAC");
        net
    }

    fn syn_to(dst: &[u8; 4]) -> Vec<u8> {
        let mut t = vec![0u8; 20];
        t[0..2].copy_from_slice(&40000u16.to_be_bytes()); t[2..4].copy_from_slice(&7000u16.to_be_bytes());
        t[12] = 0x50; t[13] = 0x02;
        ip_packet(6, &[10, 0, 2, 15], dst, &t)
    }

    fn udp_to(dst: &[u8; 4], port: u16) -> Vec<u8> {
        ip_packet(17, &[10, 0, 2, 15], dst, &udp_packet(&[10, 0, 2, 15], dst, 7001, port, b"hello"))
    }

    #[test]
    fn a_syn_to_a_peer_leaves_on_the_lan_not_through_the_nat() {
        for nat in [false, true] {
            let mut net = lan_net();
            let handle = nat.then(|| {
                let (t, h) = crate::nat::QueueTransport::new();
                net.nat = Some(crate::nat::Nat::with_transport(false, Box::new(t)));
                h
            });
            let packet = syn_to(&PEER);
            assert!(net.handle(&ethernet(&GATEWAY_MAC, &STA_MAC, 0x0800, &packet), 1234).is_empty());
            let lan = net.lan.as_ref().unwrap();
            assert_eq!(lan.out, vec![(1234, packet)]);
            assert_eq!(lan.sent, 1);
            assert_eq!(net.tcp_rejects, 0);
            if let Some(h) = handle { assert_eq!(h.take_ops(&mut Vec::new()), 0, "nothing for the host"); }
        }
    }

    #[test]
    fn a_non_peer_without_a_nat_is_still_refused() {
        let mut net = lan_net();
        let reply = net.handle(&ethernet(&GATEWAY_MAC, &STA_MAC, 0x0800, &syn_to(&[10, 0, 2, 20])), 5);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0][14 + 20 + 13], 0x14, "RST|ACK");
        assert!(net.lan.as_ref().unwrap().out.is_empty());
    }

    #[test]
    fn udp_broadcast_and_multicast_go_to_the_peers_but_dhcp_stays() {
        let mut net = lan_net();
        for (dst, port) in [([10, 0, 2, 255], 7001), ([255, 255, 255, 255], 7001), ([224, 0, 0, 251], 5353)] {
            assert!(net.handle(&ethernet(&[0xff; 6], &STA_MAC, 0x0800, &udp_to(&dst, port)), 7).is_empty());
        }
        let out: Vec<[u8; 4]> = net.lan.as_ref().unwrap().out.iter().map(|(_, p)| p[16..20].try_into().unwrap()).collect();
        assert_eq!(out, vec![[10, 0, 2, 255], [255, 255, 255, 255], [224, 0, 0, 251]]);
        // a DHCP DISCOVER is a broadcast too, but the gateway answers it
        let mut dhcp = vec![0u8; 240];
        dhcp[0] = 1; dhcp[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        dhcp.extend_from_slice(&[53, 1, 1, 255]);
        let discover = ip_packet(17, &[0; 4], &[255; 4], &udp_packet(&[0; 4], &[255; 4], 68, 67, &dhcp));
        assert_eq!(net.handle(&ethernet(&[0xff; 6], &STA_MAC, 0x0800, &discover), 8).len(), 1, "an OFFER");
        assert_eq!(net.lan.as_ref().unwrap().out.len(), 3);
        // TCP to a broadcast address is not the LAN's
        assert!(!net.lan_out(6, &[0; 20], &[10, 0, 2, 255], &[]));
    }

    #[test]
    fn a_peer_packet_reaches_the_station_from_the_gateway_mac() {
        let mut net = lan_net();
        let packet = syn_to(&net.sta_ip.clone());
        let frame = net.receive_lan(&packet).unwrap();
        assert_eq!(&frame[..6], &STA_MAC);
        assert_eq!(&frame[6..12], &GATEWAY_MAC);
        assert_eq!(&frame[12..14], &[8, 0]);
        assert_eq!(&frame[14..], &packet[..]);
        let mut padded = udp_to(&[10, 0, 2, 255], 7001);
        let len = padded.len();
        padded.extend_from_slice(&[0; 6]);
        assert_eq!(net.receive_lan(&padded).unwrap().len(), 14 + len, "a broadcast, trimmed to its length");
        assert_eq!(&net.receive_lan(&udp_to(&[224, 0, 0, 251], 5353)).unwrap()[..6], &STA_MAC, "multicast as L2 unicast");
        assert_eq!(net.lan.as_ref().unwrap().received, 3);
        assert!(net.receive_lan(&syn_to(&[10, 0, 2, 17])).is_err(), "another station's");
        assert!(net.receive_lan(&packet[..19]).is_err());
        assert!(net.receive_lan(&packet[..30]).is_err(), "shorter than its total length");
        let mut fresh = VirtualNet::new(false);
        fresh.lan = Some(Lan::new(vec![PEER]));
        assert!(fresh.receive_lan(&packet).is_err(), "the station's MAC is not known yet");
        net.lan = None;
        assert!(net.receive_lan(&packet).is_err(), "no LAN");
    }

    #[test]
    fn the_lan_holds_a_bounded_number_of_untaken_packets() {
        let mut net = lan_net();
        for _ in 0..LAN_OUT_MAX + 3 { net.handle(&ethernet(&GATEWAY_MAC, &STA_MAC, 0x0800, &syn_to(&PEER)), 1); }
        let lan = net.lan.as_ref().unwrap();
        assert_eq!((lan.out.len(), lan.sent, lan.dropped), (LAN_OUT_MAX, LAN_OUT_MAX as u64, 3));
    }

    #[test]
    fn ntp_fraction_converts_nanoseconds() {
        assert_eq!(ntp_fraction(500_000_000), 0x8000_0000);
        assert_eq!(ntp_fraction(250_000_000), 0x4000_0000);
    }

    #[test]
    fn malformed_ip_udp_and_icmp_are_rejected_before_dispatch() {
        let mut net = VirtualNet::new(false);
        net.nat = Some(crate::nat::Nat::new(false));
        for proto in [1, 17] {
            for len in 0..8 {
                let mut body = vec![0; len];
                if proto == 1 && len > 0 { body[0] = 8; }
                let packet = ip_packet(proto, &net.sta_ip, &net.gw_ip, &body);
                assert!(net.ipv4(&packet, &[2; 6]).is_empty());
            }
        }
        let mut packet = ip_packet(1, &net.sta_ip, &net.gw_ip, &[8; 8]);
        for ihl in 0..5 {
            packet[0] = 0x40 | ihl;
            assert!(net.ipv4(&packet, &[2; 6]).is_empty());
        }
        packet[0] = 0x45;
        packet[6] = 0x20; // fragments are not reassembled by this relay
        assert!(net.ipv4(&packet, &[2; 6]).is_empty());
        assert_eq!(net.pings, 0);
        assert_eq!(net.unhandled, 22);
    }

    #[test]
    fn ipv4_options_df_and_frame_padding_preserve_icmp_payload() {
        let mut net = VirtualNet::new(false);
        let request = [8, 0, 0, 0, 0x12, 0x34, 0, 1, 0x42];
        let mut packet = ip_packet(1, &net.sta_ip, &net.gw_ip, &request);
        packet.splice(20..20, [1, 1, 0, 0]); // Four bytes of IPv4 options.
        packet[0] = 0x46;
        packet[6] = 0x40; // DF does not require fragment reassembly.
        let total = packet.len() as u16;
        packet[2..4].copy_from_slice(&total.to_be_bytes());
        packet.extend_from_slice(&[0; 12]); // Link-layer trailer is outside total_len.
        let reply = net.ipv4(&packet, &[2; 6]);
        assert_eq!(reply.len(), 1);
        assert_eq!(&reply[0][38..], &request[4..]);
        assert_eq!(net.unhandled, 0);
    }

    #[test]
    fn udp_length_excludes_extra_ip_body_bytes() {
        let mut net = VirtualNet::new(false);
        let mut request = [0; 48]; request[0] = 0x23;
        let mut udp = udp_packet(&net.sta_ip, &net.gw_ip, 1234, 123, &request);
        udp.extend_from_slice(b"padding");
        let packet = ip_packet(17, &net.sta_ip, &net.gw_ip, &udp);
        let reply = net.ipv4(&packet, &[2; 6]);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].len(), 14 + 20 + 8 + 48);
        assert_eq!(net.ntp_answers, 1);
        assert_eq!(net.unhandled, 0);
    }

    #[test]
    fn icmp_reply_uses_shared_valid_packet_checksums() {
        let mut net = VirtualNet::new(false);
        let mut request = vec![8, 0, 0, 0, 0x12, 0x34, 0, 1, 0x42];
        let check = checksum(&request, 0);
        request[2..4].copy_from_slice(&check.to_be_bytes());
        let packet = ip_packet(1, &net.sta_ip, &net.gw_ip, &request);
        let reply = net.ipv4(&packet, &[2; 6]);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0][34], 0);
        assert_eq!(checksum(&reply[0][14..34], 0), 0);
        assert_eq!(checksum(&reply[0][34..], 0), 0);
    }
}
