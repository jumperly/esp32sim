//! Jumperly patch J3: the ABI `@jumperly/mcu-esp32` drives — pin states with cycles, GPIO inputs,
//! raw console bytes per port, RMT frames, PWM, the ADC and the bus bridges. ESP32-S3, (patch J9)
//! ESP32-C3 and (patch J28) the classic ESP32, which adds the DAC outputs and the touch pads.
//! Variable-size results go to `Emu::buf`, read through `esp32sim_buf_ptr` until the next `*_take`
//! call. Jumperly patch J28: every export dispatches once (`MachineKind::chip_mut`) to the chip's
//! `JumperlyChip` (`chip.rs`, `chips/*.rs`); what differs between chips lives there.
use super::{bytes, log, Emu};
#[cfg(not(target_arch = "wasm32"))]
pub use crate::chip::set_native_bus_host;

/// USB-Serial/JTAG as a fourth console "port" after UART0..2.
pub(crate) const USB_PORT: u32 = 3;

/// Switch an emulator to Jumperly mode before boot: no JSON outbox, raw console bytes, every pin
/// watched, RMT frames and PWM changes recorded, the linear ADC. ESP32-S3, ESP32-C3 and (J28) the
/// classic ESP32; returns 1 for other chips or after boot.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_jumperly_mode(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted { return 1; }
    let Some(c) = e.m.chip_mut() else { log("[emu] jumperly mode: ESP32-S3, ESP32-C3 and ESP32 only"); return 1; };
    c.jumperly_mode();
    0
}

/// Set the voltage (millivolts) the circuit puts on an ADC pad. On the S3, GPIO1..10 = ADC1
/// CH0..9 and GPIO11..20 = ADC2 CH0..9. On the C3, GPIO0..4 = ADC1 CH0..4 and GPIO5 = ADC2 CH0.
/// On the classic ESP32 (J28), ADC1 = GPIO36..39, 32..35 and ADC2 = GPIO4, 0, 2, 15, 13, 12, 14,
/// 27, 25, 26. Returns 1 for other pins or outside Jumperly mode.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_adc_set(e: *mut Emu, gpio: u32, mv: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(c) = e.m.chip_mut() else { return 1 };
    u32::from(!c.adc_set(gpio, mv))
}

/// Watch only the pins in `lo | hi << 32` (default: all of the chip's: S3 49, C3 22, classic 40).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_watch(e: *mut Emu, lo: u32, hi: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let mask = u64::from(lo) | (u64::from(hi) << 32);
    let Some(c) = e.m.chip_mut() else { return 1 };
    u32::from(!c.set_pin_watch(mask))
}

/// The current state word of `pin` (see `esp32s3::jumperly::state`), 0 for an invalid pin.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_pin_state(e: *mut Emu, pin: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    match e.m.chip_mut() {
        Some(c) if pin < c.pins() => c.pin_state(pin),
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
    let Some(c) = e.m.chip_mut() else { return 0 };
    let events = c.take_pin_events();
    for (cycle, pin, state) in &events {
        e.buf.extend_from_slice(&(*cycle as f64).to_le_bytes());
        e.buf.extend_from_slice(&u32::from(*pin).to_le_bytes());
        e.buf.extend_from_slice(&state.to_le_bytes());
    }
    events.len() as u32
}

/// Set the level the circuit applies to `pin`'s input: 0 low, 1 high, 2 floating (nothing drives
/// the net; Jumperly patch J28). A floating pad reads low; on the classic ESP32 it is no driven pad,
/// so the strap at a reset takes its reset pull. Returns 1 for another level, pin or chip.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gpio_set_input(e: *mut Emu, pin: u32, level: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(c) = e.m.chip_mut() else { return 1 };
    let level = match level { 0 => Some(false), 1 => Some(true), 2 => None, _ => return 1 };
    if pin >= c.pins() { return 1; }
    c.gpio_set_input(pin as u8, level);
    0
}

/// Take the bytes the guest wrote to console `port` (0..2 UART, 3 USB-Serial/JTAG — not on the
/// classic ESP32): length; bytes in the buffer.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_uart_take(e: *mut Emu, port: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let Some(c) = e.m.chip_mut() else { return 0 };
    let uarts = c.console_ports();
    let index = match port { p if p < uarts => port as usize + 1, USB_PORT => 0, _ => return 0 };
    let data = match c.booted_console().raw.as_mut() {
        Some(raw) => std::mem::take(&mut raw[index]),
        None => return 0,
    };
    e.buf.extend_from_slice(&data);
    data.len() as u32
}

/// Bytes from the host into console `port`'s receive side (0..2 UART FIFO, 3 USB-Serial/JTAG):
/// the number of leading bytes accepted. A UART takes what fits in its 128-byte receive FIFO and
/// never overflows it — the host keeps the rest and offers it again after the guest has read; the
/// USB-Serial/JTAG port queues everything. An invalid port (the C3 has UART0..1, the classic ESP32
/// no USB-Serial/JTAG) or a chip without the ABI takes 0.
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
    let Some(c) = e.m.chip_mut() else { log("[emu] uart in: ESP32-S3, ESP32-C3 and ESP32 only"); return 0 };
    match c.uart_in(port, data) {
        Some(n) => n,
        None => { log(&format!("[emu] uart in: no console port {port}")); 0 }
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
    let Some(c) = e.m.chip_mut() else { return 0 };
    let frames = c.take_rmt();
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
    let Some(c) = e.m.chip_mut() else { return 0 };
    let outs = c.take_pwm();
    for o in &outs {
        e.buf.extend_from_slice(&(o.cycle as f64).to_le_bytes());
        e.buf.extend_from_slice(&o.signal.to_le_bytes());
        e.buf.extend_from_slice(&(u32::from(o.enabled) | (u32::from(o.idle_high) << 1)).to_le_bytes());
        e.buf.extend_from_slice(&o.freq.to_le_bytes());
        e.buf.extend_from_slice(&o.duty.to_le_bytes());
    }
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
    let Some(c) = e.m.chip_mut() else { log("[emu] reset: ESP32-S3, ESP32-C3 and ESP32 only"); return 1 };
    c.reset();
    0
}

/// Jumperly patch J16: the module's flash is octal SPI (ESP32-S3-WROOM-2: Macronix octal commands,
/// JEDEC ID C2 80 xx, efuse FLASH_TYPE octal) when `on` is nonzero, quad SPI otherwise (the
/// default). It survives a chip reset. Before boot, ESP32-S3 only; returns 1 otherwise.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_flash_octal(e: *mut Emu, on: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted { return 1; }
    if !e.m.chip_mut().is_some_and(|c| c.flash_octal(on != 0)) { log("[emu] flash octal: ESP32-S3 only"); return 1; }
    0
}

/// Jumperly patch J28: take the DAC output changes since the last call (the classic ESP32's
/// GPIO25 = DAC1, GPIO26 = DAC2): count; the buffer holds 8 bytes per change, u32 GPIO and u32
/// millivolts LE, `u32::MAX` millivolts when the output turned off (reported once). In Jumperly
/// mode a changed output ends the slice. 0 for a chip without a DAC.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_dac_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let Some(c) = e.m.chip_mut() else { return 0 };
    let changes = c.take_dac();
    for (gpio, mv) in &changes {
        e.buf.extend_from_slice(&u32::from(*gpio).to_le_bytes());
        e.buf.extend_from_slice(&mv.to_le_bytes());
    }
    changes.len() as u32
}

/// Jumperly patch J28: a finger on (`touched` nonzero) or off touch pad `gpio` (the classic ESP32:
/// T0..T9 = GPIO4, 0, 2, 15, 13, 12, 14, 27, 33, 32); the touch sensor then counts a touched pad
/// lower. It survives a chip reset. Returns 0, or 1 for another pin or a chip without touch pads.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_touch_set(e: *mut Emu, gpio: u32, touched: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(c) = e.m.chip_mut() else { return 1 };
    u32::from(!c.touch_set(gpio, touched != 0))
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

// ------------------------------------------------------------------ Jumperly patch J6/J7/J18: bus bridges
// The host imports (`host_i2c`, `host_spi_bus`) and their native stand-ins are in `chip.rs` (J28).

/// Turn the I2C / SPI host bridges on or off (they survive chip resets); `spi` covers SPI2 and,
/// on the S3, SPI3 (Jumperly patch J18), on the classic ESP32 HSPI and VSPI (J28). Turning a
/// bridge off finishes what still waits for the host without it: an I2C transaction NACKs unknown
/// addresses, a SPI transfer reads 0xFF.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_bus_host(e: *mut Emu, i2c: u32, spi: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(c) = e.m.chip_mut() else { return 1 };
    c.bus_host(i2c != 0, spi != 0);
    0
}

/// Run the I2C / SPI transactions the guest started since the last call, through `host_i2c` /
/// `host_spi_bus` (SPI2, then SPI3 on the S3; on the classic ESP32 HSPI = host 2 and VSPI = host
/// 3, one waiting transfer at a time, then the other), and raise their completion interrupts.
/// Returns how many ran. Call it between runs, never from inside a host import.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_bus_service(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.m.chip_mut().map_or(0, |c| c.bus_service())
}

/// The pin a matrix input signal reads (FSPIQ = 102 / SPI3_Q = 67 for MISO, I2CEXT0 SDA = 90 ...), 255 if none.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_gpio_in_pin(e: *mut Emu, signal: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.m.chip_mut().map_or(255, |c| c.gpio_in_pin(signal))
}

/// Jumperly patch J19: the pads UART `port` is on, through the GPIO matrix or on its IO_MUX pads, as
/// `tx | rx << 8` with 255 for none (S3 and classic ESP32: ports 0..2; C3: 0..1; other ports and
/// chips: none).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_uart_pins(e: *mut Emu, port: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let port = port.min(255) as u8;
    let (tx, rx) = e.m.chip_mut().map_or((None, None), |c| c.uart_pins(port));
    u32::from(tx.unwrap_or(255)) | u32::from(rx.unwrap_or(255)) << 8
}

/// Jumperly patch J21: the baud UART `port` is set to (its clock source and dividers), 0 when it is
/// not configured or there is no such port (S3 and classic ESP32: 0..2; C3: 0, 1).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_uart_baud(e: *mut Emu, port: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let port = port.min(255) as u8;
    e.m.chip_mut().and_then(|c| c.uart_baud(port)).unwrap_or(0)
}

// ------------------------------------------------------------------ Jumperly patch J17: I2S RX

/// Bytes `esp32sim_i2s_rx_need` writes to the buffer: eight u32.
pub const I2S_RX_INFO_BYTES: usize = 32;

/// Jumperly patch J17: the host supplies what I2S RX receives (`esp32sim_i2s_rx_push`) when `on` is
/// nonzero; otherwise a running receiver reads upstream's inputs (silence in the browser). A frame
/// falling due with no host word queued is silence and ends the slice once, so the host can queue
/// more. Survives a chip reset. ESP32-S3 and (J28) the classic ESP32; returns 1 otherwise.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_i2s_host(e: *mut Emu, on: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let Some(c) = e.m.chip_mut() else { return 1 };
    if c.i2s(0).is_none() { return 1; }
    for port in 0..2 {
        if let Some(i2s) = c.i2s(port) { i2s.set_host(on != 0); }
    }
    0
}

/// Jumperly patch J17: how many frames I2S controller `port` (0, 1) should get from the host now
/// so the next `cycles` CPU cycles of a run find them queued (0 while its receiver is stopped, in
/// a mode the model does not receive, or for another chip or port). The buffer holds the format,
/// eight u32 LE: frame rate Hz, BCK cycles per slot, data bits per slot, received slot mask, slots
/// per frame, flags (bit 0 running, bit 1 Philips one-BCK delay), frames queued, BCK Hz.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_i2s_rx_need(e: *mut Emu, port: u32, cycles: u32) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let Some(c) = e.m.chip_mut() else { return 0 };
    let pending = c.pending_cycles();
    let Some(i2s) = c.i2s(port) else { return 0 };
    for v in i2s.host_format() { e.buf.extend_from_slice(&v.to_le_bytes()); }
    // the cycles the devices have not seen yet fall due in the next run too
    i2s.host_frames_needed(pending + u64::from(cycles))
}

/// Jumperly patch J17: queue `frames` frames for I2S controller `port`'s receiver: 8 bytes each,
/// the left (WS low) then the right (WS high) slot word, u32 LE, MSB-aligned (bit 31 is the first
/// bit after the WS edge and the I2S one-BCK delay; the receiver takes its data bits from the top).
/// Returns how many it took (at most 65536 wait). ESP32-S3 and the classic ESP32; 0 otherwise.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access. For nonzero
/// `frames`, `ptr` must be non-null and readable for `8 * frames` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_i2s_rx_push(e: *mut Emu, port: u32, ptr: *const u8, frames: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable input buffer for this call.
    let data = unsafe { bytes(ptr, frames.saturating_mul(8)) };
    let Some(i2s) = e.m.chip_mut().and_then(|c| c.i2s(port)) else { return 0 };
    let words: Vec<[u32; 2]> = data.as_chunks::<8>().0.iter().map(|c| [u32::from_le_bytes(c[0..4].try_into().unwrap()), u32::from_le_bytes(c[4..8].try_into().unwrap())]).collect();
    i2s.host_push(&words) as u32
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

/// Jumperly patch J22: the networks around the user (the local helper's scan), heard by the
/// board's scans but not joinable, replacing the ones there were. `data` holds entries of
/// `[ssid_len u8][ssid][channel u8][rssi i8][secure u8][bssid 6]` (empty: none), at most 32. A network
/// on a BSSID already listed, or with the access point's own SSID or BSSID, is left out with a log
/// line (`StationLink::set_beacons`). Returns 0, or 1 (no access point, bad data; logged).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; for a nonzero `len`,
/// `data` must be readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_wifi_beacons(e: *mut Emu, data: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let data = unsafe { bytes(data, len) };
    let list = match esp_soc::wifi::BeaconAp::parse_list(data) {
        Ok(list) => list,
        Err(reason) => { log(&format!("[emu] wifi: {reason}")); return 1; }
    };
    // a real scan may list one BSSID twice: the first one answers, as on the air
    let mut seen = Vec::new();
    let list = list.into_iter().filter(|b| {
        let first = !seen.contains(&b.bssid);
        if first { seen.push(b.bssid); } else { log(&format!("[emu] wifi: network around '{}' left out: its BSSID {} is listed already", b.ssid, esp_soc::wifi::mac_str(&b.bssid))); }
        first
    }).collect();
    let Some(left_out) = e.m.chip_mut().and_then(|c| c.wifi_link().set_beacons(list)) else {
        log("[emu] wifi: beacons need the virtual access point (esp32sim_wifi_ap)");
        return 1;
    };
    for (b, why) in left_out { log(&format!("[emu] wifi: network around '{}' left out: it has {why}", b.ssid)); }
    0
}

/// Jumperly patch J23: the virtual BLE controller (`--ble` of the CLI) on the S3, the C3 or (J28)
/// the classic ESP32. `elf`
/// is the application ELF with its symbols (the guest's controller functions are found by name);
/// before boot. Its log lines (`t=<s>s <line>`, the CLI's `[ble]` output) are kept for
/// `esp32sim_ble_log_take`. Returns 0, or 1 (another chip, after boot, a bad or unsuitable ELF;
/// logged).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; for a nonzero `len`,
/// `elf` must be readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_virtual(e: *mut Emu, elf: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    if e.booted { log("[emu] ble: the virtual controller is enabled before boot"); return 1; }
    // SAFETY: the caller provides a readable buffer for this call.
    let data = unsafe { bytes(elf, len) };
    let elf = match esp_soc::elf::parse(data) {
        Ok(elf) => elf,
        Err(reason) => { log(&format!("[emu] ble: {reason}")); return 1; }
    };
    let result = match e.m.chip_mut() {
        Some(c) => {
            c.ble().capture = Some(Default::default());
            c.enable_ble(&elf)
        }
        None => Err("the virtual BLE controller needs an ESP32-S3, ESP32-C3 or ESP32".into()),
    };
    match result {
        Ok(()) => 0,
        Err(reason) => { log(&format!("[emu] ble: {reason}")); 1 }
    }
}

/// Jumperly patch J23: a command for the virtual peer, now: `connect`, `discover`, `read HANDLE`,
/// `write HANDLE HEX` (at most 20 bytes), `subscribe CCC_HANDLE` (the CLI's script `ble` commands).
/// Returns 0, or 1 (no virtual controller, a bad command; the reason is the next log line).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; for a nonzero `len`,
/// `cmd` must be readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_virtual_command(e: *mut Emu, cmd: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let Ok(cmd) = std::str::from_utf8(unsafe { bytes(cmd, len) }) else { return 1 };
    let Some(c) = e.m.chip_mut() else { return 1 };
    match c.ble_command(cmd) {
        Ok(()) => 0,
        Err(reason) => { c.ble().note(format!("error {reason}")); 1 }
    }
}

/// Jumperly patch J25: the Bluetooth devices around the user (the web page found them), heard by
/// the guest's scans and not connectable, replacing the ones there were. `data`: entries of
/// `[addr 6][addr_type u8 (0 public, 1 random)][rssi i8][adv_len u8][adv][rsp_len u8][rsp]`, the
/// data at most 31 bytes each, at most `NEARBY_MAX` (32) devices (empty: none). Returns 0, or 1 (no virtual
/// controller, bad data; logged).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; for a nonzero `len`,
/// `data` must be readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_nearby(e: *mut Emu, data: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let data = unsafe { bytes(data, len) };
    let list = match esp_soc::ble::peer::Nearby::parse_list(data) {
        Ok(list) => list,
        Err(reason) => { log(&format!("[emu] ble: {reason}")); return 1; }
    };
    let result = match e.m.chip_mut() {
        Some(c) => {
            let cycles = c.flush_ticks();
            let hz = c.cpu_hz();
            c.ble().set_nearby(list, cycles, hz)
        }
        None => Err("BLE needs an ESP32-S3, ESP32-C3 or ESP32".into()),
    };
    match result {
        Ok(()) => 0,
        Err(reason) => { log(&format!("[emu] ble: {reason}")); 1 }
    }
}

/// Jumperly patch J26: the air between the boards of a project: this board's public address
/// (`addr`: 6 bytes, least significant first; READ_BD_ADDR answers it) and connections to other
/// boards through the host (`esp32sim_ble_air_take` / `esp32sim_ble_air_put`). After
/// `esp32sim_ble_virtual`, before boot. Returns 0, or 1 (no virtual controller; logged).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; `addr` must be
/// readable for 6 bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_air(e: *mut Emu, addr: *const u8) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides 6 readable bytes for this call.
    let Ok(address) = <[u8; 6]>::try_from(unsafe { bytes(addr, 6) }) else { return 1 };
    let result = match e.m.chip_mut() {
        Some(c) => c.ble().enable_air(address),
        None => Err("BLE needs an ESP32-S3, ESP32-C3 or ESP32".into()),
    };
    match result {
        Ok(()) => 0,
        Err(reason) => { log(&format!("[emu] ble air: {reason}")); 1 }
    }
}

/// Jumperly patch J26: the frames this board sent into the air since the last call, into the
/// buffer: their count; each `[len u16][frame]` (`Air::encode`).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_air_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    let frames = match e.m.chip_mut() {
        Some(c) => {
            let cycles = c.flush_ticks();
            let hz = c.cpu_hz();
            c.ble().take_air(cycles, hz)
        }
        None => Vec::new(),
    };
    e.buf.clear();
    for f in &frames {
        let b = f.encode();
        e.buf.extend_from_slice(&(b.len() as u16).to_le_bytes());
        e.buf.extend_from_slice(&b);
    }
    frames.len() as u32
}

/// Jumperly patch J26: one frame from another board (`Air::encode`). Returns 0, or 1 (a bad frame;
/// logged).
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access; for a nonzero `len`,
/// `ptr` must be readable for `len` bytes throughout this call.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_air_put(e: *mut Emu, ptr: *const u8, len: usize) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    // SAFETY: the caller provides a readable buffer for this call.
    let frame = match esp_soc::ble::peer::Air::decode(unsafe { bytes(ptr, len) }) {
        Ok(f) => f,
        Err(reason) => { log(&format!("[emu] ble air: {reason}")); return 1; }
    };
    let Some(c) = e.m.chip_mut() else { return 1 };
    let cycles = c.flush_ticks();
    let hz = c.cpu_hz();
    c.ble().air_in(frame, cycles, hz);
    0
}

/// Jumperly patch J23: the oldest log line of the virtual BLE controller, into the buffer: its
/// length, 0 when there is none.
///
/// # Safety
/// `e` must point to a live emulator to which the caller has exclusive access.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_ble_log_take(e: *mut Emu) -> u32 {
    // SAFETY: the caller provides exclusive access to a live emulator.
    let e = unsafe { &mut *e };
    e.buf.clear();
    let Some(line) = e.m.chip_mut().and_then(|c| c.ble().take_line()) else { return 0 };
    e.buf.extend_from_slice(line.as_bytes());
    line.len() as u32
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
    let Some(ssid) = e.m.chip_mut().and_then(|c| c.wifi_link().take_probed_ssid()) else { return 0 };
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

    /// Jumperly patch J18: SPI2 and SPI3 transfers reach `host_spi_bus` with their controller, each
    /// gets its own MISO bytes; turning the bridge off finishes a waiting SPI3 transfer with 0xFF.
    #[test]
    fn spi2_and_spi3_reach_the_host_with_their_controller() {
        use xtensa_lx7::bus::Bus;
        use std::{cell::RefCell, rc::Rc};
        const SPI2: u32 = 0x6002_4000;
        const SPI3: u32 = 0x6002_5000;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        set_native_bus_host(|_, _, _| 0, move |host, tx, rx| {
            s.borrow_mut().push((host, tx.to_vec()));
            rx.fill(host as u8 * 0x11);
        });
        let board = "none";
        // SAFETY: the board name is readable for the call; the emulator is ours until deleted.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null());
        assert_eq!(unsafe { esp32sim_bus_host(e, 0, 1) }, 0);
        fn bus<'a>(e: *mut Emu) -> &'a mut esp32s3::bus::SocBus {
            // SAFETY: the test owns the live emulator.
            &mut unsafe { &mut *e }.m.s3_mut().unwrap().bus
        }
        let start = |e: *mut Emu, base: u32, byte: u32| {
            let b = bus(e);
            b.write32(base + 0x10, (1 << 27) | (1 << 28) | 1).unwrap();   // full duplex, one byte
            b.write32(base + 0x1c, 7).unwrap();
            b.write32(base + 0x98, byte).unwrap();
            b.write32(base, 1 << 24).unwrap();
        };
        start(e, SPI2, 0xa2);
        start(e, SPI3, 0xa3);
        assert_eq!(unsafe { esp32sim_bus_service(e) }, 2);
        assert_eq!(&*seen.borrow(), &[(2, vec![0xa2]), (3, vec![0xa3])]);
        let b = bus(e);
        assert_eq!((b.read32(SPI2 + 0x98).unwrap() & 0xff, b.read32(SPI3 + 0x98).unwrap() & 0xff), (0x22, 0x33));
        assert_eq!((b.read32(SPI2).unwrap() | b.read32(SPI3).unwrap()) & (1 << 24), 0, "both done");
        start(e, SPI3, 0x55);
        assert_ne!(bus(e).read32(SPI3).unwrap() & (1 << 24), 0, "waits for the host");
        assert_eq!(unsafe { esp32sim_bus_host(e, 0, 0) }, 0);
        let b = bus(e);
        assert_eq!(b.read32(SPI3).unwrap() & (1 << 24), 0, "finished without the host");
        assert_eq!(b.read32(SPI3 + 0x98).unwrap() & 0xff, 0xff);
        assert_eq!(unsafe { esp32sim_bus_service(e) }, 0);
        assert_eq!(seen.borrow().len(), 2);
        unsafe { esp32sim_delete(e) };
    }
}
