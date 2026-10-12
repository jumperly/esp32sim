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
/// A merged image of `flash_mb`; a sketch built with FQBN options may have its own bootloader and
/// partition table (`fixtures/bin/<app>.bootloader.bin`, emulator/fixtures/build.sh).
fn merged_sized(app: &str, flash_mb: usize) -> Vec<u8> {
    let mut flash = vec![0xffu8; flash_mb << 20];
    let own = |f: &str| if emulator_dir().join(format!("fixtures/bin/{app}.{f}")).exists() { format!("fixtures/bin/{app}.{f}") } else { format!("fixtures/bin/{f}") };
    for (off, file) in [(0x0, own("bootloader.bin")), (0x8000, own("partitions.bin")), (0x10000, format!("fixtures/bin/{app}.bin"))] {
        let d = read(&file);
        flash[off..off + d.len()].copy_from_slice(&d);
    }
    flash
}

struct Emu(*mut esp32sim_wasm::Emu);
impl Emu {
    fn boot(app: &str) -> Emu { Emu::boot_with(app, |_| {}) }
    /// Boot `app`, calling `setup` on the emulator after the images are loaded and before boot.
    fn boot_with(app: &str, setup: impl FnOnce(*mut esp32sim_wasm::Emu)) -> Emu { Emu::boot_sized(app, 4, 0, setup) }
    /// `boot_with` on a chip with `flash_mb` of flash (the image padded to it) and `psram_mb` of PSRAM.
    fn boot_sized(app: &str, flash_mb: u32, psram_mb: u32, setup: impl FnOnce(*mut esp32sim_wasm::Emu)) -> Emu {
        let board = "none";
        // SAFETY: the board name is readable for the call; the pointer is checked for null.
        let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), flash_mb, psram_mb) };
        assert!(!e.is_null());
        let emu = Emu(e);
        let (rom, flash) = (read("rom/esp32s3_rev0_rom.elf"), merged_sized(app, flash_mb as usize));
        // SAFETY: `e` is live and exclusively ours; the buffers are readable for each call.
        unsafe {
            assert_eq!(esp32sim_jumperly_mode(e), 0);
            assert_eq!(esp32sim_load(e, 0, rom.as_ptr(), rom.len()), 0);
            assert_eq!(esp32sim_load(e, 5, flash.as_ptr(), flash.len()), 0);
        }
        setup(e);
        // SAFETY: as above.
        unsafe { assert_eq!(esp32sim_boot(e, 0), 0) };
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
    }, |_host, _tx, _rx| {});
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
    set_native_bus_host(|_, _, _| 0, |host, tx, rx| { assert_eq!(host, 2); for (r, t) in rx.iter_mut().zip(tx) { *r = !t; } });
    let mut e = Emu::boot("spi");
    assert_eq!(unsafe { esp32sim_bus_host(e.0, 0, 1) }, 0);
    let text = run_with_bus_service(&mut e, "spi=");
    assert!(text.contains("spi=5a edcb"), "{text}");
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 102) }, 13);               // MISO on GPIO13
}

/// Jumperly patch J18: the `spi3` fixture drives an ILI9341 on SPI2 (FSPI) and a microSD on SPI3
/// (`SPIClass(HSPI)`, MISO on GPIO16). Nothing answers, so the card never mounts, but its CMD0 reaches
/// the host as a controller-3 transfer and the TFT's traffic as controller-2 transfers.
#[test]
fn spi3_transfers_reach_the_host_as_controller_3() {
    use std::{cell::RefCell, rc::Rc};
    let seen = Rc::new(RefCell::new((Vec::new(), Vec::new())));               // (SPI2 bytes, SPI3 bytes)
    let s = seen.clone();
    set_native_bus_host(|_, _, _| 0, move |host, tx, _rx| {
        let mut s = s.borrow_mut();
        match host { 2 => s.0.extend_from_slice(tx), 3 => s.1.extend_from_slice(tx), _ => panic!("controller {host}") }
    });
    let mut e = Emu::boot("spi3");
    assert_eq!(unsafe { esp32sim_bus_host(e.0, 0, 1) }, 0);
    let text = run_with_bus_service(&mut e, "mount failed");
    assert!(text.contains("tft ready") && text.contains("mount failed"), "{text}");
    let cmd0 = [0x40u8, 0, 0, 0, 0, 0x95];
    let (spi2, spi3) = &*seen.borrow();
    assert!(spi3.windows(6).any(|w| w == cmd0), "CMD0 on SPI3");
    assert!(!spi2.windows(6).any(|w| w == cmd0), "no CMD0 on SPI2");
    assert!(spi2.len() > 16, "the TFT's init went over SPI2");
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 67) }, 16);                // SPI3_Q on GPIO16
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 102) }, 13);               // FSPIQ on GPIO13
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

// ------------------------------------------------------------------ Jumperly patch J19: UART pins

/// The matrix output signal in a pin state word (bits 16..24).
fn signal_of(e: &Emu, pin: u32) -> u32 { (unsafe { esp32sim_pin_state(e.0, pin) } >> 16) & 0x1ff }

/// `esp32sim_uart_pins` as (TX, RX), 255 for none.
fn uart_pins(e: &Emu, port: u32) -> (u32, u32) {
    let v = unsafe { esp32sim_uart_pins(e.0, port) };
    (v & 0xff, v >> 8 & 0xff)
}

/// `serial12` runs Serial1 (RX 16, TX 17) and Serial2 (RX 4, TX 5): what they send comes out of
/// console ports 1 and 2, and lines the host puts in come back on UART0. GPIO17 is UART1's own
/// IO_MUX TX pad, so U1TXD leaves there without the matrix; RX on GPIO16 and all of UART2 go through
/// the matrix (U1RXD 15, U2TXD / U2RXD 18).
#[test]
fn serial1_and_serial2_on_matrix_pins() {
    let mut e = Emu::boot("serial12");
    let (_, text) = e.run_for(400_000 * MHZ);
    assert!(text.contains("Ready"), "{text}");
    let (one, two) = (e.uart(1), e.uart(2));
    assert!(one.contains("one 0
") && one.contains("one 2
"), "{one}");
    assert!(two.contains("two 0
") && !two.contains("one"), "{two}");
    assert_eq!(signal_of(&e, 5), 18);                                      // U2TXD_OUT on the matrix
    assert_eq!(unsafe { (esp32sim_gpio_in_pin(e.0, 15), esp32sim_gpio_in_pin(e.0, 18)) }, (16, 4));
    assert_eq!(uart_pins(&e, 0), (43, 44));                                // UART0 on its IO_MUX pads
    assert_eq!(uart_pins(&e, 1), (17, 16));
    assert_eq!(uart_pins(&e, 2), (5, 4));
    assert_eq!(uart_pins(&e, 3), (255, 255));
    unsafe {
        assert_eq!(esp32sim_uart_in(e.0, 1, b"hi
".as_ptr(), 3), 3);
        assert_eq!(esp32sim_uart_in(e.0, 2, b"yo
".as_ptr(), 3), 3);
    }
    let (_, text) = e.run_for(50_000 * MHZ);
    assert!(text.contains("rx1=hi") && text.contains("rx2=yo"), "{text}");
}

/// The C3's input selection has SIG_IN_SEL at bit 6 and the pad in bits 0..4 (the S3: bit 7, 0..5).
/// `c3-serial1` runs Serial1 on RX 3, TX 4.
#[test]
fn c3_serial1_on_matrix_pins() {
    let mut e = Emu::boot_c3("c3-serial1");
    let (_, _, text) = e.run_c3(400_000);
    assert!(text.contains("Ready"), "{text}");
    let one = e.uart(1);
    assert!(one.contains("one 0\n") && one.contains("one 2\n"), "{one}");
    assert_eq!(signal_of(&e, 4), 9);                                       // U1TXD_OUT
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 9) }, 3);                // U1RXD_IN reads GPIO3
    assert_eq!(unsafe { esp32sim_gpio_in_pin(e.0, 6) }, 255);              // UART0 RX on its IO_MUX pad
    assert_eq!(uart_pins(&e, 0), (21, 20));
    assert_eq!(uart_pins(&e, 1), (4, 3));
    assert_eq!(uart_pins(&e, 2), (255, 255));
    assert_eq!(unsafe { esp32sim_uart_in(e.0, 1, b"hi\n".as_ptr(), 3) }, 3);
    let (_, _, text) = e.run_c3(50_000);
    assert!(text.contains("rx1=hi"), "{text}");
}

// ------------------------------------------------------------------ Jumperly patch J11: Wi-Fi

/// Attach the virtual AP from fields (`esp32sim_wifi_ap`); returns its result code.
fn wifi_ap(e: *mut esp32sim_wasm::Emu, ssid: &str, psk: &str, channel: u32) -> u32 {
    // SAFETY: `e` is live and exclusively ours; the strings are readable for the call.
    unsafe { esp32sim_wifi_ap(e, ssid.as_ptr(), ssid.len(), psk.as_ptr(), psk.len(), channel) }
}

/// The SSID `esp32sim_wifi_probe_take` hands over, if any.
fn probed(e: &mut Emu) -> Option<String> {
    let n = unsafe { esp32sim_wifi_probe_take(e.0) } as usize;
    (n > 0).then(|| String::from_utf8_lossy(&e.buf(n)).into_owned())
}

/// Run until UART0 shows `text`, at most `max_us` of device time; the probed SSIDs on the way.
fn run_until(e: &mut Emu, text: &str, max_us: u64) -> (String, Vec<String>) {
    let (mut out, mut ssids) = (String::new(), Vec::new());
    let end = e.cycles() + max_us * MHZ;
    while e.cycles() < end && !out.contains(text) {
        let (_, t) = e.run_for(10_000 * MHZ);
        out += &t;
        ssids.extend(probed(e));
    }
    (out, ssids)
}

#[test]
fn wifi_ap_rejects_bad_fields_before_anything_is_attached() {
    let board = "none";
    let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
    assert!(!e.is_null());
    assert_eq!(wifi_ap(e, "", "", 6), 1);
    assert_eq!(wifi_ap(e, "Jumperly", "short", 6), 1);
    assert_eq!(wifi_ap(e, "Jumperly", "", 15), 1);
    assert_eq!(wifi_ap(e, "a,b=c", "pass,word=1", 1), 0);
    assert_eq!(unsafe { esp32sim_wifi_probe_take(e) }, 0, "nothing probed yet");
    unsafe { esp32sim_delete(e) };
}

#[test]
fn a_sketch_joining_another_network_shows_its_ssid_and_joins_once_the_ap_matches() {
    let mut e = Emu::boot_with("wifi-home", |e| assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0));
    let (_, ssids) = run_until(&mut e, "connected", 4_000_000);
    assert!(ssids.iter().any(|s| s == "Home"), "probed {ssids:?}");
    assert!(ssids.iter().all(|s| s == "Home"), "probed {ssids:?}");
    drop(e);
    let mut e = Emu::boot_with("wifi-home", |e| assert_eq!(wifi_ap(e, "Home", "home-pass-123", 6), 0));
    let (text, ssids) = run_until(&mut e, "connected ip=10.0.2.15", 8_000_000);
    assert!(text.contains("connected ip=10.0.2.15"), "{text}");
    assert!(ssids.is_empty(), "probed {ssids:?}");
}

// ------------------------------------------------------------------ Jumperly patch J13: the host's network

/// One record of `esp32sim_net_take`: (op, flags, port, id, data).
type NetOp = (u8, u8, u16, u32, Vec<u8>);

fn net_take(e: &mut Emu) -> Vec<NetOp> {
    let n = unsafe { esp32sim_net_take(e.0) } as usize;
    let b = e.buf(unsafe { esp32sim_buf_len(e.0) });
    let (mut out, mut at) = (Vec::new(), 0);
    for _ in 0..n {
        let r = &b[at..];
        let len = u32::from_le_bytes(r[8..12].try_into().unwrap()) as usize;
        out.push((r[0], r[1], u16::from_le_bytes([r[2], r[3]]), u32::from_le_bytes(r[4..8].try_into().unwrap()), r[12..12 + len].to_vec()));
        at += (12 + len).next_multiple_of(4);
    }
    assert_eq!(at, b.len());
    out
}

fn net_event(e: &mut Emu, kind: u32, id: u32, arg: u32, data: &[u8]) -> u32 {
    // SAFETY: `e` is live and exclusively ours; `data` is readable for the call.
    unsafe { esp32sim_net_event(e.0, kind, id, arg, data.as_ptr(), data.len()) }
}

/// A scripted internet: names resolve (example.com to 93.184.215.14, mcu1.jumperly.internal to the
/// first board, AAAA to nothing), every TCP
/// connection opens, and a request is answered `HTTP/1.0 200 OK` with `hi`, then EOF.
#[derive(Default)]
struct ScriptedHost { ops: Vec<NetOp>, requests: std::collections::HashMap<u32, Vec<u8>> }
const HTTP_REPLY: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhi";

impl ScriptedHost {
    fn serve(&mut self, e: &mut Emu) {
        for op in net_take(e) {
            let (kind, _, port, id, data) = op.clone();
            match kind {
                1 => assert_eq!(net_event(e, 1, id, 65536, &[]), 1),
                2 => {
                    let req = self.requests.entry(id).or_default();
                    let complete = req.windows(4).any(|w| w == b"\r\n\r\n");
                    req.extend_from_slice(&data);
                    if !complete && req.windows(4).any(|w| w == b"\r\n\r\n") {
                        assert_eq!(net_event(e, 3, id, 0, HTTP_REPLY), HTTP_REPLY.len() as u32);
                        assert_eq!(net_event(e, 4, id, 0, &[]), 1);
                    }
                }
                8 => {
                    let name = String::from_utf8(data).unwrap();
                    let addrs: &[u8] = match (name.as_str(), port) { (_, 28) => &[], ("example.com", _) => &[93, 184, 215, 14], ("mcu1.jumperly.internal", _) => &[10, 0, 2, 15], _ => &[203, 0, 113, 7] };
                    assert_eq!(net_event(e, 8, id, 60, addrs), 1);
                }
                _ => {}
            }
            self.ops.push(op);
        }
    }
}

#[test]
fn net_host_needs_an_access_point_and_without_it_nothing_is_queued() {
    let board = "none";
    let e = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
    assert!(!e.is_null());
    assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 1);
    assert_eq!(unsafe { esp32sim_net_take(e) }, 0);
    assert_eq!(unsafe { esp32sim_net_event(e, 1, 1, 0, std::ptr::null(), 0) }, 0);
    assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
    assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 0);
    assert_eq!(unsafe { esp32sim_wifi_stat(e, 0) }, 0.0);
    assert_eq!(unsafe { esp32sim_net_event(e, 99, 1, 0, std::ptr::null(), 0) }, 0, "unknown kind");
    assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
    assert_eq!(unsafe { esp32sim_net_event(e, 9, 1, 3, std::ptr::null(), 0) }, 0, "a new access point drops the host transport");
    unsafe { esp32sim_delete(e) };
}

#[test]
fn wifi_net_reaches_a_scripted_internet_through_the_host() {
    let mut e = Emu::boot_with("wifi-net", |e| {
        assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
        assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 0);
    });
    let mut host = ScriptedHost::default();
    let mut text = String::new();
    let end = e.cycles() + 30_000_000 * MHZ;
    while e.cycles() < end && !text.contains("http GET") {
        let (_, t) = e.run_for(10_000 * MHZ);
        text += &t;
        host.serve(&mut e);
    }
    assert!(text.contains("ip=10.0.2.15 gw=10.0.2.2 dns=10.0.2.3"), "{text}");
    assert!(text.contains("dns: example.com -> 93.184.215.14"), "{text}");
    assert!(text.contains("time: 20"), "SNTP answered locally: {text}");
    assert!(text.contains("http GET http://example.com/ -> 200"), "{text}");
    let kinds: Vec<u8> = host.ops.iter().map(|o| o.0).collect();
    assert!(!kinds.contains(&5), "no UDP through the host (DNS by RESOLVE, SNTP local): {kinds:?}");
    let connect = host.ops.iter().find(|o| o.0 == 1).expect("no TCP_CONNECT").clone();
    assert_eq!((connect.2, connect.4.as_slice()), (80, &[93, 184, 215, 14][..]));
    let request = String::from_utf8_lossy(&host.requests[&connect.3]).into_owned();
    assert!(request.starts_with("GET / HTTP/1.1\r\n") && request.contains("Host: example.com"), "{request}");
    let raw = e.0;
    let stat = |which| unsafe { esp32sim_wifi_stat(raw, which) };
    assert_eq!((stat(0), stat(1)), (1.0, 1.0), "joined, leased");
    assert_eq!(stat(3), request.len() as f64);
    assert_eq!(stat(4), HTTP_REPLY.len() as f64);
    assert!(stat(6) >= 2.0, "example.com and pool.ntp.org");
    assert_eq!(stat(5), 0.0);
    // http.end() closes the flow: the guest's FIN, then the NAT forgets it.
    for _ in 0..100 {
        if host.ops.iter().any(|o| o.0 == 4 && o.3 == connect.3) { break; }
        e.run_for(10_000 * MHZ);
        host.serve(&mut e);
    }
    let closes: Vec<_> = host.ops.iter().filter(|o| o.3 == connect.3 && matches!(o.0, 3 | 4)).map(|o| (o.0, o.1)).collect();
    assert_eq!(closes, vec![(3, 0), (4, 0)], "shutdown, then a graceful close");
    assert_eq!(stat(2), 0.0);
}

// ------------------------------------------------------------------ Jumperly patch J14: the LAN between boards

const LAN_LATENCY_US: u64 = 500;

fn set_mac(e: *mut esp32sim_wasm::Emu, mac: [u8; 6]) -> u32 { unsafe { esp32sim_set_mac(e, mac.as_ptr()) } }

/// The packets of `esp32sim_lan_take`: (device µs, IPv4 packet).
fn lan_take(e: &mut Emu) -> Vec<(f64, Vec<u8>)> {
    let n = unsafe { esp32sim_lan_take(e.0) } as usize;
    let b = e.buf(unsafe { esp32sim_buf_len(e.0) });
    let (mut out, mut at) = (Vec::new(), 0);
    for _ in 0..n {
        let t = f64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let len = u32::from_le_bytes(b[at + 8..at + 12].try_into().unwrap()) as usize;
        out.push((t, b[at + 12..at + 12 + len].to_vec()));
        at += (12 + len).next_multiple_of(4);
    }
    assert_eq!(at, b.len());
    out
}

/// A board of the test network and the packets due to it: (device µs, packet).
struct Board { e: Emu, ip: [u8; 4], inbox: Vec<(u64, Vec<u8>)>, text: String, host: Option<ScriptedHost> }
impl Board {
    fn now_us(&self) -> u64 { self.e.cycles() / MHZ }
}

fn group(ip: &[u8]) -> bool { ip == [10, 0, 2, 255] || ip == [255; 4] || ip[0] & 0xf0 == 224 }

/// Run every board to `until_us` in lockstep slices of `slice_us`, a packet reaching its peers
/// `LAN_LATENCY_US` after it was sent, by their own clocks, until `done` holds.
fn run_lan(boards: &mut [Board], slice_us: u64, until_us: u64, done: impl Fn(&[Board]) -> bool) {
    let mut t = boards.iter().map(Board::now_us).max().unwrap();
    while t < until_us && !done(boards) {
        t += slice_us;
        for i in 0..boards.len() {
            loop {
                let b = &mut boards[i];
                let target = b.inbox.iter().map(|p| p.0).min().map_or(t, |due| due.min(t));
                let start = b.now_us();
                if start < target {
                    let left = (target * MHZ).saturating_sub(b.e.cycles()).min(u32::MAX as u64) as u32;
                    assert_eq!(b.e.run(left.max(1)), 0);
                    b.text += &b.e.uart(0);
                    b.e.pin_events();
                    if let Some(h) = &mut b.host { h.serve(&mut b.e); }
                }
                let now = b.e.cycles() as f64 / MHZ as f64;
                let sent = lan_take(&mut boards[i].e);
                for (stamp, packet) in sent {
                    assert!(stamp >= start as f64 - 1.0 && stamp <= now + 1.0, "stamp {stamp} outside {start}..{now}");
                    for (j, peer) in boards.iter_mut().enumerate() {
                        if j == i || !(packet[16..20] == peer.ip || group(&packet[16..20])) { continue; }
                        let due = stamp as u64 + LAN_LATENCY_US;
                        assert!(due >= peer.now_us(), "a packet due at {due} reaches a board already at {}", peer.now_us());
                        peer.inbox.push((due, packet.clone()));
                    }
                }
                let b = &mut boards[i];
                let now = b.now_us();
                let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut b.inbox).into_iter().partition(|p| p.0 <= now);
                b.inbox = later;
                for (_, packet) in due {
                    assert_eq!(unsafe { esp32sim_lan_receive(b.e.0, packet.as_ptr(), packet.len()) }, 0);
                }
                if now >= t { break; }
            }
        }
    }
}

fn lan_board(app: &str, ip: [u8; 4], mac_last: u8, peers: &[[u8; 4]], host: bool, cpi: u32) -> Board {
    let e = Emu::boot_with(app, |e| {
        if cpi > 1 { assert_eq!(unsafe { esp32sim_set_approximate_jit_timing(e, cpi, 256 / cpi) }, 0); }
        assert_eq!(set_mac(e, [0x44, 0x1b, 0xf6, 0x75, 0xdc, mac_last]), 0);
        assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
        assert_eq!(unsafe { esp32sim_net_station(e, ip.as_ptr()) }, 0);
        let flat: Vec<u8> = peers.concat();
        assert_eq!(unsafe { esp32sim_lan_peers(e, flat.as_ptr(), peers.len() as u32) }, 0);
        if host { assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 0); }
    });
    Board { e, ip, inbox: Vec::new(), text: String::new(), host: host.then(ScriptedHost::default) }
}

#[test]
fn two_boards_talk_tcp_and_udp_over_the_lan() {
    let (server_ip, client_ip) = ([10, 0, 2, 15], [10, 0, 2, 16]);
    let mut boards = [
        lan_board("lan-server", server_ip, 0xe0, &[client_ip], false, 1),
        // the client resolves names through the host and runs at a 80 MHz CPU limit (CPI 3)
        lan_board("lan-client", client_ip, 0xe4, &[server_ip], true, 3),
    ];
    run_lan(&mut boards, 250, 20_000_000, |b| b[1].text.contains("done") && b[0].text.contains("udp: hello"));
    let (server, client) = (&boards[0], &boards[1]);
    assert!(server.text.contains("server ip=10.0.2.15 port=7000"), "{}", server.text);
    assert!(client.text.contains("wifi: connected to Jumperly ip=10.0.2.16"), "{}", client.text);
    assert_eq!(client.text.matches("lan: pong from 10.0.2.15").count(), 2, "by name and by address: {}", client.text);
    assert_eq!(server.text.matches("lan: ping from 10.0.2.16").count(), 2, "{}", server.text);
    assert!(client.text.contains("udp: hello to 10.0.2.255:7001 sent"), "{}", client.text);
    assert!(server.text.contains("udp: hello from 10.0.2.16"), "the broadcast reached the server: {}", server.text);
    // the name went to the host, the connections did not: the LAN comes before the NAT
    let ops = &client.host.as_ref().unwrap().ops;
    assert!(ops.iter().any(|o| o.0 == 8 && o.4 == b"mcu1.jumperly.internal"), "no RESOLVE");
    assert!(!ops.iter().any(|o| o.0 == 1), "a TCP_CONNECT reached the host");
    for b in &boards {
        let stat = |which| unsafe { esp32sim_wifi_stat(b.e.0, which) };
        assert!(stat(7) > 0.0 && stat(8) > 0.0, "LAN packets out {} in {}", stat(7), stat(8));
    }
}

#[test]
fn the_station_mac_reaches_the_firmware_and_is_fixed_at_boot() {
    let mut e = Emu::boot_with("net-mqtt", |e| {
        assert_eq!(set_mac(e, [0x44, 0x1b, 0xf6, 0x75, 0xdc, 0xe8]), 0);
        assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
        assert_eq!(unsafe { esp32sim_net_station(e, [10, 0, 2, 17].as_ptr()) }, 0);
    });
    let (text, _) = run_until(&mut e, "mqtt: connecting", 8_000_000);
    assert!(text.contains("ip=10.0.2.17"), "{text}");
    assert!(text.contains("as jumperly-75dce8"), "{text}");
    assert_eq!(set_mac(e.0, [2; 6]), 1, "after boot");
    assert_eq!(unsafe { esp32sim_net_station(e.0, [10, 0, 2, 18].as_ptr()) }, 1, "after boot");
    let board = "none";
    let fresh = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
    assert_eq!(unsafe { esp32sim_net_station(fresh, [10, 0, 2, 18].as_ptr()) }, 1, "no access point");
    assert_eq!(unsafe { esp32sim_lan_peers(fresh, [10, 0, 2, 18].as_ptr(), 1) }, 1, "no access point");
    assert_eq!(unsafe { esp32sim_lan_take(fresh) }, 0);
    assert_eq!(unsafe { esp32sim_lan_receive(fresh, [0u8; 20].as_ptr(), 20) }, 1);
    unsafe { esp32sim_delete(fresh) };
}

// ------------------------------------------------------------------ Jumperly patch J15: the host connects to the guest

#[test]
fn the_host_connects_to_a_guest_server_and_a_closed_port_refuses() {
    let mut e = Emu::boot_with("lan-server", |e| {
        assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
        assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 0);
    });
    let (mut text, _) = run_until(&mut e, "server ip=", 10_000_000);
    assert!(text.contains("server ip=10.0.2.15 port=7000"), "{text}");
    const REFUSED: u32 = 0x8000_0001;
    const ID: u32 = 0x8000_0002;
    let from = [203, 0, 113, 9, 0x50, 0xc3]; // 203.0.113.9:50000, port little-endian
    assert_eq!(net_event(&mut e, 10, REFUSED, 81, &from), 1, "nobody listens on 81");
    assert_eq!(net_event(&mut e, 10, ID, 7000, &from), 1);
    assert_eq!(net_event(&mut e, 10, ID, 7000, &from), 0, "the id is taken");
    assert_eq!(net_event(&mut e, 10, ID + 1, 7000, &from[..4]), 0, "short data");
    let mut ops: Vec<NetOp> = Vec::new();
    let step = |e: &mut Emu, ops: &mut Vec<NetOp>, text: &mut String| {
        let (_, t) = e.run_for(10_000 * MHZ);
        *text += &t;
        ops.extend(net_take(e));
    };
    for _ in 0..200 {
        if ops.iter().filter(|o| o.0 == 9).count() == 2 { break; }
        step(&mut e, &mut ops, &mut text);
    }
    assert!(ops.contains(&(9, 0, 0, REFUSED, vec![])), "{ops:?}");
    assert!(ops.contains(&(9, 1, 0, ID, vec![])), "{ops:?}");
    assert_eq!(unsafe { esp32sim_wifi_stat(e.0, 2) }, 1.0, "one flow open");
    assert_eq!(net_event(&mut e, 3, ID, 0, b"ping\n"), 5);
    for _ in 0..300 {
        if ops.iter().any(|o| o.0 == 2 && o.3 == ID) { break; }
        step(&mut e, &mut ops, &mut text);
    }
    assert_eq!(net_event(&mut e, 4, ID, 0, &[]), 1, "the host is done");
    for _ in 0..300 {
        if ops.iter().any(|o| o.0 == 4 && o.3 == ID) { break; }
        step(&mut e, &mut ops, &mut text);
    }
    let flow: Vec<_> = ops.iter().filter(|o| o.3 == ID).map(|o| (o.0, o.1, o.4.clone())).collect();
    assert_eq!(flow, vec![(9, 1, vec![]), (2, 0, b"pong\n".to_vec()), (3, 0, vec![]), (4, 0, vec![])], "accepted, the answer, shutdown, close");
    assert!(!ops.iter().any(|o| o.3 == REFUSED && o.0 != 9), "nothing follows a refusal: {ops:?}");
    assert!(text.contains("lan: client 10.0.2.2"), "the guest sees the gateway: {text}");
    assert!(text.contains("lan: ping from 10.0.2.2"), "{text}");
    let stat = |which| unsafe { esp32sim_wifi_stat(e.0, which) };
    assert_eq!((stat(2), stat(3), stat(4)), (0.0, 5.0, 5.0));
}

// ------------------------------------------------------------------ Jumperly patch J13: bulk TCP through the host

/// What `bulk_transfer` measured: bytes per device second and per wall second, each way.
#[derive(Debug)]
struct Bulk { down_dev: f64, down_wall: f64, up_dev: f64, up_wall: f64 }

/// `net-bulk` against a host that streams `down` bytes (i % 251) to port 9001 as fast as the
/// emulator takes them, then EOF, and takes everything uploaded to port 9002 at once (credit back
/// for every byte). The download's device time is the sketch's own `millis()` until it read the
/// last byte; the upload's runs from the connect to the host's last byte.
fn bulk_transfer(down: u32, cpi: u32) -> Bulk {
    let mut e = Emu::boot_with("net-bulk", |e| {
        if cpi > 1 { assert_eq!(unsafe { esp32sim_set_approximate_jit_timing(e, cpi, 256 / cpi) }, 0); }
        assert_eq!(wifi_ap(e, "Jumperly", "", 6), 0);
        assert_eq!(unsafe { esp32sim_net_host(e, 0b111) }, 0);
    });
    let (mut text, _) = run_until(&mut e, "wifi: connected", 20_000_000);
    let (mut down_id, mut up_id, mut sent, mut eof, mut got) = (None, None, 0u32, false, 0u32);
    let (mut down_wall, mut up_wall) = (None::<std::time::Instant>, None::<std::time::Instant>);
    let (mut down_secs, mut up_secs, mut up_from, mut up_us) = (0.0, 0.0, 0u64, 0u64);
    let chunk: Vec<u8> = (0..16384u32).map(|i| (i % 251) as u8).collect();
    let end = e.cycles() + 600_000_000 * MHZ;
    while e.cycles() < end && !(text.contains("done") && got >= 262_144) {
        let (_, t) = e.run_for(1_000 * MHZ);
        text += &t;
        if text.contains("down: ") && down_secs == 0.0 { down_secs = down_wall.map_or(0.0, |w| w.elapsed().as_secs_f64()); }
        for (kind, _, port, id, data) in net_take(&mut e) {
            match (kind, port) {
                (1, 9001) => { down_id = Some(id); down_wall = Some(std::time::Instant::now()); assert_eq!(net_event(&mut e, 1, id, 65536, &[]), 1); }
                (1, 9002) => { up_id = Some(id); up_from = e.cycles(); up_wall = Some(std::time::Instant::now()); assert_eq!(net_event(&mut e, 1, id, 65536, &[]), 1); }
                (2, _) if Some(id) == up_id => {
                    for (i, b) in data.iter().enumerate() { assert_eq!(*b, ((got as usize + i) % 251) as u8, "upload byte {}", got as usize + i); }
                    got += data.len() as u32;
                    if got == 262_144 { up_us = (e.cycles() - up_from) / MHZ; up_secs = up_wall.unwrap().elapsed().as_secs_f64(); }
                    assert_eq!(net_event(&mut e, 6, id, data.len() as u32, &[]), 1);
                }
                _ => {}
            }
        }
        if let Some(id) = down_id {
            while sent < down {
                let at = (sent % 251) as usize;
                let n = (down - sent).min(16384 - 251) as usize;
                let taken = net_event(&mut e, 3, id, 0, &chunk[at..at + n]);
                sent += taken;
                if (taken as usize) < n { break; }
            }
            if sent == down && !eof { eof = true; assert_eq!(net_event(&mut e, 4, id, 0, &[]), 1); }
        }
    }
    let line = |prefix: &str| text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` in {text}")).to_string();
    let field = |l: &str, after: &str| -> f64 { l.split(after).nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap() };
    let (d, u) = (line("down: "), line("up: "));
    assert_eq!(d, format!("down: {down} bytes in {} ms bad=0", field(&d, " in ")), "{text}");
    assert_eq!((field(&u, "up: ") as u32, got), (262_144, 262_144), "{text}");
    Bulk {
        down_dev: f64::from(down) / (field(&d, " in ") / 1e3), down_wall: f64::from(down) / down_secs,
        up_dev: f64::from(got) / (up_us as f64 / 1e6), up_wall: f64::from(got) / up_secs,
    }
}

/// The NAT and the queue are not what limits a sketch's throughput: measured 2026-10-07 (release)
/// 2.5 MB/s down and 0.68 MB/s up at 240 MHz, 0.8 and 0.33 MB/s at 40 MHz (CPI 6).
#[test]
fn bulk_tcp_through_the_host_both_ways() {
    for cpi in [1, 6] {
        let b = bulk_transfer(262_144, cpi);
        assert!(b.down_dev > 100e3 && b.up_dev > 100e3, "{b:?}");
        eprintln!("net-bulk at {} MHz: down {:.0} KB/s device, {:.0} KB/s wall; up {:.0} KB/s device, {:.0} KB/s wall",
            240 / cpi, b.down_dev / 1e3, b.down_wall / 1e3, b.up_dev / 1e3, b.up_wall / 1e3);
    }
}

/// Jumperly patch J16: ESP32-S3-WROOM-2 firmware (core 3.3.12, FlashMode=opi, PSRAM=opi; the
/// octal16 / octal32 fixtures) on an octal flash. The bootloader and the app take the octal path
/// (efuse FLASH_TYPE, Macronix ID, WRCR2 to DTR OPI) and the sketch sees the module's flash and
/// PSRAM: the ID and sizes, a PSRAM fill-and-check of the whole heap block (16 MB with no wrap at
/// 8 MB), erase / program / read at the chip's last sector (past 16 MB on 32 MB) and in the spiffs
/// partition (read back through the cache too). It restarts once: the ROM resets the flash out of
/// DTR OPI and the second boot passes the same checks.
#[test]
fn octal_flash_modules_run_opi_firmware() {
    for (app, flash_mb, psram_mb, id) in [("octal16", 16, 8, "c28038"), ("octal32", 32, 16, "c28039")] {
        let mut emu = Emu::boot_sized(app, flash_mb, psram_mb, |e| assert_eq!(unsafe { esp32sim_flash_octal(e, 1) }, 0));
        let mut text = String::new();
        for _ in 0..40 {
            text += &emu.run_for(50_000 * MHZ).1;
            if text.matches("done").count() == 2 { break; }
        }
        let (flash, psram) = ((flash_mb as usize) << 20, (psram_mb as usize) << 20);
        let what = format!("{app} with {psram_mb} MB PSRAM: {text}");
        assert_eq!(text.matches("done").count(), 2, "{what}");
        assert!(text.contains("Octal Flash Mode Enabled"), "the ROM sees the efuse: {what}");
        for again in [0, 1] {
            let boot = text.split(&format!("boot again={again}")).nth(1).unwrap_or_else(|| panic!("boot {again}: {what}"));
            let boot = boot.split("done").next().unwrap();
            assert!(boot.contains(&format!("flash id={id} chip={flash} physical={flash}")), "{what}");
            assert!(boot.contains(&format!("psram size={psram} heap={psram}")), "{what}");
            assert!(boot.contains("bad=0"), "{what}");
            for ok in ["flash end=ok", "flash spiffs=ok", "app magic=e9"] { assert!(boot.contains(ok), "{ok}: {what}"); }
        }
    }
    // The same octal16 image on the quad flash model: ESP-IDF's flash mode auto-detection
    // (CONFIG_ESPTOOLPY_FLASH_MODE_AUTO_DETECT) falls back to DIO, so it runs, as a quad module.
    let mut emu = Emu::boot_sized("octal16", 16, 8, |_| {});
    let text = emu.run_for(300_000 * MHZ).1;
    assert!(text.contains("flash id=204018 chip=16777216") && text.contains("done") && !text.contains("Octal Flash Mode"), "{text}");
}

/// Jumperly patch J16: octal flash is set before boot, on the ESP32-S3 only.
#[test]
fn flash_octal_is_refused_after_boot() {
    let emu = Emu::boot("blink");
    assert_eq!(unsafe { esp32sim_flash_octal(emu.0, 1) }, 1);
}

// ------------------------------------------------------------------ Jumperly patch J17: I2S RX

/// The I2S RX format `esp32sim_i2s_rx_need` reports: (frames needed, the eight u32 of the buffer).
fn i2s_need(e: &mut Emu, port: u32, cycles: u32) -> (u32, [u32; 8]) {
    let n = unsafe { esp32sim_i2s_rx_need(e.0, port, cycles) };
    let b = e.buf(I2S_RX_INFO_BYTES);
    (n, std::array::from_fn(|i| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap())))
}

/// Run the `i2s-mic` fixture (ESP_I2S, RX master on GPIO4/5/6, 16 kHz, 32-bit mono left) for
/// `ms` of device time in 1 ms slices, feeding I2S0 from `word(frame index) -> (left, right)`
/// before every slice as the web adapter does. Returns the UART0 text and the last format seen.
fn run_i2s_mic(host: bool, ms: u64, word: impl Fn(u64) -> [u32; 2]) -> (String, [u32; 8]) {
    let mut e = Emu::boot("i2s-mic");
    assert_eq!(unsafe { esp32sim_i2s_host(e.0, u32::from(host)) }, 0);
    let (mut text, mut fmt, mut k) = (String::new(), [0; 8], 0u64);
    let end = e.cycles() + ms * 1_000 * MHZ;
    while e.cycles() < end {
        let slice = (1_000 * MHZ) as u32;
        let (n, f) = i2s_need(&mut e, 0, slice);
        if f[0] != 0 && f[5] & 1 != 0 { fmt = f; }
        if host && n > 0 {
            let mut bytes = Vec::with_capacity(n as usize * 8);
            for _ in 0..n { for w in word(k) { bytes.extend_from_slice(&w.to_le_bytes()); } k += 1; }
            assert_eq!(unsafe { esp32sim_i2s_rx_push(e.0, 0, bytes.as_ptr(), n as usize) }, n);
        }
        assert_eq!(e.run(slice), 0);
        text += &e.uart(0);
    }
    (text, fmt)
}

/// The values of `key=` in the fixture's last complete report line.
fn last_report(text: &str, key: &str) -> f64 {
    let line = text.lines().rev().find(|l| l.starts_with("n=1600 ")).unwrap_or_else(|| panic!("no full report in {text}"));
    let v = line.split(' ').find_map(|f| f.strip_prefix(key)).unwrap_or_else(|| panic!("{key} in {line}"));
    v.parse().unwrap_or(f64::NEG_INFINITY)
}

#[test]
fn i2s_mic_reports_the_level_of_the_host_sine_on_the_left_slot() {
    // a 1 kHz tone with a 24-bit peak of 420 426 (−26 dBFS, the INMP441 at 94 dB SPL) in the left
    // slot, a full-scale square in the right one the receiver must ignore
    let (text, fmt) = run_i2s_mic(true, 600, |k| {
        let s = (420_426.0 * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / 16_000.0).sin()).round() as i32;
        [(s << 8) as u32, 0x7fff_ff00]
    });
    assert!(text.contains("Ready"), "{text}");
    assert_eq!(fmt, [16_000, 32, 32, 1, 2, 0b11, fmt[6], 1_024_000], "rate, slot bits, data bits, left slot, 2 slots, running Philips, queued, BCK");
    let dbfs = last_report(&text, "dbfs=");
    assert!((dbfs + 26.0).abs() < 0.2, "{text}");
    let peak = last_report(&text, "peak=");
    assert!((peak - 420_426.0).abs() < 2.0, "{text}");
}

#[test]
fn i2s_mic_without_the_host_reads_silence_at_16_khz() {
    let (text, fmt) = run_i2s_mic(false, 400, |_| [0x7fff_ff00; 2]);
    assert_eq!(&fmt[..6], [16_000, 32, 32, 1, 2, 0b11]);
    assert!(text.contains("n=1600 rms=0 peak=0 dbfs=-inf"), "{text}");
}

#[test]
fn i2s_host_is_s3_only_and_bad_ports_need_nothing() {
    let mut e = Emu::boot("blink");
    assert_eq!(i2s_need(&mut e, 0, 1_000_000).0, 0, "the receiver is off");
    assert_eq!(i2s_need(&mut e, 2, 1_000_000).0, 0);
    assert_eq!(unsafe { esp32sim_i2s_rx_push(e.0, 7, [0u8; 8].as_ptr(), 1) }, 0);
    let board = "esp32c3";
    // SAFETY: the board name is readable for the call; the emulator is deleted below.
    let c3 = unsafe { esp32sim_new(board.as_ptr(), board.len(), 4, 0) };
    assert_eq!(unsafe { esp32sim_i2s_host(c3, 1) }, 1);
    assert_eq!(unsafe { esp32sim_i2s_rx_need(c3, 0, 1000) }, 0);
    unsafe { esp32sim_delete(c3) };
}
