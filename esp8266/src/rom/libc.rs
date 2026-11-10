//! The ROM's C library: memory and string functions (both the `ets_` names and the plain ones,
//! with the standard C semantics; prototypes in tools/sdk/include/ets_sys.h:204-218) and the
//! libgcc integer helpers the LX106 needs without a divider (call0 ABI: 64-bit values in a
//! register pair, low word first). They take one cycle: the ROM's own timing is not modelled.
use super::{arg, ret, ret64, SocBus};
use xtensa_lx7::Lx106;

/// A byte at `addr`, or 0 (with one log line) where the ROM's code would have faulted.
fn get(bus: &mut SocBus, addr: u32) -> u8 {
    bus.peek8(addr).unwrap_or_else(|| { bus.rom.log_once("libc-read", || format!("a string or memory function read {addr:#010x}, outside memory (reads 0)")); 0 })
}

fn set(bus: &mut SocBus, addr: u32, v: u8) {
    if !bus.poke8(addr, v) { bus.rom.log_once("libc-write", || format!("a string or memory function wrote {addr:#010x}, outside RAM (dropped)")); }
}

/// The NUL-terminated string at `addr` is this long.
fn len(bus: &mut SocBus, addr: u32) -> u32 {
    let mut n = 0;
    while n < MAX_LEN && get(bus, addr.wrapping_add(n)) != 0 { n += 1; }
    n
}

/// The longest run one call handles: more than the chip's memory, so it only stops a guest's
/// runaway length from holding the host for minutes.
const MAX_LEN: u32 = 1 << 24;

/// The first three arguments, the third a length.
fn args3(core: &Lx106, bus: &mut SocBus) -> (u32, u32, u32) { (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2).min(MAX_LEN)) }

pub fn memcpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    for i in 0..n { let b = get(bus, s.wrapping_add(i)); set(bus, d.wrapping_add(i), b); }
    ret(core, bus, d);
}

pub fn memmove(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    let bytes: Vec<u8> = (0..n).map(|i| get(bus, s.wrapping_add(i))).collect();
    for (i, b) in bytes.into_iter().enumerate() { set(bus, d.wrapping_add(i as u32), b); }
    ret(core, bus, d);
}

pub fn memset(core: &mut Lx106, bus: &mut SocBus) {
    let (d, c, n) = args3(core, bus);
    for i in 0..n { set(bus, d.wrapping_add(i), c as u8); }
    ret(core, bus, d);
}

pub fn bzero(core: &mut Lx106, bus: &mut SocBus) {
    let (d, n) = (arg(core, bus, 0), arg(core, bus, 1).min(MAX_LEN));
    for i in 0..n { set(bus, d.wrapping_add(i), 0); }
    ret(core, bus, 0);
}

pub fn memcmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b, n) = args3(core, bus);
    let mut r = 0i32;
    for i in 0..n {
        let (x, y) = (get(bus, a.wrapping_add(i)), get(bus, b.wrapping_add(i)));
        if x != y { r = x as i32 - y as i32; break; }
    }
    ret(core, bus, r as u32);
}

pub fn strlen(core: &mut Lx106, bus: &mut SocBus) { let s = arg(core, bus, 0); let n = len(bus, s); ret(core, bus, n); }

/// Copies the string and its NUL; a runaway one stops after `MAX_LEN` bytes, unterminated.
pub fn strcpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s) = (arg(core, bus, 0), arg(core, bus, 1));
    for i in 0..MAX_LEN {
        let b = get(bus, s.wrapping_add(i));
        set(bus, d.wrapping_add(i), b);
        if b == 0 { break; }
    }
    ret(core, bus, d);
}

/// Copies at most `n` bytes and pads the rest of them with NULs.
pub fn strncpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    let mut end = false;
    for i in 0..n {
        let b = if end { 0 } else { get(bus, s.wrapping_add(i)) };
        end |= b == 0;
        set(bus, d.wrapping_add(i), b);
    }
    ret(core, bus, d);
}

/// The C comparison of two strings, over at most `n` bytes.
fn compare(bus: &mut SocBus, a: u32, b: u32, n: u32) -> i32 {
    for i in 0..n {
        let (x, y) = (get(bus, a.wrapping_add(i)), get(bus, b.wrapping_add(i)));
        if x != y { return x as i32 - y as i32; }
        if x == 0 { break; }
    }
    0
}

pub fn strcmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    let r = compare(bus, a, b, MAX_LEN);
    ret(core, bus, r as u32);
}

pub fn strncmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b, n) = args3(core, bus);
    let r = compare(bus, a, b, n);
    ret(core, bus, r as u32);
}

/// The first `needle` in `haystack`, or NULL.
pub fn strstr(core: &mut Lx106, bus: &mut SocBus) {
    let (h, n) = (arg(core, bus, 0), arg(core, bus, 1));
    let needle: Vec<u8> = (0..len(bus, n)).map(|i| get(bus, n.wrapping_add(i))).collect();
    let hay: Vec<u8> = (0..len(bus, h)).map(|i| get(bus, h.wrapping_add(i))).collect();
    let at = if needle.is_empty() { Some(0) } else { hay.windows(needle.len()).position(|w| w == needle.as_slice()) };
    ret(core, bus, at.map_or(0, |i| h.wrapping_add(i as u32)));
}

/// A division by zero, which libgcc leaves undefined: 0, with one log line.
fn div_zero(bus: &mut SocBus) { bus.rom.log_once("div0", || "integer division by zero in a libgcc helper (returns 0)".into()); }

fn arg64(core: &Lx106, bus: &mut SocBus, pair: usize) -> u64 { u64::from(arg(core, bus, pair * 2)) | u64::from(arg(core, bus, pair * 2 + 1)) << 32 }

pub fn divsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0) as i32, arg(core, bus, 1) as i32);
    if b == 0 { div_zero(bus); }
    ret(core, bus, a.checked_div(b).unwrap_or(if b == 0 { 0 } else { a }) as u32);   // i32::MIN / -1 wraps
}

pub fn udivsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    if b == 0 { div_zero(bus); }
    ret(core, bus, a.checked_div(b).unwrap_or(0));
}

pub fn umodsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    if b == 0 { div_zero(bus); }
    ret(core, bus, a.checked_rem(b).unwrap_or(0));
}

pub fn umulsidi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    ret64(core, bus, u64::from(a) * u64::from(b));
}

pub fn muldi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0), arg64(core, bus, 1));
    ret64(core, bus, a.wrapping_mul(b));
}

pub fn divdi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0) as i64, arg64(core, bus, 1) as i64);
    if b == 0 { div_zero(bus); }
    ret64(core, bus, if b == 0 { 0 } else { a.wrapping_div(b) as u64 });
}

pub fn udivdi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0), arg64(core, bus, 1));
    if b == 0 { div_zero(bus); }
    ret64(core, bus, a.checked_div(b).unwrap_or(0));
}

pub fn umoddi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0), arg64(core, bus, 1));
    if b == 0 { div_zero(bus); }
    ret64(core, bus, a.checked_rem(b).unwrap_or(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{DRAM_LOW, IRAM_LOW};
    use emu_core::Core;

    const RA: u32 = IRAM_LOW + 0x40;

    /// Call `f` with `args` in a2.. and return a2, a3.
    fn call(bus: &mut SocBus, f: super::super::Handler, args: &[u32]) -> (u32, u32) {
        let mut c = Lx106::new();
        c.set_ar(0, RA);
        for (i, &a) in args.iter().enumerate() { c.set_ar(2 + i as u8, a); }
        f(&mut c, bus);
        assert_eq!(c.pc(), RA);
        (c.get_ar(2), c.get_ar(3))
    }

    #[test]
    fn memory_and_string_functions_have_c_semantics() {
        let mut b = SocBus::new(1 << 20, [0; 6]);
        let (s, d) = (DRAM_LOW, DRAM_LOW + 0x100);
        b.load_bytes(s, b"hello world\0").unwrap();
        assert_eq!(call(&mut b, strlen, &[s]).0, 11);
        assert_eq!(call(&mut b, strcpy, &[d, s]).0, d);
        assert_eq!(call(&mut b, strcmp, &[d, s]).0, 0);
        assert_eq!(call(&mut b, strstr, &[s, s + 6]).0, s + 6);
        b.load_bytes(d + 20, b"hellp\0").unwrap();
        assert_eq!(call(&mut b, strcmp, &[s, d + 20]).0 as i32, -1);
        assert_eq!(call(&mut b, strncmp, &[s, d + 20, 4]).0, 0);
        assert_eq!(call(&mut b, memcmp, &[s, d + 20, 5]).0 as i32, -1);
        assert_eq!(call(&mut b, memset, &[d, b'x' as u32, 3]).0, d);
        assert_eq!(&b.dram[0x100..0x105], b"xxxlo");
        call(&mut b, strncpy, &[d, s + 6, 8]);
        assert_eq!(&b.dram[0x100..0x108], b"world\0\0\0");
        call(&mut b, memmove, &[s + 1, s, 4]);
        assert_eq!(&b.dram[..6], b"hhell ");
        call(&mut b, bzero, &[s, 2]);
        assert_eq!(&b.dram[..3], b"\0\0e");
        // the IRAM takes bytes from the ROM's own copies
        call(&mut b, memcpy, &[IRAM_LOW + 1, s + 2, 3]);
        assert_eq!(&b.iram[1..4], b"ell");
    }

    #[test]
    fn libgcc_integer_helpers() {
        let mut b = SocBus::new(1 << 20, [0; 6]);
        assert_eq!(call(&mut b, divsi3, &[-7i32 as u32, 2]).0 as i32, -3);
        assert_eq!(call(&mut b, divsi3, &[i32::MIN as u32, -1i32 as u32]).0, i32::MIN as u32);
        assert_eq!(call(&mut b, udivsi3, &[7, 0]).0, 0);
        assert_eq!(call(&mut b, umodsi3, &[17, 5]).0, 2);
        assert_eq!(call(&mut b, umulsidi3, &[0xffff_ffff, 2]), (0xffff_fffe, 1));
        assert_eq!(call(&mut b, muldi3, &[3, 1, 2, 0]), (6, 2));
        let (lo, hi) = call(&mut b, divdi3, &[-10i64 as u32, (-10i64 >> 32) as u32, 3, 0]);
        assert_eq!((u64::from(hi) << 32 | u64::from(lo)) as i64, -3);
        assert_eq!(call(&mut b, udivdi3, &[0, 1, 2, 0]), (0x8000_0000, 0));
        assert_eq!(call(&mut b, umoddi3, &[5, 1, 0, 1]), (5, 0));
    }
}
