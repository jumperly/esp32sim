//! Boot from the modelled mask ROM: an Arduino ESP8266 3.1.2 Blink image (`generic`, 4M1M),
//! eboot at 0 and the app at 0x1000, as `elf2bin.py` writes it. The image is the committed
//! fixture `fixtures/bin/esp8266-blink.bin` next to this workspace (`ESP8266_BLINK_BIN` or
//! `JUMPERLY_FIXTURES_DIR` point elsewhere).
use esp8266::{rom, Machine, Stop};
use emu_core::Core;
use esp_soc::{RunUntil, SocBus};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::env::var_os("ESP8266_BLINK_BIN").filter(|_| name == "esp8266-blink").map(PathBuf::from)
        .or_else(|| std::env::var_os("JUMPERLY_FIXTURES_DIR").map(|d| PathBuf::from(d).join(format!("bin/{name}.bin"))))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/bin/{name}.bin")));
    println!("fixture: {}", path.display());
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (an Arduino ESP8266 3.1.2 image)", path.display()))
}

/// A machine with `name` in its flash, booting from the ROM, unknown ROM entries stopping it.
fn machine(name: &str) -> Machine {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.console.capture = true;
    m.bus.rom.strict = true;
    m.write_flash(0, &fixture(name)).unwrap();
    m.boot_rom();
    m
}

/// The unimplemented ROM entries the run met, with their call sites.
fn unknown(m: &Machine) -> Vec<String> {
    m.bus.rom.unknown.iter().map(|&(a, from)| format!("{} ({a:#010x}) from {from:#010x}", rom::name_at(a).unwrap_or("?"))).collect()
}

/// Run until `seconds` of device time; a stop before that fails the test with what the run met.
fn run_for(m: &mut Machine, seconds: f64) {
    let target = (seconds * esp8266::periph::CPU_HZ as f64) as u64;
    loop {
        match m.run_until_cycle(target) {
            RunUntil::Reached => { m.drain_console(); println!("{:.3} s of device time: {} instructions", m.seconds(), m.insns()); return; }
            RunUntil::Yield => continue,
            RunUntil::Stop(s) => panic!("stop {s:?} at {:.3} s, pc {:#010x}, unknown ROM entries {:?}\nUART0:\n{}",
                                        m.seconds(), Core::pc(&m.cores[0]), unknown(m), String::from_utf8_lossy(&m.console.uart0)),
        }
    }
}

#[test]
fn rom_boot_loads_eboot_and_app_and_prints_the_banner() {
    let mut m = machine("esp8266-blink");
    let stop = m.run(2_000_000);
    let text = String::from_utf8_lossy(&m.console.uart0).into_owned();
    println!("stop: {stop:?} after {} instructions\nUART0:\n{text}", m.insns());
    // the ROM's banner and its loader's lines for eboot (2 segments: IRAM text, DRAM data)
    assert!(text.contains("ets Jan  8 2013,rst cause:1, boot mode:(3,6)\r\n\r\n"), "{text}");
    assert!(text.contains("load 0x4010f000, len 3424, room 16 \r\ntail 0\r\nchksum "), "{text}");
    assert!(text.contains("load 0x3fff20b8, len 40, room 8 \r\ntail 0\r\nchksum 0x2b\r\ncsum 0x2b\r\n"), "{text}");
    // eboot: print_version ("v%08x\n": the app's first IROM word) and '~' for "no command,
    // load the app", then "ld" before it reads the app through SPIRead
    let eboot = &text[text.find("csum 0x2b\r\n").unwrap() + 11..];
    assert!(eboot.starts_with('v'), "{eboot:?}");
    assert!(eboot.contains("\r\n~ld\r\n"), "{eboot:?}");
    // the app runs (VECBASE points at its vectors), with no ROM entry this model lacks
    assert!(matches!(stop, Stop::MaxInsns), "{stop:?}, unknown ROM entries {:?}", unknown(&m));
    assert_eq!(m.cores[0].state().vecbase, 0x4010_0000, "call_user_start sets VECBASE");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
}

/// Blink in strict mode for 3 s of device time: the SDK starts, `setup()` prints `Ready`, `loop()`
/// toggles GPIO2 every 500 ms and prints `t=<millis> led=1|0`.
///
/// The tolerance of the period: `delay(500)` is `esp_delay(500)` (core_esp8266_main.cpp:152-160),
/// an `os_timer` armed for 500 ms whose expiry wakes the loop task; the SDK's timer counts in
/// milliseconds on FRC2, so one period is 500 ms of timer plus what one pass of `loop()` and the
/// scheduler take before the next GPIO write (a `printf` of ~20 characters into the UART FIFO,
/// tens of microseconds at 80 MHz), which stays well inside one timer millisecond: 500 000 us
/// +- 1 000 us.
#[test]
fn blink_reaches_setup_and_toggles_gpio2() {
    let mut m = machine("esp8266-blink");
    m.bus.observe_gpio(true);
    run_for(&mut m, 3.0);
    let text = String::from_utf8_lossy(&m.console.uart0).into_owned();
    println!("UART0:\n{text}");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("Ready\n"), "{text}");
    assert!(text.contains(" led=1\n") && text.contains(" led=0\n"), "{text}");
    assert!(text.lines().any(|l| l.starts_with("t=") && l.ends_with(" led=1")), "{text}");
    // GPIO2's output changes: the setup's HIGH, then one toggle per loop()
    let toggles: Vec<(u64, bool)> = m.bus.take_gpio_events().into_iter().filter(|e| e.1 == 2).map(|e| (e.0, e.2)).collect();
    println!("GPIO2: {:?}", toggles.iter().map(|&(c, l)| (c / 80, l)).collect::<Vec<_>>());
    assert!(toggles.len() >= 6, "{toggles:?}");
    assert!(toggles.windows(2).all(|w| w[0].1 != w[1].1), "every event is a change");
    let periods: Vec<u64> = toggles[1..].windows(2).map(|w| (w[1].0 - w[0].0) / 80).collect();
    println!("periods (us): {periods:?}");
    assert!(periods.iter().all(|&p| p.abs_diff(500_000) <= 1_000), "{periods:?}");
    assert_eq!(m.bus.gpio_output() >> 2 & 1, u32::from(toggles.last().unwrap().1));
}

/// GPIO0 low at reset release (`--strap 1`): the ROM prints its banner and waits for the download
/// tool, which is not modelled; the core waits with every interrupt masked and device time runs
/// on (no stop), until a reset with GPIO0 released boots from flash.
#[test]
fn uart_download_mode_waits_for_a_reset() {
    let mut m = machine("esp8266-blink");
    m.bus.set_strap(1);
    run_for(&mut m, 0.5);
    let text = String::from_utf8_lossy(&m.console.uart0).into_owned();
    assert!(text.contains("rst cause:1, boot mode:(1,6)") && !text.contains("Ready"), "{text}");
    assert!(m.seconds() >= 0.5);
    m.bus.strap = None;
    m.bus.request_reset(rom::boot::RST_EXT);
    m.reboot();
    run_for(&mut m, 1.0);
    let text = String::from_utf8_lossy(&m.console.uart0).into_owned();
    assert!(text.contains("rst cause:2, boot mode:(3,6)") && text.contains("Ready"), "{text}");
}

/// A stop the ROM asks for is told from the end of a run's budget (`RomState::halted`), its line
/// waits in the notes for a front end without stderr, and the chip stays stopped: later runs end at
/// once, without another line, until a reset.
#[test]
fn a_rom_stop_stays_until_a_reset_and_its_line_waits_in_the_notes() {
    let mut image = fixture("esp8266-blink");
    // eboot's checksum: the last byte of its 16-byte block (2 segments of 3424 and 40 bytes)
    let at = (8 + 8 + 3424 + 8 + 40) | 15;
    image[at] ^= 0xff;
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.write_flash(0, &image).unwrap();
    m.boot_rom();
    assert!(matches!(m.run(1_000_000), Stop::Halted));
    let reason = m.bus.rom.halted().expect("the ROM stopped the chip").to_string();
    assert!(reason.starts_with("flash boot failed: image checksum"), "{reason}");
    let notes = m.bus.rom.take_notes();
    assert_eq!(notes.last().map(String::as_str), Some(format!("[emu] rom: {reason}").as_str()), "{notes:?}");
    let (pc, insns, cycles) = (Core::pc(&m.cores[0]), m.insns(), m.bus.cycles);
    for _ in 0..3 { assert!(matches!(m.run(1_000_000), Stop::Halted)); }
    assert_eq!((Core::pc(&m.cores[0]), m.insns(), m.bus.cycles), (pc, insns, cycles), "standing still");
    assert!(m.bus.rom.take_notes().is_empty(), "no line again");
    assert!(m.bus.rom.halted().is_some());
    // a reset (the RST pad) boots again: the same failure, one line
    m.bus.chip_reset(rom::boot::RST_EXT);
    assert!(matches!(m.run(1_000_000), Stop::SwReset));
    m.reboot();
    assert!(m.bus.rom.halted().is_none(), "the reset cleared it");
    assert!(matches!(m.run(1_000_000), Stop::Halted));
    assert_eq!(m.bus.rom.take_notes().iter().filter(|l| l.contains("flash boot failed")).count(), 1);
}

/// An unmodelled ROM entry outside strict mode returns 0 with one line, which the notes keep
/// (the newest 64 of them).
#[test]
fn an_unmodelled_rom_entry_leaves_a_note_naming_it() {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.bus.rom.boot_pending = false;
    m.bus.load_bytes(0x4010_0100, &[0x06, 0xff, 0xff /* j . */]).unwrap();
    let c = &mut m.cores[0];
    c.set_pc(rom::addr_of("ets_timer_arm"));
    c.set_ar(0, 0x4010_0100);
    assert!(matches!(m.run(100), Stop::MaxInsns));
    assert_eq!(Core::pc(&m.cores[0]), 0x4010_0100, "returned");
    let notes = m.bus.rom.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with(&format!("[emu] rom: unimplemented ets_timer_arm ({:#010x}) from ", rom::addr_of("ets_timer_arm"))), "{notes:?}");
    for i in 0..100 { m.bus.rom.note(format!("line {i}")); }
    let notes = m.bus.rom.take_notes();
    assert_eq!((notes.len(), notes[0].as_str(), notes[63].as_str()), (rom::NOTES_CAP, "[emu] rom: line 36", "[emu] rom: line 99"));
}
