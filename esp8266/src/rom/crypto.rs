//! The ROM's crypto functions, which the SDK's Wi-Fi libraries (libwpa, libnet80211) call. The
//! prototypes are hostap's (wpa_supplicant 0.7/1.x, which the ESP8266 SDK is built from:
//! src/crypto/md5-internal.c, md5.c, sha1.c, sha1-pbkdf2.c, rc4.c, aes-internal-dec.c); the
//! algorithms are the standards': MD5 RFC 1321, HMAC RFC 2104, SHA-1 FIPS 180-4, PBKDF2 RFC 8018,
//! the IEEE 802.11 PRF, RC4 with a skipped key stream, AES FIPS 197 (its equivalent inverse
//! cipher, section 5.3.5, with the decryption key schedule rijndael-alg-fst.c builds). SHA-1,
//! HMAC-SHA1, PBKDF2 and the PRF are esp-periph's.
use super::{arg, ret, Lx106, SocBus};
use esp_periph::crypto as pc;

/// The longest buffer a call reads, against a runaway guest length.
const MAX_LEN: u32 = 1 << 20;

fn read(bus: &mut SocBus, addr: u32, len: u32) -> Vec<u8> {
    (0..len.min(MAX_LEN)).map(|i| bus.peek8(addr.wrapping_add(i)).unwrap_or(0)).collect()
}

fn write(bus: &mut SocBus, addr: u32, data: &[u8]) {
    for (i, &b) in data.iter().enumerate() { bus.poke8(addr.wrapping_add(i as u32), b); }
}

fn read_words<const N: usize>(bus: &mut SocBus, addr: u32) -> [u32; N] {
    let b = read(bus, addr, 4 * N as u32);
    std::array::from_fn(|i| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]))
}

fn write_words(bus: &mut SocBus, addr: u32, w: &[u32]) {
    let b: Vec<u8> = w.iter().flat_map(|v| v.to_le_bytes()).collect();
    write(bus, addr, &b);
}

/// A NUL-terminated string (hostap passes the passphrase and the PRF label so).
fn c_string(bus: &mut SocBus, addr: u32) -> Vec<u8> {
    let mut s = Vec::new();
    while s.len() < 4096 { match bus.peek8(addr.wrapping_add(s.len() as u32)) { Some(0) | None => break, Some(b) => s.push(b) } }
    s
}

/// The `num` buffers of a hostap vector call: `addr[i]`, `len[i]`, concatenated.
fn vector(bus: &mut SocBus, num: u32, addrs: u32, lens: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..num.min(16) {
        let [a] = read_words::<1>(bus, addrs + 4 * i);
        let [l] = read_words::<1>(bus, lens + 4 * i);
        v.extend(read(bus, a, l));
    }
    v
}

// ------------------------------------------------------------------ MD5 (RFC 1321)

const MD5_INIT: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
/// RFC 1321 section 3.4: the per-step shifts and T[i] = floor(2^32 * abs(sin(i + 1))).
const MD5_S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
const MD5_T: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391];

fn md5_block(h: &mut [u32; 4], block: &[u8]) {
    let x: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]));
    let [mut a, mut b, mut c, mut d] = *h;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((b & d) | (c & !d), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let t = a.wrapping_add(f).wrapping_add(MD5_T[i]).wrapping_add(x[g]).rotate_left(MD5_S[(i / 16) * 4 + i % 4]);
        a = d; d = c; c = b; b = b.wrapping_add(t);
    }
    for (s, v) in h.iter_mut().zip([a, b, c, d]) { *s = s.wrapping_add(v); }
}

/// hostap's `struct MD5Context` (md5_i.h): u32 buf[4], u32 bits[2] (the length in bits, low word
/// first), u8 in[64] (the partial block): 88 bytes in the guest's memory.
struct Md5 { buf: [u32; 4], bits: u64, input: Vec<u8> }

impl Md5 {
    fn new() -> Self { Md5 { buf: MD5_INIT, bits: 0, input: Vec::new() } }
    fn update(&mut self, data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) << 3);
        self.input.extend_from_slice(data);
        let full = self.input.len() / 64 * 64;
        for block in self.input[..full].chunks(64) { md5_block(&mut self.buf, block); }
        self.input.drain(..full);
    }
    fn finish(mut self) -> [u8; 16] {
        let bits = self.bits;
        let mut pad = vec![0x80u8];
        pad.resize((55usize.wrapping_sub(self.input.len()) % 64) + 1, 0);
        pad.extend_from_slice(&bits.to_le_bytes());
        self.update(&pad);
        let mut out = [0; 16];
        for (i, w) in self.buf.iter().enumerate() { out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes()); }
        out
    }
    fn load(bus: &mut SocBus, ctx: u32) -> Self {
        let w = read_words::<6>(bus, ctx);
        let bits = u64::from(w[4]) | u64::from(w[5]) << 32;
        let input = read(bus, ctx + 24, ((bits >> 3) % 64) as u32);
        Md5 { buf: [w[0], w[1], w[2], w[3]], bits, input }
    }
    fn store(&self, bus: &mut SocBus, ctx: u32) {
        write_words(bus, ctx, &[self.buf[0], self.buf[1], self.buf[2], self.buf[3], self.bits as u32, (self.bits >> 32) as u32]);
        let mut block = self.input.clone();
        block.resize(64, 0);
        write(bus, ctx + 24, &block);
    }
}

pub fn md5(data: &[u8]) -> [u8; 16] { let mut m = Md5::new(); m.update(data); m.finish() }

/// HMAC (RFC 2104) over MD5.
pub fn hmac_md5_bytes(key: &[u8], data: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 64];
    if key.len() > 64 { k[..16].copy_from_slice(&md5(key)); } else { k[..key.len()].copy_from_slice(key); }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&md5(&inner));
    md5(&outer)
}

/// `MD5Init(ctx)`.
pub fn md5_init(core: &mut Lx106, bus: &mut SocBus) { let ctx = arg(core, bus, 0); Md5::new().store(bus, ctx); ret(core, bus, 0); }

/// `MD5Update(ctx, buf, len)`.
pub fn md5_update(core: &mut Lx106, bus: &mut SocBus) {
    let (ctx, buf, len) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let mut m = Md5::load(bus, ctx);
    let data = read(bus, buf, len);
    m.update(&data);
    m.store(bus, ctx);
    ret(core, bus, 0);
}

/// `MD5Final(digest, ctx)`: the digest, and the context cleared (md5-internal.c).
pub fn md5_final(core: &mut Lx106, bus: &mut SocBus) {
    let (digest, ctx) = (arg(core, bus, 0), arg(core, bus, 1));
    let out = Md5::load(bus, ctx).finish();
    write(bus, digest, &out);
    write(bus, ctx, &[0; 88]);
    ret(core, bus, 0);
}

/// `md5_vector(num_elem, addr[], len[], mac)`: 0.
pub fn md5_vector(core: &mut Lx106, bus: &mut SocBus) {
    let (num, addrs, lens, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3));
    let data = vector(bus, num, addrs, lens);
    write(bus, mac, &md5(&data));
    ret(core, bus, 0);
}

/// `hmac_md5(key, key_len, data, data_len, mac)`: 0.
pub fn hmac_md5(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, d, dl, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4));
    let (key, data) = (read(bus, k, kl), read(bus, d, dl));
    write(bus, mac, &hmac_md5_bytes(&key, &data));
    ret(core, bus, 0);
}

/// `hmac_md5_vector(key, key_len, num_elem, addr[], len[], mac)`: 0.
pub fn hmac_md5_vector(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, num, addrs, lens, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4), arg(core, bus, 5));
    let key = read(bus, k, kl);
    let data = vector(bus, num, addrs, lens);
    write(bus, mac, &hmac_md5_bytes(&key, &data));
    ret(core, bus, 0);
}

// ------------------------------------------------------------------ SHA-1 family (FIPS 180-4)

/// `sha1_vector(num_elem, addr[], len[], mac)`: 0.
pub fn sha1_vector(core: &mut Lx106, bus: &mut SocBus) {
    let (num, addrs, lens, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3));
    let data = vector(bus, num, addrs, lens);
    write(bus, mac, &pc::sha1(&data));
    ret(core, bus, 0);
}

/// `hmac_sha1(key, key_len, data, data_len, mac)`: 0.
pub fn hmac_sha1(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, d, dl, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4));
    let (key, data) = (read(bus, k, kl), read(bus, d, dl));
    write(bus, mac, &pc::hmac_sha1(&key, &data));
    ret(core, bus, 0);
}

/// `hmac_sha1_vector(key, key_len, num_elem, addr[], len[], mac)`: 0.
pub fn hmac_sha1_vector(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, num, addrs, lens, mac) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4), arg(core, bus, 5));
    let key = read(bus, k, kl);
    let data = vector(bus, num, addrs, lens);
    write(bus, mac, &pc::hmac_sha1(&key, &data));
    ret(core, bus, 0);
}

/// `pbkdf2_sha1(passphrase, ssid, ssid_len, iterations, buf, buflen)`: the WPA PSK; 0.
pub fn pbkdf2_sha1(core: &mut Lx106, bus: &mut SocBus) {
    let (p, s, sl, iter, buf, bl) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4), arg(core, bus, 5));
    let pass = c_string(bus, p);
    let ssid = read(bus, s, sl);
    let out = pc::pbkdf2_sha1(&pass, &ssid, iter.clamp(1, 1 << 20), bl.min(MAX_LEN) as usize);
    write(bus, buf, &out);
    ret(core, bus, 0);
}

/// `sha1_prf(key, key_len, label, data, data_len, buf, buf_len)` (the 7th argument on the
/// stack): the IEEE 802.11 PRF; 0.
pub fn sha1_prf(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, l, d, dl, buf, bl) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4), arg(core, bus, 5), arg(core, bus, 6));
    let key = read(bus, k, kl);
    let label = String::from_utf8_lossy(&c_string(bus, l)).into_owned();
    let data = read(bus, d, dl);
    let out = pc::prf(&key, &label, &data, bl.min(MAX_LEN) as usize * 8);
    write(bus, buf, &out);
    ret(core, bus, 0);
}

// ------------------------------------------------------------------ RC4

/// RC4 keyed with `key`, its first `skip` key stream bytes dropped, XORed into `data`.
pub fn rc4(key: &[u8], skip: u32, data: &mut [u8]) {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    if key.is_empty() { return; }
    let mut j = 0u8;
    for i in 0..256 { j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]); s.swap(i, j as usize); }
    let (mut i, mut j) = (0u8, 0u8);
    let mut next = || {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        s[s[i as usize].wrapping_add(s[j as usize]) as usize]
    };
    for _ in 0..skip.min(MAX_LEN) { next(); }
    for b in data.iter_mut() { *b ^= next(); }
}

/// `rc4_skip(key, keylen, skip, data, data_len)`: in place; 0.
pub fn rc4_skip(core: &mut Lx106, bus: &mut SocBus) {
    let (k, kl, skip, d, dl) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4));
    let key = read(bus, k, kl);
    let mut data = read(bus, d, dl);
    rc4(&key, skip, &mut data);
    write(bus, d, &data);
    ret(core, bus, 0);
}

// ------------------------------------------------------------------ AES (FIPS 197)

/// GF(2^8) product with the AES polynomial x^8 + x^4 + x^3 + x + 1 (FIPS 197 section 4.2).
const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 { p ^= a; }
        a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
        b >>= 1;
    }
    p
}

/// The S-box (FIPS 197 section 5.1.1): the multiplicative inverse, then the affine map.
const SBOX: [u8; 256] = {
    let mut s = [0u8; 256];
    let mut x = 0;
    while x < 256 {
        // x^254 is the inverse (0 for 0)
        let (mut inv, mut base, mut e) = (1u8, x as u8, 254u32);
        while e > 0 { if e & 1 != 0 { inv = gmul(inv, base); } base = gmul(base, base); e >>= 1; }
        if x == 0 { inv = 0; }
        s[x] = 0x63 ^ inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4);
        x += 1;
    }
    s
};
const INV_SBOX: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 { t[SBOX[i] as usize] = i as u8; i += 1; }
    t
};

/// InvMixColumns of one column (FIPS 197 section 5.3.3), the column as a big-endian word.
fn inv_mix(w: u32) -> u32 {
    let c = w.to_be_bytes();
    let m = |a: u8, b: u8, d: u8, e: u8| gmul(c[0], a) ^ gmul(c[1], b) ^ gmul(c[2], d) ^ gmul(c[3], e);
    u32::from_be_bytes([m(14, 11, 13, 9), m(9, 14, 11, 13), m(13, 9, 14, 11), m(11, 13, 9, 14)])
}

/// AES-128's rounds; the ROM's `aes_decrypt` and `rijndaelKeySetupDec` are AES-128 only
/// (hostap aes-internal-dec.c `aes_decrypt_init` refuses other key lengths).
const NR: usize = 10;

/// `rijndaelKeySetupDec`'s schedule: the encryption schedule (FIPS 197 section 5.2, as
/// big-endian words), the round keys in reverse order, InvMixColumns on all but the first and
/// the last (the equivalent inverse cipher's dw, section 5.3.5).
pub fn key_setup_dec(key: &[u8; 16]) -> [u32; 44] {
    let mut w = [0u32; 44];
    for i in 0..4 { w[i] = u32::from_be_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]); }
    let mut rcon = 1u8;
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            let b = t.rotate_left(8).to_be_bytes();
            t = u32::from_be_bytes([SBOX[b[0] as usize] ^ rcon, SBOX[b[1] as usize], SBOX[b[2] as usize], SBOX[b[3] as usize]]);
            rcon = gmul(rcon, 2);
        }
        w[i] = w[i - 4] ^ t;
    }
    let mut rk = [0u32; 44];
    for r in 0..=NR { rk[4 * r..4 * r + 4].copy_from_slice(&w[4 * (NR - r)..4 * (NR - r) + 4]); }
    for word in rk.iter_mut().take(4 * NR).skip(4) { *word = inv_mix(*word); }
    rk
}

/// One block through the equivalent inverse cipher with `rk` from `key_setup_dec`.
pub fn decrypt_block(rk: &[u32; 44], input: &[u8; 16]) -> [u8; 16] {
    let mut s: [u32; 4] = std::array::from_fn(|c| u32::from_be_bytes([input[4 * c], input[4 * c + 1], input[4 * c + 2], input[4 * c + 3]]) ^ rk[c]);
    for round in 1..=NR {
        // InvShiftRows + InvSubBytes: row r of column c comes from column c - r
        let b: Vec<[u8; 4]> = s.iter().map(|w| w.to_be_bytes()).collect();
        let mut t: [u32; 4] = std::array::from_fn(|c| u32::from_be_bytes(std::array::from_fn(|r| INV_SBOX[b[(c + 4 - r) % 4][r] as usize])));
        if round < NR { for w in t.iter_mut() { *w = inv_mix(*w); } }
        for (c, w) in t.iter_mut().enumerate() { *w ^= rk[4 * round + c]; }
        s = t;
    }
    let mut out = [0; 16];
    for (c, w) in s.iter().enumerate() { out[4 * c..4 * c + 4].copy_from_slice(&w.to_be_bytes()); }
    out
}

/// `rijndaelKeySetupDec(rk, cipherKey)`: the 44-word decryption schedule into `rk`.
pub fn rijndael_key_setup_dec(core: &mut Lx106, bus: &mut SocBus) {
    let (rk, k) = (arg(core, bus, 0), arg(core, bus, 1));
    let key: [u8; 16] = read(bus, k, 16).try_into().unwrap();
    write_words(bus, rk, &key_setup_dec(&key));
    ret(core, bus, NR as u32);
}

/// `aes_decrypt(ctx, crypt, plain)`: one block with the schedule at `ctx` (what
/// `aes_decrypt_init` made with `rijndaelKeySetupDec`).
pub fn aes_decrypt(core: &mut Lx106, bus: &mut SocBus) {
    let (ctx, c, p) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let rk = read_words::<44>(bus, ctx);
    let input: [u8; 16] = read(bus, c, 16).try_into().unwrap();
    write(bus, p, &decrypt_block(&rk, &input));
    ret(core, bus, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

    #[test]
    fn md5_and_hmac_md5_match_rfc_1321_and_rfc_2104() {
        assert_eq!(md5(b"").to_vec(), hex("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(md5(b"abc").to_vec(), hex("900150983cd24fb0d6963f7d28e17f72"));
        let long = b"12345678901234567890123456789012345678901234567890123456789012345678901234567890";
        assert_eq!(md5(long).to_vec(), hex("57edf4a22be3c955ac49da2e2107b67a"));
        // RFC 2104 test 2
        assert_eq!(hmac_md5_bytes(b"Jefe", b"what do ya want for nothing?").to_vec(), hex("750c783e6ab0b503eaa86e310a5db738"));
    }

    #[test]
    fn md5_context_survives_the_guest_memory_round_trip() {
        let mut bus = SocBus::new(1 << 20, [0; 6]);
        let ctx = 0x3FFE_9000;
        Md5::new().store(&mut bus, ctx);
        for chunk in [&b"1234567890"[..], &[b'x'; 70][..], b"tail"] {
            let mut m = Md5::load(&mut bus, ctx);
            m.update(chunk);
            m.store(&mut bus, ctx);
        }
        let mut all = b"1234567890".to_vec();
        all.extend([b'x'; 70]);
        all.extend(b"tail");
        assert_eq!(Md5::load(&mut bus, ctx).finish(), md5(&all));
    }

    #[test]
    fn rc4_skip_matches_rfc_6229_and_skips_the_stream() {
        // RFC 6229, key 0x0102030405, key stream at offset 0
        let mut d = [0u8; 8];
        rc4(&hex("0102030405"), 0, &mut d);
        assert_eq!(d.to_vec(), hex("b2396305f03dc027"));
        let mut all = [0u8; 300];
        rc4(&hex("0102030405"), 0, &mut all);
        let mut tail = [0u8; 44];
        rc4(&hex("0102030405"), 256, &mut tail);
        assert_eq!(tail[..], all[256..]);
    }

    #[test]
    fn aes_decrypt_inverts_the_fips_197_example() {
        assert_eq!((SBOX[0], SBOX[0x53], INV_SBOX[0xed]), (0x63, 0xed, 0x53));
        // FIPS 197 appendix C.1
        let key: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let ct: [u8; 16] = hex("69c4e0d86a7b0430d8cdb78070b4c55a").try_into().unwrap();
        assert_eq!(decrypt_block(&key_setup_dec(&key), &ct).to_vec(), hex("00112233445566778899aabbccddeeff"));
        // and against esp-periph's AES on another block
        let block = [0x5au8; 16];
        let enc = pc::aes_block(&key, &block, false);
        assert_eq!(decrypt_block(&key_setup_dec(&key), &enc), block);
    }
}
