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

/// SPI0, the flash controller: CMD (esp8266_peri.h:301) and its command bits (:382-391).
const SPI0_CMD: u32 = PERIPH_BASE + 0x200;
const SPI_READ: u32 = 1 << 31;
const SPI_RDID: u32 = 1 << 28;
const SPI_RDSR: u32 = 1 << 27;
const SPI_PP: u32 = 1 << 25;
const SPI_SE: u32 = 1 << 24;
const SPI_BE: u32 = 1 << 23;
const SPI_CE: u32 = 1 << 22;

/// Where an address goes.
enum At { Dram(usize), Iram(usize), Rtc(usize), Flash(usize), Rom, Mmio, Unmapped }

/// SPI1's CMD register (esp8266_peri.h:337 SPI1CMD).
const SPI1_CMD: u32 = PERIPH_BASE + 0x100;
/// FRC1's LOAD and CTRL (esp8266_peri.h:147,149 T1L, T1C): a write starts the count from now.
const FRC1_LOAD: u32 = PERIPH_BASE + 0x600;
const FRC1_CTRL: u32 = PERIPH_BASE + 0x608;

/// Code pages (`Bus::page_versions`): `1 << VPAGE_SHIFT` bytes of IRAM, then of the flash window,
/// then one page for every other address, which holds no code (`fetch` refuses it).
const PAGE_SHIFT: u32 = emu_core::bus::VPAGE_SHIFT;
const IRAM_PAGES: usize = ((IRAM_HIGH - IRAM_LOW) >> PAGE_SHIFT) as usize;
const WINDOW_PAGES: usize = ((FLASH_WIN_HIGH - FLASH_WIN_LOW) >> PAGE_SHIFT) as usize;
const CODE_PAGES: usize = IRAM_PAGES + WINDOW_PAGES + 1;

/// What a reset keeps (`SocBus::reboot`). The ROM numbering of the causes is `rom::boot`'s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResetKind {
    /// A jump to the ROM's reset vector (`system_restart_core` does it for `ESP.restart()`, after
    /// an exception and after a soft watchdog reset): nothing is reset by hardware, so the ROM's
    /// cause stays what it was (digitake esp8266-emulator src/main.c:281,635-636: "jump to the
    /// reset vector: peripherals and RAM keep their state"; Arduino ESP8266 3.1.2
    /// cores/esp8266/hwdt_app_entry.cpp:596-597: the ROM API "does not change the status after a
    /// software restart"; the table in `rom::boot`). Model choice: the digital peripherals are
    /// re-created anyway; the ROM and the SDK set them up again.
    #[default]
    Jump,
    /// The digital core resets (hardware watchdog, RTC software reset): the RTC domain stays.
    Core,
    /// The chip resets through EXT_RSTB (the RST pin, also the deep-sleep wake through GPIO16):
    /// the RTC registers clear, RTC memory stays. The SDK needs both: it tells an external reset
    /// ("External System") from a watchdog one by RTC STORE0 being 0 again (libmain's start-up,
    /// Blink image 0x4023383c..0x40233886), and finds its deep-sleep record in RTC memory.
    Chip,
    /// Power-on: RTC memory and RAM clear too.
    PowerOn,
}

impl ResetKind {
    /// The kind of reset a ROM cause stands for.
    pub fn of(cause: u32) -> Self {
        use crate::rom::boot::{RST_DEEPSLEEP, RST_EXT, RST_POWERON};
        match cause { RST_POWERON => ResetKind::PowerOn, RST_EXT | RST_DEEPSLEEP => ResetKind::Chip, _ => ResetKind::Core }
    }
}

pub struct SocBus {
    pub dram: Vec<u8>,
    pub iram: Vec<u8>,
    /// RTC memory: kept across a chip reset, lost with power
    pub rtc_mem: Vec<u8>,
    pub flash: Flash,
    /// flash offset of the window's first byte (`Cache_Read_Enable`; set it through `cache_read_enable`,
    /// which moves the code pages with it)
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
    /// what the pending reset keeps
    pub reset_kind: ResetKind,
    /// the host's strapping (`--strap`), GPIO15 << 2 | GPIO0 << 1 | GPIO2; `None`: the pins' levels
    pub strap: Option<u32>,
    /// the last reset was a jump to the reset vector: the ROM's boot keeps the strapping latched
    /// at the hardware reset before it (`latch_strap`)
    pub strap_kept: bool,
    /// a host serves SPI1's transactions (`Hspi::host_wait`); without one the board answers at once
    pub spi_host: bool,
    /// something the host must see happened (an SPI1 transaction waits): `take_host_event`
    pub host_event: bool,
    /// the half APB cycle a CPU2X core clock has run past `cycles`
    pub clock_half: u64,
    /// the interrupt lines as last derived (`Peripherals::refresh_lines`)
    pub lines: u32,
    pub debug: esp_soc::DebugFlags,
    /// Jumperly patch J29: pin states with cycles for the host (`crate::jumperly`), the PWM
    /// detector on the output edges (`crate::pwm`), the soft I2C decoders on the circuit's pin
    /// pairs (`crate::soft_i2c`), the request to end the slice (`SocBus::take_slice_yield`) and
    /// the rest of the host side (`jumperly::HostState`).
    pub pin_watch: Option<crate::jumperly::PinWatch>,
    pub pwm_tap: Option<crate::pwm::PwmDetector>,
    pub soft_i2c: Option<crate::soft_i2c::SoftI2c>,
    pub(crate) slice_yield: bool,
    pub jumperly: crate::jumperly::HostState,
    /// A core runs in the current scheduling round (`Soc::select_core` sets it, `tick` clears
    /// it): device time is the round's start until the round's `tick`, not the instruction's.
    pub in_round: bool,
    /// FRC1 was loaded or started inside the round: it counts from the round's `tick`, which the
    /// machine moves up to the write (`esp_soc::SocBus::round_cut`), not from the round's start.
    pub frc1_rearmed: bool,
    /// write versions of the code pages (`Bus::page_versions`): a store to IRAM, a change of the
    /// flash under the window or a remap of the window moves them, and the core's decoded blocks
    /// with them. Code that changes `iram` or `flash.bytes` directly, while the core runs, must
    /// call `code_changed`.
    code_ver: Vec<u32>,
}

impl SocBus {
    pub fn new(flash_size: usize, mac: [u8; 6]) -> Self {
        SocBus {
            dram: vec![0; (DRAM_HIGH - DRAM_LOW) as usize], iram: vec![0; (IRAM_HIGH - IRAM_LOW) as usize],
            rtc_mem: vec![0; (RTC_MEM_HIGH - RTC_MEM_LOW) as usize], flash: Flash::new(flash_size), cache_base: 0, cache_enabled: false,
            periph: Peripherals::new(mac), rom: RomState::default(), board: Box::new(esp_soc::NoBoard),
            cycles: 0, irq_dirty: true, last_fault: None, rom_data_reads: 0, reset_cause: crate::rom::boot::RST_POWERON,
            sw_reset: false, reset_kind: ResetKind::Jump, strap: None, strap_kept: false, spi_host: false, host_event: false, clock_half: 0, lines: 0,
            debug: Default::default(), in_round: false, frc1_rearmed: false, code_ver: vec![0; CODE_PAGES],
            pin_watch: None, pwm_tap: None, soft_i2c: None, slice_yield: false, jumperly: Default::default(),   // Jumperly patch J29
        }
    }

    /// `Cache_Read_Enable(map, p, v)`: point the window at flash. Arduino ESP8266 3.1.2
    /// cores/esp8266/mmu_iram.cpp:61-79: `map` 0/1 selects the even/odd megabyte (flash address
    /// bit 20; "no guesses for a value of 2 or greater", so only bit 0 is used), `p` is bit 21 (or
    /// bits 21-23), `v` the cache size (16/32 KB), which this model does not need.
    pub fn cache_read_enable(&mut self, map: u32, p: u32, _v: u32) {
        let base = ((map & 1) << 20) | ((p & 7) << 21);
        if base != self.cache_base {
            // other bytes behind every pc of the window
            self.cache_base = base;
            self.bump_pages(IRAM_PAGES..IRAM_PAGES + WINDOW_PAGES);
        }
        self.cache_enabled = true;
    }

    /// The code page of `addr` (`Bus::code_page`).
    fn code_page_of(addr: u32) -> usize {
        match addr {
            IRAM_LOW..IRAM_HIGH => ((addr - IRAM_LOW) >> PAGE_SHIFT) as usize,
            FLASH_WIN_LOW..FLASH_WIN_HIGH => IRAM_PAGES + ((addr - FLASH_WIN_LOW) >> PAGE_SHIFT) as usize,
            _ => CODE_PAGES - 1,
        }
    }

    fn bump_pages(&mut self, pages: std::ops::Range<usize>) {
        for v in &mut self.code_ver[pages] { *v = v.wrapping_add(1); }
    }

    /// `len` bytes of IRAM from offset `o` may have changed.
    #[inline]
    fn iram_changed(&mut self, o: usize, len: usize) {
        let end = o.saturating_add(len).min(IRAM_PAGES << PAGE_SHIFT);
        if o >= end { return; }
        self.bump_pages(o >> PAGE_SHIFT..((end - 1) >> PAGE_SHIFT) + 1);
    }

    /// `len` bytes of flash from offset `off` changed: the window pages showing them move.
    pub fn flash_changed(&mut self, off: usize, len: usize) {
        let lo = self.cache_base as usize;
        let hi = lo + (WINDOW_PAGES << PAGE_SHIFT);
        let (a, b) = (off.max(lo), off.saturating_add(len).min(hi));
        if a >= b { return; }
        let (first, last) = ((a - lo) >> PAGE_SHIFT, (b - 1 - lo) >> PAGE_SHIFT);
        self.bump_pages(IRAM_PAGES + first..IRAM_PAGES + last + 1);
    }

    /// Any code may have changed (after direct writes to `iram` or `flash.bytes`).
    pub fn code_changed(&mut self) { self.bump_pages(0..CODE_PAGES); }

    /// Program flash (`Flash::write`), keeping the code pages current.
    pub fn flash_write(&mut self, off: u32, data: &[u8]) -> bool {
        let ok = self.flash.write(off, data);
        if ok { self.flash_changed(off as usize, data.len()); }
        ok
    }

    /// Erase a 4 KB sector (`Flash::erase_sector`), keeping the code pages current.
    pub fn flash_erase_sector(&mut self, n: u32) -> bool {
        let ok = self.flash.erase_sector(n);
        if ok { self.flash_changed(n as usize * crate::flash::SECTOR, crate::flash::SECTOR); }
        ok
    }

    /// Erase a 64 KB block (`Flash::erase_block`), keeping the code pages current.
    pub fn flash_erase_block(&mut self, n: u32) -> bool {
        let ok = self.flash.erase_block(n);
        if ok { self.flash_changed(n as usize * crate::flash::BLOCK, crate::flash::BLOCK); }
        ok
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
        if self.rom_data_reads == 1 { self.rom.note(format!("data read at {addr:#010x} reads 0: the ESP8266 mask ROM's bytes are not modelled (logged once)")); }
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
            At::Mmio => {
                // a read of a receive FIFO may lower interrupt 5
                if addr & !3 == PERIPH_BASE || addr & !3 == UART1_BASE { self.irq_dirty = true; }
                Self::get::<N>(&self.periph.read32(addr & !3).to_le_bytes(), (addr & 3) as usize)
            }
            At::Unmapped => None,
        };
        match bytes { Some(b) => Ok(b), None => self.fault(addr, false, Fault::Unmapped) }
    }

    fn write_n<const N: usize>(&mut self, addr: u32, v: [u8; N]) -> Result<(), Fault> {
        let mem = match self.at(addr) {
            At::Dram(o) => self.dram.get_mut(o..o + N),
            At::Rtc(o) => self.rtc_mem.get_mut(o..o + N),
            At::Iram(o) if N == 4 => { self.iram_changed(o, N); self.iram.get_mut(o..o + N) }
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
        if self.in_round && (a == FRC1_LOAD || a == FRC1_CTRL) { self.frc1_rearmed = true; }
        if a == SPI0_CMD && w != 0 { self.spi0_command(w); }
        if a == SPI1_CMD { self.spi1_transfer(); }
        if self.jumperly_hooks() { self.note_pin_write(a, w); }   // Jumperly patch J29
        if let Some(cause) = self.periph.reset.take() { self.chip_reset(cause); }
    }

    /// A transaction SPI1's USR started goes to the host (`spi_host`) or, without one, to the
    /// board, which answers at once (the bare module: nothing on MISO, 0xFF).
    fn spi1_transfer(&mut self) {
        let Some(t) = self.periph.spi1.take_transfer() else { return };
        if self.spi_host {
            self.periph.spi1.host_wait = Some(t);
            self.host_event = true;
            self.slice_yield = true;   // Jumperly patch J29: the host serves it between slices
        } else {
            let rx = self.board.spi_transfer(1, &t.tx, t.rx_len);
            self.periph.spi1.finish_transfer(t, &rx);
        }
    }

    /// Complete the transaction the host was given (`Hspi::host_wait`) with the bytes it read.
    pub fn finish_spi_transfer(&mut self, rx: &[u8]) {
        if let Some(t) = self.periph.spi1.host_wait.take() { self.periph.spi1.finish_transfer(t, rx); }
    }

    /// A reset with `cause` in the ROM's numbering (`rom::boot::RST_*`, what the banner's
    /// `rst cause` prints): 1 power-on, 2 external (the RST pin), 3 software, 4 hardware watchdog,
    /// 5 deep-sleep wake. This is not the ESP32 numbering of `esp_periph::RST_*`, which hosts
    /// shared with the other chips pass through `esp_soc::SocBus::request_reset`; there 1 is
    /// POWERON_RESET too, but 2..5 mean other things. A cause outside 1..=5 is logged and taken as
    /// an external reset, the reset a host can give the chip. An external reset of a chip in deep
    /// sleep is its wake-up, DEEPSLEEP_RESET (the GPIO16 wire to RST does it).
    pub fn chip_reset(&mut self, cause: u32) {
        use crate::rom::boot::{RST_DEEPSLEEP, RST_EXT, RST_POWERON};
        let cause = if (RST_POWERON..=RST_DEEPSLEEP).contains(&cause) { cause } else {
            self.rom.push_note(format!("[emu] reset cause {cause} is not an ESP8266 ROM cause (1..=5): taken as an external reset ({RST_EXT})"));
            RST_EXT
        };
        let cause = if cause == RST_EXT && self.deep_sleep() { RST_DEEPSLEEP } else { cause };
        self.reset_cause = cause;
        self.reset_kind = ResetKind::of(cause);
        self.sw_reset = true;
    }

    /// The guest jumped to the ROM's reset vector: a restart that resets nothing (`ResetKind::Jump`).
    pub fn restart_from_reset_vector(&mut self) {
        self.reset_kind = ResetKind::Jump;
        self.sw_reset = true;
    }

    /// Whether the chip is in deep sleep.
    pub fn deep_sleep(&self) -> bool { self.periph.rtc.sleep.is_some() }

    /// When the deep-sleep timer runs out (it then pulls GPIO16 low for
    /// `periph::WAKE_PULSE_CYCLES`); `None` when awake, after it ran out, or with no timer.
    pub fn deep_sleep_until(&self) -> Option<u64> { self.periph.rtc.sleep.and_then(|s| s.until) }

    /// The boot mode at reset release: the host's `--strap`, or the strapping pins' levels, which
    /// GPIO_IN then reports in its bits 16 and up. After a jump to the reset vector nothing was
    /// released from reset, so the boot mode is the one latched before (`strap_kept`): the chip
    /// samples GPIO15/GPIO0/GPIO2 only on a hardware reset. That is why `ESP.restart()` right
    /// after a serial upload, with GPIO0 still held low by the programmer, hangs in the UART
    /// download mode: real logs show `rst cause:2, boot mode:(1,7)` there and a normal boot after
    /// a press of RST (esp8266/Arduino issue #1722, "hanging ESP.restart()"; the boot mode number
    /// is GPIO15 << 2 | GPIO0 << 1 | GPIO2, Arduino ESP8266 3.1.2 doc/boards.rst "Boot Messages
    /// and Modes"). The second number of the banner (7 there) is not modelled.
    pub fn latch_strap(&mut self) -> u32 {
        if std::mem::take(&mut self.strap_kept) { return self.periph.gpio.strap; }
        let mode = self.periph.latch_strap();
        if let Some(s) = self.strap { self.periph.gpio.strap = s; return s; }
        mode
    }

    /// A command written to SPI0's CMD register (esp8266_peri.h:301-320 for the registers,
    /// :382-395 for the command bits), done at once; CMD then reads 0, which is what the SDK polls
    /// for (`spi_flash_get_id`, Blink image 0x401061c8). READ takes the length from ADDR bits
    /// 24..31 and the address from bits 0..23 (digitake esp8266-emulator src/soc.c:424-436 reads
    /// them the same way), at most the 64 bytes of W0..W15.
    fn spi0_command(&mut self, cmd: u32) {
        let reg = |off: u32| SPI0_CMD + off;
        let a = self.periph.spi0.read(reg(0x04));
        let (addr, len) = (a & 0xff_ffff, ((a >> 24) as usize).min(64));
        let mut w = [0u8; 64];
        for (i, c) in w.chunks_mut(4).enumerate() { c.copy_from_slice(&self.periph.spi0.read(reg(0x40 + 4 * i as u32)).to_le_bytes()); }
        let store = |bus: &mut SocBus, w: &[u8; 64]| {
            for (i, c) in w.chunks(4).enumerate() { bus.periph.spi0.write(reg(0x40 + 4 * i as u32), u32::from_le_bytes(c.try_into().unwrap())); }
        };
        if cmd & SPI_READ != 0 {
            let mut buf = [0u8; 64];
            self.flash.read(addr, &mut buf[..len]);
            store(self, &buf);
        } else if cmd & SPI_RDID != 0 {
            let id = self.flash.jedec_id();
            self.periph.spi0.write(reg(0x40), id);
        } else if cmd & SPI_RDSR != 0 {
            // the status register (SPI0RS 0x210): never busy, no write enable latched
            self.periph.spi0.write(reg(0x10), 0);
        } else if cmd & SPI_PP != 0 {
            self.flash_write(addr, &w[..len]);
        } else if cmd & SPI_SE != 0 {
            self.flash_erase_sector(addr / crate::flash::SECTOR as u32);
        } else if cmd & SPI_BE != 0 {
            self.flash_erase_block(addr / crate::flash::BLOCK as u32);
        } else if cmd & SPI_CE != 0 {
            self.flash.bytes.fill(0xff);
            self.flash_changed(0, self.flash.len());
        }
        // WREN, WRDI, WRSR, DP, RES, HPM and user commands change nothing a model reads
        self.periph.spi0.write(SPI0_CMD, 0);
    }

    /// Plain memory as the ROM model and the loaders see it, not held to the instruction bus's
    /// 32-bit rule: `len` bytes at `addr` in DRAM, IRAM or RTC memory.
    pub fn ram_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let (mem, o) = match self.at(addr) {
            At::Dram(o) => (&mut self.dram, o),
            At::Iram(o) => { self.iram_changed(o, len); (&mut self.iram, o) }
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

    pub fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        self.flash.load(offset, data)?;
        self.flash_changed(offset, data.len());
        Ok(())
    }

    /// The pins the chip drives high: GPIO_OUT & GPIO_ENABLE (esp8266_peri.h:48,51), bit 16 GPIO16.
    pub fn gpio_output(&self) -> u32 { self.periph.gpio_output() }
}

impl Bus for SocBus {
    fn note_code_page(&mut self, _vidx: u32) {} // Every write to a code page moves its version.
    /// IRAM and the flash window in 256-byte pages (`code_ver`).
    fn page_versions(&self) -> &[u32] { &self.code_ver }
    fn code_page(&mut self, pc: u32) -> u32 { Self::code_page_of(pc) as u32 }
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
    /// The core ends its dispatch after a store that re-armed FRC1, so the machine can close the
    /// round on it (`esp_soc::SocBus::round_cut`).
    #[inline(always)]
    fn block_break(&self) -> bool { self.frc1_rearmed }
    /// Device time moves on by `cycles` CPU cycles: as many APB cycles, or half as many with
    /// CPU2X (the timers, the UARTs and WDEV keep the 80 MHz APB clock; Arduino ESP8266 3.1.2
    /// core_esp8266_waveform_pwm.cpp:664 halves its CPU-cycle counts for FRC1 at 160 MHz). A timer
    /// that fell due re-derives the interrupt lines.
    #[inline]
    fn tick(&mut self, cycles: u32) -> u32 {
        let half = u64::from(cycles) * self.periph.cpu_scale_half + self.clock_half;
        self.clock_half = half & 1;
        self.cycles += half >> 1;
        self.periph.now = self.cycles;
        self.in_round = false;
        // FRC1 re-armed by an instruction of this round counts from here: the round ended right
        // after that instruction (`round_cut`), or later on a path that does not cut rounds,
        // where it then counts late, never early. Counted from the round's start it could run
        // out up to a round early. The SDK's NMI wrapper routes FRC1 away from the NMI while it
        // runs (NONOS SDK 2.2.x libmain: `NMI_Handler` clears DPORT 0x3ff00000 bit 0 on entry,
        // `_NMILevelVector` writes 15 there just before its `rfi 3`), 49 instructions after the
        // Arduino waveform generator's last FRC1 load (core_esp8266_waveform_pwm.cpp:664), whose
        // shortest interval is 140 cycles (:654-662, MINIRQTIME - DELTAIRQ at 80 MHz). An edge
        // in that window goes to interrupt 9, which nothing serves: the generator never runs again.
        if std::mem::take(&mut self.frc1_rearmed) { self.periph.frc1.rebase(self.cycles); }
        if self.cycles < self.periph.next_event() { return 0; }
        let frc1 = self.periph.frc1.next;
        self.periph.advance(self.cycles);
        if frc1 <= self.cycles { self.jumperly.frc1_fired = frc1; }   // Jumperly patch J29: the waveform's timer
        if self.pin_watch.is_some() && self.periph.rtc.sleep.is_some() { self.pins_changed(); }   // Jumperly patch J29: GPIO16 in deep sleep
        if let Some(cause) = self.periph.reset.take() { self.chip_reset(cause); }
        self.irq_dirty = true;
        1
    }
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
    fn spi0_commands_complete_at_once_on_the_flash() {
        // SPI0A 0x204, SPI0RS 0x210, SPI0W0 0x240 (esp8266_peri.h:302,305,317)
        let (mut b, a, w0) = (bus(), SPI0_CMD + 0x04, SPI0_CMD + 0x40);
        b.write32(SPI0_CMD, SPI_RDID).unwrap();
        assert_eq!(b.read32(SPI0_CMD), Ok(0), "done: CMD reads 0");
        assert_eq!(b.read32(w0), Ok(0x16_40ef), "a W25Q-style id of a 4 MB chip, low byte first");
        b.flash.bytes[0x1234..0x123c].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        b.write32(a, 8 << 24 | 0x1234).unwrap();
        b.write32(SPI0_CMD, SPI_READ).unwrap();
        assert_eq!((b.read32(w0), b.read32(w0 + 4)), (Ok(0x0403_0201), Ok(0x0807_0605)));
        // page program ANDs W0.. into the cells; a sector erase sets them back
        b.write32(w0, 0x0000_00f0).unwrap();
        b.write32(a, 4 << 24 | 0x1234).unwrap();
        b.write32(SPI0_CMD, SPI_PP).unwrap();
        assert_eq!(b.flash.bytes[0x1234..0x1238], [0, 0, 0, 0]);
        b.write32(a, 0x1000).unwrap();
        b.write32(SPI0_CMD, SPI_SE).unwrap();
        assert_eq!(b.flash.bytes[0x1234..0x1238], [0xff; 4]);
        b.write32(SPI0_CMD + 0x10, 3).unwrap();
        b.write32(SPI0_CMD, SPI_RDSR).unwrap();
        assert_eq!(b.read32(SPI0_CMD + 0x10), Ok(0), "never busy");
    }

    #[test]
    fn spi1_transfers_wait_for_a_host_or_complete_at_once_from_the_board() {
        let (mut b, spi) = (bus(), PERIPH_BASE + 0x100);
        b.write32(spi + 0x1C, 1 << 27 | 1).unwrap();          // USER: MOSI, full duplex
        b.write32(spi + 0x20, 7 << 17 | 7 << 8).unwrap();     // USER1: 8 bits
        b.write32(spi + 0x40, 0x55).unwrap();
        b.write32(spi, 1 << 18).unwrap();                     // CMD: USR
        assert_eq!(b.read32(spi), Ok(0), "no host: done at once");
        assert_eq!(b.read32(spi + 0x40), Ok(0xff), "the bare module's MISO");
        assert!(!b.host_event);
        b.spi_host = true;
        b.write32(spi + 0x40, 0x54).unwrap();
        b.write32(spi, 1 << 18).unwrap();
        assert_eq!(b.read32(spi), Ok(1 << 18), "busy until the host answers");
        assert!(std::mem::take(&mut b.host_event));
        assert_eq!(b.periph.spi1.host_wait.as_ref().map(|t| t.tx.clone()), Some(vec![0x54]));
        b.finish_spi_transfer(&[0x12]);
        assert_eq!((b.read32(spi), b.read32(spi + 0x40)), (Ok(0), Ok(0x12)));
        assert_eq!(b.periph.spi1.transfers, 2);
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
