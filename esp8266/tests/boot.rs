//! Boot from the modelled mask ROM: an Arduino ESP8266 3.1.2 Blink image (`generic`, 4M1M),
//! eboot at 0 and the app at 0x1000, as `elf2bin.py` writes it. The image is the committed
//! fixture `fixtures/bin/esp8266-blink.bin` next to this workspace (`ESP8266_BLINK_BIN` or
//! `JUMPERLY_FIXTURES_DIR` point elsewhere).
use esp8266::{rom, Machine, Stop};
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
                                        m.seconds(), emu_core::Core::pc(&m.cores[0]), unknown(m), String::from_utf8_lossy(&m.console.uart0)),
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
