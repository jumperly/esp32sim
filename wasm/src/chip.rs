//! Jumperly patch J28: one chip interface behind the Jumperly ABI. The exports of `jumperly.rs` and
//! `net_host.rs` dispatch once (`MachineKind::chip_mut`) and call `JumperlyChip`; `chips/*.rs`
//! implement it for each chip with a host ABI. Also the host imports of the bus bridges (J6/J7/J18).

/// One PWM output change as the host sees it (the S3, C3 and classic `PwmOut` have these fields).
pub(crate) struct PwmEvent { pub cycle: u64, pub signal: u32, pub enabled: bool, pub idle_high: bool, pub freq: f64, pub duty: f64 }

/// Jumperly patch J28: what every chip with a host ABI provides; `wasm/src/chips/*.rs` implement it.
/// Methods a chip lacks keep the defaults (`None` / `false` / 0); the export maps them to the same
/// return code and log line as for a chip without the ABI.
pub(crate) trait JumperlyChip {
    /// The pins `esp32sim_pin_state` / `esp32sim_gpio_set_input` take (GPIO0..pins-1).
    fn pins(&self) -> u32;
    /// The UART console ports (0..console_ports-1); the USB-Serial/JTAG port is 3 after them.
    fn console_ports(&self) -> u32;
    /// The CPU clock the BLE controller's time is counted in.
    fn cpu_hz(&self) -> u64;
    /// The machine's console (`Console::raw` holds the bytes per port in Jumperly mode).
    fn booted_console(&mut self) -> &mut esp_soc::Console;
    /// Switch to Jumperly mode (no JSON outbox, raw console bytes, every pin watched, RMT and PWM
    /// taps, the linear ADC). The export checks that the chip has not booted.
    fn jumperly_mode(&mut self);
    /// Watch only the pins in `mask`; false outside Jumperly mode.
    fn set_pin_watch(&mut self, mask: u64) -> bool;
    /// The state word of `pin` (< `pins()`).
    fn pin_state(&self, pin: u32) -> u32;
    /// The pin changes since the last call (cycle, pin, state); empty outside Jumperly mode.
    fn take_pin_events(&mut self) -> Vec<(u64, u8, u32)>;
    /// The level the circuit applies to `pin`'s input (`pin` < `pins()`).
    fn gpio_set_input(&mut self, pin: u8, level: bool);
    /// The pin matrix input `signal` reads, 255 if none.
    fn gpio_in_pin(&self, signal: u32) -> u32;
    /// UART `port`'s pads (tx, rx).
    fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>);
    /// UART `port`'s baud, None when not configured or no such port.
    fn uart_baud(&self, port: u8) -> Option<u32>;
    /// Bytes from the host into console `port`'s receive side: the number taken, None for a port
    /// the chip does not have (a UART takes what fits in its receive FIFO).
    fn uart_in(&mut self, port: u32, data: &[u8]) -> Option<u32>;
    /// The RMT frames since the last call (cycle, pin, bits); empty outside Jumperly mode.
    fn take_rmt(&mut self) -> Vec<(u64, u8, Vec<bool>)>;
    /// The PWM output changes since the last call; empty outside Jumperly mode.
    fn take_pwm(&mut self) -> Vec<PwmEvent>;
    /// The pad voltage of ADC pin `gpio`; false for another pin or outside Jumperly mode.
    fn adc_set(&mut self, gpio: u32, mv: u32) -> bool;
    /// Restart the chip from the host (RESET / EN), like a guest software reset.
    fn reset(&mut self);
    /// Turn the I2C / SPI host bridges on or off, finishing what still waits without the host.
    fn bus_host(&mut self, i2c: bool, spi: bool);
    /// Run the waiting bus transactions through the host imports: how many ran.
    fn bus_service(&mut self) -> u32;
    /// I2S controller `port` for the host's receive side (J17), None when the chip has no host I2S.
    fn i2s(&mut self, port: u32) -> Option<&mut esp_periph::I2s> { let _ = port; None }
    /// Elapsed cycles the device models have not seen yet.
    fn pending_cycles(&self) -> u64 { 0 }
    /// Deliver the bus's deferred device time; the cycle count after it.
    fn flush_ticks(&mut self) -> u64;
    /// The station's Wi-Fi link (no deferred time delivered: see `flush_ticks`).
    fn wifi_link(&mut self) -> &mut esp_soc::wifi::StationLink;
    /// The station's MAC (efuse and machine); false when the chip cannot take one.
    fn set_mac(&mut self, mac: [u8; 6]) -> bool { let _ = mac; false }
    /// The virtual BLE controller.
    fn ble(&mut self) -> &mut esp_soc::ble::vhci::Ble;
    /// A command for the virtual BLE peer.
    fn ble_command(&mut self, command: &str) -> Result<(), String>;
    /// Enable the virtual BLE controller from the application ELF.
    fn enable_ble(&mut self, elf: &esp_soc::elf::Elf) -> Result<(), String>;
    /// Octal SPI flash (J16) when `on`; false when the chip has no octal flash.
    fn flash_octal(&mut self, on: bool) -> bool { let _ = on; false }
    /// The DAC output changes since the last call: (GPIO, mV) for an enabled output, (GPIO,
    /// `u32::MAX`) once when it turns off; empty for a chip without a DAC.
    fn take_dac(&mut self) -> Vec<(u8, u32)> { Vec::new() }
    /// Whether the finger is on touch pad `gpio`; false for another pin or a chip without touch pads.
    fn touch_set(&mut self, gpio: u32, touched: bool) -> bool { let _ = (gpio, touched); false }
}

/// The station's MAC in efuse BLK1 words 0 and 1, as `Efuse::new` lays them out; the rest of
/// word 1 is the chip's (Jumperly patch J14; the S3 and the C3 share the efuse model).
pub(crate) fn write_efuse_mac(efuse: &mut esp_periph::Efuse, mac: [u8; 6]) {
    efuse.write(0x44, u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]));
    let high = efuse.read(0x48) & !0xffff;
    efuse.write(0x48, high | (u32::from(mac[0]) << 8) | u32::from(mac[1]));
}

// ------------------------------------------------------------------ Jumperly patch J6/J7/J18: bus bridges

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    /// Jumperly patch J6: op 0 start (arg = addr << 1 | read) → ACK, 1 write (arg = byte) → ACK, 2 read → byte, 3 stop.
    fn host_i2c(bus: u32, op: u32, arg: u32) -> u32;
    /// Jumperly patch J7 / J18: one full-duplex transfer of GP-SPI controller `host` (2 = SPI2 / FSPI,
    /// 3 = SPI3 / HSPI); the host writes `rx_len` MISO bytes to `rx`. Replaces J7's `host_spi`
    /// (the same call without `host`, SPI2 only).
    fn host_spi_bus(host: u32, tx: *const u8, tx_len: usize, rx: *mut u8, rx_len: usize);
}

#[cfg(not(target_arch = "wasm32"))]
type NativeI2c = std::cell::RefCell<Option<Box<dyn FnMut(u32, u32, u32) -> u32>>>;
#[cfg(not(target_arch = "wasm32"))]
type NativeSpi = std::cell::RefCell<Option<Box<dyn FnMut(u32, &[u8], &mut [u8])>>>;
#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static NATIVE_I2C: NativeI2c = std::cell::RefCell::new(None);
    static NATIVE_SPI: NativeSpi = std::cell::RefCell::new(None);
}
/// Native builds (tests): answer the bus bridges with closures; WASM uses the host imports. The SPI
/// closure gets the controller (2 or 3) first, like `host_spi_bus`.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_native_bus_host(i2c: impl FnMut(u32, u32, u32) -> u32 + 'static, spi: impl FnMut(u32, &[u8], &mut [u8]) + 'static) {
    NATIVE_I2C.with(|h| *h.borrow_mut() = Some(Box::new(i2c)));
    NATIVE_SPI.with(|h| *h.borrow_mut() = Some(Box::new(spi)));
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_i2c(bus: u32, op: u32, arg: u32) -> u32 {
    NATIVE_I2C.with(|h| h.borrow_mut().as_mut().map_or(0, |f| f(bus, op, arg)))
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_spi_bus(host: u32, tx: *const u8, tx_len: usize, rx: *mut u8, rx_len: usize) {
    // SAFETY: the caller passes the buffers of a live transfer for the duration of this call.
    let (tx, rx) = unsafe { (std::slice::from_raw_parts(tx, tx_len), std::slice::from_raw_parts_mut(rx, rx_len)) };
    NATIVE_SPI.with(|h| if let Some(f) = h.borrow_mut().as_mut() { f(host, tx, rx) });
}

/// Hand one waiting GP-SPI transfer of controller `host` (MOSI bytes `tx`, `rx_len` MISO bytes) to
/// the host and return the MISO bytes.
pub(crate) fn host_transfer(host: u32, tx: &[u8], rx_len: usize) -> Vec<u8> {
    let mut rx = vec![0xffu8; rx_len];
    // SAFETY: both buffers live for the call; the host only writes `rx_len` bytes to `rx`.
    unsafe { host_spi_bus(host, tx.as_ptr(), tx.len(), rx.as_mut_ptr(), rx.len()) };
    rx
}

/// I2C controller `bus`'s transactions for addresses no attached device answers, through `host_i2c`.
pub(crate) struct HostI2c { pub bus: u32 }
impl esp_periph::i2c::I2cHost for HostI2c {
    fn start(&mut self, addr: u8, read: bool) -> bool { unsafe { host_i2c(self.bus, 0, (u32::from(addr) << 1) | u32::from(read)) != 0 } }
    fn write(&mut self, b: u8) -> bool { unsafe { host_i2c(self.bus, 1, u32::from(b)) != 0 } }
    fn read(&mut self) -> u8 { unsafe { host_i2c(self.bus, 2, 0) as u8 } }
    fn stop(&mut self) { unsafe { host_i2c(self.bus, 3, 0); } }
}

#[cfg(test)]
mod tests {
    use crate::{esp32sim_delete, esp32sim_new, Emu};

    fn new(board: &str) -> *mut Emu {
        // SAFETY: the board name is readable for the call.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null(), "{board}");
        e
    }

    /// Jumperly patch J28: the chips with a host ABI and what each has; the defaults hold for what a
    /// chip lacks (the C3: no host I2S, no octal flash, no DAC or touch; the classic: no octal flash).
    #[test]
    fn chips_report_their_shape_and_keep_the_defaults_for_what_they_lack() {
        let s3 = new("none");
        // SAFETY: the test owns the live emulator until it deletes it.
        let c = unsafe { &mut *s3 }.m.chip_mut().expect("the S3 has a host ABI");
        assert_eq!((c.pins(), c.console_ports(), c.cpu_hz()), (49, 3, 240_000_000));
        assert!(c.i2s(0).is_some() && c.i2s(1).is_some() && c.i2s(2).is_none());
        assert!(c.take_pin_events().is_empty() && c.take_rmt().is_empty() && c.take_pwm().is_empty());
        assert!(!c.set_pin_watch(1), "outside Jumperly mode");
        assert!(!c.adc_set(1, 1000), "outside Jumperly mode");
        unsafe { esp32sim_delete(s3) };

        let c3 = new("esp32c3");
        // SAFETY: the test owns the live emulator until it deletes it.
        let c = unsafe { &mut *c3 }.m.chip_mut().expect("the C3 has a host ABI");
        assert_eq!((c.pins(), c.console_ports(), c.cpu_hz()), (22, 2, 160_000_000));
        assert!(c.i2s(0).is_none());
        assert_eq!(c.pending_cycles(), 0);
        assert!(!c.flash_octal(true));
        assert_eq!(c.uart_in(2, b"x"), None, "the C3 has two UARTs");
        assert_eq!(c.gpio_in_pin(9999), 255);
        assert!(!c.touch_set(4, true) && c.take_dac().is_empty(), "no touch pads, no DAC");
        unsafe { esp32sim_delete(c3) };

        let classic = new("esp32");
        // SAFETY: the test owns the live emulator until it deletes it.
        let c = unsafe { &mut *classic }.m.chip_mut().expect("the classic ESP32 has a host ABI");
        assert_eq!((c.pins(), c.console_ports(), c.cpu_hz()), (40, 3, 240_000_000));
        assert!(c.i2s(0).is_some() && c.i2s(1).is_some() && c.i2s(2).is_none());
        assert!(!c.flash_octal(true));
        assert_eq!(c.uart_in(3, b"x"), None, "the classic has no USB-Serial/JTAG");
        assert!(c.touch_set(4, true) && !c.touch_set(5, true));
        assert!(c.take_dac().is_empty());
        unsafe { esp32sim_delete(classic) };

        let c6 = new("esp32c6");
        // SAFETY: the test owns the live emulator until it deletes it.
        assert!(unsafe { &mut *c6 }.m.chip_mut().is_none(), "the C6 has no host ABI");
        unsafe { esp32sim_delete(c6) };
    }
}
