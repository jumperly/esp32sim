//! Jumperly patch J28: the ESP32-C3 behind the Jumperly ABI (the bodies of the C3 branches of the
//! J9 exports): two UARTs and USB-Serial/JTAG, PWM from LEDC, one I2C controller and the SPI2
//! bridge; no host I2S, no octal flash (the trait's defaults).
use crate::chip::{host_transfer, write_efuse_mac, HostI2c, JumperlyChip, PwmEvent};
use esp_soc::SocBus;
use esp32c3::jumperly::{pin_state, PinWatch, PINS};

/// Jumperly patch J9: the C3 has two UARTs; its USB-Serial/JTAG keeps the port number 3.
const C3_UARTS: u32 = 2;
const C3_ALL_PINS: u64 = (1u64 << PINS) - 1;

impl JumperlyChip for esp32c3::Machine {
    fn pins(&self) -> u32 { PINS as u32 }
    fn console_ports(&self) -> u32 { C3_UARTS }
    fn cpu_hz(&self) -> u64 { esp32c3::periph::CPU_HZ }
    fn booted_console(&mut self) -> &mut esp_soc::Console { &mut self.console }

    /// J9: the S3's Jumperly mode, the linear ADC on the C3's SAR ADC.
    fn jumperly_mode(&mut self) {
        self.web = None;
        self.console.raw = Some(Default::default());
        self.bus.pin_watch = Some(PinWatch::new(&self.bus.periph, C3_ALL_PINS));
        self.bus.rmt_tap = Some(Vec::new());
        self.bus.pwm_tap = Some(Default::default());   // Jumperly patch J4
        self.bus.periph.adc.adc_linear = true;   // Jumperly patch J5
    }

    fn set_pin_watch(&mut self, mask: u64) -> bool {
        let Some(w) = &mut self.bus.pin_watch else { return false };
        w.mask = mask & C3_ALL_PINS;
        true
    }

    fn pin_state(&self, pin: u32) -> u32 { pin_state(&self.bus.periph, pin as usize) }

    fn take_pin_events(&mut self) -> Vec<(u64, u8, u32)> {
        self.bus.pin_watch.as_mut().map(|w| std::mem::take(&mut w.events)).unwrap_or_default()
    }

    fn gpio_set_input(&mut self, pin: u8, level: Option<bool>) { self.bus.gpio_set_input(pin, level.unwrap_or(false)); }

    /// Jumperly patch J19: the C3 has SIG_IN_SEL at bit 6 and the pad in bits 0..4 (the S3: 7, 0..5).
    fn gpio_in_pin(&self, signal: u32) -> u32 {
        match self.bus.periph.gpio.func_in_sel.get(signal as usize).copied() { Some(sel) if sel & 0x40 != 0 => sel & 0x1f, _ => 255 }
    }

    fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) { self.bus.periph.uart_pins(port) }
    fn uart_baud(&self, port: u8) -> Option<u32> { self.bus.periph.uart_baud(port) }

    fn uart_in(&mut self, port: u32, data: &[u8]) -> Option<u32> {
        match port {
            p if p < C3_UARTS => {
                let n = data.len().min(self.bus.periph.uart[p as usize].rx_free());
                if n > 0 { self.bus.uart_input(p as usize, &data[..n]); }
                Some(n as u32)
            }
            crate::jumperly::USB_PORT => { self.bus.serial_input(data); Some(data.len() as u32) }
            _ => None,
        }
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

    /// GPIO0..4 = ADC1 CH0..4, GPIO5 = ADC2 CH0.
    fn adc_set(&mut self, gpio: u32, mv: u32) -> bool {
        if !(0..=5).contains(&gpio) { return false; }
        if self.bus.pin_watch.is_none() { return false; }
        self.bus.periph.adc.analog.set(gpio as u8, esp_periph::AnalogSource::Const(mv as f32 / 1000.0));
        true
    }

    fn reset(&mut self) { crate::machine::reboot(self); }

    /// One I2C controller and SPI2.
    fn bus_host(&mut self, i2c: bool, spi: bool) {
        self.bus.i2c_host = i2c;
        let mut finished = self.bus.periph.i2c.set_defer(i2c);
        self.bus.spi_host = spi;
        if !spi {
            if let Some(t) = self.bus.periph.spi2.host_wait.take() { self.bus.periph.spi2.finish_transfer(t, &[]); finished = true; }
        }
        if finished { self.bus.irq_dirty = true; }
    }

    fn bus_service(&mut self) -> u32 {
        let mut n = 0;
        let mut host = HostI2c { bus: 0 };
        if self.bus.periph.i2c.service(&mut host) { n += 1; }
        if let Some(t) = self.bus.periph.spi2.host_wait.take() {
            let rx = host_transfer(2, &t.tx, t.rx_len);
            self.bus.periph.spi2.finish_transfer(t, &rx);
            n += 1;
        }
        if n > 0 { self.bus.irq_dirty = true; }
        n
    }

    fn flush_ticks(&mut self) -> u64 {
        esp_soc::SocBus::flush_ticks(&mut self.bus);
        self.bus.cycles
    }

    fn wifi_link(&mut self) -> &mut esp_soc::wifi::StationLink { &mut self.bus.periph.wifi.link }

    fn set_mac(&mut self, mac: [u8; 6]) -> bool {
        self.mac = mac;
        write_efuse_mac(&mut self.bus.periph.efuse, mac);
        true
    }

    fn ble(&mut self) -> &mut esp_soc::ble::vhci::Ble { &mut self.bus.ble }
    fn ble_command(&mut self, command: &str) -> Result<(), String> { self.bus.ble_command(command) }
    fn enable_ble(&mut self, elf: &esp_soc::elf::Elf) -> Result<(), String> { self.bus.enable_ble(elf) }
}
