//! The ROM's soft-float helpers: the LX106 has no FPU, and GCC calls these libgcc routines for
//! float and double arithmetic (names and argument passing: GCC's libgcc "Routines for
//! floating point emulation"; call0 ABI: a float in one register, a double in a register pair,
//! low word first, results in a2 or a2:a3). IEEE 754 binary32/binary64 with round to nearest
//! even, which Rust's f32/f64 operations implement. A conversion to an integer truncates toward
//! zero and saturates out of range (NaN gives 0), which libgcc leaves undefined.
use super::{arg, ret, ret64, Lx106, SocBus};

fn f(core: &Lx106, bus: &mut SocBus, n: usize) -> f32 { f32::from_bits(arg(core, bus, n)) }
/// The double in arguments n and n + 1.
fn d(core: &Lx106, bus: &mut SocBus, n: usize) -> f64 { f64::from_bits(u64::from(arg(core, bus, n)) | u64::from(arg(core, bus, n + 1)) << 32) }
fn rf(core: &mut Lx106, bus: &mut SocBus, v: f32) { ret(core, bus, v.to_bits()); }
fn rd(core: &mut Lx106, bus: &mut SocBus, v: f64) { ret64(core, bus, v.to_bits()); }

pub fn addsf3(core: &mut Lx106, bus: &mut SocBus) { let v = f(core, bus, 0) + f(core, bus, 1); rf(core, bus, v); }
pub fn subsf3(core: &mut Lx106, bus: &mut SocBus) { let v = f(core, bus, 0) - f(core, bus, 1); rf(core, bus, v); }
pub fn mulsf3(core: &mut Lx106, bus: &mut SocBus) { let v = f(core, bus, 0) * f(core, bus, 1); rf(core, bus, v); }
pub fn fixunssfsi(core: &mut Lx106, bus: &mut SocBus) { let v = f(core, bus, 0) as u32; ret(core, bus, v); }
pub fn floatsisf(core: &mut Lx106, bus: &mut SocBus) { let v = arg(core, bus, 0) as i32 as f32; rf(core, bus, v); }
pub fn floatunsisf(core: &mut Lx106, bus: &mut SocBus) { let v = arg(core, bus, 0) as f32; rf(core, bus, v); }
pub fn adddf3(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) + d(core, bus, 2); rd(core, bus, v); }
pub fn subdf3(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) - d(core, bus, 2); rd(core, bus, v); }
pub fn muldf3(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) * d(core, bus, 2); rd(core, bus, v); }
pub fn divdf3(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) / d(core, bus, 2); rd(core, bus, v); }
pub fn fixdfsi(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) as i32; ret(core, bus, v as u32); }
pub fn fixunsdfsi(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) as u32; ret(core, bus, v); }
pub fn truncdfsf2(core: &mut Lx106, bus: &mut SocBus) { let v = d(core, bus, 0) as f32; rf(core, bus, v); }
pub fn extendsfdf2(core: &mut Lx106, bus: &mut SocBus) { let v = f64::from(f(core, bus, 0)); rd(core, bus, v); }
pub fn floatsidf(core: &mut Lx106, bus: &mut SocBus) { let v = f64::from(arg(core, bus, 0) as i32); rd(core, bus, v); }
pub fn floatunsidf(core: &mut Lx106, bus: &mut SocBus) { let v = f64::from(arg(core, bus, 0)); rd(core, bus, v); }

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::Core;

    /// Call `h` with the argument words `a` and a0 = 0x1000; the result words.
    fn call(h: fn(&mut Lx106, &mut SocBus), a: &[u32]) -> (u32, u32) {
        let (mut c, mut bus) = (Lx106::new(), SocBus::new(1 << 20, [0; 6]));
        c.set_ar(0, 0x1000);
        for (i, &v) in a.iter().enumerate() { c.set_ar(2 + i as u8, v); }
        h(&mut c, &mut bus);
        assert_eq!(c.pc(), 0x1000);
        (c.get_ar(2), c.get_ar(3))
    }
    fn dw(v: f64) -> [u32; 2] { let b = v.to_bits(); [b as u32, (b >> 32) as u32] }

    #[test]
    fn float_and_double_helpers_follow_ieee_754() {
        assert_eq!(f32::from_bits(call(addsf3, &[1.5f32.to_bits(), 2.25f32.to_bits()]).0), 3.75);
        assert_eq!(f32::from_bits(call(mulsf3, &[(-3.0f32).to_bits(), 0.5f32.to_bits()]).0), -1.5);
        assert_eq!(call(fixunssfsi, &[3.99f32.to_bits()]).0, 3);
        assert_eq!(f32::from_bits(call(floatsisf, &[-7i32 as u32]).0), -7.0);
        let [a0, a1] = dw(0.1);
        let [b0, b1] = dw(0.2);
        let (lo, hi) = call(adddf3, &[a0, a1, b0, b1]);
        assert_eq!(f64::from_bits(u64::from(lo) | u64::from(hi) << 32), 0.1 + 0.2);
        let (lo, hi) = call(divdf3, &[dw(1.0)[0], dw(1.0)[1], dw(3.0)[0], dw(3.0)[1]]);
        assert_eq!(f64::from_bits(u64::from(lo) | u64::from(hi) << 32), 1.0 / 3.0);
        assert_eq!(call(fixdfsi, &dw(-2.5)).0 as i32, -2);
        assert_eq!(f32::from_bits(call(truncdfsf2, &dw(1.0 / 3.0)).0), 1.0f32 / 3.0);
        let (lo, hi) = call(floatunsidf, &[u32::MAX]);
        assert_eq!(f64::from_bits(u64::from(lo) | u64::from(hi) << 32), 4_294_967_295.0);
    }
}
