//! The LX106 interpreter's decoded blocks on the ESP8266's memory: code written to IRAM, flash
//! erased and programmed under the window and the window remapped all reach the core, and whole
//! sketches (the committed fixtures `fixtures/bin/esp8266-{serial,pwm}.bin`) retire the same
//! instructions, with the same timing, as the uncached interpreter.
use emu_core::{Bus, Core};
use esp8266::bus::{SocBus, DRAM_LOW, FLASH_WIN_LOW, IRAM_LOW};
use esp8266::periph::{self, DPORT_TEIE, PERIPH_BASE};
use esp8266::{rom, Esp8266, Machine};
use esp_soc::observe::{Observer, Wants};
use esp_soc::{RunUntil, SocBus as _};
use std::path::PathBuf;
use xtensa_lx7::state::{ps, sr};
use xtensa_lx7::Lx106;

const MAC: [u8; 6] = [0x5c, 0xcf, 0x7f, 0, 0x11, 0x22];
/// `movi a2, n` (Xtensa ISA RRI8: imm8, r 0xA, s imm[11:8], t 2, op0 2) and `j .`.
fn movi_a2(n: u8) -> [u8; 3] { [0x22, 0xa0, n] }
const J_SELF: [u8; 3] = [0x06, 0xff, 0xff];

fn fixture(name: &str) -> Vec<u8> {
    let path = std::env::var_os("JUMPERLY_FIXTURES_DIR").map(|d| PathBuf::from(d).join(format!("bin/{name}.bin")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/bin/{name}.bin")));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (an Arduino ESP8266 3.1.2 image)", path.display()))
}

/// `movi a2, n` then `j .` at flash offset `off`.
fn flash_program(b: &mut SocBus, off: usize, n: u8) {
    b.flash.bytes[off..off + 3].copy_from_slice(&movi_a2(n));
    b.flash.bytes[off + 3..off + 6].copy_from_slice(&J_SELF);
}

/// The ROM's flash function `name` with `args`, as a guest call reaches it; it must return
/// SPI_FLASH_RESULT_OK (0).
fn rom_call(c: &mut Lx106, b: &mut SocBus, name: &str, args: &[u32]) {
    let entry = rom::ENTRIES.iter().find(|e| e.name == name).unwrap().addr;
    c.set_ar(0, IRAM_LOW);
    for (i, &a) in args.iter().enumerate() { c.set_ar(2 + i as u8, a); }
    c.set_pc(entry);
    assert!(rom::dispatch(c, b, 1) > 0, "{name}");
    assert_eq!(c.get_ar(2), 0, "{name} returns SPI_FLASH_RESULT_OK");
}

/// Run two instructions at `pc`: the MOVI and the jump; a2 then says which code ran.
fn a2_after_running(c: &mut Lx106, b: &mut SocBus, pc: u32) -> u32 {
    c.set_pc(pc);
    assert_eq!(c.run(b, 2), (2, None));
    assert_eq!(c.pc(), pc + 3);
    c.get_ar(2)
}

#[test]
fn a_remapped_flash_window_runs_the_new_megabyte() {
    let mut b = SocBus::new(4 << 20, MAC);
    flash_program(&mut b, 0x1000, 1);
    flash_program(&mut b, 0x10_1000, 2);
    let (mut c, pc) = (Lx106::new(), FLASH_WIN_LOW + 0x1000);
    b.cache_read_enable(0, 0, 1);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 1);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 1);
    let (builds, _) = c.block_cache_stats();
    // `Cache_Read_Enable(1, 0, 1)`: the odd megabyte (mmu_iram.cpp:61-79)
    b.cache_read_enable(1, 0, 1);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 2);
    assert!(c.block_cache_stats().0 > builds, "decoded again");
    // the same base again changes nothing
    b.cache_read_enable(1, 0, 1);
    let (builds, _) = c.block_cache_stats();
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 2);
    assert_eq!(c.block_cache_stats().0, builds, "still decoded");
}

/// Flash erased and programmed under code that ran, through the ROM's `SPIEraseSector` and
/// `SPIWrite` and through SPI0's own commands (esp8266_peri.h:301-320, 382-391), as the SDK does.
#[test]
fn flash_erased_and_programmed_under_executed_code_runs_the_new_code() {
    let mut b = SocBus::new(4 << 20, MAC);
    flash_program(&mut b, 0x2000, 1);
    let (mut c, pc) = (Lx106::new(), FLASH_WIN_LOW + 0x2000);
    b.cache_read_enable(0, 0, 1);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 1);
    // the ROM: SPIEraseSector(2), then SPIWrite(0x2000, DRAM, 8) of `movi a2, 2; j .`
    rom_call(&mut c, &mut b, "SPIEraseSector", &[2]);
    let mut code = [0xff; 8];
    code[..3].copy_from_slice(&movi_a2(2));
    code[3..6].copy_from_slice(&J_SELF);
    b.load_bytes(DRAM_LOW, &code).unwrap();
    rom_call(&mut c, &mut b, "SPIWrite", &[0x2000, DRAM_LOW, 8]);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 2);
    // SPI0: SE at 0x2000, then PP of 8 bytes from W0 (CMD 0x200, ADDR 0x204, W0 0x240)
    let spi0 = PERIPH_BASE + 0x200;
    b.write32(spi0 + 4, 0x2000).unwrap();
    b.write32(spi0, 1 << 24).unwrap();
    assert_eq!(b.flash.bytes[0x2000..0x2008], [0xff; 8]);
    code[..3].copy_from_slice(&movi_a2(3));
    b.write32(spi0 + 0x40, u32::from_le_bytes(code[..4].try_into().unwrap())).unwrap();
    b.write32(spi0 + 0x44, u32::from_le_bytes(code[4..].try_into().unwrap())).unwrap();
    b.write32(spi0 + 4, 8 << 24 | 0x2000).unwrap();
    b.write32(spi0, 1 << 25).unwrap();
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 3);
    // programming alone only clears bits, and that is new code too: 3 & 1, then 1 & 0
    b.load_bytes(DRAM_LOW, &movi_a2(1)).unwrap();
    rom_call(&mut c, &mut b, "SPIWrite", &[0x2000, DRAM_LOW, 3]);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 1);
    b.write32(spi0 + 0x40, u32::from_le_bytes([0x22, 0xa0, 0x00, 0x06])).unwrap();
    b.write32(spi0 + 4, 4 << 24 | 0x2000).unwrap();
    b.write32(spi0, 1 << 25).unwrap();
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 0);
    // a write to flash that the window does not show leaves the decoded code alone
    let (builds, _) = c.block_cache_stats();
    assert!(b.flash_write(0x3f_0000, &[0]));
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 0);
    assert_eq!(c.block_cache_stats().0, builds);
}

/// A guest store to IRAM over code that ran (the SDK and eboot copy code there) and a store that
/// rewrites the next instruction of its own block: both run the new instruction.
#[test]
fn guest_stores_to_iram_reach_decoded_code() {
    let mut b = SocBus::new(4 << 20, MAC);
    let main = IRAM_LOW + 0x100;
    // s32i a5, a6, 8 / nop / nop.n / movi a2, 1 / j .
    let mut program = vec![0x52, 0x66, 0x02, 0xf0, 0x20, 0x00, 0x3d, 0xf0];
    program.extend(movi_a2(1));
    program.extend(J_SELF);
    b.load_bytes(main, &program).unwrap();
    let mut c = Lx106::new();
    c.set_ar(5, u32::from_le_bytes([0x22, 0xa0, 0x07, 0x06]));   // movi a2, 7 and the J's first byte
    c.set_ar(6, IRAM_LOW + 0x800);                                  // first elsewhere in IRAM
    c.set_pc(main);
    assert_eq!(c.run(&mut b, 5), (5, None));
    assert_eq!((c.get_ar(2), c.pc()), (1, main + 11));
    c.set_ar(6, main);
    c.set_pc(main);
    assert_eq!(c.run(&mut b, 5), (5, None));
    assert_eq!((c.get_ar(2), c.pc()), (7, main + 11), "the store's own block runs the new MOVI");
    c.set_ar(2, 0);
    c.set_pc(main + 8);
    assert_eq!(c.run(&mut b, 2), (2, None));
    assert_eq!(c.get_ar(2), 7, "and so does the next run");
}

/// A reboot, and `code_changed()` after a direct write to `iram`, both reach code that ran.
#[test]
fn reboot_and_code_changed_reach_decoded_code() {
    let mut b = SocBus::new(4 << 20, MAC);
    let (main, o) = (IRAM_LOW + 0x200, 0x200);
    b.load_bytes(main, &[movi_a2(1), J_SELF].concat()).unwrap();
    let mut c = Lx106::new();
    assert_eq!(a2_after_running(&mut c, &mut b, main), 1);
    // a direct write, then a jump reset (IRAM kept)
    b.iram[o..o + 3].copy_from_slice(&movi_a2(2));
    b.reboot(MAC);
    assert_eq!(a2_after_running(&mut c, &mut b, main), 2);
    // a direct write announced by `code_changed()`
    b.iram[o..o + 3].copy_from_slice(&movi_a2(3));
    b.code_changed();
    assert_eq!(a2_after_running(&mut c, &mut b, main), 3);
}

/// The ROM's `SPIRead` copying new code from flash into IRAM over code that ran (eboot and the
/// SDK load IRAM code this way).
#[test]
fn rom_spi_read_into_iram_reaches_decoded_code() {
    let mut b = SocBus::new(4 << 20, MAC);
    let main = IRAM_LOW + 0x300;
    b.load_bytes(main, &[movi_a2(1), J_SELF].concat()).unwrap();
    let mut c = Lx106::new();
    assert_eq!(a2_after_running(&mut c, &mut b, main), 1);
    b.write_flash(0x3000, &[movi_a2(2), J_SELF, [0xff; 3]].concat()).unwrap();
    rom_call(&mut c, &mut b, "SPIRead", &[0x3000, main, 8]);
    assert_eq!(a2_after_running(&mut c, &mut b, main), 2);
}

/// The host writing flash (`write_flash`, as a programmer does) under the mapped window over code
/// that ran from there.
#[test]
fn host_flash_writes_under_the_window_reach_decoded_code() {
    let mut b = SocBus::new(4 << 20, MAC);
    flash_program(&mut b, 0x4000, 1);
    let (mut c, pc) = (Lx106::new(), FLASH_WIN_LOW + 0x4000);
    b.cache_read_enable(0, 0, 1);
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 1);
    b.write_flash(0x4000, &[movi_a2(2), J_SELF].concat()).unwrap();
    assert_eq!(a2_after_running(&mut c, &mut b, pc), 2);
}

/// Snapshot of what a run leaves: the core's registers and counters, the machine's interrupt and
/// exception counts, device time and the consoles.
fn snapshot(m: &mut Machine) -> String {
    m.drain_console();
    let c = m.cores[0].state();
    format!("pc={:#x} ar={:x?} ps={:#x} sar={} ccount={} insns={} epc={:x?} eps={:x?} exccause={} excvaddr={:#x} interrupt={:#x} \
             intenable={:#x} ccompare0={:#x} cycles={} interrupts={} exceptions={} irqs={:?} uart0={:?} all={:?}",
        c.pc, c.ar, c.ps, c.sar, c.ccount, c.insn_count, c.epc, c.eps, c.exccause, c.excvaddr, c.interrupt, c.intenable,
        c.ccompare[0], m.bus.cycles, m.interrupts, m.exceptions, m.irq_hist[0], String::from_utf8_lossy(&m.console.uart0),
        String::from_utf8_lossy(&m.console.all))
}

fn run_to(m: &mut Machine, seconds: f64) {
    let target = (seconds * periph::CPU_HZ as f64) as u64;
    loop {
        match m.run_until_cycle(target) {
            RunUntil::Reached => return,
            RunUntil::Yield => {}
            RunUntil::Stop(s) => panic!("stop {s:?} at {:.3} s", m.seconds()),
        }
    }
}

/// The fixture `name` at `cpi`, with or without decoded blocks, run to each of `marks` (seconds of
/// device time; `input` goes to UART0 at the first): the snapshot at each mark.
fn fixture_run(name: &str, cpi: u32, cached: bool, marks: &[f64], input: &[u8]) -> Vec<String> {
    let mut m = esp8266::machine(MAC, 4 << 20);
    m.console.capture = true;
    m.bus.rom.strict = true;
    if cpi > 1 { m.set_approximate_jit_timing(cpi, 256).unwrap(); }
    m.cores[0].set_block_cache(cached);
    m.write_flash(0, &fixture(name)).unwrap();
    m.boot_rom();
    let mut out = Vec::new();
    for (i, &t) in marks.iter().enumerate() {
        run_to(&mut m, t);
        if i == 0 && !input.is_empty() { m.bus.serial_input(input); m.sync_irq(); }
        out.push(snapshot(&mut m));
    }
    assert_eq!(m.cores[0].block_cache_stats().0 > 0, cached);
    out
}

/// `esp8266-serial` never idles (every instruction is a busy one); `esp8266-pwm` takes the
/// waveform generator's FRC1 NMI every few hundred microseconds. Both at the native CPI 1 and the
/// browser's CPI 2, compared at several marks.
#[test]
fn fixtures_retire_the_same_instructions_with_and_without_decoded_blocks() {
    for (name, marks, input) in [("esp8266-serial", &[0.25, 0.32][..], &b"hello\n"[..]), ("esp8266-pwm", &[0.2, 0.45][..], &b""[..])] {
        for cpi in [1, 2] {
            let cached = fixture_run(name, cpi, true, marks, input);
            let uncached = fixture_run(name, cpi, false, marks, input);
            if !input.is_empty() { assert!(cached.last().unwrap().contains("rx=hello"), "{name} echoed its input"); }
            for (i, (a, b)) in cached.iter().zip(&uncached).enumerate() {
                assert!(a == b, "{name} at CPI {cpi}, mark {}:\ncached   {a}\nuncached {b}", marks[i]);
            }
        }
    }
}

/// An FRC1 edge interrupt (esp8266_peri.h:163-167, DPORT TEIE bit 1: interrupt 9) that falls due
/// inside a run of straight-line code is taken in front of the same instruction with and without
/// decoded blocks: the handler records EPC1 and CCOUNT.
#[test]
fn an_frc1_edge_is_taken_in_front_of_the_same_instruction_with_decoded_blocks() {
    const VECBASE: u32 = IRAM_LOW;
    const MAIN: u32 = VECBASE + 0x100;
    const OUT: u32 = DRAM_LOW;
    let run = |cached: bool, cpi: u32, load: u32| {
        let mut m = esp8266::machine(MAC, 4 << 20);
        if cpi > 1 { m.set_approximate_jit_timing(cpi, 256).unwrap(); }
        m.cores[0].set_block_cache(cached);
        // handler: rsr.epc1 a8 / s32i a8, a9, 0 / rsr.ccount a8 / s32i a8, a9, 4 / j .
        m.bus.load_bytes(VECBASE + 0x50, &[0x80, 0xb1, 0x03, 0x82, 0x69, 0x00, 0x80, 0xea, 0x03, 0x82, 0x69, 0x01, 0x06, 0xff, 0xff]).unwrap();
        // main: 45 NOPs and a jump back
        let mut main: Vec<u8> = (0..45).flat_map(|_| [0xf0, 0x20, 0x00]).collect();
        main.extend(((((-(3 * 45) - 4) as u32 & 0x3ffff) << 6) | 6).to_le_bytes()[..3].to_vec());
        m.bus.load_bytes(MAIN, &main).unwrap();
        let c = &mut m.cores[0];
        c.set_pc(MAIN);
        c.write_sr(sr::VECBASE, VECBASE).unwrap();
        c.write_sr(sr::PS, ps::UM).unwrap();
        c.write_sr(sr::INTENABLE, 1 << periph::INUM_FRC1).unwrap();
        c.set_ar(9, OUT);
        m.bus.rom.boot_pending = false;
        m.bus.write32(DPORT_TEIE, 1 << 1).unwrap();
        m.bus.write32(PERIPH_BASE + 0x600, load).unwrap();       // FRC1 LOAD
        m.bus.write32(PERIPH_BASE + 0x608, 1 << 7).unwrap();     // CTRL: enabled, edge, divider 1
        m.sync_irq();
        let target = m.bus.cycles + u64::from(load) + 2000;
        while m.bus.cycles < target {
            if let RunUntil::Stop(s) = m.run_until_cycle(target) { panic!("{s:?}") }
        }
        assert_eq!(m.interrupts, 1);
        (m.bus.read32(OUT).unwrap(), m.bus.read32(OUT + 4).unwrap(), m.insns(), m.cores[0].pc())
    };
    for cpi in [1, 2] {
        for load in [100, 237, 1001, 4109] {
            let cached = run(true, cpi, load);
            assert_eq!(cached, run(false, cpi, load), "CPI {cpi}, FRC1 at {load}");
            assert!((MAIN..MAIN + 138).contains(&cached.0), "EPC1 {:#x} in the main loop", cached.0);
        }
    }
}

/// A per-instruction observer: the machine then runs the core one `step` at a time.
struct EveryInsn;
impl Observer<Esp8266> for EveryInsn {
    fn name(&self) -> &'static str { "every-insn" }
    fn wants(&self) -> Wants { Wants::INSN }
}

/// FRC1 loaded by an instruction counts from that instruction (esp8266_peri.h:147 T1L), not from
/// the start of the scheduling round it ran in. The main code runs 40 NOPs, reads CCOUNT and
/// loads FRC1; the FRC1 edge interrupt's handler reads CCOUNT again: the interval is the load,
/// plus the store and the interrupt's entry (a few instructions), on every run path and CPI, with
/// decoded blocks, without them and one step at a time. Before, device time inside a round was
/// the round's start, so the timer ran out up to a round early: 41 instructions here (960, 984
/// and 920 cycles instead of 1000 in the three runs below; the SDK's NMI wrapper then lost the
/// waveform generator's NMI, `esp8266-pwm` at CPI 2 in tests/periph.rs).
#[test]
fn an_frc1_load_counts_from_the_instruction_that_wrote_it() {
    const VECBASE: u32 = IRAM_LOW;
    const MAIN: u32 = VECBASE + 0x100;
    const OUT: u32 = DRAM_LOW;
    const LOAD: u32 = 1000;
    // dispatch 0: decoded blocks, 1: the uncached loop, 2: one step at a time
    let run = |dispatch: u8, cpi: u32, slices: bool| {
        let mut m = esp8266::machine(MAC, 4 << 20);
        if cpi > 1 { m.set_approximate_jit_timing(cpi, 128).unwrap(); }
        m.cores[0].set_block_cache(dispatch == 0);
        if dispatch == 2 { m.add_observer(Box::new(EveryInsn)); }
        // handler: rsr.ccount a8 / s32i a8, a9, 0 / j .
        m.bus.load_bytes(VECBASE + 0x50, &[0x80, 0xea, 0x03, 0x82, 0x69, 0x00, 0x06, 0xff, 0xff]).unwrap();
        // main: 40 NOPs / rsr.ccount a8 / s32i a10, a11, 0 (FRC1 LOAD) / s32i a8, a9, 4 / j .
        let mut main: Vec<u8> = (0..40).flat_map(|_| [0xf0, 0x20, 0x00]).collect();
        main.extend([0x80, 0xea, 0x03, 0xa2, 0x6b, 0x00, 0x82, 0x69, 0x01, 0x06, 0xff, 0xff]);
        m.bus.load_bytes(MAIN, &main).unwrap();
        let c = &mut m.cores[0];
        c.set_pc(MAIN);
        c.write_sr(sr::VECBASE, VECBASE).unwrap();
        c.write_sr(sr::PS, ps::UM).unwrap();
        c.write_sr(sr::INTENABLE, 1 << periph::INUM_FRC1).unwrap();
        c.set_ar(9, OUT);
        c.set_ar(10, LOAD);
        c.set_ar(11, PERIPH_BASE + 0x600);
        m.bus.rom.boot_pending = false;
        m.bus.write32(DPORT_TEIE, 1 << 1).unwrap();
        m.bus.write32(PERIPH_BASE + 0x608, 1 << 7).unwrap();     // CTRL: enabled, edge, divider 1
        m.sync_irq();
        let target = m.bus.cycles + u64::from(LOAD) + 2000;
        if slices {
            // as the browser runs it: `Machine::run` up to a cycle ceiling
            m.max_cycles = target;
            assert!(matches!(m.run(u64::MAX), esp8266::Stop::Halted));
        } else {
            while m.bus.cycles < target {
                if let RunUntil::Stop(s) = m.run_until_cycle(target) { panic!("{s:?}") }
            }
        }
        assert_eq!(m.interrupts, 1);
        let (at_irq, at_store) = (m.bus.read32(OUT).unwrap(), m.bus.read32(OUT + 4).unwrap());
        (at_irq.wrapping_sub(at_store), m.quantum as u32)
    };
    // `run_until_cycle` charges device time per instruction, without the approximate CPI
    for (cpi, slices) in [(1, false), (1, true), (2, true)] {
        let (cached, quantum) = run(0, cpi, slices);
        for dispatch in [1, 2] {
            assert_eq!(cached, run(dispatch, cpi, slices).0, "CPI {cpi}, slices {slices}: dispatch {dispatch} against decoded blocks");
        }
        // the store's cycles, the load, then the interrupt is taken in front of the next
        // instruction (its step charges device time, not CCOUNT). `Machine::run` without a
        // timing model ends its rounds at whole quanta, not at device deadlines: there the
        // interrupt comes up to a quantum late, never early.
        let late = if slices && cpi == 1 { quantum } else { 0 };
        assert!((LOAD + cpi..=LOAD + 3 * cpi + late).contains(&cached), "CPI {cpi}, slices {slices}: {cached} cycles from the load to the interrupt");
    }
}
