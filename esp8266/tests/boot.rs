//! Boot from the modelled mask ROM: an Arduino ESP8266 3.1.2 Blink image (`generic`, 4M1M),
//! eboot at 0 and the app at 0x1000, as `elf2bin.py` writes it. The image is a local build:
//! `ESP8266_BLINK_BIN`, else `bin/esp8266-blink.bin` under `JUMPERLY_FIXTURES_DIR`, else
//! `fixtures/bin/esp8266-blink.bin` next to this workspace.
use emu_core::Core;
use esp8266::{rom, Stop};
use std::path::PathBuf;

fn blink() -> Vec<u8> {
    let path = std::env::var_os("ESP8266_BLINK_BIN").map(PathBuf::from)
        .or_else(|| std::env::var_os("JUMPERLY_FIXTURES_DIR").map(|d| PathBuf::from(d).join("bin/esp8266-blink.bin")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bin/esp8266-blink.bin"));
    println!("fixture: {}", path.display());
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (an Arduino ESP8266 3.1.2 Blink .bin; set ESP8266_BLINK_BIN)", path.display()))
}

#[test]
fn rom_boot_loads_eboot_and_app_and_prints_the_banner() {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.console.capture = true;
    m.bus.rom.strict = true;
    m.write_flash(0, &blink()).unwrap();
    m.boot_rom();
    let stop = m.run(2_000_000);
    let text = String::from_utf8_lossy(&m.console.uart0).into_owned();
    println!("stop: {stop:?} after {} instructions
UART0:
{text}", m.insns());
    // the ROM's banner and its loader's lines for eboot (2 segments: IRAM text, DRAM data)
    assert!(text.contains("ets Jan  8 2013,rst cause:1, boot mode:(3,6)\r\n\r\n"), "{text}");
    assert!(text.contains("load 0x4010f000, len 3424, room 16 \r\ntail 0\r\nchksum "), "{text}");
    assert!(text.contains("load 0x3fff20b8, len 40, room 8 \r\ntail 0\r\nchksum 0x2b\r\ncsum 0x2b\r\n"), "{text}");
    // eboot: print_version ("v%08x\n": the app's first IROM word) and '~' for "no command,
    // load the app", then "ld" before it reads the app through SPIRead
    let eboot = &text[text.find("csum 0x2b\r\n").unwrap() + 11..];
    assert!(eboot.starts_with('v'), "{eboot:?}");
    assert!(eboot.contains("\r\n~ld\r\n"), "{eboot:?}");
    // the app ran (its IRAM is loaded and VECBASE points at its vectors) until the first ROM
    // entry this model does not implement yet, which strict mode stops at: the Arduino core
    // installs its handler for byte loads from flash (core_esp8266_non32xfer.cpp)
    assert!(matches!(stop, Stop::Halted), "{stop:?}");
    assert_eq!(m.cores[0].state().vecbase, 0x4010_0000, "call_user_start sets VECBASE");
    let met: Vec<String> = m.bus.rom.unknown.iter().map(|&(a, from)| format!("{} ({a:#010x}) from {from:#010x}", rom::name_at(a).unwrap_or("?"))).collect();
    println!("unimplemented ROM entries met: {met:?}");
    assert_eq!(m.bus.rom.unknown.len(), 1, "{met:?}");
    assert_eq!(rom::name_at(m.bus.rom.unknown[0].0), Some("_xtos_set_exception_handler"), "{met:?}");
    assert_eq!(m.cores[0].pc(), 0x4000_0454, "stopped on the entry");
}
