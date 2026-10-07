//! Jumperly patch J3: the ABI `@jumperly/mcu-esp32` drives — pin states with cycles, GPIO inputs,
//! raw console bytes per port, RMT frames. ESP32-S3 and (patch J9) ESP32-C3; PWM and the bus
//! bridges are S3 only, the ADC is both. Variable-size results go to `Emu::buf`, read through `esp32sim_buf_ptr` until
//! the next `*_take` call.
use super::{bytes, log, Emu};
use esp_soc::SocBus;
use esp32s3::jumperly::{pin_state, PinWatch, PINS};

/// USB-Serial/JTAG as a fourth console "port" after UART0..2.
const USB_PORT: u32 = 3;
const ALL_PINS: u64 = (1u64 << PINS) - 1;
/// Jumperly patch J9: the C3 has two UARTs; its USB-Serial/JTAG keeps the port number 3.
const C3_UARTS: u32 = 2;
const C3_ALL_PINS: u64 = (1u64 << esp32c3::jumperly::PINS) - 1;

/// Switch an S3 emulator to Jumperly mode before boot: no JSON outbox, raw console bytes, every pin
/// watched, RMT frames recorded. Returns 1 for other chips or after boot. (J9: the C3 gets the same
/// minus PWM, plus the linear ADC.)
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jumperly_mode(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted { return 1; }
    if let Some(m) = e.m.c3_mut() {
        m.web = None;
        m.console.raw = Some(Default::default());
        m.bus.pin_watch = Some(esp32c3::jumperly::PinWatch::new(&m.bus.periph, C3_ALL_PINS));
        m.bus.rmt_tap = Some(Vec::new());
        m.bus.pwm_tap = Some(Default::default());   // Jumperly patch J4
        m.bus.periph.adc.adc_linear = true;   // Jumperly patch J5
        return 0;
    }
    let Some(m) = e.m.s3_mut() else { log("[emu] jumperly mode: ESP32-S3 and ESP32-C3 only"); return 1; };
    m.web = None;
    m.console.raw = Some(Default::default());
    m.bus.pin_watch = Some(PinWatch::new(&m.bus.periph, ALL_PINS));
    m.bus.rmt_tap = Some(Vec::new());
    m.bus.pwm_tap = Some(Default::default());
    m.bus.periph.rtc.adc_linear = true;   // Jumperly patch J5
    0
}

/// Set the voltage (millivolts) the circuit puts on an ADC pad. On the S3, GPIO1..10 = ADC1
/// CH0..9 and GPIO11..20 = ADC2 CH0..9. On the C3, GPIO0..4 = ADC1 CH0..4 and GPIO5 = ADC2 CH0.
/// Returns 1 for other pins or outside Jumperly mode.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_adc_set(e: *mut Emu, gpio: u32, mv: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Some(m) = e.m.c3_mut() {
        if !(0..=5).contains(&gpio) { return 1; }
        if m.bus.pin_watch.is_none() { return 1; }
        m.bus.periph.adc.analog.set(gpio as u8, esp_periph::AnalogSource::Const(mv as f32 / 1000.0));
        return 0;
    }
    if !(1..=20).contains(&gpio) { return 1; }
    let Some(m) = e.m.s3_mut() else { return 1 };
    if m.bus.pin_watch.is_none() { return 1; }
    // Upstream's SENS one-shot ADC reads the pad through `AnalogInputs`; J5 only picks the linear transfer.
    m.bus.periph.rtc.analog.set(gpio as u8, esp_periph::AnalogSource::Const(mv as f32 / 1000.0));
    0
}

/// Watch only the pins in `lo | hi << 32` (default: all 49).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_watch(e: *mut Emu, lo: u32, hi: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let mask = u64::from(lo) | (u64::from(hi) << 32);
    if let Some(m) = e.m.c3_mut() {
        let Some(w) = &mut m.bus.pin_watch else { return 1 };
        w.mask = mask & C3_ALL_PINS;
        return 0;
    }
    let Some(m) = e.m.s3_mut() else { return 1 };
    let Some(w) = &mut m.bus.pin_watch else { return 1 };
    w.mask = mask & ALL_PINS;
    0
}

/// The current state word of `pin` (see `esp32s3::jumperly::state`), 0 for an invalid pin.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_state(e: *mut Emu, pin: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Some(m) = e.m.c3_mut() {
        return if (pin as usize) < esp32c3::jumperly::PINS { esp32c3::jumperly::pin_state(&m.bus.periph, pin as usize) } else { 0 };
    }
    match e.m.s3_mut() {
        Some(m) if (pin as usize) < PINS => pin_state(&m.bus.periph, pin as usize),
        _ => 0,
    }
}

/// Take the pin changes since the last call: count; the buffer holds 16 bytes per change
/// (f64 cycle, u32 pin, u32 state, little-endian).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_events_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let events = if let Some(m) = e.m.c3_mut() {
        match m.bus.pin_watch.as_mut() { Some(w) => std::mem::take(&mut w.events), None => return 0 }
    } else {
        match e.m.s3_mut().and_then(|m| m.bus.pin_watch.as_mut()) { Some(w) => std::mem::take(&mut w.events), None => return 0 }
    };
    for (cycle, pin, state) in &events {
        e.buf.extend_from_slice(&(*cycle as f64).to_le_bytes());
        e.buf.extend_from_slice(&u32::from(*pin).to_le_bytes());
        e.buf.extend_from_slice(&state.to_le_bytes());
    }
    events.len() as u32
}

/// Set the level the circuit applies to `pin`'s input.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gpio_set_input(e: *mut Emu, pin: u32, level: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Some(m) = e.m.c3_mut() {
        if pin as usize >= esp32c3::jumperly::PINS { return 1; }
        m.bus.gpio_set_input(pin as u8, level != 0);
        return 0;
    }
    let Some(m) = e.m.s3_mut() else { return 1 };
    if pin as usize >= PINS { return 1; }
    m.bus.gpio_set_input(pin as u8, level != 0);
    0
}

/// Take the bytes the guest wrote to console `port` (0..2 UART, 3 USB-Serial/JTAG): length; bytes in the buffer.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_uart_take(e: *mut Emu, port: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let uarts = if e.m.c3_mut().is_some() { C3_UARTS } else { 3 };
    let index = match port { p if p < uarts => port as usize + 1, USB_PORT => 0, _ => return 0 };
    let data = match e.m.console_mut().and_then(|c| c.raw.as_mut()) {
        Some(raw) => std::mem::take(&mut raw[index]),
        None => return 0,
    };
    e.buf.extend_from_slice(&data);
    data.len() as u32
}

/// Bytes from the host into console `port`'s receive side (0..2 UART FIFO, 3 USB-Serial/JTAG):
/// the number of leading bytes accepted. A UART takes what fits in its 128-byte receive FIFO and
/// never overflows it — the host keeps the rest and offers it again after the guest has read; the
/// USB-Serial/JTAG port queues everything. An invalid port (or a chip other than the S3) takes 0.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero `len`,
/// `ptr` must be non-null and readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_uart_in(e: *mut Emu, port: u32, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable input buffer for this call.
    let data = unsafe { bytes(ptr, len) };
    if let Some(m) = e.m.c3_mut() {
        return match port {
            p if p < C3_UARTS => {
                let n = data.len().min(m.bus.periph.uart[p as usize].rx_free());
                if n > 0 { m.bus.uart_input(p as usize, &data[..n]); }
                n as u32
            }
            USB_PORT => { m.bus.serial_input(data); data.len() as u32 }
            _ => { log(&format!("[emu] uart in: no console port {port}")); 0 }
        };
    }
    let Some(m) = e.m.s3_mut() else { log("[emu] uart in: ESP32-S3 and ESP32-C3 only"); return 0 };
    match port {
        0..=2 => {
            let n = data.len().min(m.bus.periph.uart[port as usize].rx_free());
            if n > 0 { m.bus.uart_input(port as usize, &data[..n]); }
            n as u32
        }
        USB_PORT => { m.bus.serial_input(data); data.len() as u32 }
        _ => { log(&format!("[emu] uart in: no console port {port}")); 0 }
    }
}

/// Take the RMT frames since the last call: count; per frame the buffer holds f64 cycle, u32 pin
/// (255 = not routed), u32 bit count, then the bits MSB-first padded to a multiple of 4 bytes.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_rmt_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let frames = if let Some(m) = e.m.c3_mut() {
        match m.bus.rmt_tap.as_mut() { Some(tap) => std::mem::take(tap), None => return 0 }
    } else {
        match e.m.s3_mut().and_then(|m| m.bus.rmt_tap.as_mut()) { Some(tap) => std::mem::take(tap), None => return 0 }
    };
    for (cycle, pin, bits) in &frames {
        e.buf.extend_from_slice(&(*cycle as f64).to_le_bytes());
        e.buf.extend_from_slice(&u32::from(*pin).to_le_bytes());
        e.buf.extend_from_slice(&(bits.len() as u32).to_le_bytes());
        let mut packed = vec![0u8; bits.len().div_ceil(8).next_multiple_of(4)];
        for (i, &bit) in bits.iter().enumerate() { if bit { packed[i / 8] |= 0x80 >> (i % 8); } }
        e.buf.extend_from_slice(&packed);
    }
    frames.len() as u32
}

/// Take the PWM output changes since the last call: count; 32 bytes each in the buffer
/// (f64 cycle, u32 matrix signal, u32 flags: bit 0 enabled, bit 1 idle high, f64 Hz, f64 duty 0..1).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pwm_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    fn push(buf: &mut Vec<u8>, cycle: u64, signal: u32, enabled: bool, idle_high: bool, freq: f64, duty: f64) {
        buf.extend_from_slice(&(cycle as f64).to_le_bytes());
        buf.extend_from_slice(&signal.to_le_bytes());
        buf.extend_from_slice(&(u32::from(enabled) | (u32::from(idle_high) << 1)).to_le_bytes());
        buf.extend_from_slice(&freq.to_le_bytes());
        buf.extend_from_slice(&duty.to_le_bytes());
    }
    if let Some(m) = e.m.c3_mut() {
        let Some(tap) = m.bus.pwm_tap.as_mut() else { return 0 };
        let outs = std::mem::take(&mut tap.events);
        for (cycle, o) in &outs { push(&mut e.buf, *cycle, o.signal, o.enabled, o.idle_high, o.freq, o.duty); }
        return outs.len() as u32;
    }
    let Some(tap) = e.m.s3_mut().and_then(|m| m.bus.pwm_tap.as_mut()) else { return 0 };
    let outs = std::mem::take(&mut tap.events);
    for (cycle, o) in &outs { push(&mut e.buf, *cycle, o.signal, o.enabled, o.idle_high, o.freq, o.duty); }
    outs.len() as u32
}

/// Jumperly patch J8: restart the chip from the host (the board's RESET button / EN pin) the way a
/// guest software reset does — same reboot, same `chip reset` log line; the cycle count keeps
/// going. Returns 1 for other chips or before boot. Call it between runs.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_reset(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if !e.booted { return 1; }
    if let Some(m) = e.m.c3_mut() { crate::machine::reboot(m); return 0; }
    let Some(m) = e.m.s3_mut() else { log("[emu] reset: ESP32-S3 and ESP32-C3 only"); return 1 };
    crate::machine::reboot(m);
    0
}

/// Start of the result buffer of the last `*_take` call.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_buf_ptr(e: *mut Emu) -> *const u8 {
    // SAFETY: the caller provides shared access to a live emulator.
    unsafe { &*e }.buf.as_ptr()
}

/// Jumperly patch J13: the length of the result buffer of the last `*_take` call.
///
/// # Safety
/// `e` must point to a live emulator, and no mutable access may overlap this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_buf_len(e: *mut Emu) -> usize {
    // SAFETY: the caller provides shared access to a live emulator.
    unsafe { &*e }.buf.len()
}

// ------------------------------------------------------------------ Jumperly patch J6/J7: bus bridges

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    /// Jumperly patch J6: op 0 start (arg = addr << 1 | read) → ACK, 1 write (arg = byte) → ACK, 2 read → byte, 3 stop.
    fn host_i2c(bus: u32, op: u32, arg: u32) -> u32;
    /// Jumperly patch J7: one full-duplex SPI2 transfer; the host writes `rx_len` MISO bytes to `rx`.
    fn host_spi(tx: *const u8, tx_len: usize, rx: *mut u8, rx_len: usize);
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static NATIVE_I2C: std::cell::RefCell<Option<Box<dyn FnMut(u32, u32, u32) -> u32>>> = std::cell::RefCell::new(None);
    static NATIVE_SPI: std::cell::RefCell<Option<Box<dyn FnMut(&[u8], &mut [u8])>>> = std::cell::RefCell::new(None);
}
/// Native builds (tests): answer the bus bridges with closures; WASM uses the host imports.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_native_bus_host(i2c: impl FnMut(u32, u32, u32) -> u32 + 'static, spi: impl FnMut(&[u8], &mut [u8]) + 'static) {
    NATIVE_I2C.with(|h| *h.borrow_mut() = Some(Box::new(i2c)));
    NATIVE_SPI.with(|h| *h.borrow_mut() = Some(Box::new(spi)));
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_i2c(bus: u32, op: u32, arg: u32) -> u32 {
    NATIVE_I2C.with(|h| h.borrow_mut().as_mut().map_or(0, |f| f(bus, op, arg)))
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_spi(tx: *const u8, tx_len: usize, rx: *mut u8, rx_len: usize) {
    // SAFETY: the caller passes the buffers of a live transfer for the duration of this call.
    let (tx, rx) = unsafe { (std::slice::from_raw_parts(tx, tx_len), std::slice::from_raw_parts_mut(rx, rx_len)) };
    NATIVE_SPI.with(|h| if let Some(f) = h.borrow_mut().as_mut() { f(tx, rx) });
}

struct HostI2c { bus: u32 }
impl esp32s3::i2c::I2cHost for HostI2c {
    fn start(&mut self, addr: u8, read: bool) -> bool { unsafe { host_i2c(self.bus, 0, (u32::from(addr) << 1) | u32::from(read)) != 0 } }
    fn write(&mut self, b: u8) -> bool { unsafe { host_i2c(self.bus, 1, u32::from(b)) != 0 } }
    fn read(&mut self) -> u8 { unsafe { host_i2c(self.bus, 2, 0) as u8 } }
    fn stop(&mut self) { unsafe { host_i2c(self.bus, 3, 0); } }
}

/// Turn the I2C / SPI2 host bridges on or off (they survive chip resets). Turning a bridge off
/// finishes what still waits for the host without it: an I2C transaction NACKs unknown addresses,
/// a SPI2 transfer reads 0xFF.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_bus_host(e: *mut Emu, i2c: u32, spi: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Some(m) = e.m.c3_mut() {
        m.bus.i2c_host = i2c != 0;
        let mut finished = m.bus.periph.i2c.set_defer(i2c != 0);
        m.bus.spi_host = spi != 0;
        if spi == 0 {
            if let Some(t) = m.bus.periph.spi2.host_wait.take() { m.bus.periph.spi2.finish_transfer(t, &[]); finished = true; }
        }
        if finished { m.bus.irq_dirty = true; }
        return 0;
    }
    let Some(m) = e.m.s3_mut() else { return 1 };
    m.bus.i2c_host = i2c != 0;
    let mut finished = false;
    for c in &mut m.bus.periph.i2c { finished |= c.set_defer(i2c != 0); }
    m.bus.spi_host = spi != 0;
    if spi == 0 {
        if let Some(t) = m.bus.periph.spi2.host_wait.take() { m.bus.periph.spi2.finish_transfer(t, &[]); finished = true; }
    }
    if finished { m.bus.irq_dirty = true; m.bus.refresh_tick_budget(); }
    0
}

/// Run the I2C / SPI2 transactions the guest started since the last call, through `host_i2c` /
/// `host_spi`, and raise their completion interrupts. Returns how many ran. Call it between runs,
/// never from inside a host import.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_bus_service(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if let Some(m) = e.m.c3_mut() {
        let mut n = 0;
        let mut host = HostI2c { bus: 0 };
        if m.bus.periph.i2c.service(&mut host) { n += 1; }
        if let Some(t) = m.bus.periph.spi2.host_wait.take() {
            let mut rx = vec![0xffu8; t.rx_len];
            // SAFETY: both buffers live for the call; the host only writes `rx_len` bytes to `rx`.
            unsafe { host_spi(t.tx.as_ptr(), t.tx.len(), rx.as_mut_ptr(), rx.len()) };
            m.bus.periph.spi2.finish_transfer(t, &rx);
            n += 1;
        }
        if n > 0 { m.bus.irq_dirty = true; }
        return n;
    }
    let Some(m) = e.m.s3_mut() else { return 0 };
    let mut n = 0;
    for bus in 0..2 {
        let mut host = HostI2c { bus: bus as u32 };
        if m.bus.periph.i2c[bus].service(&mut host) { n += 1; }
    }
    if let Some(t) = m.bus.periph.spi2.host_wait.take() {
        let mut rx = vec![0xffu8; t.rx_len];
        // SAFETY: both buffers live for the call; the host only writes `rx_len` bytes to `rx`.
        unsafe { host_spi(t.tx.as_ptr(), t.tx.len(), rx.as_mut_ptr(), rx.len()) };
        m.bus.periph.spi2.finish_transfer(t, &rx);
        n += 1;
    }
    if n > 0 { m.bus.irq_dirty = true; m.bus.refresh_tick_budget(); }
    n
}

/// The pin a matrix input signal reads (FSPIQ = 102 for MISO, I2CEXT0 SDA = 90 ...), 255 if none.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gpio_in_pin(e: *mut Emu, signal: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let sel = if let Some(m) = e.m.c3_mut() {
        m.bus.periph.gpio.func_in_sel.get(signal as usize).copied()
    } else if let Some(m) = e.m.s3_mut() {
        m.bus.periph.gpio.func_in_sel.get(signal as usize).copied()
    } else {
        None
    };
    match sel { Some(sel) if sel & 0x80 != 0 => sel & 0x3f, _ => 255 }
}

// ------------------------------------------------------------------ Jumperly patch J11: Wi-Fi

/// Jumperly patch J11: attach the virtual access point and its subnet like `esp32sim_wifi`, from
/// separate fields instead of a `key=value,...` spec, so the SSID and the passphrase may hold any
/// UTF-8 (`,` and `=` included). `psk_len` 0 is an open network; otherwise WPA2-PSK with 8..=63
/// bytes. The SSID takes 1..=32 bytes, the channel 1..=14. Returns 1 with the reason logged.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For each nonzero
/// length, its pointer must be non-null and readable for that many bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_ap(e: *mut Emu, ssid: *const u8, ssid_len: usize, psk: *const u8, psk_len: usize, channel: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides readable buffers for this call.
    let (ssid, psk) = unsafe { (bytes(ssid, ssid_len), bytes(psk, psk_len)) };
    let (Ok(ssid), Ok(psk)) = (std::str::from_utf8(ssid), std::str::from_utf8(psk)) else {
        log("[emu] wifi: the SSID or the passphrase is not UTF-8");
        return 1;
    };
    let channel = u8::try_from(channel).unwrap_or(0);
    let cfg = match esp_soc::wifi::ApConfig::from_fields(ssid, (!psk.is_empty()).then_some(psk), channel) {
        Ok(cfg) => cfg,
        Err(reason) => { log(&format!("[emu] wifi: {reason}")); return 1; }
    };
    log(&format!("[emu] virtual AP '{}' ({}, channel {}), subnet 10.0.2.0/24, no NAT in the browser", cfg.ssid, if cfg.psk.is_some() { "WPA2-PSK" } else { "open" }, cfg.channel));
    if let Err(reason) = e.m.bus().attach_wifi(cfg, None) { log(&format!("[emu] wifi: {reason}")); return 1; }
    e.net = None;   // Jumperly patch J13: a new network has no host transport until esp32sim_net_host
    0
}

/// Jumperly patch J11: the SSID of the last directed probe request for a network other than the
/// virtual access point's since the last call (the network the firmware looks for): its length,
/// 0 for none; the bytes are in the buffer (`esp32sim_buf_ptr`).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_probe_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let ssid = if let Some(m) = e.m.c3_mut() {
        m.bus.periph.wifi.link.take_probed_ssid()
    } else if let Some(m) = e.m.s3_mut() {
        m.bus.periph.wifi.link.take_probed_ssid()
    } else {
        None
    };
    let Some(ssid) = ssid else { return 0 };
    e.buf.extend_from_slice(&ssid);
    ssid.len() as u32
}

#[cfg(test)]
mod bus_host_tests {
    use super::*;
    use crate::{esp32sim_delete, esp32sim_new};

    /// Jumperly patch J6: a transaction deferred when the bridge goes off runs without the host (NACK).
    #[test]
    fn turning_the_i2c_bridge_off_finishes_a_deferred_transaction() {
        let board = "none";
        // SAFETY: the board name is readable for the call; the emulator is ours until deleted.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null());
        assert_eq!(unsafe { esp32sim_bus_host(e, 1, 0) }, 0);
        fn i2c<'a>(e: *mut Emu) -> &'a mut esp32s3::i2c::I2c {
            // SAFETY: the test owns the live emulator.
            &mut unsafe { &mut *e }.m.s3_mut().unwrap().bus.periph.i2c[0]
        }
        let c = i2c(e);
        c.write(0x58, 6 << 11);                                          // RSTART
        c.write(0x5c, (1 << 11) | (1 << 8) | 1);                         // WRITE 1 byte, ACK check
        c.write(0x60, 2 << 11);                                          // STOP
        c.write(0x1c, 0x50 << 1);
        c.write(0x04, 1 << 5);                                           // TRANS_START
        assert!(i2c(e).deferred);
        assert_eq!(unsafe { esp32sim_bus_host(e, 0, 0) }, 0);
        let c = i2c(e);
        assert!(!c.deferred);
        assert_ne!(c.int_raw & esp32s3::i2c::INT_NACK, 0);
        assert_eq!(unsafe { esp32sim_bus_service(e) }, 0);              // nothing left for the host
        unsafe { esp32sim_delete(e) };
    }
}
