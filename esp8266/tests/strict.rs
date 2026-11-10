//! Arduino ESP8266 3.1.2 sketches in strict ROM mode (an unmodelled ROM entry stops the run):
//! the committed fixtures `fixtures/bin/esp8266-{serial,wifi-off}.bin` next to this workspace (or
//! under `JUMPERLY_FIXTURES_DIR`), contracts in `fixtures/README.md`. `esp8266-strap` is in
//! `tests/periph.rs`, with the reset causes.
use esp8266::{rom, Machine, Stop};
use esp_soc::{RunUntil, SocBus};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::env::var_os("JUMPERLY_FIXTURES_DIR").map(|d| PathBuf::from(d).join(format!("bin/{name}.bin")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/bin/{name}.bin")));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (an Arduino ESP8266 3.1.2 image)", path.display()))
}

fn machine(name: &str) -> Machine {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.console.capture = true;
    m.bus.rom.strict = true;
    m.write_flash(0, &fixture(name)).unwrap();
    m.boot_rom();
    m
}

fn unknown(m: &Machine) -> Vec<String> {
    m.bus.rom.unknown.iter().map(|&(a, from)| format!("{} ({a:#010x}) from {from:#010x}", rom::name_at(a).unwrap_or("?"))).collect()
}

fn uart0(m: &Machine) -> String { String::from_utf8_lossy(&m.console.uart0).into_owned() }

/// Run until `seconds` of device time. A chip reset the guest asks for reboots the machine when
/// `resets` allows it; any other stop fails the test.
fn run_until(m: &mut Machine, seconds: f64, resets: bool) {
    let target = (seconds * esp8266::periph::CPU_HZ as f64) as u64;
    loop {
        match m.run_until_cycle(target) {
            RunUntil::Reached => { m.drain_console(); return; }
            RunUntil::Yield => {}
            RunUntil::Stop(Stop::SwReset) if resets => { m.drain_console(); m.reboot(); }
            RunUntil::Stop(s) => panic!("stop {s:?} at {:.3} s, unknown ROM entries {:?}\nUART0:\n{}", m.seconds(), unknown(m), uart0(m)),
        }
    }
}

/// `esp8266-serial`: UART0 echoes a line the host sends (the core's UART driver: receive FIFO,
/// interrupt 5), UART1 transmits `one <n>` every 100 ms. The sketch never idles (`loop()` ends in
/// `yield()`), so every device second is 80 M instructions: 2.5 s are enough for the contract.
#[test]
fn serial_echoes_uart0_lines_and_writes_uart1() {
    let mut m = machine("esp8266-serial");
    run_until(&mut m, 1.0, false);
    assert!(uart0(&m).contains("Ready\n"), "{}", uart0(&m));
    m.bus.serial_input(b"hello\n");
    m.sync_irq();
    run_until(&mut m, 1.5, false);
    // a line longer than the core's receive threshold (16, uart.cpp:449-457) raises RXFIFO_FULL:
    // the core's uart_isr drains the FIFO through interrupt 5
    let long = "0123456789abcdefghijklmnopqrstuvwxyz";
    m.bus.serial_input(format!("{long}\n").as_bytes());
    m.sync_irq();
    run_until(&mut m, 2.5, false);
    let (text, all) = (uart0(&m), String::from_utf8_lossy(&m.console.all).into_owned());
    println!("UART0:\n{text}\nall consoles: {} bytes", all.len());
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("rx=hello\n"), "{text}");
    assert!(text.contains(&format!("rx={long}\n")), "{text}");
    assert!(m.irq_hist[0][5] > 0, "the UART interrupt was taken");
    // `console.all` has UART0 and UART1: what UART0 does not have is UART1's
    assert!(all.contains("one 1\n") && !text.contains("one "), "{all}");
    assert!(all.contains("one 10\n"), "every 100 ms: {all}");
}

/// `esp8266-wifi-off`: `WiFi.begin("Jumperly")` with no radio. The SDK's connect never completes
/// (no frame is sent or received), so `WiFi.status()` stays WL_DISCONNECTED, which is 7 in the
/// core (cores/esp8266/wl_definitions.h:59; ESP8266WiFiSTA.cpp:555-572 maps the SDK's
/// "connecting" to it), and the sketch goes on without a crash or a reset.
#[test]
fn wifi_begin_without_a_radio_leaves_the_station_disconnected() {
    let mut m = machine("esp8266-wifi-off");
    run_until(&mut m, 6.0, false);
    let text = uart0(&m);
    println!("UART0:\n{text}");
    assert!(m.bus.rom.unknown.is_empty(), "{:?}", unknown(&m));
    assert!(text.contains("Ready\n"), "{text}");
    let status: Vec<&str> = text.lines().filter(|l| l.starts_with("status=")).collect();
    assert_eq!(status, ["status=7"; 10], "{text}");
    assert!(text.contains("alive\n"), "{text}");
    assert_eq!(m.reboots, 0);
    assert_eq!(text.matches(" ets Jan  8 2013").count(), 1, "no reset: {text}");
}
