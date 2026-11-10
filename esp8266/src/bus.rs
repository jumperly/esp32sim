//! ESP8266 memory map (Arduino ESP8266 3.1.2: tools/sdk/include/eagle_soc.h:84-88,
//! tools/sdk/ld/eagle.rom.addr.v6.ld, tools/sdk/ld/eagle.flash.4m1m.ld, cores/esp8266/esp8266_peri.h).
//!
//! The data bus takes every width in DRAM, RTC memory and the registers. The instruction RAM and
//! the flash cache window only take 32-bit accesses: a narrower load or store is a LoadStoreError
//! (EXCCAUSE 3), which the Arduino core relies on (`core_esp8266_non32xfer.cpp` emulates byte
//! loads from flash in that exception's handler). An address outside the map faults with
//! LoadProhibited/StoreProhibited (28/29). The mask ROM's bytes are not modelled: its functions
//! are the `rom` module's, entered by pc; a data read there is 0 (logged once).
use crate::flash::Flash;
use crate::periph::{Peripherals, RTC_MEM_HIGH, RTC_MEM_LOW, UART1_BASE};
use crate::rom::RomState;
use emu_core::{Bus, Fault};

pub use crate::periph::{DPORT_BASE, PERIPH_BASE, WDEV_BASE};

/// DRAM: 80 KB for the image (eagle.flash.4m1m.ld dram0_0_seg 0x3FFE8000 + 0x14000) and the 16 KB
/// above it that the ROM keeps for its data and the boot stack.
pub const DRAM_LOW: u32 = 0x3FFE_8000;
pub const DRAM_HIGH: u32 = 0x4000_0000;
/// IRAM: 32 KB, then the 32 KB the flash cache uses unless the core takes 16 KB of it for code
/// (`MMU_IRAM_SIZE`). Modelled as 64 KB of plain RAM: the cache is not taken away.
pub const IRAM_LOW: u32 = 0x4010_0000;
pub const IRAM_HIGH: u32 = 0x4011_0000;
/// The mask ROM: `_ResetVector` 0x40000080 (eagle.rom.addr.v6.ld) in 64 KB.
pub const ROM_LOW: u32 = 0x4000_0000;
pub const ROM_HIGH: u32 = 0x4001_0000;
/// The flash cache window: 1 MB of flash from where `Cache_Read_Enable` points it (irom0_0_seg
/// starts at 0x40201010 in eagle.flash.4m1m.ld: the app image's flash offset 0x1010).
pub const FLASH_WIN_LOW: u32 = 0x4020_0000;
pub const FLASH_WIN_HIGH: u32 = 0x4030_0000;

/// Where an address goes.
enum At { Dram(usize), Iram(usize), Rtc(usize), Flash(usize), Rom, Mmio, Unmapped }

pub struct SocBus {
    pub dram: Vec<u8>,
    pub iram: Vec<u8>,
    /// RTC memory: kept across a chip reset, lost with power
    pub rtc_mem: Vec<u8>,
    pub flash: Flash,
    /// flash offset of the window's first byte (`Cache_Read_Enable`)
    pub cache_base: u32,
    /// `Cache_Read_Enable` / `Cache_Read_Disable`: recorded; the window maps either way
    pub cache_enabled: bool,
    pub periph: Peripherals,
    pub rom: RomState,
    /// a bare module: nothing on the pins
    pub board: esp_soc::Board,
    pub cycles: u64,
    pub irq_dirty: bool,
    pub last_fault: Option<(u32, bool)>,
    /// data reads from the mask ROM, which has no bytes here; the first is logged
    pub rom_data_reads: u64,
    /// the reset cause as the ROM numbers it (`rom::boot::RST_*`); power-on at first
    pub reset_cause: u32,
    pub sw_reset: bool,
    /// strapping as the ROM reads it: GPIO15 << 2 | GPIO0 << 1 | GPIO2 (3: boot from flash)
    pub strap: u32,
    /// host-driven GPIO input levels
    pub gpio_in: u64,
    pub debug: esp_soc::DebugFlags,
}

impl SocBus {
    pub fn new(flash_size: usize, mac: [u8; 6]) -> Self {
        SocBus {
            dram: vec![0; (DRAM_HIGH - DRAM_LOW) as usize], iram: vec![0; (IRAM_HIGH - IRAM_LOW) as usize],
            rtc_mem: vec![0; (RTC_MEM_HIGH - RTC_MEM_LOW) as usize], flash: Flash::new(flash_size), cache_base: 0, cache_enabled: false,
            periph: Peripherals::new(mac), rom: RomState::default(), board: Box::new(esp_soc::NoBoard),
            cycles: 0, irq_dirty: true, last_fault: None, rom_data_reads: 0, reset_cause: crate::rom::boot::RST_POWERON,
            sw_reset: false, strap: crate::rom::boot::STRAP_FLASH, gpio_in: 0, debug: Default::default(),
        }
    }

    /// `Cache_Read_Enable(map, p, v)`: point the window at flash. Arduino ESP8266 3.1.2
    /// cores/esp8266/mmu_iram.cpp:61-79: `map` 0/1 selects the even/odd megabyte (flash address
    /// bit 20; "no guesses for a value of 2 or greater", so only bit 0 is used), `p` is bit 21 (or
    /// bits 21-23), `v` the cache size (16/32 KB), which this model does not need.
    pub fn cache_read_enable(&mut self, map: u32, p: u32, _v: u32) {
        self.cache_base = ((map & 1) << 20) | ((p & 7) << 21);
        self.cache_enabled = true;
    }

    #[inline]
    fn at(&self, addr: u32) -> At {
        match addr {
            DRAM_LOW..DRAM_HIGH => At::Dram((addr - DRAM_LOW) as usize),
            IRAM_LOW..IRAM_HIGH => At::Iram((addr - IRAM_LOW) as usize),
            FLASH_WIN_LOW..FLASH_WIN_HIGH => {
                let off = self.cache_base as usize + (addr - FLASH_WIN_LOW) as usize;
                if off < self.flash.len() { At::Flash(off) } else { At::Unmapped }
            }
            ROM_LOW..ROM_HIGH => At::Rom,
            RTC_MEM_LOW..RTC_MEM_HIGH => At::Rtc((addr - RTC_MEM_LOW) as usize),
            _ if Peripherals::contains(addr) => At::Mmio,
            _ => At::Unmapped,
        }
    }

    /// A data read from the mask ROM: 0, and one log line for the first of them.
    fn rom_read(&mut self, addr: u32) {
        self.rom_data_reads += 1;
        if self.rom_data_reads == 1 { eprintln!("[emu] rom: data read at {addr:#010x} reads 0: the ESP8266 mask ROM's bytes are not modelled (logged once)"); }
    }

    fn fault<T>(&mut self, addr: u32, write: bool, f: Fault) -> Result<T, Fault> { self.last_fault = Some((addr, write)); Err(f) }

    /// `N` bytes at `off`, if they are all there.
    fn get<const N: usize>(mem: &[u8], off: usize) -> Option<[u8; N]> { mem.get(off..off + N).map(|s| s.try_into().unwrap()) }

    fn read_n<const N: usize>(&mut self, addr: u32) -> Result<[u8; N], Fault> {
        let bytes = match self.at(addr) {
            At::Dram(o) => Self::get::<N>(&self.dram, o),
            At::Rtc(o) => Self::get::<N>(&self.rtc_mem, o),
            At::Iram(o) if N == 4 => Self::get::<N>(&self.iram, o),
            At::Flash(o) if N == 4 => Self::get::<N>(&self.flash.bytes, o),
            At::Iram(_) | At::Flash(_) => return self.fault(addr, false, Fault::LoadStoreError),
            At::Rom => { self.rom_read(addr); Some([0; N]) }
            At::Mmio => Self::get::<N>(&self.periph.read32(addr & !3).to_le_bytes(), (addr & 3) as usize),
            At::Unmapped => None,
        };
        match bytes { Some(b) => Ok(b), None => self.fault(addr, false, Fault::Unmapped) }
    }

    fn write_n<const N: usize>(&mut self, addr: u32, v: [u8; N]) -> Result<(), Fault> {
        let mem = match self.at(addr) {
            At::Dram(o) => self.dram.get_mut(o..o + N),
            At::Rtc(o) => self.rtc_mem.get_mut(o..o + N),
            At::Iram(o) if N == 4 => self.iram.get_mut(o..o + N),
            At::Iram(_) => return self.fault(addr, true, Fault::LoadStoreError),
            At::Flash(_) if N < 4 => return self.fault(addr, true, Fault::LoadStoreError),
            At::Flash(_) | At::Rom => return self.fault(addr, true, Fault::Prohibited),
            At::Mmio => { self.mmio_write(addr, &v); return Ok(()); }
            At::Unmapped => None,
        };
        match mem { Some(m) => { m.copy_from_slice(&v); Ok(()) } None => self.fault(addr, true, Fault::Unmapped) }
    }

    /// A register store. A narrower one merges into the register, except into a UART's FIFO,
    /// where reading the register would pop a received byte: that takes the byte as stored.
    fn mmio_write(&mut self, addr: u32, v: &[u8]) {
        let a = addr & !3;
        let w = if v.len() == 4 { u32::from_le_bytes(v.try_into().unwrap()) } else {
            let mut w = if a == PERIPH_BASE || a == UART1_BASE { [0; 4] } else { self.periph.peek32(a).to_le_bytes() };
            let o = (addr & 3) as usize;
            w[o..o + v.len()].copy_from_slice(v);
            u32::from_le_bytes(w)
        };
        self.periph.write32(a, w);
        self.irq_dirty = true;
    }

    /// Plain memory as the ROM model and the loaders see it, not held to the instruction bus's
    /// 32-bit rule: `len` bytes at `addr` in DRAM, IRAM or RTC memory.
    pub fn ram_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let (mem, o) = match self.at(addr) {
            At::Dram(o) => (&mut self.dram, o),
            At::Iram(o) => (&mut self.iram, o),
            At::Rtc(o) => (&mut self.rtc_mem, o),
            _ => return None,
        };
        mem.get_mut(o..o.checked_add(len)?)
    }

    /// A byte as the ROM's own code reads it: DRAM, IRAM, RTC memory or the flash window at any
    /// width; registers by their word. None for the ROM itself and unmapped space.
    pub fn peek8(&mut self, addr: u32) -> Option<u8> {
        match self.at(addr) {
            At::Dram(o) => Some(self.dram[o]),
            At::Iram(o) => Some(self.iram[o]),
            At::Rtc(o) => Some(self.rtc_mem[o]),
            At::Flash(o) => Some(self.flash.bytes[o]),
            At::Mmio => Some(self.periph.read32(addr & !3).to_le_bytes()[(addr & 3) as usize]),
            At::Rom | At::Unmapped => None,
        }
    }

    /// A byte store as the ROM's own code makes it (`peek8`'s counterpart; the flash window is read-only).
    pub fn poke8(&mut self, addr: u32, v: u8) -> bool {
        match self.at(addr) {
            At::Mmio => { self.mmio_write(addr, &[v]); true }
            _ => match self.ram_mut(addr, 1) { Some(m) => { m[0] = v; true } None => false },
        }
    }

    pub fn load_bytes(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let m = self.ram_mut(addr, data.len()).ok_or_else(|| format!("{} bytes at {addr:#010x}: not RAM", data.len()))?;
        m.copy_from_slice(data);
        Ok(())
    }

    pub fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> { self.flash.load(offset, data) }
}

impl Bus for SocBus {
    fn note_code_page(&mut self, _vidx: u32) {} // The LX106 interpreter keeps no decoded code.
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> { self.read_n::<1>(addr).map(|b| b[0]) }
    fn read16(&mut self, addr: u32) -> Result<u16, Fault> { self.read_n::<2>(addr).map(u16::from_le_bytes) }
    fn read32(&mut self, addr: u32) -> Result<u32, Fault> { self.read_n::<4>(addr).map(u32::from_le_bytes) }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> { self.write_n(addr, [v]) }
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), Fault> { self.write_n(addr, v.to_le_bytes()) }
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), Fault> { self.write_n(addr, v.to_le_bytes()) }
    /// Code runs from IRAM and the flash window. DRAM is not executable, and the mask ROM's
    /// entries are the `rom` module's: a pc there never fetches.
    fn fetch(&mut self, pc: u32) -> Result<[u8; 4], Fault> {
        let (mem, o) = match self.at(pc) {
            At::Iram(o) => (&self.iram, o),
            At::Flash(o) => (&self.flash.bytes, o),
            _ => return Err(Fault::Unmapped),
        };
        let mut b = [0; 4];
        let n = (mem.len() - o).min(4);
        b[..n].copy_from_slice(&mem[o..o + n]);
        Ok(b)
    }
    fn note_pc(&mut self, pc: u32) { self.periph.misc.cur_pc = pc; }
    fn tick(&mut self, cycles: u32) -> u32 { self.cycles += cycles as u64; 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::Core;
    use xtensa_lx7::{state::exc, Lx106, Trap};

    fn bus() -> SocBus { SocBus::new(4 << 20, [0; 6]) }

    /// Run the 3-byte instruction `raw` from IRAM with a3 = `addr`; the exception cause it raised.
    fn cause_of(b: &mut SocBus, raw: [u8; 3], addr: u32) -> Option<u32> {
        b.write32(IRAM_LOW, u32::from_le_bytes([raw[0], raw[1], raw[2], 0])).unwrap();
        let mut c = Lx106::new();
        c.set_pc(IRAM_LOW);
        c.set_ar(3, addr);
        match c.step(b).trap() { Some(Trap::Exception(e)) => { assert_eq!(c.state().excvaddr, addr); Some(e) } _ => None }
    }
    // Xtensa ISA RRI8 loads and stores of a2 at a3 + 0: L8UI r=0, L32I r=2, S8I r=4, S32I r=6
    const L8UI: [u8; 3] = [0x22, 0x03, 0x00];
    const L32I: [u8; 3] = [0x22, 0x23, 0x00];
    const S8I: [u8; 3] = [0x22, 0x43, 0x00];
    const S32I: [u8; 3] = [0x22, 0x63, 0x00];

    #[test]
    fn dram_and_iram_are_byte_addressable_and_sub_word_iram_reads_fault() {
        let mut b = bus();
        b.write32(DRAM_LOW, 0x4433_2211).unwrap();
        assert_eq!(b.read8(DRAM_LOW + 1), Ok(0x22));
        assert_eq!(b.read16(DRAM_LOW + 2), Ok(0x4433));
        b.write8(DRAM_HIGH - 1, 0x5a).unwrap();
        assert_eq!(b.read32(DRAM_HIGH - 4), Ok(0x5a00_0000));
        b.write32(IRAM_HIGH - 4, 0xdead_beef).unwrap();
        assert_eq!(b.read32(IRAM_HIGH - 4), Ok(0xdead_beef));
        assert_eq!(b.fetch(IRAM_HIGH - 4), Ok([0xef, 0xbe, 0xad, 0xde]));
        assert_eq!(b.read8(IRAM_HIGH - 4), Err(Fault::LoadStoreError));
        assert_eq!(b.read16(IRAM_HIGH - 2), Err(Fault::LoadStoreError));
        assert_eq!(b.write8(IRAM_HIGH - 4, 1), Err(Fault::LoadStoreError));
        assert_eq!(b.write16(IRAM_HIGH - 4, 1), Err(Fault::LoadStoreError));
        assert_eq!(b.read32(IRAM_HIGH - 4), Ok(0xdead_beef));
        // what the core makes of it: EXCCAUSE 3, which core_esp8266_non32xfer.cpp emulates
        assert_eq!(cause_of(&mut b, L8UI, IRAM_LOW + 0x100), Some(exc::LOAD_STORE_ERROR));
        assert_eq!(cause_of(&mut b, S8I, IRAM_LOW + 0x100), Some(exc::LOAD_STORE_ERROR));
        assert_eq!(cause_of(&mut b, L8UI, DRAM_LOW + 1), None);
    }

    #[test]
    fn flash_window_follows_cache_base() {
        let mut b = bus();
        b.flash.bytes[0x1010..0x1014].copy_from_slice(&[1, 2, 3, 4]);
        b.flash.bytes[0x10_1010..0x10_1014].copy_from_slice(&[5, 6, 7, 8]);
        b.cache_read_enable(0, 0, 1);
        assert_eq!(b.read32(0x4020_1010), Ok(0x0403_0201));
        assert_eq!(b.fetch(0x4020_1010), Ok([1, 2, 3, 4]));
        assert_eq!(b.read8(0x4020_1010), Err(Fault::LoadStoreError));
        assert_eq!(b.read16(0x4020_1010), Err(Fault::LoadStoreError));
        assert_eq!(cause_of(&mut b, L8UI, 0x4020_1011), Some(exc::LOAD_STORE_ERROR));
        // the odd megabyte (mmu_iram.cpp: `map` is flash address bit 20)
        b.cache_read_enable(1, 0, 1);
        assert_eq!(b.read32(0x4020_1010), Ok(0x0807_0605));
        assert_eq!(b.write32(0x4020_1010, 0), Err(Fault::Prohibited));
        // past the end of a 1 MB flash
        let mut small = SocBus::new(1 << 20, [0; 6]);
        small.cache_read_enable(1, 0, 1);
        assert_eq!(small.read32(0x4020_0000), Err(Fault::Unmapped));
    }

    #[test]
    fn rom_region_reads_zero_and_logs_once() {
        let mut b = bus();
        assert_eq!(b.read32(ROM_LOW + 0x100), Ok(0));
        assert_eq!(b.read8(ROM_HIGH - 1), Ok(0));
        assert_eq!(b.read16(ROM_LOW + 2), Ok(0));
        // the first read prints the one log line; the count says how many followed
        assert_eq!(b.rom_data_reads, 3);
        assert_eq!(b.write32(ROM_LOW, 1), Err(Fault::Prohibited));
        assert_eq!(b.read32(ROM_LOW), Ok(0));
    }

    #[test]
    fn a_narrow_register_store_merges_without_a_read_of_its_own() {
        let mut b = bus();
        let r = DPORT_BASE + 0x20;
        b.write32(r, 0x1122_3344).unwrap();
        b.periph.misc.mmio_log = Some(Vec::new());
        b.write8(r + 1, 0xaa).unwrap();
        // the guest made one store: the log has that write and no read
        let log = b.periph.misc.mmio_log.take().unwrap();
        assert_eq!(log.iter().map(|&(_, a, v, w)| (a, v, w)).collect::<Vec<_>>(), [(r, 0x1122_aa44, true)]);
        assert_eq!(b.read32(r), Ok(0x1122_aa44));
    }

    #[test]
    fn unmapped_address_faults_with_prohibited() {
        let mut b = bus();
        for addr in [0x0, 0x3FFE_7FFC, 0x4011_0000, 0x4030_0000, 0x6010_0000] {
            assert_eq!(b.read32(addr), Err(Fault::Unmapped), "{addr:#x}");
            assert_eq!(b.write32(addr, 0), Err(Fault::Unmapped), "{addr:#x}");
            assert_eq!(b.last_fault, Some((addr, true)));
        }
        assert_eq!(cause_of(&mut b, L32I, 0x2000_0000), Some(exc::LOAD_PROHIBITED));
        assert_eq!(cause_of(&mut b, S32I, 0x2000_0000), Some(exc::STORE_PROHIBITED));
        assert_eq!(cause_of(&mut b, L8UI, 0x2000_0001), Some(exc::LOAD_PROHIBITED));
    }
}
