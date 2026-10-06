//! Jumperly ABI (patches J1–J4) on real Arduino firmware: pin states with cycles, the slice yield,
//! GPIO input, raw UART, RMT frames, PWM outputs. Firmware: `emulator/fixtures/bin`; ROM: `emulator/rom`.
use esp32sim_wasm::*;
use std::path::PathBuf;

const MHZ: u64 = 240; // cycles per microsecond
const OUT: u32 = 1;
const ENABLE: u32 = 2;
const PULL_UP: u32 = 4;

fn emulator_dir() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..") }
fn read(rel: &str) -> Vec<u8> { std::fs::read(emulator_dir().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")) }
fn merged(app: &str) -> Vec<u8> {
    let mut flash = vec![0xffu8; 4 << 20];
    for (off, file) in [(0x0, "fixtures/bin/bootloader.bin".to_string()), (0x8000, "fixtures/bin/partitions.bin".to_string()), (0x10000, format!("fixtures/bin/{app}.bin"))] {
        let d = read(&file);
        flash[off..off + d.len()].copy_from_slice(&d);
    }
    flash
}

struct Emu(*mut esp32sim_wasm::Emu);
impl Emu {
    fn boot(app: &str) -> Emu {
        let board = "none";
        // SAFETY: the board name is readable for the call; the pointer is checked for null.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null());
        let emu = Emu(e);
        let (rom, flash) = (read("rom/esp32s3_rev0_rom.elf"), merged(app));
        // SAFETY: `e` is live and exclusively ours; the buffers are readable for each call.
        unsafe {
            assert_eq!(esp32sim_jumperly_mode(e), 0);
            assert_eq!(esp32sim_load(e, 0, rom.as_ptr(), rom.len()), 0);
            assert_eq!(esp32sim_load(e, 5, flash.as_ptr(), flash.len()), 0);
            assert_eq!(esp32sim_boot(e, 0), 0);
        }
        emu
    }
    fn run(&mut self, cycles: u32) -> u32 { unsafe { esp32sim_run(self.0, cycles, 0.0) } }
    fn cycles(&self) -> u64 { (unsafe { esp32sim_cycles(self.0) }) as u64 }
    fn buf(&self, len: usize) -> Vec<u8> { unsafe { std::slice::from_raw_parts(esp32sim_buf_ptr(self.0), len).to_vec() } }
    fn pin_events(&mut self) -> Vec<(u64, u8, u32)> {
        let n = unsafe { esp32sim_pin_events_take(self.0) } as usize;
        let b = self.buf(n * 16);
        (0..n).map(|i| {
            let c = &b[i * 16..i * 16 + 16];
            (f64::from_le_bytes(c[0..8].try_into().unwrap()) as u64, u32::from_le_bytes(c[8..12].try_into().unwrap()) as u8, u32::from_le_bytes(c[12..16].try_into().unwrap()))
        }).collect()
    }
    fn uart(&mut self, port: u32) -> String {
        let n = unsafe { esp32sim_uart_take(self.0, port) } as usize;
        String::from_utf8_lossy(&self.buf(n)).into_owned()
    }
    /// Run `cycles` more device cycles (possibly in several yielded slices), collecting pin events and UART0 text.
    fn run_for(&mut self, cycles: u64) -> (Vec<(u64, u8, u32)>, String) {
        let end = self.cycles() + cycles;
        let (mut ev, mut text) = (Vec::new(), String::new());
        while self.cycles() < end {
            let left = (end - self.cycles()).min(u32::MAX as u64) as u32;
            assert_eq!(self.run(left), 0);
            ev.extend(self.pin_events());
            text += &self.uart(0);
        }
        (ev, text)
    }
}
impl Drop for Emu {
    fn drop(&mut self) { unsafe { esp32sim_delete(self.0) } }
}

/// (cycle, visible level) of `pin` whenever its driven level changes.
fn level_changes(ev: &[(u64, u8, u32)], pin: u8) -> Vec<(u64, bool)> {
    let mut out = Vec::new();
    let mut last = None;
    for &(c, p, s) in ev {
        if p != pin { continue; }
        let level = s & OUT != 0 && s & ENABLE != 0;
        if last != Some(level) { out.push((c, level)); last = Some(level); }
    }
    out
}

#[test]
fn blink_reports_gpio2_edges_one_second_apart_and_prints() {
    let mut e = Emu::boot("blink");
    let (ev, text) = e.run_for(2_200_000 * MHZ);
    assert!(text.contains("Ready") && text.contains("led=1"), "{text}");
    let highs: Vec<u64> = level_changes(&ev, 2).into_iter().filter(|&(_, l)| l).map(|(c, _)| c).collect();
    assert!(highs.len() >= 2, "{ev:?}");
    let period_us = (highs[1] - highs[0]) / MHZ;
    assert!((999_000..=1_001_000).contains(&period_us), "period {period_us} us");
}

#[test]
fn a_pin_change_ends_the_slice_early() {
    let mut e = Emu::boot("blink");
    e.run_for(1_000_000 * MHZ);
    let start = e.cycles();
    assert_eq!(e.run((1_000_000 * MHZ) as u32), 0);              // asked for a full second ...
    let ran = e.cycles() - start;
    let ev = e.pin_events();
    assert!(!ev.is_empty(), "no pin event");
    assert!(ran < 600_000 * MHZ, "ran {} us", ran / MHZ);        // ... but the next edge (≤ 500 ms) handed control back
    let last = ev.last().unwrap().0;
    assert!(e.cycles() - last <= 2 * 256, "yielded {} cycles after the event", e.cycles() - last);
}

#[test]
fn input_pullup_shows_as_a_pull_and_gpio_input_reaches_the_sketch() {
    let mut e = Emu::boot("button");
    let (ev, text) = e.run_for(300_000 * MHZ);
    assert!(text.contains("Ready"), "{text}");
    let s4 = ev.iter().rev().find(|x| x.1 == 4).expect("no GPIO4 state").2;
    assert!(s4 & PULL_UP != 0 && s4 & ENABLE == 0, "GPIO4 state {s4:#x}");
    unsafe { assert_eq!(esp32sim_gpio_set_input(e.0, 4, 1), 0) };   // released
    e.run_for(20_000 * MHZ);
    unsafe { assert_eq!(esp32sim_gpio_set_input(e.0, 4, 0), 0) };   // pressed
    let (ev, _) = e.run_for(20_000 * MHZ);
    assert_eq!(level_changes(&ev, 2).last().map(|x| x.1), Some(true), "{ev:?}");
}

#[test]
fn uart0_bytes_in_and_out() {
    let mut e = Emu::boot("echo");
    let (_, text) = e.run_for(300_000 * MHZ);
    assert!(text.contains("Ready"), "{text}");
    let msg = b"ping\n";
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 0, msg.as_ptr(), msg.len()) }, 5);   // all accepted
    let (_, text) = e.run_for(50_000 * MHZ);
    assert!(text.contains("ping"), "{text}");
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 9, msg.as_ptr(), msg.len()) }, 0);   // no such port
}

#[test]
fn uart_in_accepts_what_fits_in_the_rx_fifo() {
    let mut e = Emu::boot("echo");
    e.run_for(300_000 * MHZ);
    let big = [b'x'; 300];
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 0, big.as_ptr(), big.len()) }, 128);   // the 128-byte RX FIFO
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 0, big.as_ptr(), big.len()) }, 0);     // still full: nothing dropped, nothing taken
    let (_, text) = e.run_for(20_000 * MHZ);
    assert_eq!(text.len(), 128, "{text}");
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 0, big.as_ptr(), big.len()) }, 128);   // drained by the sketch: room again
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 3, big.as_ptr(), big.len()) }, 300);   // USB-Serial/JTAG queues everything
}

#[test]
fn a_chip_reset_keeps_the_circuit_inputs_and_releases_the_pins() {
    let mut e = Emu::boot("restart");
    unsafe { assert_eq!(esp32sim_gpio_set_input(e.0, 4, 0), 0) };   // the circuit holds GPIO4 low throughout
    let (ev, text) = e.run_for(350_000 * MHZ);
    assert!(text.matches("boot in4=0").count() >= 2, "{text}");
    assert!(!text.contains("boot in4=1"), "{text}");
    let s2: Vec<u32> = ev.iter().filter(|x| x.1 == 2).map(|x| x.2).collect();
    let driven_high = |s: &u32| s & ENABLE != 0 && s & OUT != 0;
    let first_high = s2.iter().position(driven_high).expect("GPIO2 never driven high");
    let released = first_high + s2[first_high..].iter().position(|s| s & ENABLE == 0).expect("GPIO2 not released by the reset");
    assert!(s2[released..].iter().any(driven_high), "GPIO2 not driven high again after the reset: {s2:x?}");
}

/// Jumperly patch J8: the host restarts the chip (the RESET button); device time keeps going.
#[test]
fn a_host_reset_reboots_the_chip_and_keeps_the_clock() {
    let mut e = Emu::boot("blink");
    let (_, text) = e.run_for(600_000 * MHZ);
    assert_eq!(text.matches("Ready").count(), 1, "{text}");
    let before = e.cycles();
    assert_eq!(unsafe { esp32sim_reset(e.0) }, 0);
    assert!(e.cycles() >= before, "cycles went back: {} < {before}", e.cycles());
    let (_, text) = e.run_for(600_000 * MHZ);
    assert_eq!(text.matches("Ready").count(), 1, "{text}");
    assert!(e.cycles() >= before + 600_000 * MHZ);
}

#[test]
fn rgb_led_write_produces_an_rmt_frame_on_gpio48() {
    let mut e = Emu::boot("rgb");
    e.run_for(300_000 * MHZ);
    let n = unsafe { esp32sim_rmt_take(e.0) } as usize;
    assert!(n >= 1, "no RMT frame");
    let b = e.buf(16 + 4);
    assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 48);
    assert!(u32::from_le_bytes(b[12..16].try_into().unwrap()) >= 24);
    assert_eq!(&b[16..19], &[20, 10, 30]);                       // GRB of rgbLedWrite(48, 10, 20, 30)
}

#[test]
fn jumperly_mode_is_s3_and_c3_only_and_before_boot() {
    let board = "esp32c6";
    let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
    assert!(!e.is_null());
    assert_eq!(unsafe { esp32sim_jumperly_mode(e) }, 1);
    unsafe { esp32sim_delete(e) };
}

/// (cycle, signal, enabled, freq, duty) from esp32sim_pwm_take.
fn pwm_events(e: &mut Emu) -> Vec<(u64, u32, bool, f64, f64)> {
    let n = unsafe { esp32sim_pwm_take(e.0) } as usize;
    let b = e.buf(n * 32);
    (0..n).map(|i| {
        let c = &b[i * 32..i * 32 + 32];
        (f64::from_le_bytes(c[0..8].try_into().unwrap()) as u64, u32::from_le_bytes(c[8..12].try_into().unwrap()),
         u32::from_le_bytes(c[12..16].try_into().unwrap()) & 1 != 0, f64::from_le_bytes(c[16..24].try_into().unwrap()),
         f64::from_le_bytes(c[24..32].try_into().unwrap()))
    }).collect()
}
/// The last PWM state of the matrix signal currently routed to `pin`.
fn pwm_on_pin(e: &mut Emu, ev: &[(u64, u32, bool, f64, f64)], pin: u32) -> (bool, f64, f64) {
    let signal = unsafe { esp32sim_pin_state(e.0, pin) } >> 16 & 0x1ff;
    let (_, _, en, f, d) = *ev.iter().rev().find(|x| x.1 == signal).unwrap_or_else(|| panic!("no PWM on pin {pin} (signal {signal})"));
    (en, f, d)
}

#[test]
fn ledc_analog_write_ledc_write_and_tone() {
    let mut e = Emu::boot("pwm");
    let (_, text) = e.run_for(300_000 * MHZ);
    assert!(text.contains("Ready"), "{text}");
    let ev = pwm_events(&mut e);
    let (en, f, d) = pwm_on_pin(&mut e, &ev, 5);
    assert!(en && (f - 1000.0).abs() < 1.0 && (d - 0.25).abs() < 1e-6, "pin 5: {en} {f} {d}");
    let (en, f, d) = pwm_on_pin(&mut e, &ev, 6);
    assert!(en && (f - 5000.0).abs() < 1.0 && (d - 0.25).abs() < 1e-6, "pin 6: {en} {f} {d}");
    let (en, f, d) = pwm_on_pin(&mut e, &ev, 7);
    assert!(en && (f - 440.0).abs() < 1.0 && (d - 511.0 / 1024.0).abs() < 1e-3, "pin 7: {en} {f} {d}");
}

#[test]
fn servo_uses_mcpwm_at_50_hz_with_the_pulse_as_duty() {
    let mut e = Emu::boot("servo");
    let mut duties = Vec::new();
    for _ in 0..40 {
        e.run_for(10_000 * MHZ);
        for (_, s, en, f, d) in pwm_events(&mut e) {
            if (160..=171).contains(&s) && en {
                assert!((f - 50.0).abs() < 0.01, "freq {f}");
                if duties.last() != Some(&d) { duties.push(d); }
            }
        }
    }
    // ESP32Servo: us = map(angle, 0, 180, 544, 2400), ticks = us / 19.53125, duty = ticks / 1024
    for want in [27.0 / 1024.0, 75.0 / 1024.0, 122.0 / 1024.0] {
        assert!(duties.iter().any(|d| (d - want).abs() < 0.002), "no duty near {want}: {duties:?}");
    }
    let signal = unsafe { esp32sim_pin_state(e.0, 18) } >> 16 & 0x1ff;
    assert!((160..=171).contains(&signal), "GPIO18 routed to signal {signal}");
}

#[test]
fn analog_read_follows_the_pad_voltage() {
    let mut e = Emu::boot("adc");
    unsafe {
        assert_eq!(esp32sim_adc_set(e.0, 1, 1550), 0);
        assert_eq!(esp32sim_adc_set(e.0, 11, 3300), 0);
        assert_eq!(esp32sim_adc_set(e.0, 21, 100), 1);
    }
    let (_, text) = e.run_for(400_000 * MHZ);
    let line = text.lines().rev().find(|l| l.starts_with("a1=")).unwrap_or_else(|| panic!("no reading: {text}"));
    assert_eq!(line.trim(), format!("a1={} a11=4095", 1550 * 4095 / 3100));
    unsafe { esp32sim_adc_set(e.0, 1, 0) };
    let (_, text) = e.run_for(100_000 * MHZ);
    assert!(text.lines().rev().find(|l| l.starts_with("a1=")).is_some_and(|l| l.starts_with("a1=0 ")), "{text}");
}

#[test]
fn wire_scan_write_and_read_through_the_host_bridge() {
    use std::{cell::RefCell, rc::Rc};
    let mem = Rc::new(RefCell::new(([0u8; 256], 0usize, false)));            // (registers, pointer, expect register)
    let m = mem.clone();
    set_native_bus_host(move |_bus, op, arg| {
        let mut s = m.borrow_mut();
        match op {
            0 => { if arg >> 1 != 0x50 { return 0; } s.2 = arg & 1 == 0; 1 }
            1 => { if s.2 { s.1 = arg as usize; s.2 = false; } else { let p = s.1; s.0[p & 255] = arg as u8; s.1 += 1; } 1 }
            2 => { let p = s.1; s.1 += 1; u32::from(s.0[p & 255]) }
            _ => 0,
        }
    }, |_tx, _rx| {});
    let mut e = Emu::boot("i2c");
    assert_eq!(unsafe { esp32sim_bus_host(e.0, 1, 0) }, 0);
    let text = run_with_bus_service(&mut e, "rd=");
    assert!(text.contains("found 0x50"), "{text}");
    assert_eq!(text.matches("found").count(), 1, "{text}");                  // every other address NACKs
    assert!(text.contains("rd=66"), "{text}");                               // 0x42
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 90) }, 8);                 // SDA input on GPIO8
}

#[test]
fn spi_transfer_through_the_host_bridge() {
    set_native_bus_host(|_, _, _| 0, |tx, rx| for (r, t) in rx.iter_mut().zip(tx) { *r = !t; });
    let mut e = Emu::boot("spi");
    assert_eq!(unsafe { esp32sim_bus_host(e.0, 0, 1) }, 0);
    let text = run_with_bus_service(&mut e, "spi=");
    assert!(text.contains("spi=5a edcb"), "{text}");
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 102) }, 13);               // MISO on GPIO13
}

/// Run in ≤ 1 ms slices, serving the bus bridges after every return (a deferred transaction ends the
/// slice early), until UART0 shows `until` or 3 s of device time pass. Returns the UART0 text.
fn run_with_bus_service(e: &mut Emu, until: &str) -> String {
    let end = e.cycles() + 3_000_000 * MHZ;
    let mut text = String::new();
    while e.cycles() < end && !text.contains(until) {
        assert_eq!(e.run((1_000 * MHZ) as u32), 0);
        unsafe { esp32sim_bus_service(e.0) };
        text += &e.uart(0);
    }
    text
}

// ------------------------------------------------------------------ Jumperly patch J9: ESP32-C3

const C3_MHZ: u64 = 160;
const C3_RMT_SIGNAL: u32 = 51;

/// The C3 fixture as a whole-flash image: its own bootloader and partitions, the app at 0x10000.
fn merged_c3(app: &str) -> Vec<u8> {
    let mut flash = vec![0xffu8; 4 << 20];
    for (off, file) in [(0x0, format!("fixtures/bin/{app}.bootloader.bin")), (0x8000, format!("fixtures/bin/{app}.partitions.bin")), (0x10000, format!("fixtures/bin/{app}.bin"))] {
        let d = read(&file);
        flash[off..off + d.len()].copy_from_slice(&d);
    }
    flash
}

impl Emu {
    fn boot_c3(app: &str) -> Emu {
        let board = "esp32c3";
        // SAFETY: the board name is readable for the call; the pointer is checked for null.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
        assert!(!e.is_null());
        let emu = Emu(e);
        let (rom, flash) = (read("rom/esp32c3_rev3_rom.elf"), merged_c3(app));
        // SAFETY: `e` is live and exclusively ours; the buffers are readable for each call.
        unsafe {
            assert_eq!(esp32sim_jumperly_mode(e), 0);
            assert_eq!(esp32sim_load(e, 0, rom.as_ptr(), rom.len()), 0);
            assert_eq!(esp32sim_load(e, 5, flash.as_ptr(), flash.len()), 0);
            assert_eq!(esp32sim_boot(e, 0), 0);
        }
        emu
    }
    /// The RMT frames since the last call: (cycle, pin, bits).
    fn rmt_frames(&mut self) -> Vec<(u64, u32, Vec<bool>)> {
        let n = unsafe { esp32sim_rmt_take(self.0) } as usize;
        let b = self.buf(n * 20);                                 // the fixtures send 24-bit frames: 16 + 4 bytes each
        let mut at = 0;
        let mut out = Vec::new();
        for _ in 0..n {
            let cycle = f64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as u64;
            let pin = u32::from_le_bytes(b[at + 8..at + 12].try_into().unwrap());
            let count = u32::from_le_bytes(b[at + 12..at + 16].try_into().unwrap()) as usize;
            assert_eq!(count, 24, "only 24-bit frames are expected here");
            let packed = &b[at + 16..at + 16 + count.div_ceil(8).next_multiple_of(4)];
            out.push((cycle, pin, (0..count).map(|i| packed[i / 8] & (0x80 >> (i % 8)) != 0).collect()));
            at += 16 + packed.len();
        }
        out
    }
    /// Like `run_for`, but also collecting the RMT frames, at the C3 clock.
    fn run_c3(&mut self, us: u64) -> (Vec<(u64, u8, u32)>, Vec<(u64, u32, Vec<bool>)>, String) {
        let end = self.cycles() + us * C3_MHZ;
        let (mut ev, mut frames, mut text) = (Vec::new(), Vec::new(), String::new());
        while self.cycles() < end {
            let left = (end - self.cycles()).min(u32::MAX as u64) as u32;
            assert_eq!(self.run(left), 0);
            ev.extend(self.pin_events());
            frames.extend(self.rmt_frames());
            text += &self.uart(0);
        }
        (ev, frames, text)
    }
}

#[test]
fn c3_blink_prints_on_uart0_and_drives_the_rgb_led_through_rmt_on_gpio8() {
    let mut e = Emu::boot_c3("c3-blink");
    let (ev, frames, text) = e.run_c3(2_200_000);
    assert!(text.contains("Ready") && text.contains("led=1") && text.contains("led=0"), "{text}");
    // Every digitalWrite(LED_BUILTIN) is one 24-bit WS2812 frame on GPIO8: lit, then dark, 500 ms apart.
    assert!(frames.len() >= 4, "{} frames", frames.len());
    assert!(frames.iter().all(|f| f.1 == 8 && f.2.len() == 24), "{frames:?}");
    let lit = |f: &(u64, u32, Vec<bool>)| f.2.iter().any(|&b| b);
    assert!(lit(&frames[0]) && !lit(&frames[1]) && lit(&frames[2]), "{frames:?}");
    let period_us = (frames[2].0 - frames[0].0) / C3_MHZ;
    assert!((999_000..=1_001_000).contains(&period_us), "period {period_us} us");
    // GPIO8 is routed to the RMT output signal; the C3 reports it in the S3 state-word layout.
    let s8 = ev.iter().rev().find(|x| x.1 == 8).expect("no GPIO8 state").2;
    assert_eq!(s8 >> 16, C3_RMT_SIGNAL, "GPIO8 state {s8:#x}");
}

#[test]
fn c3_an_rmt_frame_ends_the_slice_early() {
    let mut e = Emu::boot_c3("c3-blink");
    e.run_c3(1_000_000);
    let start = e.cycles();
    assert_eq!(e.run((1_000_000 * C3_MHZ) as u32), 0);
    let ran = e.cycles() - start;
    assert!(!e.rmt_frames().is_empty(), "no RMT frame");
    assert!(ran < 600_000 * C3_MHZ, "ran {} us", ran / C3_MHZ);
}

#[test]
fn c3_pin_ranges_and_console_ports() {
    let mut e = Emu::boot_c3("c3-blink");
    e.run_c3(300_000);
    unsafe {
        assert_eq!(esp32sim_gpio_set_input(e.0, 21, 1), 0);
        assert_eq!(esp32sim_gpio_set_input(e.0, 22, 1), 1);          // the C3 has GPIO0..21
        assert_eq!(esp32sim_pin_state(e.0, 22), 0);
        assert_eq!(esp32sim_pin_watch(e.0, 0x3f, 0), 0);
        assert_eq!(esp32sim_uart_take(e.0, 2), 0);                   // no third UART
        let msg = b"x";
        assert_eq!(esp32sim_uart_in(e.0, 1, msg.as_ptr(), 1), 1);    // UART1 exists
        assert_eq!(esp32sim_uart_in(e.0, 2, msg.as_ptr(), 1), 0);
    }
}

#[test]
fn c3_a_host_reset_reboots_the_chip_and_keeps_the_clock() {
    let mut e = Emu::boot_c3("c3-blink");
    let (_, _, text) = e.run_c3(600_000);
    assert_eq!(text.matches("Ready").count(), 1, "{text}");
    let before = e.cycles();
    assert_eq!(unsafe { esp32sim_reset(e.0) }, 0);
    let (_, _, text) = e.run_c3(600_000);
    assert_eq!(text.matches("Ready").count(), 1, "{text}");
    assert!(e.cycles() >= before + 600_000 * C3_MHZ);
}