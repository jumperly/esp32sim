//! The ROM's SPI flash API over the flash model. Prototypes: Arduino ESP8266 3.1.2
//! bootloaders/eboot/flash.h:18-21 (`SPIRead(addr, dest, size)`, `SPIWrite(addr, src, size)`,
//! `SPIEraseSector(sector)`, `SPIEraseBlock(block)`), cores/esp8266/esp8266_undocumented.h:244
//! (`Wait_SPI_Idle(fc)`); results are `SpiFlashOpResult` (tools/sdk/include/spi_flash.h:35-39:
//! 0 OK, 1 ERR). The flash answers at once: it is always idle and never busy in its status.
//!
//! A flash range past the chip's end is an error result; a memory side (`SPIRead`'s destination,
//! `SPIWrite`'s source, `SPI_read_status`'s word) the ROM's own load or store faults on is that
//! exception (`super::fault`).
use super::{arg, done, load8, ret, store32, store8, Mem, SocBus};
use xtensa_lx7::Lx106;

const OK: u32 = 0;
const ERR: u32 = 1;

/// `SPIRead(addr, dest, size)`: flash into RAM (any of DRAM, IRAM, RTC memory, at byte granularity
/// as the ROM's own copy loop writes it).
pub fn spi_read(core: &mut Lx106, bus: &mut SocBus) {
    let (addr, dest, size) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2) as usize);
    let mut buf = vec![0; size.min(bus.flash.len())];
    if buf.len() != size || !bus.flash.read(addr, &mut buf) {
        bus.rom.log_once("spi-read", || format!("SPIRead({addr:#x}, {dest:#010x}, {size}) outside the flash: error"));
        ret(core, bus, ERR);
        return;
    }
    if bus.load_bytes(dest, &buf).is_ok() { ret(core, bus, OK); return; }
    // not one RAM: byte by byte, up to the first store that faults
    let r = buf.iter().enumerate().try_for_each(|(i, &b)| store8(bus, dest.wrapping_add(i as u32), b));
    done(core, bus, r.map(|()| OK));
}

/// `SPIWrite(addr, src, size)`: program (bits only clear) from memory.
pub fn spi_write(core: &mut Lx106, bus: &mut SocBus) {
    let (addr, src, size) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    if size as usize > bus.flash.len() { ret(core, bus, ERR); return; }
    let data: Mem<Vec<u8>> = (0..size).map(|i| load8(bus, src.wrapping_add(i))).collect();
    let r = data.map(|d| if bus.flash_write(addr, &d) { OK } else { ERR });
    done(core, bus, r);
}

pub fn spi_erase_sector(core: &mut Lx106, bus: &mut SocBus) {
    let n = arg(core, bus, 0);
    let r = if bus.flash_erase_sector(n) { OK } else { ERR };
    ret(core, bus, r);
}

pub fn spi_erase_block(core: &mut Lx106, bus: &mut SocBus) {
    let n = arg(core, bus, 0);
    let r = if bus.flash_erase_block(n) { OK } else { ERR };
    ret(core, bus, r);
}

/// `SPI_read_status(fc, &status)`: status 0, not busy, nothing protected.
pub fn spi_read_status(core: &mut Lx106, bus: &mut SocBus) {
    let st = arg(core, bus, 1);
    let r = store32(bus, st, 0);
    done(core, bus, r.map(|()| OK));
}

/// The calls that only configure or wait for the flash: `Wait_SPI_Idle`, `SPI_write_status`,
/// `SPI_write_enable`, `SPIParamCfg`, `spi_flash_attach`.
pub fn ok(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, OK); }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{DRAM_LOW, IRAM_LOW};

    fn call(bus: &mut SocBus, f: super::super::Handler, args: &[u32]) -> u32 {
        let mut c = Lx106::new();
        c.set_ar(0, IRAM_LOW);
        for (i, &a) in args.iter().enumerate() { c.set_ar(2 + i as u8, a); }
        f(&mut c, bus);
        c.get_ar(2)
    }

    #[test]
    fn read_write_and_erase_through_the_rom_api() {
        let mut b = SocBus::new(1 << 20, [0; 6]);
        b.flash.bytes[0x1000..0x1004].copy_from_slice(&[0xe9, 5, 3, 0x20]);
        assert_eq!(call(&mut b, spi_read, &[0x1000, IRAM_LOW + 4, 4]), OK);
        assert_eq!(&b.iram[4..8], &[0xe9, 5, 3, 0x20]);
        assert_eq!(call(&mut b, spi_read, &[(1 << 20) - 2, DRAM_LOW, 4]), ERR);
        b.load_bytes(DRAM_LOW, &[0x0f, 0xf0]).unwrap();
        assert_eq!(call(&mut b, spi_write, &[0x1000, DRAM_LOW, 2]), OK);
        assert_eq!(&b.flash.bytes[0x1000..0x1002], &[0x09, 0x00]);
        assert_eq!(call(&mut b, spi_erase_sector, &[1]), OK);
        assert_eq!(&b.flash.bytes[0x1000..0x1002], &[0xff, 0xff]);
        assert_eq!(call(&mut b, spi_erase_block, &[16]), ERR);
        b.dram[8..12].fill(0xaa);
        assert_eq!(call(&mut b, spi_read_status, &[0, DRAM_LOW + 8]), OK);
        assert_eq!(&b.dram[8..12], &[0; 4]);
    }
}
