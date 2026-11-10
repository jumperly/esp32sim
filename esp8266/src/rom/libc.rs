//! The ROM's C library: memory and string functions (both the `ets_` names and the plain ones,
//! with the standard C semantics; prototypes in tools/sdk/include/ets_sys.h:204-218) and the
//! libgcc integer helpers the LX106 needs without a divider (call0 ABI: 64-bit values in a
//! register pair, low word first). They take one cycle: the ROM's own timing is not modelled.
//!
//! An address the chip's own load or store faults on ends the function with that exception
//! (`super::load8`, `super::store8`, `super::fault`): `strlen(NULL)` is LoadProhibited with
//! EXCVADDR 0, as on the chip. A division by zero is the chip's too (`div_zero`).
use super::{arg, done, load8, ret, ret64, store8, Mem, SocBus};
use xtensa_lx7::{state::exc, Lx106};

/// The NUL-terminated string at `addr` is this long.
fn len(bus: &mut SocBus, addr: u32) -> Mem<u32> {
    let mut n = 0;
    while n < MAX_LEN && load8(bus, addr.wrapping_add(n))? != 0 { n += 1; }
    Ok(n)
}

/// The longest run one call handles: more than the chip's memory, so it only stops a guest's
/// runaway length from holding the host for minutes.
const MAX_LEN: u32 = 1 << 24;

/// The first three arguments, the third a length.
fn args3(core: &Lx106, bus: &mut SocBus) -> (u32, u32, u32) { (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2).min(MAX_LEN)) }

pub fn memcpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    let r = (0..n).try_for_each(|i| { let b = load8(bus, s.wrapping_add(i))?; store8(bus, d.wrapping_add(i), b) });
    done(core, bus, r.map(|()| d));
}

/// Reads the whole source before it writes, so overlapping ranges copy as C requires.
pub fn memmove(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    let r = (0..n).map(|i| load8(bus, s.wrapping_add(i))).collect::<Mem<Vec<u8>>>()
        .and_then(|bytes| bytes.into_iter().enumerate().try_for_each(|(i, b)| store8(bus, d.wrapping_add(i as u32), b)));
    done(core, bus, r.map(|()| d));
}

pub fn memset(core: &mut Lx106, bus: &mut SocBus) {
    let (d, c, n) = args3(core, bus);
    let r = (0..n).try_for_each(|i| store8(bus, d.wrapping_add(i), c as u8));
    done(core, bus, r.map(|()| d));
}

pub fn bzero(core: &mut Lx106, bus: &mut SocBus) {
    let (d, n) = (arg(core, bus, 0), arg(core, bus, 1).min(MAX_LEN));
    let r = (0..n).try_for_each(|i| store8(bus, d.wrapping_add(i), 0));
    done(core, bus, r.map(|()| 0));
}

pub fn memcmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b, n) = args3(core, bus);
    let r = (|| {
        for i in 0..n {
            let (x, y) = (load8(bus, a.wrapping_add(i))?, load8(bus, b.wrapping_add(i))?);
            if x != y { return Ok(x as i32 - y as i32); }
        }
        Ok(0)
    })();
    done(core, bus, r.map(|v| v as u32));
}

pub fn strlen(core: &mut Lx106, bus: &mut SocBus) { let s = arg(core, bus, 0); let r = len(bus, s); done(core, bus, r); }

/// Copies the string and its NUL; a runaway one stops after `MAX_LEN` bytes, unterminated.
pub fn strcpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s) = (arg(core, bus, 0), arg(core, bus, 1));
    let r = (|| {
        for i in 0..MAX_LEN {
            let b = load8(bus, s.wrapping_add(i))?;
            store8(bus, d.wrapping_add(i), b)?;
            if b == 0 { break; }
        }
        Ok(d)
    })();
    done(core, bus, r);
}

/// Copies at most `n` bytes and pads the rest of them with NULs.
pub fn strncpy(core: &mut Lx106, bus: &mut SocBus) {
    let (d, s, n) = args3(core, bus);
    let r = (|| {
        let mut end = false;
        for i in 0..n {
            let b = if end { 0 } else { load8(bus, s.wrapping_add(i))? };
            end |= b == 0;
            store8(bus, d.wrapping_add(i), b)?;
        }
        Ok(d)
    })();
    done(core, bus, r);
}

/// The C comparison of two strings, over at most `n` bytes.
fn compare(bus: &mut SocBus, a: u32, b: u32, n: u32) -> Mem<u32> {
    for i in 0..n {
        let (x, y) = (load8(bus, a.wrapping_add(i))?, load8(bus, b.wrapping_add(i))?);
        if x != y { return Ok((x as i32 - y as i32) as u32); }
        if x == 0 { break; }
    }
    Ok(0)
}

pub fn strcmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    let r = compare(bus, a, b, MAX_LEN);
    done(core, bus, r);
}

pub fn strncmp(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b, n) = args3(core, bus);
    let r = compare(bus, a, b, n);
    done(core, bus, r);
}

/// The bytes of the NUL-terminated string at `addr`.
fn string(bus: &mut SocBus, addr: u32) -> Mem<Vec<u8>> {
    let n = len(bus, addr)?;
    (0..n).map(|i| load8(bus, addr.wrapping_add(i))).collect()
}

/// The first `needle` in `haystack`, or NULL.
pub fn strstr(core: &mut Lx106, bus: &mut SocBus) {
    let (h, n) = (arg(core, bus, 0), arg(core, bus, 1));
    let r = string(bus, n).and_then(|needle| {
        let hay = string(bus, h)?;
        let at = if needle.is_empty() { Some(0) } else { hay.windows(needle.len()).position(|w| w == needle.as_slice()) };
        Ok(at.map_or(0, |i| h.wrapping_add(i as u32)))
    });
    done(core, bus, r);
}

/// Where the ROM's integer division helpers go on a zero divisor: an ILL (`00 00 00`) inside
/// `__divsi3` (0x4000dc88), which raises IllegalInstruction (EXCCAUSE 0) with EPC1 here. The
/// Arduino core's postmortem knows the address and prints such a crash as `Exception (6)`
/// (IntegerDivideByZero) with the caller, which it reads from EXCSAVE1 (where the SDK's user
/// vector saved a0) in place of EPC1 (cores/esp8266/core_esp8266_postmortem.cpp:168-178, Arduino
/// ESP8266 3.1.2: "The GCC divide routine in ROM jumps to the address below and executes ILL (00
/// 00 00) on div-by-zero").
pub const DIV_ZERO_ILL: u32 = 0x4000_dce5;

/// A division by zero: the ILL at `DIV_ZERO_ILL`, a0 still the caller's. The 64-bit helpers do
/// the same here: libgcc's `__udivmoddi4` divides by the zero divisor on purpose ("Divide
/// intentionally by zero", libgcc2.c), which reaches the ROM's 32-bit division and so this ILL
/// (inferred, not observed: there a0 would point into the 64-bit helper, here it is its caller).
fn div_zero(core: &mut Lx106) { core.raise_from_hook(DIV_ZERO_ILL, exc::ILLEGAL, None); }

fn arg64(core: &Lx106, bus: &mut SocBus, pair: usize) -> u64 { u64::from(arg(core, bus, pair * 2)) | u64::from(arg(core, bus, pair * 2 + 1)) << 32 }

pub fn divsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0) as i32, arg(core, bus, 1) as i32);
    if b == 0 { div_zero(core); return; }
    ret(core, bus, a.wrapping_div(b) as u32);   // i32::MIN / -1 wraps
}

pub fn udivsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    if b == 0 { div_zero(core); return; }
    ret(core, bus, a / b);
}

pub fn umodsi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg(core, bus, 0), arg(core, bus, 1));
    if b == 0 { div_zero(core); return; }
    ret(core, bus, a % b);
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
    if b == 0 { div_zero(core); return; }
    ret64(core, bus, a.wrapping_div(b) as u64);
}

pub fn udivdi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0), arg64(core, bus, 1));
    if b == 0 { div_zero(core); return; }
    ret64(core, bus, a / b);
}

pub fn umoddi3(core: &mut Lx106, bus: &mut SocBus) {
    let (a, b) = (arg64(core, bus, 0), arg64(core, bus, 1));
    if b == 0 { div_zero(core); return; }
    ret64(core, bus, a % b);
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
        // a zero divisor leaves the ILL of the ROM for the core: no return
        let mut c = Lx106::new();
        c.set_ar(0, RA);
        c.set_ar(2, 7);
        c.set_ar(3, 0);
        udivsi3(&mut c, &mut b);
        assert!(c.hook_exception_pending());
        assert_eq!(c.pc(), DIV_ZERO_ILL);
        assert_eq!(call(&mut b, umodsi3, &[17, 5]).0, 2);
        assert_eq!(call(&mut b, umulsidi3, &[0xffff_ffff, 2]), (0xffff_fffe, 1));
        assert_eq!(call(&mut b, muldi3, &[3, 1, 2, 0]), (6, 2));
        let (lo, hi) = call(&mut b, divdi3, &[-10i64 as u32, (-10i64 >> 32) as u32, 3, 0]);
        assert_eq!((u64::from(hi) << 32 | u64::from(lo)) as i64, -3);
        assert_eq!(call(&mut b, udivdi3, &[0, 1, 2, 0]), (0x8000_0000, 0));
        assert_eq!(call(&mut b, umoddi3, &[5, 1, 0, 1]), (5, 0));
    }
}
