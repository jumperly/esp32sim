//! The peripherals under Arduino ESP8266 3.1.2 sketches in strict ROM mode: the committed
//! fixtures `fixtures/bin/esp8266-{pwm,servo,strap,exc,sleep,spi,blink}.bin` next to this
//! workspace (or under `JUMPERLY_FIXTURES_DIR`), contracts in `fixtures/README.md`.
use emu_core::{Bus, Fault};
use esp8266::rom::{self, boot};
use esp8266::{periph, Machine, Stop};
use esp_soc::{RunUntil, SocBus};
use std::path::PathBuf;

const MAC: [u8; 6] = [0x5c, 0xcf, 0x7f, 0, 0x11, 0x22];
const US: u64 = periph::CPU_HZ / 1_000_000;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::env::var_os("JUMPERLY_FIXTURES_DIR").map(|d| PathBuf::from(d).join(format!("bin/{name}.bin")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/bin/{name}.bin")));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (an Arduino ESP8266 3.1.2 image)", path.display()))
}

fn machine_with(name: &str, mac: [u8; 6]) -> Machine {
    let mut m = esp8266::machine(mac, 4 << 20);
    m.console.capture = true;
    m.bus.rom.strict = true;
    m.write_flash(0, &fixture(name)).unwrap();
    m.boot_rom();
    m
}

fn machine(name: &str) -> Machine { machine_with(name, MAC) }

fn unknown(m: &Machine) -> Vec<String> {
    m.bus.rom.unknown.iter().map(|&(a, from)| format!("{} ({a:#010x}) from {from:#010x}", rom::name_at(a).unwrap_or("?"))).collect()
}

fn uart0(m: &Machine) -> String { String::from_utf8_lossy(&m.console.uart0).into_owned() }

/// Run until `seconds` of device time; a reset the chip asks for reboots it when `resets` allows,
/// `on_yield` serves what the host must (an SPI1 transaction); any other stop fails the test.
fn run_until(m: &mut Machine, seconds: f64, resets: bool, mut on_yield: impl FnMut(&mut Machine)) {
    let target = (seconds * periph::CPU_HZ as f64) as u64;
    loop {
        match m.run_until_cycle(target) {
            RunUntil::Reached => { m.drain_console(); return; }
            RunUntil::Yield => on_yield(m),
            RunUntil::Stop(Stop::SwReset) if resets => { m.drain_console(); m.reboot(); }
            RunUntil::Stop(s) => panic!("stop {s:?} at {:.3} s, unknown ROM entries {:?}\nUART0:\n{}", m.seconds(), unknown(m), uart0(m)),
        }
    }
}

fn run(m: &mut Machine, seconds: f64, resets: bool) { run_until(m, seconds, resets, |_| {}) }

/// The (cycle, level) changes of `pin`'s output between `from` and `to` seconds.
fn edges(events: &[(u64, u8, bool)], pin: u8, from: f64, to: f64) -> Vec<(u64, bool)> {
    let (a, b) = ((from * periph::CPU_HZ as f64) as u64, (to * periph::CPU_HZ as f64) as u64);
    events.iter().filter(|e| e.1 == pin && (a..b).contains(&e.0)).map(|e| (e.0, e.2)).collect()
}

/// Periods and high times of a square wave, in microseconds, from its rising edges.
fn wave(e: &[(u64, bool)]) -> Vec<(u64, u64)> {
    let start = e.iter().position(|x| x.1).expect("a rising edge");
    e[start..].windows(3).step_by(2).map(|w| ((w[2].0 - w[0].0) / US, (w[1].0 - w[0].0) / US)).collect()
}

/// `esp8266-pwm`: `analogWrite(4, 256)` and `analogWrite(4, 768)` are both full on in core 3.1.2,
/// whose `analogWriteRange` defaults to 255 (cores/esp8266/core_esp8266_wiring_pwm.cpp:29) and
/// clamps the value to it (:57-58; the fixture's "25 % of 1023" assumes the 2.x range), so GPIO4
/// goes high once and stays high until `analogWrite(4, 0)` at 4 s. `tone(5, 440)` from 1 s to 3 s
/// is the waveform generator on FRC1 in the NMI (core_esp8266_waveform_pwm.cpp:102-107), which
/// toggles GPIO5 every 1136 us (Tone.cpp:35-46: high = low = half the period).
#[test]
fn pwm_fixture_drives_gpio4_full_on_and_a_440_hz_tone_on_gpio5_through_the_nmi() {
    let mut m = machine("esp8266-pwm");
    m.bus.observe_gpio(true);
    run(&mut m, 4.5, false);
    let text = uart0(&m);
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("pwm=256\n") && text.contains("pwm=768\n") && text.contains("pwm=0\n"), "{text}");
    let events = m.bus.take_gpio_events();
    let gpio4 = edges(&events, 4, 0.0, 4.5);
    println!("GPIO4: {:?}", gpio4.iter().map(|&(c, l)| (c / US, l)).collect::<Vec<_>>());
    assert_eq!(gpio4.iter().map(|e| e.1).collect::<Vec<_>>(), [true, false], "on, then off at 4 s");
    assert!((4.0..4.1).contains(&(gpio4[1].0 as f64 / periph::CPU_HZ as f64)));
    let tone = edges(&events, 5, 1.2, 2.8);
    let w = wave(&tone);
    println!("GPIO5: {} edges, first periods (us, high us) {:?}", tone.len(), &w[..w.len().min(6)]);
    assert!(w.len() > 600, "{}", w.len());
    assert!(w.iter().all(|&(p, h)| p.abs_diff(2273) <= 10 && h.abs_diff(1136) <= 10), "{w:?}");
    assert!(edges(&events, 5, 3.1, 4.5).is_empty(), "noTone at 3 s");
    assert!(m.irq_hist[0][periph::INUM_NMI as usize] > 1000, "the waveform runs in the NMI");
}

/// `esp8266-servo`: `Servo` on GPIO14 through the same waveform generator: 50 Hz, 1000 us at 0
/// degrees, 1500 us at 90 (libraries/Servo/src/Servo.h:54-56: 1000..2000 us).
#[test]
fn servo_fixture_pulses_1500us_at_50hz() {
    let mut m = machine("esp8266-servo");
    m.bus.observe_gpio(true);
    run(&mut m, 1.8, false);
    let text = uart0(&m);
    assert!(text.contains("servo=0\n") && text.contains("servo=90\n"), "{text}");
    let events = m.bus.take_gpio_events();
    let at0 = wave(&edges(&events, 14, 0.2, 0.9));
    let at90 = wave(&edges(&events, 14, 1.2, 1.8));
    println!("0 deg: {:?}\n90 deg: {:?}", &at0[..at0.len().min(4)], &at90[..at90.len().min(4)]);
    assert!(at0.len() >= 30 && at90.len() >= 25, "{} {}", at0.len(), at90.len());
    assert!(at0.iter().all(|&(p, h)| p.abs_diff(20_000) <= 10 && h.abs_diff(1000) <= 10), "{at0:?}");
    assert!(at90.iter().all(|&(p, h)| p.abs_diff(20_000) <= 10 && h.abs_diff(1500) <= 10), "{at90:?}");
}

/// `esp8266-strap` after power-on: `ESP.restart()` is a jump to the ROM's reset vector, which
/// leaves the ROM's cause as it was (POWERON_RESET, `rst cause:1` again), while the SDK reads its
/// own record, REASON_SOFT_RESTART, from RTC STORE0 and RTC memory, which the restart kept
/// (libmain `system_restart_local`, Blink image 0x40234aba..0x40234ad0). The word the sketch
/// left in RTC user memory survives too, so it does not restart again.
#[test]
fn strap_fixture_reports_power_on_then_software_restart() {
    let mut m = machine("esp8266-strap");
    run(&mut m, 3.0, true);
    let text = uart0(&m);
    println!("UART0:\n{text}");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert_eq!(m.reboots, 1, "one restart: {text}");
    let resets: Vec<&str> = text.lines().filter(|l| l.starts_with("reset=")).collect();
    assert_eq!(resets, ["reset=Power On info=Power On", "reset=Software/System restart info=Software/System restart"], "{text}");
    let banners: Vec<&str> = text.lines().filter(|l| l.starts_with(" ets Jan  8 2013,")).collect();
    assert_eq!(banners, [" ets Jan  8 2013,rst cause:1, boot mode:(3,6)"; 2], "{text}");
    assert_eq!(m.bus.read32(0x6000_1200).unwrap(), 0x4A4D_5052, "the sketch's word in RTC user memory");
}

/// `esp8266-exc` after the external reset that a programmer's auto-reset gives (EXT_RESET):
/// the store to 0 is EXCCAUSE 29, the core's postmortem prints it and restarts through the reset
/// vector, so the ROM's cause stays EXT_RESET: `rst cause:2` twice, as in real boards' logs.
#[test]
fn exc_fixture_dumps_and_restarts() {
    let mut m = machine("esp8266-exc");
    m.bus.set_reset_cause(boot::RST_EXT);
    run(&mut m, 0.35, true);
    let text = uart0(&m);
    println!("UART0:\n{text}");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("Exception (29):") && text.contains("epc1=0x4020"), "{text}");
    assert!(text.contains("excvaddr=0x00000000"), "{text}");
    assert_eq!(m.reboots, 1);
    let banners: Vec<&str> = text.lines().filter(|l| l.starts_with(" ets Jan  8 2013,")).collect();
    assert_eq!(banners, [" ets Jan  8 2013,rst cause:2, boot mode:(3,6)"; 2], "{text}");
    assert_eq!(text.matches("Ready\n").count(), 2, "{text}");
    // the SDK's record of the crash, kept in RTC STORE0 across the restart (REASON_EXCEPTION_RST)
    assert_eq!(m.bus.read32(0x6000_0730).unwrap(), 2);
}

/// `esp8266-sleep`: `ESP.deepSleep(2e6)` 1 s after the start. The SDK programs the RTC sleep timer
/// and enters deep sleep; when the timer runs out the RTC pulls GPIO16 low for 100 us. Wired to
/// RST, that pulse resets the chip (the test does what the wire would: an external reset), which
/// the ROM reports as DEEPSLEEP_RESET and the SDK, from its RTC record, as REASON_DEEP_SLEEP_AWAKE.
#[test]
fn sleep_fixture_wakes_through_gpio16_to_rst() {
    let mut m = machine("esp8266-sleep");
    m.bus.observe_gpio(true);
    run(&mut m, 1.5, false);
    assert!(m.bus.deep_sleep(), "{}", uart0(&m));
    let until = m.bus.deep_sleep_until().expect("a timer wake");
    let entered = m.bus.take_gpio_events().iter().find(|e| e.1 == 16).map(|e| e.0).expect("GPIO16 goes high at the sleep");
    let slept = (until - entered) as f64 / periph::CPU_HZ as f64;
    println!("deep sleep at {:.4} s for {slept:.4} s", entered as f64 / periph::CPU_HZ as f64);
    assert!((slept - 2.0).abs() < 0.01, "{slept}");
    let insns = m.insns();
    run(&mut m, until as f64 / periph::CPU_HZ as f64 + 0.001, false);
    assert!(m.insns() - insns < 100, "the core is off: {} instructions", m.insns() - insns);
    let pulse: Vec<(u64, bool)> = m.bus.take_gpio_events().iter().filter(|e| e.1 == 16).map(|e| (e.0, e.2)).collect();
    assert_eq!(pulse, [(until, false), (until + 100 * US, true)]);
    assert_eq!(m.bus.gpio_output() >> 16, 1);
    assert!(m.bus.deep_sleep(), "no wire to RST: it sleeps on");
    m.bus.request_reset(boot::RST_EXT);
    m.reboot();
    run(&mut m, until as f64 / periph::CPU_HZ as f64 + 0.5, false);
    let text = uart0(&m);
    println!("UART0:\n{text}");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("wake=Power On\n"), "{text}");
    assert!(text.contains(" ets Jan  8 2013,rst cause:5, boot mode:(3,6)"), "{text}");
    assert!(text.contains("wake=Deep-Sleep Wake\n"), "{text}");
    assert!(!m.bus.deep_sleep());
}

/// `esp8266-spi` with a host on SPI1 (HSPI): the first three steps are `SPI.transfer(0x55 ^ step)`
/// between a low and a high of the latch, GPIO15.
#[test]
fn spi_fixture_sends_bytes_to_host_hspi() {
    let mut m = machine("esp8266-spi");
    m.bus.spi_host = true;
    m.bus.observe_gpio(true);
    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    run_until(&mut m, 0.5, false, |m| {
        if let Some(t) = m.bus.periph.spi1.host_wait.as_ref() {
            assert_eq!(t.rx_len, 1, "full duplex");
            seen.push((m.bus.cycles, t.tx.clone()));
            m.bus.finish_spi_transfer(&[0]);
        }
    });
    let text = uart0(&m);
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("out=55 via=spi\nout=54 via=spi\nout=57 via=spi\n"), "{text}");
    assert_eq!(seen.iter().take(3).map(|s| s.1.clone()).collect::<Vec<_>>(), [vec![0x55], vec![0x54], vec![0x57]]);
    let latch = edges(&m.bus.take_gpio_events(), 15, 0.0, 0.5);
    for (at, _) in seen.iter().take(3) {
        let low = latch.iter().rev().find(|e| e.0 <= *at).expect("a latch edge before");
        let high = latch.iter().find(|e| e.0 > *at).expect("a latch edge after");
        assert!(!low.1 && high.1, "GPIO15 low around the transfer: {latch:?}");
    }
    assert_eq!(m.bus.periph.spi1.transfers, seen.len() as u64);
}

/// A byte load from the flash window is a LoadStoreError (EXCCAUSE 3) on the bus and in the core;
/// the core's emulation of it (`core_esp8266_non32xfer.cpp`) is only installed with
/// NON32XFER_HANDLER or MMU_IRAM_HEAP (:65,:183-196), which the fixtures' default build lacks.
#[test]
fn byte_read_from_flash_window_raises_cause_3() {
    use emu_core::Core;
    // before the boot: the core is not waiting for an interrupt, and the window maps flash 0
    let mut m = machine("esp8266-blink");
    assert_eq!(m.bus.read8(0x4020_1010), Err(Fault::LoadStoreError));
    assert_eq!(m.bus.read16(0x4020_1012), Err(Fault::LoadStoreError));
    assert!(m.bus.read32(0x4020_1010).is_ok());
    // l8ui a2, a3, 0 in IRAM, a3 in the flash window
    let at = 0x4010_ff00;
    m.bus.write32(at, u32::from_le_bytes([0x22, 0x03, 0x00, 0x00])).unwrap();
    let c = &mut m.cores[0];
    c.set_pc(at);
    c.set_ar(3, 0x4020_1011);
    let trap = c.step(&mut m.bus).trap();
    assert!(matches!(trap, Some(xtensa_lx7::Trap::Exception(3))), "{trap:?}");
    assert_eq!(c.state().excvaddr, 0x4020_1011);
}

/// The OTP words carry the whole MAC: a locally administered address (not Espressif's 18:fe:34)
/// is what the SDK takes as the station MAC, found in its RAM after the start.
#[test]
fn the_sdk_reads_the_machines_mac_from_the_otp_words() {
    let mac = [0x02, 0x12, 0x34, 0x56, 0x78, 0x9a];
    let mut m = machine_with("esp8266-blink", mac);
    run(&mut m, 0.2, false);
    assert!(uart0(&m).contains("Ready\n"), "{}", uart0(&m));
    assert!(m.bus.dram.windows(6).any(|w| w == mac), "the MAC is in DRAM");
}
