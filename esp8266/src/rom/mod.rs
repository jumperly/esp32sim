//! The ESP8266 mask ROM, modelled in Rust: Espressif publishes no ROM ELF for this chip, so its
//! functions are entered by pc. Every symbol of the Arduino core's `eagle.rom.addr.v6.ld` is in
//! `table::ENTRIES`; a call to one (`call0` into 0x4000_0000..0x4001_0000) reaches `dispatch`
//! through `Soc::function_hook`, which reads the arguments (a2..a7, then the stack), does the work
//! and returns through a0 with the result in a2 (call0 ABI). The addresses are facts of the linker
//! script; the behaviour follows the SDK and core headers cited at each function.
//!
//! An entry without a model logs `[emu] rom: unimplemented <name> (<addr>) from <call site>` once
//! and returns 0; with `RomState::strict` it stops the run there instead (`SocBus::take_stop`).
//!
//! 0x4000_F000..0x4000_FF00 holds no ROM symbol; this model keeps it for addresses of its own
//! (`ENTRY_RETURN`, and later the PHY function table and the return points of calls into the guest).
//! Hooking all of it also covers every bucket of the machine's pc bloom, so every pc in the ROM
//! reaches `dispatch`, including the ones outside the table.
pub mod boot;
mod libc;
mod print;
mod spi;
pub mod table;

use crate::bus::{SocBus, ROM_HIGH, ROM_LOW};
use emu_core::Core;
use esp_soc::Stop;
use std::sync::OnceLock;
use xtensa_lx7::Lx106;

pub use table::ENTRIES;

/// A ROM symbol: where the linker script puts it.
#[derive(Clone, Copy, Debug)]
pub struct RomEntry { pub addr: u32, pub name: &'static str }

/// A modelled ROM function: it returns to the guest itself (`ret`) or moves its pc.
pub type Handler = fn(&mut Lx106, &mut SocBus);

/// This model's own addresses in the ROM (see the module comment).
pub const PRIVATE_LOW: u32 = 0x4000_F000;
pub const PRIVATE_HIGH: u32 = 0x4000_FF00;
/// a0 of the boot image's entry: reaching it means the image returned to the ROM.
pub const ENTRY_RETURN: u32 = 0x4000_FE04;

/// What the modelled ROM keeps between calls.
pub struct RomState {
    /// stop the run at an entry without a model instead of returning 0 from it
    pub strict: bool,
    /// entries met without a model (or pcs in the ROM that are no entry): (address, call site)
    pub unknown: Vec<(u32, u32)>,
    /// the loader runs at the reset vector: set by every reset, cleared when it has run
    pub boot_pending: bool,
    /// `ets_install_putc1`: the character function of `ets_printf` (0: none)
    pub putc1: u32,
    /// `uart_buff_switch`: the UART of `ets_putc` and `ets_uart_putc1`
    pub uart_sel: usize,
    /// cycles an `ets_delay_us` in progress still has to spend
    delay_left: u64,
    stop: Option<Stop>,
    /// one-time log lines already written
    logged: Vec<&'static str>,
}

impl Default for RomState {
    fn default() -> Self {
        RomState { strict: false, unknown: Vec::new(), boot_pending: true, putc1: print::ETS_UART_PUTC1, uart_sel: 0, delay_left: 0, stop: None, logged: Vec::new() }
    }
}

impl RomState {
    /// A chip reset: the ROM starts over (the strict setting is the host's and stays).
    pub fn reset(&mut self) { *self = RomState { strict: self.strict, unknown: std::mem::take(&mut self.unknown), ..Default::default() }; }
    /// The stop a ROM function asked for; reading it clears it.
    pub fn take_stop(&mut self) -> Option<Stop> { self.stop.take() }
    /// End the run after this call (`SocBus::take_stop`).
    pub fn request_stop(&mut self) { self.stop = Some(Stop::Halted); }
    /// Write `msg` once per `key`.
    pub fn log_once(&mut self, key: &'static str, msg: impl FnOnce() -> String) {
        if !self.logged.contains(&key) { self.logged.push(key); eprintln!("[emu] rom: {}", msg()); }
    }
}

/// The ROM symbol at `addr`, if any.
pub fn name_at(addr: u32) -> Option<&'static str> { index(addr).map(|i| ENTRIES[i].name) }

/// The table entry at `addr`: the modelled one where two names share it.
fn index(addr: u32) -> Option<usize> {
    let first = ENTRIES.partition_point(|e| e.addr < addr);
    let same = (first..ENTRIES.len()).take_while(|&i| ENTRIES[i].addr == addr);
    let mut found = None;
    for i in same {
        if handlers()[i].is_some() { return Some(i); }
        found.get_or_insert(i);
    }
    found
}

/// The modelled functions, by `ENTRIES` index.
fn handlers() -> &'static [Option<Handler>] {
    static H: OnceLock<Vec<Option<Handler>>> = OnceLock::new();
    H.get_or_init(|| ENTRIES.iter().map(|e| IMPLEMENTED.iter().find(|(n, _)| *n == e.name).map(|&(_, h)| h)).collect())
}

/// Every pc the machine must offer to `dispatch`: the ROM's entries and the private range.
pub fn hooks() -> &'static [u32] {
    static H: OnceLock<Vec<u32>> = OnceLock::new();
    H.get_or_init(|| {
        let mut v: Vec<u32> = ENTRIES.iter().map(|e| e.addr).filter(|a| (ROM_LOW..ROM_HIGH).contains(a)).collect();
        v.dedup();
        v.extend((PRIVATE_LOW..PRIVATE_HIGH).step_by(4));
        v
    })
}

/// The ROM function at the core's pc. False for a pc outside the ROM: the guest's own code.
pub fn dispatch(core: &mut Lx106, bus: &mut SocBus) -> bool {
    let pc = core.pc();
    if !(ROM_LOW..ROM_HIGH).contains(&pc) { return false; }
    if pc == ENTRY_RETURN {
        eprintln!("[emu] rom: the boot image's entry returned to the ROM (a2={:#x})", core.get_ar(2));
        bus.rom.request_stop();
        return true;
    }
    match index(pc) {
        Some(i) if pc < PRIVATE_LOW => match handlers()[i] {
            Some(run) => run(core, bus),
            None => unimplemented(core, bus, pc, ENTRIES[i].name),
        },
        _ => {
            let from = call_site(core);
            eprintln!("[emu] rom: execution at {pc:#010x}, which is no ROM entry (from {from:#010x}): stopping");
            if !bus.rom.unknown.iter().any(|&(a, _)| a == pc) { bus.rom.unknown.push((pc, from)); }
            bus.rom.request_stop();
        }
    }
    true
}

/// Where the call came from: a call0/callx0 is 3 bytes before its return address.
fn call_site(core: &Lx106) -> u32 { core.get_ar(0).wrapping_sub(3) }

fn unimplemented(core: &mut Lx106, bus: &mut SocBus, pc: u32, name: &str) {
    let from = call_site(core);
    let first = !bus.rom.unknown.iter().any(|&(a, _)| a == pc);
    if first { bus.rom.unknown.push((pc, from)); }
    if bus.rom.strict {
        eprintln!("[emu] rom: unimplemented {name} ({pc:#010x}) from {from:#010x}: stopping (strict ROM)");
        bus.rom.request_stop();
        return;
    }
    if first { eprintln!("[emu] rom: unimplemented {name} ({pc:#010x}) from {from:#010x}: returns 0"); }
    ret(core, bus, 0);
}

/// Argument `n` of the call (a2 + n, the stack from the seventh).
fn arg(core: &Lx106, bus: &mut SocBus, n: usize) -> u32 { core.arg(bus, n) }

/// Return `v` to the caller.
fn ret(core: &mut Lx106, bus: &mut SocBus, v: u32) { core.return_from_stub(bus, v); }

/// Return a 64-bit `v` in a2 (low) and a3 (high).
fn ret64(core: &mut Lx106, bus: &mut SocBus, v: u64) { core.set_ar(3, (v >> 32) as u32); ret(core, bus, v as u32); }

// ------------------------------------------------------------------ small functions

/// `ets_delay_us(us)`: busy-wait. The core spins here one cycle per call until the time has
/// passed, so device time moves with it as it would under the ROM's loop.
fn ets_delay_us(core: &mut Lx106, bus: &mut SocBus) {
    if bus.rom.delay_left == 0 { bus.rom.delay_left = u64::from(arg(core, bus, 0)) * (crate::periph::CPU_HZ / 1_000_000); }
    if bus.rom.delay_left <= 1 { bus.rom.delay_left = 0; ret(core, bus, 0); return; }
    bus.rom.delay_left -= 1;
    core.advance_cycles(1);
}

/// `Cache_Read_Enable(map, p, v)` (cores/esp8266/mmu_iram.cpp:61-79; `SocBus::cache_read_enable`).
fn cache_read_enable(core: &mut Lx106, bus: &mut SocBus) {
    let (map, p, v) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    bus.cache_read_enable(map, p, v);
    ret(core, bus, 0);
}

fn cache_read_disable(core: &mut Lx106, bus: &mut SocBus) { bus.cache_enabled = false; ret(core, bus, 0); }

/// `rtc_get_reset_reason()`: the reset cause in the ROM's numbering (`boot::RST_*`).
fn rtc_get_reset_reason(core: &mut Lx106, bus: &mut SocBus) { let c = bus.reset_cause; ret(core, bus, c); }

/// `software_reset()`: the chip resets with the ROM's SW_RESET cause. The core stays here; the
/// machine reboots it.
fn software_reset(_core: &mut Lx106, bus: &mut SocBus) {
    bus.reset_cause = boot::RST_SW;
    bus.sw_reset = true;
}

/// The ROM's watchdog API (`ets_wdt_enable/disable/get_mode/init/restore`): no watchdog yet.
fn ets_wdt_nop(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

/// The modelled functions by ROM symbol name.
const IMPLEMENTED: &[(&str, Handler)] = &[
    ("_ResetVector", boot::reset_vector), ("_xtos_unhandled_exception", boot::unhandled_exception),
    ("ets_printf", print::ets_printf), ("ets_vprintf", print::ets_vprintf), ("ets_uart_printf", print::ets_uart_printf),
    ("ets_putc", print::ets_putc), ("ets_write_char", print::ets_write_char), ("ets_uart_putc1", print::ets_uart_putc1),
    ("uart_tx_one_char", print::uart_tx_one_char),
    ("ets_install_putc1", print::ets_install_putc1), ("ets_install_uart_printf", print::ets_install_uart_printf),
    ("rom_uart_div_modify", print::rom_uart_div_modify), ("Uart_Init", print::uart_init), ("uart_buff_switch", print::uart_buff_switch),
    ("ets_delay_us", ets_delay_us),
    ("ets_memcpy", libc::memcpy), ("memcpy", libc::memcpy), ("ets_memmove", libc::memmove), ("memmove", libc::memmove),
    ("ets_memset", libc::memset), ("memset", libc::memset), ("ets_memcmp", libc::memcmp), ("memcmp", libc::memcmp),
    ("ets_bzero", libc::bzero), ("bzero", libc::bzero),
    ("ets_strlen", libc::strlen), ("strlen", libc::strlen), ("ets_strcpy", libc::strcpy), ("strcpy", libc::strcpy),
    ("ets_strncpy", libc::strncpy), ("strncpy", libc::strncpy), ("ets_strcmp", libc::strcmp), ("strcmp", libc::strcmp),
    ("ets_strncmp", libc::strncmp), ("strncmp", libc::strncmp), ("ets_strstr", libc::strstr), ("strstr", libc::strstr),
    ("__divsi3", libc::divsi3), ("__udivsi3", libc::udivsi3), ("__umodsi3", libc::umodsi3), ("__umulsidi3", libc::umulsidi3),
    ("__muldi3", libc::muldi3), ("__divdi3", libc::divdi3), ("__udivdi3", libc::udivdi3), ("__umoddi3", libc::umoddi3),
    ("SPIRead", spi::spi_read), ("SPIWrite", spi::spi_write), ("SPIEraseSector", spi::spi_erase_sector), ("SPIEraseBlock", spi::spi_erase_block),
    ("Wait_SPI_Idle", spi::ok), ("SPI_read_status", spi::spi_read_status), ("SPI_write_status", spi::ok), ("SPI_write_enable", spi::ok),
    ("SPIParamCfg", spi::ok), ("spi_flash_attach", spi::ok),
    ("Cache_Read_Enable", cache_read_enable), ("Cache_Read_Disable", cache_read_disable),
    ("rtc_get_reset_reason", rtc_get_reset_reason), ("software_reset", software_reset),
    ("ets_wdt_enable", ets_wdt_nop), ("ets_wdt_disable", ets_wdt_nop), ("ets_wdt_get_mode", ets_wdt_nop),
    ("ets_wdt_init", ets_wdt_nop), ("ets_wdt_restore", ets_wdt_nop),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_linker_scripts_and_every_model_names_one_of_its_entries() {
        assert_eq!(ENTRIES.len(), 348);
        assert!(ENTRIES.windows(2).all(|w| w[0].addr <= w[1].addr));
        assert_eq!(name_at(0x4000_0080), Some("_ResetVector"));
        assert_eq!(name_at(0x4000_4b1c), Some("SPIRead"));
        // ets_post and ets_post_rom share an address
        assert_eq!(name_at(0x4000_0e24).map(|n| n.starts_with("ets_post")), Some(true));
        for (name, _) in IMPLEMENTED { assert!(ENTRIES.iter().any(|e| e.name == *name), "{name} is no ROM symbol"); }
        let hooks = hooks();
        assert!(hooks.contains(&0x4000_0080) && hooks.contains(&ENTRY_RETURN) && !hooks.contains(&0x3fffc000));
        // the private range alone covers every bucket of the machine's pc bloom
        assert_eq!((PRIVATE_LOW..PRIVATE_HIGH).step_by(4).fold(0u64, |m, pc| m | emu_core::core::pc_bit(pc)), u64::MAX);
    }
}
