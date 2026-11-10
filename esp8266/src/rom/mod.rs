//! The ESP8266 mask ROM, modelled in Rust: Espressif publishes no ROM ELF for this chip, so its
//! functions are entered by pc. Every symbol of the Arduino core's `eagle.rom.addr.v6.ld` is in
//! `table::ENTRIES`; a call to one (`call0` into 0x4000_0000..0x4001_0000) reaches `dispatch`
//! through `Soc::function_hook`, which reads the arguments (a2..a7, then the stack), does the work
//! and returns through a0 with the result in a2 (call0 ABI). The addresses are facts of the linker
//! script; the behaviour follows the SDK and core headers cited at each function.
//!
//! The machine stops the core in front of every pc in the ROM (`Soc::FUNCTION_HOOK_RANGE`), not
//! through its pc bloom, so code outside the ROM runs in long stretches.
//!
//! An entry without a model logs `[emu] rom: unimplemented <name> (<addr>) from <call site>` once
//! and returns 0; with `RomState::strict` it stops the run there instead (`SocBus::take_stop`).
//!
//! Diagnostics: every `[emu] rom: ...` line goes to stderr and into `RomState::take_notes`, for a
//! front end without stderr (the browser). A stop the ROM asks for (`RomState::halt`: the boot
//! image failed to load or returned, a pc in the ROM that is no entry, an exception without a
//! handler, a strict stop) ends the run with `Stop::Halted` and stays (`RomState::halted`): the
//! core stands at that ROM pc and every later run ends there at once, silently, until a reset.
//!
//! Faults: a modelled function that walks guest memory does it through `load8`, `store8`,
//! `load32` and `store32`; an address the chip's own load or store would fault on ends the
//! function there with that exception (`fault`): LoadProhibited (28) / StoreProhibited (29),
//! EXCVADDR the address, EPC1 the function's entry (the ROM's code is not available, so the pc of
//! the faulting instruction inside it is not known; the entry is in the same function). What the
//! function wrote before the fault stays written; it may differ from what the ROM's loop had
//! written by then, which is not known either. A byte access to IRAM or the flash window, which is
//! a LoadStoreError (3) for the guest's own `l8ui` / `s8i`, works here: whether the ROM's code
//! makes byte or word accesses there is not known.
//!
//! 0x4000_F000..0x4000_FF00 holds no ROM symbol; this model keeps it for addresses of its own:
//! the PHY function stubs (`phy::STUBS`, 0x4000_F000..), the return points of calls into the
//! guest (`xtos::GUEST_RET`, 0x4000_FD00..), `ENTRY_RETURN`, `DELAY_LOOP` and the scheduler's
//! loop (`sched::RUN_LOOP`).
//! Hooking all of it also covers every bucket of the machine's stub bloom, which it tests when the
//! core stops in front of the ROM, so every pc there reaches `dispatch`, including the ones outside
//! the table.
pub mod boot;
mod crypto;
mod gpio;
mod libc;
mod libgcc;
pub mod phy;
mod print;
pub mod sched;
mod spi;
pub mod table;
pub mod xtos;

use crate::bus::{SocBus, ROM_HIGH, ROM_LOW};
use emu_core::{Bus, Core, Fault};
use esp_soc::Stop;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use xtensa_lx7::{state::exc, Lx106};

pub use table::ENTRIES;
pub use xtos::GuestCall;

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
/// The busy-wait of `ets_delay_us`: a3 holds CCOUNT at the call, a4 the cycles to wait.
pub const DELAY_LOOP: u32 = 0x4000_FE08;

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
    /// calls into the guest waiting for their return, innermost last
    pub calls: Vec<GuestCall>,
    /// `ets_isr_attach`: handler and argument by interrupt number
    pub isr: [(u32, u32); 16],
    /// `ets_task`: by priority
    pub tasks: [sched::Task; sched::PRIORITIES],
    /// `ets_set_idle_cb`: function and argument
    pub idle: (u32, u32),
    /// the idle callback ran since the scheduler last waited
    pub idle_called: bool,
    /// the stack `ets_run` was called on
    pub run_sp: u32,
    /// `ets_update_cpu_frequency`: MHz, what `ets_delay_us` counts in
    pub cpu_mhz: u32,
    /// the RF analog registers of `rom_i2c_*`, by (block, host, register)
    pub i2c: HashMap<u32, u8>,
    /// PHY table slots already logged
    pub phy_seen: HashSet<u32>,
    stop: Option<Stop>,
    /// `halt`: the reason of the stop the ROM asked for; cleared by a reset
    halted: Option<String>,
    /// one-time log lines already written
    logged: Vec<&'static str>,
    /// the diagnostic lines not yet taken (`take_notes`), oldest first, at most `NOTES_CAP`
    notes: VecDeque<String>,
}

/// The diagnostic lines a front end without stderr can still take: the newest this many.
pub const NOTES_CAP: usize = 64;

impl Default for RomState {
    fn default() -> Self {
        RomState {
            strict: false, unknown: Vec::new(), boot_pending: true, putc1: print::ETS_UART_PUTC1, uart_sel: 0, calls: Vec::new(),
            isr: [(0, 0); 16], tasks: [sched::Task::default(); sched::PRIORITIES], idle: (0, 0), idle_called: false, run_sp: 0,
            // Model choice: the CPU runs at 80 MHz from reset here, so the ROM counts 80 cycles per
            // microsecond until the SDK says otherwise
            cpu_mhz: (crate::periph::CPU_HZ / 1_000_000) as u32, i2c: HashMap::new(), phy_seen: HashSet::new(), stop: None, halted: None,
            logged: Vec::new(), notes: VecDeque::new(),
        }
    }
}

impl RomState {
    /// A chip reset: the ROM starts over (the strict setting is the host's and stays, so do the
    /// diagnostic lines not yet taken).
    pub fn reset(&mut self) {
        *self = RomState { strict: self.strict, unknown: std::mem::take(&mut self.unknown), notes: std::mem::take(&mut self.notes), ..Default::default() };
    }
    /// The stop a ROM function asked for; reading it clears it.
    pub fn take_stop(&mut self) -> Option<Stop> { self.stop.take() }
    /// End the run after this call (`SocBus::take_stop`) and stay stopped until a reset: write
    /// `msg` (`note`) and keep it as the reason (`halted`).
    pub fn halt(&mut self, msg: String) {
        self.note(msg.clone());
        self.halted = Some(msg);
        self.stop = Some(Stop::Halted);
    }
    /// Why the ROM stopped the chip (`halt`), until the next reset: a front end tells this stop
    /// from the end of a run's budget by it.
    pub fn halted(&self) -> Option<&str> { self.halted.as_deref() }
    /// Write `[emu] rom: <msg>` to stderr and keep the line for `take_notes`.
    pub fn note(&mut self, msg: String) { self.push_note(format!("[emu] rom: {msg}")); }
    /// Write `line` as it is to stderr and keep it for `take_notes` (the newest `NOTES_CAP`).
    pub fn push_note(&mut self, line: String) {
        eprintln!("{line}");
        if self.notes.len() == NOTES_CAP { self.notes.pop_front(); }
        self.notes.push_back(line);
    }
    /// The diagnostic lines since the last call, oldest first: for front ends without stderr.
    pub fn take_notes(&mut self) -> Vec<String> { self.notes.drain(..).collect() }
    /// Write `msg` once per `key`.
    pub fn log_once(&mut self, key: &'static str, msg: impl FnOnce() -> String) {
        if !self.logged.contains(&key) { self.logged.push(key); self.note(msg()); }
    }
}

// ------------------------------------------------------------------ guest memory

/// A load or store of a modelled ROM function that faults on the chip: the exception cause
/// (`xtensa_lx7::state::exc`) and the address, EXCVADDR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemFault { pub cause: u32, pub addr: u32 }

/// What a guest memory access of a ROM function gives: the value or the fault.
pub type Mem<T> = Result<T, MemFault>;

/// The exception the chip raises for a bus fault of a load (`write` false) or store at `addr`:
/// LoadStoreError for a narrow access to word-only memory, else Load/StoreProhibited, as
/// `xtensa_lx7`'s executor maps `Fault`s.
fn mem_fault(f: Fault, write: bool, addr: u32) -> MemFault {
    let cause = if f == Fault::LoadStoreError { exc::LOAD_STORE_ERROR } else if write { exc::STORE_PROHIBITED } else { exc::LOAD_PROHIBITED };
    MemFault { cause, addr }
}

/// A byte as the ROM's code reads it (`SocBus::peek8`: DRAM, IRAM, RTC memory, the flash window,
/// registers); the mask ROM's own bytes read 0, as a data load there does (`SocBus`); elsewhere
/// LoadProhibited.
#[inline]
pub fn load8(bus: &mut SocBus, addr: u32) -> Mem<u8> {
    match bus.peek8(addr) {
        Some(b) => Ok(b),
        None => bus.read8(addr).map_err(|f| mem_fault(f, false, addr)),
    }
}

/// A byte store as the ROM's code makes it (`SocBus::poke8`: RAM and registers); the mask ROM, the
/// flash window and unmapped space are StoreProhibited.
#[inline]
pub fn store8(bus: &mut SocBus, addr: u32, v: u8) -> Mem<()> {
    if bus.poke8(addr, v) { return Ok(()); }
    bus.last_fault = Some((addr, true));
    Err(MemFault { cause: exc::STORE_PROHIBITED, addr })
}

/// A word load (`l32i`): the bus's own access, its faults as the core takes them.
pub fn load32(bus: &mut SocBus, addr: u32) -> Mem<u32> { bus.read32(addr).map_err(|f| mem_fault(f, false, addr)) }

/// A word store (`s32i`).
pub fn store32(bus: &mut SocBus, addr: u32, v: u32) -> Mem<()> { bus.write32(addr, v).map_err(|f| mem_fault(f, true, addr)) }

/// End the ROM function at the core's pc (its entry) with the exception `f`: the guest's vector
/// takes it as one of its own instructions' (`Lx106::raise_from_hook`).
pub fn fault(core: &mut Lx106, f: MemFault) { let pc = core.pc(); core.raise_from_hook(pc, f.cause, Some(f.addr)); }

/// Return `r`'s value to the caller, or end the function with its fault.
pub fn done(core: &mut Lx106, bus: &mut SocBus, r: Mem<u32>) {
    match r { Ok(v) => ret(core, bus, v), Err(f) => fault(core, f) }
}

/// The ROM symbol at `addr`, if any.
pub fn name_at(addr: u32) -> Option<&'static str> { index(addr).map(|i| ENTRIES[i].name) }

/// The address of the ROM symbol `name` (one the model uses, so it is in the table).
pub fn addr_of(name: &str) -> u32 { ENTRIES.iter().find(|e| e.name == name).map(|e| e.addr).unwrap_or_else(|| panic!("{name} is no ROM symbol")) }

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

/// The ROM function at the core's pc, in a dispatch that may retire up to `budget` (at least 1)
/// iterations. Returns the iterations it took: 1 for a call, more for a busy-wait, 0 for a pc
/// outside the ROM (the guest's own code) or a busy-wait an interrupt is about to cut into.
///
/// A function that faults (`fault`) also returns 0: the core takes the exception it left at that
/// pc. A chip the ROM stopped (`RomState::halt`) stays stopped: the run ends here again at once.
pub fn dispatch(core: &mut Lx106, bus: &mut SocBus, budget: u32) -> u32 {
    let pc = core.pc();
    if !(ROM_LOW..ROM_HIGH).contains(&pc) || core.hook_exception_pending() { return 0; }
    if bus.rom.halted.is_some() { bus.rom.stop = Some(Stop::Halted); return 1; }
    if pc == DELAY_LOOP { return delay_loop(core, budget); }
    if pc == sched::RUN_LOOP { return sched::run_loop(core, bus); }
    if (xtos::GUEST_RET..xtos::GUEST_RET_END).contains(&pc) { xtos::returned(core, bus, pc); return 1; }
    if (phy::STUBS..phy::STUBS + 4 * phy::SLOTS).contains(&pc) { phy::slot(core, bus, pc); return 1; }
    if pc == ENTRY_RETURN {
        bus.rom.halt(format!("the boot image's entry returned to the ROM (a2={:#x}): stopping", core.get_ar(2)));
        return 1;
    }
    match index(pc) {
        Some(i) if pc < PRIVATE_LOW => match handlers()[i] {
            Some(run) => run(core, bus),
            None => unimplemented(core, bus, pc, ENTRIES[i].name),
        },
        _ => {
            let from = call_site(core);
            if !bus.rom.unknown.iter().any(|&(a, _)| a == pc) { bus.rom.unknown.push((pc, from)); }
            bus.rom.halt(format!("execution at {pc:#010x}, which is no ROM entry (from {from:#010x}): stopping"));
        }
    }
    u32::from(!core.hook_exception_pending())
}

/// Where the call came from: a call0/callx0 is 3 bytes before its return address.
fn call_site(core: &Lx106) -> u32 { core.get_ar(0).wrapping_sub(3) }

fn unimplemented(core: &mut Lx106, bus: &mut SocBus, pc: u32, name: &str) {
    let from = call_site(core);
    let first = !bus.rom.unknown.iter().any(|&(a, _)| a == pc);
    if first { bus.rom.unknown.push((pc, from)); }
    if bus.rom.strict {
        bus.rom.halt(format!("unimplemented {name} ({pc:#010x}) from {from:#010x}: stopping (strict ROM)"));
        return;
    }
    if first { bus.rom.note(format!("unimplemented {name} ({pc:#010x}) from {from:#010x}: returns 0")); }
    ret(core, bus, 0);
}

/// Argument `n` of the call (a2 + n, the stack from the seventh).
fn arg(core: &Lx106, bus: &mut SocBus, n: usize) -> u32 { core.arg(bus, n) }

/// Return `v` to the caller.
fn ret(core: &mut Lx106, bus: &mut SocBus, v: u32) { core.return_from_stub(bus, v); }

/// Return a 64-bit `v` in a2 (low) and a3 (high).
fn ret64(core: &mut Lx106, bus: &mut SocBus, v: u64) { core.set_ar(3, (v >> 32) as u32); ret(core, bus, v as u32); }

// ------------------------------------------------------------------ small functions

/// `ets_delay_us(us)`: a busy-wait on CCOUNT until `us` microseconds of CPU cycles have passed,
/// at the frequency `ets_update_cpu_frequency` set (160 with CPU2X, whose CPU cycles take half the
/// device time: `SocBus::tick`), the cycle count computed in 32 bits as a CCOUNT difference is.
/// The call itself is one step; the
/// wait goes on at `DELAY_LOOP` (`delay_loop`) with its state in a3 and a4, scratch registers of
/// the call0 ABI (an interrupt handler saves them as it saves any interrupted code's).
fn ets_delay_us(core: &mut Lx106, bus: &mut SocBus) {
    let cycles = arg(core, bus, 0).wrapping_mul(bus.rom.cpu_mhz);
    let start = core.read_sr(xtensa_lx7::state::sr::CCOUNT).unwrap_or(0);
    core.set_ar(3, start);
    core.set_ar(4, cycles);
    core.set_pc(DELAY_LOOP);
    core.spin(1);
}

/// The wait of `ets_delay_us`: spend steps (of `Lx106::cycles_per_step` cycles each, so device
/// time and CCOUNT move by cycles under an approximate CPI too) until CCOUNT - a3 >= a4, then
/// return to a0. A chunk ends at the budget and at the next CCOMPARE0 match, so a timer interrupt
/// that falls inside the wait becomes pending on its cycle; when one is pending this takes no
/// step, the core takes the interrupt here, and its return (`rfe` to this pc) resumes the wait,
/// which, being a CCOUNT difference, includes the handler's time as a ROM loop's would.
fn delay_loop(core: &mut Lx106, budget: u32) -> u32 {
    use xtensa_lx7::state::sr;
    if core.irq_pending() { return 0; }
    let now = core.read_sr(sr::CCOUNT).unwrap_or(0);
    let (elapsed, total) = (now.wrapping_sub(core.get_ar(3)), core.get_ar(4));
    if elapsed >= total {
        core.set_pc(core.get_ar(0));
        core.spin(1);
        return 1;
    }
    let cpi = core.cycles_per_step();
    let to_match = match core.read_sr(sr::CCOMPARE0).unwrap_or(0).wrapping_sub(now) { 0 => 1u64 << 32, d => u64::from(d) };
    let steps = u64::from(total - elapsed).min(to_match).div_ceil(u64::from(cpi)).min(u64::from(budget)).max(1);
    core.spin(steps as u32);
    steps as u32
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
fn software_reset(_core: &mut Lx106, bus: &mut SocBus) { bus.chip_reset(boot::RST_SW); }

/// The ROM's watchdog API (`ets_wdt_enable/disable/get_mode/init/restore`): no watchdog yet.
fn ets_wdt_nop(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

/// The modelled functions by ROM symbol name.
const IMPLEMENTED: &[(&str, Handler)] = &[
    ("_ResetVector", boot::reset_vector),
    // interrupts, exceptions
    ("_xtos_unhandled_exception", xtos::unhandled_exception), ("_xtos_l1int_handler", xtos::l1int_handler),
    ("_xtos_c_wrapper_handler", xtos::c_wrapper_handler), ("_xtos_set_exception_handler", xtos::set_exception_handler),
    ("_xtos_p_none", xtos::p_none), ("ets_isr_attach", xtos::ets_isr_attach), ("ets_isr_mask", xtos::ets_isr_mask),
    ("ets_isr_unmask", xtos::ets_isr_unmask), ("_xtos_ints_on", xtos::xtos_ints_on), ("_xtos_ints_off", xtos::xtos_ints_off),
    ("_xtos_set_intlevel", xtos::xtos_set_intlevel),
    // scheduler
    ("ets_task", sched::ets_task), ("ets_post_rom", sched::ets_post), ("ets_run", sched::ets_run), ("ets_set_idle_cb", sched::ets_set_idle_cb),
    // PHY
    ("phy_get_romfuncs", phy::phy_get_romfuncs), ("rom_i2c_readReg", phy::i2c_read), ("rom_i2c_readReg_Mask", phy::i2c_read_mask),
    ("rom_i2c_writeReg", phy::i2c_write), ("rom_i2c_writeReg_Mask", phy::i2c_write_mask),
    ("rom_chip_v5_enable_cca", phy::nop), ("rom_chip_v5_disable_cca", phy::nop), ("rom_sar_init", phy::nop),
    // GPIO, descriptors, CPU frequency
    ("gpio_output_set", gpio::gpio_output_set), ("gpio_input_get", gpio::gpio_input_get),
    ("gpio_pin_wakeup_enable", gpio::gpio_pin_wakeup_enable), ("gpio_pin_wakeup_disable", gpio::gpio_pin_wakeup_disable),
    ("lldesc_build_chain", gpio::lldesc_build_chain), ("lldesc_num2link", gpio::lldesc_num2link), ("lldesc_set_owner", gpio::lldesc_set_owner),
    ("roundup2", gpio::roundup2), ("ets_update_cpu_frequency", gpio::ets_update_cpu_frequency),
    ("ets_get_cpu_frequency", gpio::ets_get_cpu_frequency), ("ets_rtc_int_register", gpio::ets_rtc_int_register),
    // crypto
    ("MD5Init", crypto::md5_init), ("MD5Update", crypto::md5_update), ("MD5Final", crypto::md5_final), ("md5_vector", crypto::md5_vector),
    ("hmac_md5", crypto::hmac_md5), ("hmac_md5_vector", crypto::hmac_md5_vector), ("sha1_vector", crypto::sha1_vector),
    ("hmac_sha1", crypto::hmac_sha1), ("hmac_sha1_vector", crypto::hmac_sha1_vector), ("pbkdf2_sha1", crypto::pbkdf2_sha1),
    ("sha1_prf", crypto::sha1_prf), ("rc4_skip", crypto::rc4_skip), ("aes_decrypt", crypto::aes_decrypt),
    ("rijndaelKeySetupDec", crypto::rijndael_key_setup_dec),
    // soft float
    ("__addsf3", libgcc::addsf3), ("__subsf3", libgcc::subsf3), ("__mulsf3", libgcc::mulsf3), ("__fixunssfsi", libgcc::fixunssfsi),
    ("__floatsisf", libgcc::floatsisf), ("__floatunsisf", libgcc::floatunsisf), ("__adddf3", libgcc::adddf3), ("__subdf3", libgcc::subdf3),
    ("__muldf3", libgcc::muldf3), ("__divdf3", libgcc::divdf3), ("__fixdfsi", libgcc::fixdfsi), ("__fixunsdfsi", libgcc::fixunsdfsi),
    ("__truncdfsf2", libgcc::truncdfsf2), ("__extendsfdf2", libgcc::extendsfdf2), ("__floatsidf", libgcc::floatsidf),
    ("__floatunsidf", libgcc::floatunsidf),
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
        // `ets_post` at the same address is commented out in the linker script
        assert_eq!(name_at(0x4000_0e24), Some("ets_post_rom"));
        for (name, _) in IMPLEMENTED { assert!(ENTRIES.iter().any(|e| e.name == *name), "{name} is no ROM symbol"); }
        let hooks = hooks();
        assert!(hooks.contains(&0x4000_0080) && hooks.contains(&ENTRY_RETURN) && !hooks.contains(&0x3fffc000));
        // the private range alone covers every bucket of the machine's stub bloom
        assert_eq!((PRIVATE_LOW..PRIVATE_HIGH).step_by(4).fold(0u64, |m, pc| m | emu_core::core::pc_bit(pc)), u64::MAX);
    }
}
