//! The ROM's radio (PHY) functions. libphy reaches most of them through the table
//! `phy_get_romfuncs()` returns rather than by address, so the table here points at private
//! stubs, `STUBS + 4 * i`; which ROM function each slot is is not published. Slots 36..39 are
//! the analog registers behind the internal I2C bus (digitake esp8266-emulator src/rom.c:622-641):
//! `rom_i2c_readReg(block, host, reg)`, `rom_i2c_readReg_Mask(block, host, reg, msb, lsb)`,
//! `rom_i2c_writeReg(block, host, reg, data)`, `rom_i2c_writeReg_Mask(block, host, reg, msb,
//! lsb, data)`; they and the same functions called by name keep the registers as plain storage.
//! There is no radio: every other slot logs its first call and returns 0.
use super::{arg, ret, Lx106, SocBus};
use emu_core::Bus;

/// Where `phy_get_romfuncs` returns the table (digitake esp8266-emulator src/rom.c:39, its stubs
/// at :30; the ROM's own address is not published).
pub const TABLE: u32 = 0x3FFF_D400;
/// The stubs the table points at, and how many.
pub const STUBS: u32 = 0x4000_F000;
pub const SLOTS: u32 = 256;
/// The I2C register slots (see the module comment).
const I2C_READ: u32 = 36;
const I2C_READ_MASK: u32 = 37;
const I2C_WRITE: u32 = 38;
const I2C_WRITE_MASK: u32 = 39;

/// Fill the table (at the ROM's reset).
pub fn init(bus: &mut SocBus) {
    for i in 0..SLOTS { let _ = bus.write32(TABLE + 4 * i, STUBS + 4 * i); }
}

/// `phy_get_romfuncs()`.
pub fn phy_get_romfuncs(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, TABLE); }

/// A call through the table's slot at `pc`.
pub fn slot(core: &mut Lx106, bus: &mut SocBus, pc: u32) {
    match (pc - STUBS) / 4 {
        I2C_READ => i2c_read(core, bus),
        I2C_READ_MASK => i2c_read_mask(core, bus),
        I2C_WRITE => i2c_write(core, bus),
        I2C_WRITE_MASK => i2c_write_mask(core, bus),
        k => {
            // one line for all of them; each slot's first call under `--debug rom`
            if bus.rom.phy_seen.is_empty() { eprintln!("[emu] rom: the PHY functions of phy_get_romfuncs return 0 (no radio; --debug rom lists them)"); }
            if bus.rom.phy_seen.insert(k) && bus.debug.has("rom") {
                eprintln!("[emu] rom: PHY function {k} of phy_get_romfuncs (a2={:#x} a3={:#x}) returns 0", core.get_ar(2), core.get_ar(3));
            }
            ret(core, bus, 0);
        }
    }
}

/// The register `(block, host, reg)` as read. libphy's `wait_rfpll_cal_end` polls block 98 (0x62)
/// register 7 bit 7 for "PLL calibrated" (digitake esp8266-emulator src/rom.c:622-633): it reads
/// set, or the PHY start would never end.
fn i2c_value(bus: &SocBus, block: u32, host: u32, reg: u32) -> u8 {
    let v = bus.rom.i2c.get(&key(block, host, reg)).copied().unwrap_or(0);
    if block & 0xff == 98 && reg & 0xff == 7 { v | 0x80 } else { v }
}

fn key(block: u32, host: u32, reg: u32) -> u32 { (block & 0xff) << 16 | (host & 7) << 8 | (reg & 0xff) }

/// The field msb..lsb of a byte register, as a mask at bit 0.
fn field(msb: u32, lsb: u32) -> (u32, u8) {
    let (msb, lsb) = (msb & 7, lsb & 7);
    if msb < lsb { (lsb, 0) } else { (lsb, ((1u32 << (msb - lsb + 1)) - 1) as u8) }
}

pub fn i2c_read(core: &mut Lx106, bus: &mut SocBus) {
    let (b, h, r) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let v = i2c_value(bus, b, h, r);
    ret(core, bus, u32::from(v));
}

pub fn i2c_read_mask(core: &mut Lx106, bus: &mut SocBus) {
    let (b, h, r) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let v = i2c_value(bus, b, h, r);
    let (lsb, mask) = field(arg(core, bus, 3), arg(core, bus, 4));
    ret(core, bus, u32::from((v >> lsb) & mask));
}

pub fn i2c_write(core: &mut Lx106, bus: &mut SocBus) {
    let k = key(arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let v = arg(core, bus, 3) as u8;
    bus.rom.i2c.insert(k, v);
    ret(core, bus, 0);
}

pub fn i2c_write_mask(core: &mut Lx106, bus: &mut SocBus) {
    let k = key(arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    let (lsb, mask) = field(arg(core, bus, 3), arg(core, bus, 4));
    let data = arg(core, bus, 5) as u8;
    let old = bus.rom.i2c.get(&k).copied().unwrap_or(0);
    bus.rom.i2c.insert(k, (old & !(mask << lsb)) | ((data & mask) << lsb));
    ret(core, bus, 0);
}

/// `rom_chip_v5_enable_cca` / `rom_chip_v5_disable_cca` (clear channel assessment) and
/// `rom_sar_init`: settings of the radio and its ADC, which is not modelled.
pub fn nop(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pll_calibrated_bit_reads_set_and_fields_merge() {
        let mut bus = SocBus::new(1 << 20, [0; 6]);
        assert_eq!(i2c_value(&bus, 98, 0, 7), 0x80);
        assert_eq!(i2c_value(&bus, 98, 0, 6), 0);
        assert_eq!(field(5, 2), (2, 0xf));
        bus.rom.i2c.insert(key(103, 4, 9), 0xff);
        let (lsb, mask) = field(5, 2);
        let k = key(103, 4, 9);
        let old = bus.rom.i2c[&k];
        bus.rom.i2c.insert(k, (old & !(mask << lsb)) | ((0x5 & mask) << lsb));
        assert_eq!(bus.rom.i2c[&k], 0b1101_0111);
    }
}
