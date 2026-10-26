//! Jumperly patch J28: the classic ESP32 (D0WD, NodeMCU-32S) behind the Jumperly ABI: three UARTs
//! and no USB-Serial/JTAG, PWM from LEDC, two I2C controllers, the HSPI (host 2) and VSPI (host 3)
//! bridges, the host I2S receive side, the linear ADC on ADC1 and ADC2, the two DAC outputs and
//! the ten touch pads; no octal flash (the trait's default).
use crate::chip::{host_transfer, HostI2c, JumperlyChip, PwmEvent};
use ::esp32::jumperly::{pin_state, PinWatch, PINS};
use esp_soc::SocBus;

/// UART0..2; console port 3 (USB-Serial/JTAG on the S3 and C3) does not exist here.
const UARTS: u32 = 3;
const ALL_PINS: u64 = (1u64 << PINS) - 1;
/// The host log line for a guest that starts deep sleep with a wake source the model lacks (no
/// "reset" in it: the adapter takes a warning with that word for a chip reset).
pub(crate) const DEEP_SLEEP_NOTE: &str = "[emu] deep sleep: only an EXT0 pin wake is modelled, other wake sources never fire";

impl JumperlyChip for ::esp32::Machine {
    fn pins(&self) -> u32 { PINS as u32 }
    fn console_ports(&self) -> u32 { UARTS }
    fn cpu_hz(&self) -> u64 { ::esp32::periph::CPU_HZ }
    fn booted_console(&mut self) -> &mut esp_soc::Console { &mut self.console }

    /// The S3's Jumperly mode with the linear ADC (as on the C3).
    fn jumperly_mode(&mut self) {
        self.web = None;
        self.console.raw = Some(Default::default());
        self.bus.pin_watch = Some(PinWatch::new(&self.bus.periph, ALL_PINS));
        self.bus.rmt_tap = Some(Vec::new());
        self.bus.pwm_tap = Some(Default::default());
        self.bus.periph.adc.adc_linear = true;
    }

    fn set_pin_watch(&mut self, mask: u64) -> bool {
        let Some(w) = &mut self.bus.pin_watch else { return false };
        w.mask = mask & ALL_PINS;
        true
    }

    fn pin_state(&self, pin: u32) -> u32 { pin_state(&self.bus.periph, pin as usize) }

    fn take_pin_events(&mut self) -> Vec<(u64, u8, u32)> {
        self.bus.pin_watch.as_mut().map(|w| std::mem::take(&mut w.events)).unwrap_or_default()
    }

    /// A floating pad leaves the driven pads, so the strap at a reset takes its reset pull; before
    /// the first instruction an input also re-latches the power-on strap.
    fn gpio_set_input(&mut self, pin: u8, level: Option<bool>) {
        match level {
            Some(level) => self.bus.gpio_set_input(pin, level),
            None => self.bus.gpio_float_input(pin),
        }
    }

    /// The classic's GPIO_FUNCn_IN_SEL_CFG has the S3 layout (SIG_IN_SEL bit 7, the pad in bits
    /// 0..5); `input_pin` also finds a peripheral on its IO_MUX pad (VSPI MISO on GPIO19 when the
    /// driver picks the direct pins), which the matrix alone does not show.
    fn gpio_in_pin(&self, signal: u32) -> u32 {
        let gpio = &self.bus.periph.gpio;
        if signal as usize >= gpio.gpio.func_in_sel.len() { return 255; }
        gpio.input_pin(signal as usize).map_or(255, u32::from)
    }

    fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) {
        if u32::from(port) >= UARTS { return (None, None); }
        let r = self.bus.periph.uart_route(port as usize);
        ((r.tx_pins != 0).then(|| r.tx_pins.trailing_zeros() as u8), r.rx_pin)
    }

    fn uart_baud(&self, port: u8) -> Option<u32> {
        if u32::from(port) >= UARTS { return None; }
        self.bus.periph.uart_route(port as usize).baud
    }

    fn uart_in(&mut self, port: u32, data: &[u8]) -> Option<u32> {
        if port >= UARTS { return None; }
        let n = data.len().min(self.bus.periph.uart[port as usize].rx_free());
        if n > 0 { self.bus.uart_input(port as usize, &data[..n]); }
        Some(n as u32)
    }

    fn take_rmt(&mut self) -> Vec<(u64, u8, Vec<bool>)> {
        self.bus.rmt_tap.as_mut().map(std::mem::take).unwrap_or_default()
    }

    fn take_pwm(&mut self) -> Vec<PwmEvent> {
        let Some(tap) = self.bus.pwm_tap.as_mut() else { return Vec::new() };
        std::mem::take(&mut tap.events).into_iter()
            .map(|(cycle, o)| PwmEvent { cycle, signal: o.signal, enabled: o.enabled, idle_high: o.idle_high, freq: o.freq, duty: o.duty })
            .collect()
    }

    /// ADC1: GPIO36..39, 32..35; ADC2: GPIO4, 0, 2, 15, 13, 12, 14, 27, 25, 26. An ADC2 pin takes
    /// the voltage while Wi-Fi runs too (Arduino's analogRead then answers 0 by itself).
    fn adc_set(&mut self, gpio: u32, mv: u32) -> bool {
        if gpio >= PINS as u32 || !::esp32::adc::valid_pin(gpio as u8) { return false; }
        if self.bus.pin_watch.is_none() { return false; }
        self.bus.periph.adc.analog.set(gpio as u8, esp_periph::AnalogSource::Const(mv as f32 / 1000.0));
        true
    }

    /// The host's reset is EN (CHIP_PU), a power-on reset: unlike a guest's software reset it also
    /// clears the RTC domain, so the DACs power down (RTC_IO PAD_DAC1/2 XPD_DAC bit 18 and
    /// DAC_XPD_FORCE bit 10, ESP-IDF v5.5.4 `rtc_io_reg.h:783,826`) and `take_dac` reports them
    /// off. The other RTC registers keep the guest's values (the trait's reset keeps them).
    fn reset(&mut self) {
        crate::machine::reboot(self);
        for reg in [0x3ff4_8484, 0x3ff4_8488] {
            let v = self.bus.periph.read32(reg);
            self.bus.periph.write32(reg, v & !((1 << 18) | (1 << 10)));
        }
    }

    /// Two I2C controllers, HSPI and VSPI. A transfer the host still has finishes with 0xFF.
    fn bus_host(&mut self, i2c: bool, spi: bool) {
        self.bus.i2c_host = i2c;
        let mut finished = false;
        for c in &mut self.bus.periph.i2c { finished |= c.set_defer(i2c); }
        self.bus.spi_host = spi;
        if !spi { finished |= self.bus.finish_spi_host(&[]); }
        if finished { self.bus.irq_dirty = true; }
    }

    /// The I2C controllers' interrupts are read live (`I2c<16>`, no cached sources as on the S3);
    /// the SPI bridge holds one transfer at a time, and finishing it may queue the other
    /// controller's (at most two rounds).
    fn bus_service(&mut self) -> u32 {
        let mut n = 0;
        for bus in 0..2 {
            let mut host = HostI2c { bus: bus as u32 };
            if self.bus.periph.i2c[bus].service(&mut host) { n += 1; }
        }
        while let Some((host, tx, rx_len)) = self.bus.pending_spi_host() {
            let rx = host_transfer(u32::from(host), &tx, rx_len);
            self.bus.finish_spi_host(&rx);
            n += 1;
        }
        if n > 0 { self.bus.irq_dirty = true; }
        n
    }

    fn i2s(&mut self, port: u32) -> Option<&mut esp_periph::I2s> {
        self.bus.periph.i2s.get_mut(port as usize).map(|i| &mut i.inner)
    }

    /// The classic ticks its devices inline: nothing is deferred.
    fn flush_ticks(&mut self) -> u64 {
        SocBus::flush_ticks(&mut self.bus);
        self.bus.cycles
    }

    fn wifi_link(&mut self) -> &mut esp_soc::wifi::StationLink { &mut self.bus.periph.wifi.link }

    /// The classic keeps the MAC in eFuse BLK0 with its CRC8 (not the S3/C3 BLK1 layout).
    fn set_mac(&mut self, mac: [u8; 6]) -> bool {
        self.mac = mac;
        self.bus.periph.efuse = ::esp32::periph::ClassicEfuse::new(mac);
        true
    }

    fn ble(&mut self) -> &mut esp_soc::ble::vhci::Ble { &mut self.bus.ble }
    fn ble_command(&mut self, command: &str) -> Result<(), String> { SocBus::ble_command(&mut self.bus, command) }
    fn enable_ble(&mut self, elf: &esp_soc::elf::Elf) -> Result<(), String> { SocBus::enable_ble(&mut self.bus, elf) }

    fn take_dac(&mut self) -> Vec<(u8, u32)> { self.bus.periph.adc.take_dac_changes() }

    /// Touch pads T0..T9: GPIO4, 0, 2, 15, 13, 12, 14, 27, 33, 32.
    fn touch_set(&mut self, gpio: u32, touched: bool) -> bool {
        gpio < PINS as u32 && self.bus.periph.adc.set_touch_input(gpio as u8, touched)
    }

    /// Only an EXT0 wake ends deep sleep: once per sleep, a line when the guest enables another.
    fn take_notes(&mut self) -> Vec<&'static str> {
        if self.bus.take_unmodelled_wake() { vec![DEEP_SLEEP_NOTE] } else { Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use crate::{esp32sim_delete, esp32sim_new, Emu};

    fn classic() -> *mut Emu {
        let board = "esp32";
        // SAFETY: the board name is readable for the call.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null());
        e
    }

    /// Jumperly patch J28: every ADC1 and ADC2 pad takes a voltage (ADC2 too: the Wi-Fi rule is
    /// the firmware's), other pins do not; outside Jumperly mode nothing does.
    #[test]
    fn adc_set_accepts_every_adc_pin_and_refuses_others() {
        let e = classic();
        // SAFETY: the test owns the live emulator until it deletes it.
        let c = unsafe { &mut *e }.m.chip_mut().unwrap();
        assert!(!c.adc_set(36, 1650), "outside Jumperly mode");
        c.jumperly_mode();
        for gpio in [36, 37, 38, 39, 32, 33, 34, 35, 4, 0, 2, 15, 13, 12, 14, 27, 25, 26] {
            assert!(c.adc_set(gpio, 1000), "GPIO{gpio}");
        }
        for gpio in [1, 3, 5, 16, 21, 22, 23, 40, 255, 256 + 36] {
            assert!(!c.adc_set(gpio, 0), "GPIO{gpio}");
        }
        unsafe { esp32sim_delete(e) };
    }

    /// Jumperly patch J28: `esp32sim_dac_take` hands (GPIO, mV) pairs as u32 LE, `u32::MAX` once
    /// for an output turned off; `esp32sim_touch_set` takes the ten touch pads only, and no chip
    /// without touch pads (the S3) takes any.
    #[test]
    fn dac_take_and_touch_set_exports() {
        use crate::{esp32sim_buf_len, esp32sim_buf_ptr, esp32sim_dac_take, esp32sim_touch_set};
        use xtensa_lx7::bus::Bus;
        let e = classic();
        let take = |e: *mut Emu| -> Vec<u32> {
            // SAFETY: the test owns the live emulator; the buffer is read before the next call.
            let n = unsafe { esp32sim_dac_take(e) } as usize;
            let buf = unsafe { std::slice::from_raw_parts(esp32sim_buf_ptr(e), esp32sim_buf_len(e)) };
            assert_eq!(buf.len(), 8 * n);
            buf.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
        };
        assert!(take(e).is_empty());
        {
            // SAFETY: the test owns the live emulator.
            let crate::machine::MachineKind::Esp32(m) = &mut unsafe { &mut *e }.m else { panic!("the classic") };
            m.bus.write32(0x3ff4_8488, (128 << 19) | (1 << 18) | (1 << 10)).unwrap();   // DAC2 (GPIO26) on
        }
        assert_eq!(take(e), vec![26, 1656]);
        assert!(take(e).is_empty());
        {
            // SAFETY: the test owns the live emulator.
            let crate::machine::MachineKind::Esp32(m) = &mut unsafe { &mut *e }.m else { unreachable!() };
            m.bus.write32(0x3ff4_8488, 0).unwrap();
        }
        assert_eq!(take(e), vec![26, u32::MAX]);
        for (gpio, code) in [(4, 0), (0, 0), (2, 0), (15, 0), (13, 0), (12, 0), (14, 0), (27, 0), (33, 0), (32, 0), (5, 1), (34, 1), (260, 1)] {
            assert_eq!(unsafe { esp32sim_touch_set(e, gpio, 1) }, code, "GPIO{gpio}");
        }
        unsafe { esp32sim_delete(e) };

        let board = "none";
        // SAFETY: the board name is readable for the call; the emulator is ours until deleted.
        let s3 = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert_eq!(unsafe { (esp32sim_touch_set(s3, 4, 1), esp32sim_dac_take(s3)) }, (1, 0));
        unsafe { esp32sim_delete(s3) };
    }

    /// Jumperly patch J28: a guest's software reset (`esp_restart`) keeps the RTC domain and with it
    /// the DAC; the host's reset (EN, a power-on reset) turns it off, reported once.
    #[test]
    fn the_host_reset_turns_the_dac_off_and_a_software_reset_does_not() {
        use xtensa_lx7::bus::Bus;
        let e = classic();
        // SAFETY: the test owns the live emulator until it deletes it.
        let emu = unsafe { &mut *e };
        {
            let crate::machine::MachineKind::Esp32(m) = &mut emu.m else { panic!("the classic") };
            m.bus.write32(0x3ff4_8484, (255 << 19) | (1 << 18) | (1 << 10)).unwrap();   // DAC1 (GPIO25) on
            assert_eq!(m.bus.periph.adc.take_dac_changes(), [(25, 3300)]);
            crate::machine::reboot(m.as_mut());                                     // the guest's reset path
            assert!(m.bus.periph.adc.take_dac_changes().is_empty(), "esp_restart keeps the DAC");
        }
        let c = emu.m.chip_mut().unwrap();
        c.reset();
        assert_eq!(c.take_dac(), [(25, u32::MAX)]);
        assert!(c.take_dac().is_empty());
        unsafe { esp32sim_delete(e) };
    }

    /// Jumperly patch J28: the classic's module has 4 MB of flash whatever the host asks for.
    #[test]
    fn the_classic_gets_the_modules_4_mb_of_flash() {
        let board = "esp32";
        for mb in [1, 4, 16] {
            // SAFETY: the board name is readable for the call; the emulator is ours until deleted.
            let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), mb, 0) };
            let crate::machine::MachineKind::Esp32(m) = &unsafe { &*e }.m else { panic!("the classic") };
            assert_eq!(m.bus.flash.len(), 4 << 20, "{mb} MB asked");
            unsafe { esp32sim_delete(e) };
        }
    }

    /// Jumperly patch J28: Jumperly mode turns the linear ADC on and watches all 40 pins.
    #[test]
    fn jumperly_mode_sets_the_linear_adc_and_the_taps() {
        let e = classic();
        // SAFETY: the test owns the live emulator until it deletes it.
        let m = unsafe { &mut *e };
        m.m.chip_mut().unwrap().jumperly_mode();
        let crate::machine::MachineKind::Esp32(machine) = &m.m else { panic!("the classic") };
        assert!(machine.bus.periph.adc.adc_linear);
        assert_eq!(machine.bus.pin_watch.as_ref().unwrap().mask, (1u64 << 40) - 1);
        assert!(machine.bus.rmt_tap.is_some() && machine.bus.pwm_tap.is_some() && machine.web.is_none());
        unsafe { esp32sim_delete(e) };
    }

    /// Jumperly patch J28: the I2C and SPI bridges through the host imports: HSPI is host 2, VSPI
    /// host 3; turning the SPI bridge off finishes a waiting transfer with 0xFF.
    #[test]
    fn the_classic_bridges_reach_the_host() {
        use std::{cell::RefCell, rc::Rc};
        use xtensa_lx7::bus::Bus;
        const VSPI: u32 = 0x3ff6_5000;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        crate::chip::set_native_bus_host(|_, op, _| u32::from(op == 0), move |host, tx, rx| {
            s.borrow_mut().push((host, tx.to_vec()));
            rx.fill(0xa0 | host as u8);
        });
        let e = classic();
        // SAFETY: the test owns the live emulator until it deletes it.
        let m = unsafe { &mut *e };
        m.m.chip_mut().unwrap().bus_host(true, true);
        let crate::machine::MachineKind::Esp32(machine) = &mut m.m else { panic!("the classic") };
        let b = &mut machine.bus;
        b.write32(VSPI + 0x80, 0x5a).unwrap();
        b.write32(VSPI + 0x1c, (1 << 28) | (1 << 27)).unwrap();
        b.write32(VSPI + 0x28, 7).unwrap();
        b.write32(VSPI + 0x2c, 7).unwrap();
        b.write32(VSPI, 1 << 18).unwrap();
        assert_ne!(b.read32(VSPI).unwrap() & (1 << 18), 0, "waits for the host");
        assert_eq!(m.m.chip_mut().unwrap().bus_service(), 1);
        assert_eq!(&*seen.borrow(), &[(3, vec![0x5a])]);
        let crate::machine::MachineKind::Esp32(machine) = &mut m.m else { unreachable!() };
        let b = &mut machine.bus;
        assert_eq!(b.read32(VSPI + 0x80).unwrap() & 0xff, 0xa3);
        b.write32(VSPI, 1 << 18).unwrap();
        m.m.chip_mut().unwrap().bus_host(true, false);
        let crate::machine::MachineKind::Esp32(machine) = &mut m.m else { unreachable!() };
        let b = &mut machine.bus;
        assert_eq!(b.read32(VSPI).unwrap() & (1 << 18), 0, "finished without the host");
        assert_eq!(b.read32(VSPI + 0x80).unwrap() & 0xff, 0xff);
        assert_eq!(seen.borrow().len(), 1);
        unsafe { esp32sim_delete(e) };
    }
}
