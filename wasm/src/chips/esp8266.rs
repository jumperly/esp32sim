//! Jumperly patch J29: the ESP8266 (ESP-12F) behind the Jumperly ABI: UART0 and the transmit-only
//! UART1, GPIO0..16 and the TOUT pad as pin 17, PWM measured from the software waveform
//! (`esp8266::pwm`), I2C decoded from the software master on the circuit's pin pairs
//! (`esp8266::soft_i2c`, `esp32sim_soft_i2c`), the HSPI bridge as host 1. No radio is modelled:
//! no Wi-Fi link, no BLE (the trait's `None`), the host log says so when the sketch starts Wi-Fi.
use crate::chip::{host_transfer, HostI2c, JumperlyChip, PwmEvent};
use ::esp8266::jumperly::{pin_state, PinWatch, ADC_PIN, PINS};
use esp_periph::i2c::I2cHost;

/// UART0 and UART1 (UART1 has no receive pin on the module).
const UARTS: u32 = 2;
const ALL_PINS: u64 = (1u64 << PINS) - 1;
/// The `host_spi_bus` controller number of HSPI (SPI1).
const HSPI_HOST: u32 = 1;

/// The soft I2C decoders' targets: `host_i2c` with the pair as the bus (`sda | scl << 8`).
fn soft_i2c_host() -> ::esp8266::soft_i2c::HostFn {
    Box::new(|bus, op, arg| {
        let mut h = HostI2c { bus };
        match op {
            0 => u32::from(h.start((arg >> 1) as u8, arg & 1 != 0)),
            1 => u32::from(h.write(arg as u8)),
            2 => u32::from(h.read()),
            _ => { h.stop(); 0 }
        }
    })
}

impl JumperlyChip for ::esp8266::Machine {
    fn pins(&self) -> u32 { PINS as u32 }
    fn console_ports(&self) -> u32 { UARTS }
    /// The clock the cycle counts are in: the 80 MHz APB clock, also with CPU2X (the bus counts
    /// device time in APB cycles; a CPU2X core runs two CPU cycles per one).
    fn cpu_hz(&self) -> u64 { ::esp8266::periph::CPU_HZ }
    fn booted_console(&mut self) -> &mut esp_soc::Console { &mut self.console }

    /// Every pin watched, the PWM detector on, the TOUT pad at 0 V; the ROM model logs an
    /// unknown ROM entry once instead of stopping (not strict).
    fn jumperly_mode(&mut self) {
        self.web = None;
        self.console.raw = Some(Default::default());
        self.bus.pin_watch = Some(PinWatch::new(&self.bus.periph, ALL_PINS));
        self.bus.pwm_tap = Some(Default::default());
        self.bus.jumperly.last_out = self.bus.periph.gpio.output();
        self.bus.set_rom_strict(false);
        self.bus.adc_set_mv(0);
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

    /// GPIO0..16; a floating pad takes its pull (GPIO0/2 the ROM's pull-up, GPIO15 none, GPIO16
    /// its pull-down), so the strapping at reset reads the circuit. The TOUT pad (17) is `adc_set`'s.
    fn gpio_set_input(&mut self, pin: u8, level: Option<bool>) {
        if u32::from(pin) < ADC_PIN { self.bus.circuit_input(pin, level); }
    }

    /// No input matrix: the pads' functions are fixed (IO_MUX).
    fn gpio_in_pin(&self, _signal: u32) -> u32 { 255 }

    fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) { self.bus.uart_pins(port) }
    fn uart_baud(&self, port: u8) -> Option<u32> { self.bus.uart_baud(port) }

    /// UART0 takes what fits in its FIFO; UART1 has no receiver on the module and takes nothing.
    fn uart_in(&mut self, port: u32, data: &[u8]) -> Option<u32> {
        match port {
            0 => {
                let n = data.len().min(self.bus.periph.uart[0].rx_free());
                if n > 0 { esp_soc::SocBus::uart_input(&mut self.bus, 0, &data[..n]); }
                Some(n as u32)
            }
            1 => Some(0),
            _ => None,
        }
    }

    fn take_rmt(&mut self) -> Vec<(u64, u8, Vec<bool>)> { Vec::new() }

    fn take_pwm(&mut self) -> Vec<PwmEvent> {
        self.bus.take_pwm().into_iter()
            .map(|(cycle, o)| PwmEvent { cycle, signal: o.signal, enabled: o.enabled, idle_high: o.idle_high, freq: o.freq, duty: o.duty })
            .collect()
    }

    /// The TOUT pad (pin 17): 0..1000 mV reads 0..1023.
    fn adc_set(&mut self, gpio: u32, mv: u32) -> bool {
        if gpio != ADC_PIN || self.bus.pin_watch.is_none() { return false; }
        self.bus.adc_set_mv(mv);
        true
    }

    /// The host's reset is the RST pad: ROM cause 2 (EXT_RESET), RTC memory kept; in deep sleep
    /// it is the wake-up (cause 5), as GPIO16 wired to RST gives.
    fn reset(&mut self) {
        self.bus.chip_reset(::esp8266::rom::boot::RST_EXT);
        crate::machine::reboot(self);
    }

    /// The ROM's causes from outside the chip: 1 power-on (the supply switched on: RTC memory,
    /// RAM and the RTC registers cleared, the strapping latched again; the SDK reports "Power
    /// On") and 2 the RST pad, as `reset` (RTC memory kept; "External System", or the wake-up in
    /// deep sleep). The others are the chip's own doing.
    fn reset_with_cause(&mut self, cause: u32) -> bool {
        use ::esp8266::rom::boot::{RST_EXT, RST_POWERON};
        if cause != RST_POWERON && cause != RST_EXT { return false; }
        self.bus.chip_reset(cause);
        crate::machine::reboot(self);
        true
    }

    /// I2C: the soft I2C decoders run on the registered pairs; SPI: HSPI waits for the host. A
    /// transfer still waiting when the SPI bridge goes off reads 0xFF.
    fn bus_host(&mut self, i2c: bool, spi: bool) {
        self.bus.set_soft_i2c_host(i2c.then(soft_i2c_host));
        self.bus.spi_host = spi;
        if !spi && self.bus.periph.spi1.host_wait.is_some() { self.bus.finish_spi_transfer(&[]); }
        self.bus.irq_dirty = true;
    }

    /// The waiting HSPI transfer through `host_spi_bus` (host 1); the soft I2C is served during
    /// the slice, so this counts HSPI transfers only.
    fn bus_service(&mut self) -> u32 {
        let Some(t) = self.bus.periph.spi1.host_wait.as_ref() else { return 0 };
        let rx = host_transfer(HSPI_HOST, &t.tx, t.rx_len);
        self.bus.finish_spi_transfer(&rx);
        self.bus.irq_dirty = true;
        1
    }

    fn flush_ticks(&mut self) -> u64 {
        esp_soc::SocBus::flush_ticks(&mut self.bus);
        self.bus.cycles
    }

    fn wifi_link(&mut self) -> Option<&mut esp_soc::wifi::StationLink> { None }

    /// The OTP words (DPORT 0x50..0x5C) carry the MAC with its CRC (`periph::otp_words`); a chip
    /// reset rebuilds them from the machine's MAC.
    fn set_mac(&mut self, mac: [u8; 6]) -> bool {
        self.mac = mac;
        self.bus.periph.dport.otp = ::esp8266::periph::otp_words(mac);
        true
    }

    fn ble(&mut self) -> Option<&mut esp_soc::ble::vhci::Ble> { None }

    /// The ROM model's `[emu] rom: ...` lines (an unmodelled ROM entry, a ROM stop) and the
    /// Jumperly notes (Wi-Fi, deep sleep).
    fn take_notes(&mut self) -> Vec<String> { self.bus.take_notes() }

    /// The modelled ROM stopped the chip (`esp32sim_run` code 8).
    fn halted(&self) -> bool { self.bus.rom_halted() }

    fn soft_i2c(&mut self, sda: u8, scl: u8, on: bool) -> bool { self.bus.set_soft_i2c(sda, scl, on) }
}
