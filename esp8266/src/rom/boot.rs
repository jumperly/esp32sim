//! The mask ROM's boot: at the reset vector it prints its banner on UART0 at 74880 baud, reads the
//! strapping pins, and from flash loads the image at offset 0 (on Arduino builds that is eboot,
//! which loads the application at 0x1000 itself through `SPIRead`) and jumps to its entry.
//!
//! The ROM's code is not available; the banner and the per-segment lines reproduce what the real
//! ROM prints (" ets Jan  8 2013,rst cause:2, boot mode:(3,6)" after an external reset, then for
//! an Arduino 3.x image
//! "load 0x4010f000, len 3424, room 16 ", "tail 0", "chksum 0x2e", ... "csum 0x2b"). `room` and
//! `tail` are derived from those logs: `room` is the distance from a segment's data to the next
//! 16-byte boundary of the flash image (16 when it is on one), `tail` what is left of the segment
//! past that boundary modulo 16.
use super::print::rom_print;
use super::{SocBus, ENTRY_RETURN};
use emu_core::Core;
use xtensa_lx7::{state::{ps, sr}, Lx106};

/// The reset causes as the ROM numbers them (what `rtc_get_reset_reason` returns and the banner's
/// `rst cause` prints): Arduino ESP8266 3.1.2 cores/esp8266/hwdt_app_entry.cpp:580-589,
/// ROM_RST_REASON: 0 NO_MEAN, 1 POWERON_RESET, 2 EXT_RESET ("External reset or wake-up from
/// Deep-sleep"), 3 SW_RESET, 4 OWDT_RESET ("Hardware WDT reset"), 5 DEEPSLEEP_RESET ("Deep Sleep
/// reset digital core", the RTOS SDK's comment there), 6 SDIO_RESET.
/// `SocBus::reset_cause` and `request_reset` use this numbering.
///
/// EXT_RESET's comment there names the deep-sleep wake too, but a chip woken from deep sleep
/// through its RST pin reports 5: real boot logs read `rst cause:5, boot mode:(3,6)` after the
/// GPIO16 pulse, and a press of RST during deep sleep gives the same 5, which the SDK names
/// REASON_DEEP_SLEEP_AWAKE (5, user_interface.h:63), so `getResetReason()` cannot tell the two
/// apart (esp8266/Arduino issue #7067, "ESP.getResetReason() doesn't allow to distinguish between
/// timer wakeup and physical reset during deep sleep", with such a log). So an external reset is
/// 2 when the chip is awake and 5 when it sleeps (`SocBus::chip_reset`).
///
/// The SDK's `rst_info.reason`, which `ESP.getResetReason()` names (tools/sdk/include/
/// user_interface.h:58-64, cores/esp8266/Esp.cpp:486-500), is another numbering. libmain's start-up
/// (Blink image 0x4023383c..0x40233886) derives it: RTC STORE0 (0x60000730) in 1..6 means "take
/// the record in RTC memory" (the SDK writes STORE0 and the record before it restarts: 4 in
/// `system_restart_local`, 0x40234aba; 5 before a deep sleep, 0x40105f7b; 2 after an exception;
/// 3 for a soft watchdog, 0x4010197f; 1 while it runs, 0x401019a9); with STORE0 0 a ROM cause of
/// 1 is a power-on (record cleared), 2 an external reset (6) unless the record says a deep-sleep
/// wake (5), any other the record as it is.
///
/// | What happened | ROM `rst cause` | SDK reason, `getResetReason()` |
/// | --- | --- | --- |
/// | power-on | 1 | 0 `Power On` |
/// | RST pin (external reset) | 2 | 6 `External System` |
/// | `ESP.restart()`, `ESP.reset()` (a jump to `_ResetVector`) | unchanged | 4 `Software/System restart` |
/// | an exception (the core's postmortem restarts the same way) | unchanged | 2 `Exception` |
/// | soft watchdog (the SDK's, restarts the same way) | unchanged | 3 `Software Watchdog` |
/// | hardware watchdog | 4 | 1 `Hardware Watchdog` |
/// | RTC software reset (`software_reset`, RTC 0x700 bit 31) | 3 | from the RTC record |
/// | deep-sleep timer, GPIO16 wired to RST | 5 | 5 `Deep-Sleep Wake` |
///
/// "Unchanged": the jump resets no hardware, so the ROM reports the last hardware reset again
/// (hwdt_app_entry.cpp:596-597: the ROM API "does not change the status after a software
/// restart"): `rst cause:1` after a power-on, `rst cause:2` on a board a programmer's auto-reset
/// started. That is why real boards' logs show `rst cause:2` after `ESP.restart()` and crashes.
pub const RST_POWERON: u32 = 1;
pub const RST_EXT: u32 = 2;
pub const RST_SW: u32 = 3;
pub const RST_WDT: u32 = 4;
pub const RST_DEEPSLEEP: u32 = 5;

/// The ROM's name of a reset cause (hwdt_app_entry.cpp:580-589).
pub fn reset_cause_name(cause: u32) -> &'static str {
    match cause { 1 => "POWERON_RESET", 2 => "EXT_RESET", 3 => "SW_RESET", 4 => "OWDT_RESET", 5 => "DEEPSLEEP_RESET", 6 => "SDIO_RESET", _ => "?" }
}

/// Strapping, as the banner's first `boot mode` number: GPIO15 << 2 | GPIO0 << 1 | GPIO2. Flash:
/// GPIO15 low, GPIO0 high, GPIO2 high; UART download: GPIO0 low (Ai-Thinker ESP-12F datasheet
/// v1.0 table 4). The core reads the same field from GPIO_IN bits 16+ (cores/esp8266/Updater.cpp:84:
/// `(GPI >> 16) & 0xf`, 1 = UART download).
pub const STRAP_FLASH: u32 = 0b011;
pub const STRAP_UART: u32 = 0b001;

/// The stack the ROM leaves to the image it starts (eboot hands the same to the app,
/// bootloaders/eboot/eboot.c:84-86).
pub const BOOT_STACK: u32 = 0x3FFF_FFF0;

/// UART0's divisor in the boot ROM: the one for 115200 baud from an 80 MHz clock, which a 26 MHz
/// crystal (the ROM assumes 40 MHz) turns into 74880 baud on the line.
pub const UART0_BOOT_DIV: u32 = 80_000_000 / 74_880 * 26 / 40;

/// The ROM's data in DRAM that the guest uses (eagle.rom.addr.v6.ld): the per-cause exception
/// handlers the SDK's user exception vector jumps through, the "C" handlers behind
/// `_xtos_c_wrapper_handler`, and the pointer to the flash description (spi_flash.h:41-50).
const XTOS_EXC_HANDLER_TABLE: u32 = super::xtos::EXC_HANDLER_TABLE;
const XTOS_C_HANDLER_TABLE: u32 = super::xtos::C_HANDLER_TABLE;
const FLASHCHIP: u32 = 0x3fff_c714;
/// Model choice: where `flashchip` points, the word after it (the real ROM's struct address is not
/// in the linker script).
const FLASHCHIP_DATA: u32 = 0x3fff_c718;
/// Model choice, not read from the ROM: the EXCCAUSE numbers whose entry in
/// `_xtos_exc_handler_table` is not `_xtos_unhandled_exception` at reset (SYSCALL 1,
/// LEVEL1_INTERRUPT 4, ALLOCA 5, each to the ROM handler of that name). The ROM's initial table is
/// not published; it also exports `_xtos_cause3_handler` (0x40000590, eagle.rom.addr.v6.ld), so
/// its default for cause 3 (LoadStoreError) may differ from this one.
const XTOS_DEFAULTS: [(u32, &str); 3] = [(1, "_xtos_syscall_handler"), (4, "_xtos_l1int_handler"), (5, "_xtos_alloca_handler")];

/// What the ROM's reset handler leaves in its DRAM before it loads an image. Model choices where
/// the ROM's values are not published: the flash's JEDEC id is a Winbond W25Q-series one (EF 40,
/// capacity byte log2(size)), the status mask 0xFFFF.
fn rom_data_init(bus: &mut SocBus) {
    let rom = |name| super::ENTRIES.iter().find(|e| e.name == name).map(|e| e.addr).unwrap();
    let mut words: Vec<(u32, u32)> = (0..64).map(|i| {
        let handler = XTOS_DEFAULTS.iter().find(|d| d.0 == i).map_or("_xtos_unhandled_exception", |d| d.1);
        (XTOS_EXC_HANDLER_TABLE + 4 * i, rom(handler))
    }).collect();
    words.extend((0..64).map(|i| (XTOS_C_HANDLER_TABLE + 4 * i, rom("_xtos_p_none"))));
    let (size, id) = (bus.flash.len() as u32, bus.flash.jedec_id());
    words.extend([(FLASHCHIP, FLASHCHIP_DATA), (FLASHCHIP_DATA, id), (FLASHCHIP_DATA + 4, size), (FLASHCHIP_DATA + 8, crate::flash::BLOCK as u32),
                  (FLASHCHIP_DATA + 12, crate::flash::SECTOR as u32), (FLASHCHIP_DATA + 16, 0x100), (FLASHCHIP_DATA + 20, 0xffff)]);
    for (a, v) in words { bus.load_bytes(a, &v.to_le_bytes()).unwrap(); }
    super::phy::init(bus);
}

/// The image header's magic byte (bootloaders/eboot/flash.h:24-35 `image_header_t`).
const IMAGE_MAGIC: u8 = 0xE9;
/// The checksum's seed (tools/elf2bin.py `write_bin`: `checksum = 0xef`).
const CHECKSUM_SEED: u8 = 0xEF;

/// One segment of an image: load address, its data's flash offset, length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment { pub addr: u32, pub off: u32, pub len: u32 }

/// An ESP8266 image (bootloaders/eboot/flash.h:24-47, tools/elf2bin.py `write_bin`): 0xE9, the
/// segment count, flash mode, flash size/frequency, the entry; then per segment its address,
/// length and data; then padding to the last byte of a 16-byte block, which is the XOR of all
/// segment data seeded with 0xEF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image { pub entry: u32, pub segments: Vec<Segment>, pub checksum: u8, pub stored: u8 }

/// The image at flash `at`.
pub fn parse(flash: &[u8], at: usize) -> Result<Image, String> {
    let word = |o: usize| flash.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let hdr = flash.get(at..at + 8).ok_or("no image header")?;
    if hdr[0] != IMAGE_MAGIC { return Err(format!("no image: the first byte is {:#04x}, not 0xe9", hdr[0])); }
    let entry = word(at + 4).unwrap();
    let mut off = at + 8;
    let (mut segments, mut checksum) = (Vec::new(), CHECKSUM_SEED);
    for _ in 0..hdr[1] {
        let (Some(addr), Some(len)) = (word(off), word(off + 4)) else { return Err("a segment header is past the end of the flash".into()) };
        off += 8;
        let data = off.checked_add(len as usize).and_then(|end| flash.get(off..end)).ok_or_else(|| format!("segment {addr:#010x} ({len} bytes) is past the end of the flash"))?;
        checksum = data.iter().fold(checksum, |c, b| c ^ b);
        segments.push(Segment { addr, off: off as u32, len });
        off += len as usize;
    }
    let stored = *flash.get(off | 15).ok_or("the checksum is past the end of the flash")?;
    Ok(Image { entry, segments, checksum, stored })
}

/// `_ResetVector`. After a reset (`RomState::boot_pending`) it is the boot; reached by a jump of
/// the guest's, it is a request to start over: the chip resets with the cause it had.
pub fn reset_vector(core: &mut Lx106, bus: &mut SocBus) {
    if bus.rom.boot_pending { boot(core, bus); return; }
    bus.rom.note(format!("_ResetVector reached by a jump (a0={:#010x}): restart", core.get_ar(0)));
    bus.restart_from_reset_vector();
}

/// The boot from the reset vector.
pub fn boot(core: &mut Lx106, bus: &mut SocBus) {
    bus.rom.boot_pending = false;
    bus.periph.uart[0].write(0x14, UART0_BOOT_DIV);       // uart_register.h:60 UART_CLKDIV(0)
    let mode = bus.latch_strap();
    rom_data_init(bus);
    rom_print(bus, &format!("\n ets Jan  8 2013,rst cause:{}, boot mode:({mode},6)\n\n", bus.reset_cause));
    if mode != STRAP_FLASH {
        // The ROM waits for the download tool (or, in the SDIO and other modes, for its host);
        // its protocols are not modelled, so the core only waits, with every interrupt masked,
        // until the next reset (a press of RST with GPIO0 released boots from flash).
        bus.rom.note(format!("boot mode {mode} (not a flash boot): the ROM's UART download and other modes are not modelled; waiting for a reset"));
        core.write_sr(sr::INTENABLE, 0).unwrap();
        core.waiti(15);
        return;
    }
    match load(bus) {
        Ok(entry) => {
            core.set_pc(entry);
            core.set_ar(0, ENTRY_RETURN);
            core.set_ar(1, BOOT_STACK);
            // Model choice: user vector mode, no exception in progress, interrupts at level 0
            // (INTENABLE is 0 after a reset, so none is taken until the guest enables one).
            core.write_sr(sr::PS, ps::UM).unwrap();
        }
        Err(e) => bus.rom.halt(format!("flash boot failed: {e}; stopping")),
    }
}

/// Copy the image at flash 0 into RAM, printing what the ROM prints; its entry point.
fn load(bus: &mut SocBus) -> Result<u32, String> {
    let img = parse(&bus.flash.bytes, 0)?;
    let mut running = CHECKSUM_SEED;
    for s in &img.segments {
        let room = 16 - s.off % 16;
        rom_print(bus, &format!("load {:#010x}, len {}, room {room} \n", s.addr, s.len));
        let data = bus.flash.bytes[s.off as usize..(s.off + s.len) as usize].to_vec();
        bus.load_bytes(s.addr, &data).map_err(|e| format!("segment {:#010x}: {e}", s.addr))?;
        running = data.iter().fold(running, |c, b| c ^ b);
        rom_print(bus, &format!("tail {}\nchksum {running:#04x}\n", s.len.saturating_sub(room) % 16));
    }
    rom_print(bus, &format!("csum {:#04x}\n", img.checksum));
    if img.checksum != img.stored { return Err(format!("image checksum {:#04x}, stored {:#04x}", img.checksum, img.stored)); }
    Ok(img.entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image as elf2bin.py writes it.
    fn image(entry: u32, segs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut v = vec![0xe9, segs.len() as u8, 3, 0x20];
        v.extend(entry.to_le_bytes());
        let mut ck = 0xefu8;
        for (addr, data) in segs {
            v.extend(addr.to_le_bytes()); v.extend((data.len() as u32).to_le_bytes()); v.extend(*data);
            ck = data.iter().fold(ck, |c, b| c ^ b);
        }
        while (v.len() + 1) % 16 != 0 { v.push(0); }
        v.push(ck);
        v
    }

    #[test]
    fn parse_reads_segments_and_the_checksum_in_the_last_byte_of_the_block() {
        let raw = image(0x4010_f480, &[(0x4010_f000, &[1, 2, 3, 4]), (0x3fff_20b8, &[0x10; 8])]);
        let img = parse(&raw, 0).unwrap();
        assert_eq!(img.entry, 0x4010_f480);
        assert_eq!(img.segments, vec![Segment { addr: 0x4010_f000, off: 16, len: 4 }, Segment { addr: 0x3fff_20b8, off: 28, len: 8 }]);
        assert_eq!((img.checksum, img.stored), (0xef ^ 4, 0xef ^ 4));
        assert_eq!(raw.len(), 48);
        assert!(parse(&[0xff; 16], 0).is_err());
        assert!(parse(&raw[..30], 0).is_err());
    }

    #[test]
    fn the_boot_divisor_and_the_reset_numbering() {
        assert_eq!(UART0_BOOT_DIV, 694);
        assert_eq!((RST_POWERON, RST_EXT, RST_SW, RST_WDT, RST_DEEPSLEEP), (1, 2, 3, 4, 5));
    }
}
