//! The ESP8266 as a `Soc`: one LX106 core on `bus::SocBus`, booted by the modelled mask ROM
//! (`rom`), on a board from `board::make_board` (a bare module by default).
use crate::bus::{ResetKind, SocBus, IRAM_HIGH, IRAM_LOW, ROM_HIGH, ROM_LOW};
use crate::periph::{self, Peripherals, Rtc};
use crate::rom::{self, boot};
use emu_core::Core;
use esp_periph::Misc;
use esp_soc::{BoardModel, Soc, Stop};
use xtensa_lx7::{state::{ps, sr}, Lx106};

/// Where an Arduino ESP8266 flash image keeps its app: eboot sits at 0 and loads the image at
/// 0x1000 (bootloaders/eboot/eboot.c:33-89; tools/elf2bin.py writes the app there).
pub const APP_OFFSET: usize = 0x1000;

pub struct Esp8266;
pub type Machine = esp_soc::Machine<Esp8266>;

pub fn machine(mac: [u8; 6], flash_size: usize) -> Machine { let mut m = Machine::new(mac, SocBus::new(flash_size, mac)); m.set_debug(&esp_soc::DebugFlags::from_env()); m }

impl Soc for Esp8266 {
    type Core = Lx106;
    type Bus = SocBus;
    const NAME: &'static str = "esp8266";
    /// No ROM ELF exists for the ESP8266: the ROM is `rom`.
    const ROM_ELF: Option<&'static str> = None;
    const CPU_HZ: u64 = periph::CPU_HZ;
    const CORES: usize = 1;
    const IDLE_CHUNK: u64 = 64;
    const ROM_BOOT_DEFAULT: bool = true;
    /// The image at flash 0 is the one the ROM boots (eboot on Arduino builds).
    const BOOTLOADER_OFFSET: usize = 0;
    /// The app follows eboot at 0x1000; flash 0x10000 is the app's IROM there, no image header.
    const APP_OFFSET: usize = APP_OFFSET;
    const ROM_DATA_TABLE: &'static [&'static str] = &[];
    fn new_core(_i: usize) -> Lx106 { Lx106::new() }
    fn reset_core(c: &mut Lx106, _i: usize) { c.reset(); }
    /// As eboot starts the application: at its entry, on the boot stack.
    fn boot_core(c: &mut Lx106, entry: u32) {
        c.reset();
        c.set_pc(entry);
        c.set_ar(1, boot::BOOT_STACK);
        c.write_sr(sr::PS, ps::UM).unwrap();
    }
    /// The ROM loader runs at the reset vector, after the host's last word on strapping and cause.
    fn rom_boot(m: &mut Machine) { m.bus.rom.boot_pending = true; }
    /// Lines 0..13 and the NMI at 14 (`Lx106::set_irq`), as `refresh_irq` derived them.
    fn irqs(bus: &SocBus, out: &mut [u32]) { out[0] = bus.lines; }
    /// In deep sleep the core is off: parked in WAITI with every interrupt masked, until a reset.
    fn peripheral_event(bus: &SocBus, cores: &mut [Lx106]) {
        if bus.deep_sleep() {
            for c in cores { c.waiti(15); c.write_sr(sr::INTENABLE, 0).unwrap(); }
        }
    }
    fn function_hooks(_bus: &SocBus) -> &[u32] { rom::hooks() }
    fn function_hook(core: &mut Lx106, bus: &mut SocBus) -> bool { rom::dispatch(core, bus, 1) > 0 }
    fn function_hook_steps(core: &mut Lx106, bus: &mut SocBus, budget: u32) -> u32 { rom::dispatch(core, bus, budget) }
    /// The ROM is reachable only by a control transfer, and every pc in it is `rom::dispatch`'s.
    const FUNCTION_HOOK_RANGE: Option<(u32, u32)> = Some((ROM_LOW, ROM_HIGH));
    /// An image with the 0xE9 magic must enter in IRAM, where every ESP8266 image (eboot, the
    /// SDK's, the Arduino app) does; an ESP32-family image enters elsewhere (0x4008xxxx,
    /// 0x403xxxxx, 0x408xxxxx) and is refused. Anything else is no image header and passes. A
    /// whole flash image is judged at 0 and at `APP_OFFSET` (`esp_soc::load::check_flash_image`).
    fn check_image(image: &[u8]) -> Result<(), String> {
        let Some(h) = image.get(..8) else { return Ok(()) };
        if h[0] != 0xE9 { return Ok(()); }
        let entry = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        if (IRAM_LOW..IRAM_HIGH).contains(&entry) { return Ok(()); }
        Err(format!("is not an ESP8266 image: its entry point {entry:#010x} is outside the ESP8266's IRAM \
                     ({IRAM_LOW:#010x}..{IRAM_HIGH:#010x}); it looks built for an ESP32-family chip"))
    }
}

impl esp_soc::SocBus for SocBus {
    fn cycles(&self) -> u64 { self.cycles }
    fn irq_dirty(&mut self) -> &mut bool { &mut self.irq_dirty }
    fn refresh_irq(&mut self) -> bool {
        let (lines, edge_waiting) = self.periph.refresh_lines();
        self.lines = lines;
        if edge_waiting { self.irq_dirty = true; }
        true
    }
    /// APB cycles from now as CPU cycles: twice as many with CPU2X (`tick`).
    #[inline]
    fn cpu_cycles_for_bus(&self, cycles: u64) -> u64 {
        cycles.saturating_mul(2).saturating_sub(self.clock_half).div_ceil(self.periph.cpu_scale_half)
    }
    /// The next timer, watchdog or deep-sleep event, in CPU cycles: an idle core is not
    /// fast-forwarded past it.
    fn next_deadline(&self) -> Option<u64> {
        match self.periph.next_event() { u64::MAX => None, at => Some(self.cpu_cycles_for_bus(at.saturating_sub(self.cycles)).max(1)) }
    }
    /// An SPI1 transaction waits for the host (`Hspi::host_wait`).
    fn take_host_event(&mut self) -> bool { std::mem::take(&mut self.host_event) }
    fn misc(&mut self) -> &mut Misc { &mut self.periph.misc }
    fn take_stop(&mut self) -> Option<Stop> { self.rom.take_stop() }
    fn load_bytes(&mut self, addr: u32, data: &[u8]) -> Result<(), String> { SocBus::load_bytes(self, addr, data) }
    fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> { SocBus::write_flash(self, offset, data) }
    /// What eboot does with the image at `app_off` (bootloaders/eboot/eboot.c:33-89): copy the
    /// segments for RAM, leave the flash-resident ones to the cache (mapped at flash 0).
    fn boot_app(&mut self, app_off: usize) -> Result<u32, String> {
        let img = boot::parse(&self.flash.bytes, app_off).map_err(|e| format!("flash {app_off:#x}: {e}"))?;
        for s in &img.segments {
            if s.addr >= crate::bus::FLASH_WIN_LOW && s.addr < crate::bus::FLASH_WIN_HIGH { continue; }
            let data = self.flash.bytes[s.off as usize..(s.off + s.len) as usize].to_vec();
            SocBus::load_bytes(self, s.addr, &data)?;
        }
        self.cache_read_enable(0, 0, 1);
        self.rom.boot_pending = false;
        Ok(img.entry)
    }
    /// Digital peripherals re-created; DRAM and IRAM kept; the RTC registers (with GPIO16's) and
    /// RTC memory as the reset's kind has it (`ResetKind`). The ROM boots again at the reset
    /// vector, reading the strapping then. Returns the cause the ROM will report.
    fn reboot(&mut self, mac: [u8; 6]) -> u32 {
        let mut old = std::mem::replace(&mut self.periph, Peripherals::new(mac));
        self.periph.misc.log_unknown = old.misc.log_unknown;
        self.periph.now = self.cycles;
        let g = &mut self.periph.gpio;
        (g.host_driven, g.host_level, g.observe, g.events) = (old.gpio.host_driven, old.gpio.host_level, old.gpio.observe, std::mem::take(&mut old.gpio.events));
        match self.reset_kind {
            ResetKind::Jump | ResetKind::Core => {
                self.periph.rtc = Rtc { regs: old.rtc.regs.clone(), sleep: None };
                self.periph.gpio.gp16 = old.gpio.gp16;
            }
            ResetKind::Chip => {}
            ResetKind::PowerOn => {
                self.rtc_mem.fill(0);
                self.dram.fill(0);
                self.iram.fill(0);
            }
        }
        self.periph.gpio.level = old.gpio.level;
        let before = old.gpio.output();
        let g = &mut self.periph.gpio;
        g.update_levels();
        g.status = 0;
        g.note(before, self.cycles);
        self.periph.misc.log_all = self.debug.has("mmio");
        self.reset_kind = ResetKind::Jump;
        self.clock_half = 0;
        self.host_event = false;
        self.rom.reset();
        self.cache_base = 0;
        self.cache_enabled = false;
        self.sw_reset = false;
        self.irq_dirty = true;
        self.reset_cause
    }
    fn sw_reset(&self) -> bool { self.sw_reset }
    /// A reset from outside with the ROM's `cause` (`rom::boot::RST_*`): RST_EXT is the RST pin,
    /// and in deep sleep that is the wake-up (`chip_reset`).
    fn request_reset(&mut self, cause: u32) { self.chip_reset(cause); }
    fn reset_cause(&self) -> u32 { self.reset_cause }
    fn last_fault(&self) -> Option<(u32, bool)> { self.last_fault }
    /// No USB serial on the ESP8266; UART0 and UART1.
    fn console_take(&mut self) -> [Vec<u8>; 4] { [Vec::new(), std::mem::take(&mut self.periph.uart[0].tx_out), std::mem::take(&mut self.periph.uart[1].tx_out), Vec::new()] }
    /// The ESP8266 has no USB serial: host text goes to UART0, where its console is.
    fn serial_input(&mut self, data: &[u8]) { self.uart_input(0, data); }
    fn uart_input(&mut self, n: usize, data: &[u8]) {
        let Some(u) = self.periph.uart.get_mut(n) else { return };
        u.host_input(data);
        self.irq_dirty = true;
    }
    fn gpio_set_input(&mut self, pin: u8, level: bool) {
        self.periph.gpio_set_input(pin, Some(level));
        self.irq_dirty = true;
    }
    /// The pads' levels, GPIO0..16.
    fn gpio_input(&self) -> u64 { u64::from(self.periph.gpio.level & 0x1_ffff) }
    fn observe_gpio(&mut self, on: bool) { self.periph.gpio.observe = on; }
    fn take_gpio_events(&mut self) -> Vec<(u64, u8, bool)> { std::mem::take(&mut self.periph.gpio.events) }
    fn board(&mut self) -> &mut dyn BoardModel { &mut *self.board }
    fn board_ref(&self) -> &dyn BoardModel { &*self.board }
    fn audio(&self) -> (&[i16], u32) { (&[], 44100) }
    fn irq_sources_of(&self, _core: usize, _line: u32) -> Vec<usize> { Vec::new() }
    fn set_debug(&mut self, f: &esp_soc::DebugFlags) { self.debug = f.clone(); self.periph.misc.log_all = f.has("mmio"); }
    fn set_flash_size(&mut self, bytes: usize) { self.flash = crate::flash::Flash::new(bytes); }
    /// The host's strapping instead of the pins' levels (`--strap`).
    fn set_strap(&mut self, v: u32) { self.strap = Some(v & 7); self.periph.gpio.strap = v & 7; }
    fn set_reset_cause(&mut self, cause: u32) { self.reset_cause = cause; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_image_takes_esp8266_images_and_refuses_esp32_ones() {
        // eboot in an Arduino 3.1.2 image: e9 02 03 20, entry 0x4010f480
        assert!(Esp8266::check_image(&[0xe9, 2, 3, 0x20, 0x80, 0xf4, 0x10, 0x40]).is_ok());
        // the app at 0x1000: entry 0x401000c0
        assert!(Esp8266::check_image(&[0xe9, 5, 3, 0x20, 0xc0, 0x00, 0x10, 0x40]).is_ok());
        assert!(Esp8266::check_image(&[0xff; 16]).is_ok());
        for entry in [0x4008_0c00u32, 0x4037_c4e0, 0x403b_6000, 0x4080_0000] {
            let mut h = vec![0xe9, 3, 2, 0x20];
            h.extend(entry.to_le_bytes());
            let e = Esp8266::check_image(&h).unwrap_err();
            assert!(e.contains("not an ESP8266 image"), "{e}");
        }
    }

    /// A whole flash image is judged at 0 (eboot) and at 0x1000 (the app); at 0x10000 the app's
    /// IROM may hold any word, an 0xE9 byte followed by an ESP32-like entry too.
    #[test]
    fn a_flash_image_is_judged_at_0_and_at_the_app_at_0x1000() {
        let mut flash = vec![0xff; 0x2_0000];
        flash[..8].copy_from_slice(&[0xe9, 2, 3, 0x20, 0x80, 0xf4, 0x10, 0x40]);
        flash[APP_OFFSET..APP_OFFSET + 8].copy_from_slice(&[0xe9, 5, 3, 0x20, 0xc0, 0x00, 0x10, 0x40]);
        flash[0x1_0000..0x1_0008].copy_from_slice(&[0xe9, 3, 2, 0x20, 0x00, 0x0c, 0x08, 0x40]);
        assert!(esp_soc::load::check_flash_image::<Esp8266>(&flash).is_ok());
        flash[APP_OFFSET + 4..APP_OFFSET + 8].copy_from_slice(&0x4008_0c00u32.to_le_bytes());
        let e = esp_soc::load::check_flash_image::<Esp8266>(&flash).unwrap_err();
        assert!(e.contains("app at 0x1000"), "{e}");
    }
}
