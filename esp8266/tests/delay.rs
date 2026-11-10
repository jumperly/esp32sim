//! `ets_delay_us` waits on cycles, at any CPI, and a CCOMPARE0 timer interrupt that falls due
//! inside the wait is taken on its cycle and does not lengthen the wait.
use emu_core::{Bus, Core};
use esp8266::Machine;
use xtensa_lx7::state::{ps, sr};

const VECBASE: u32 = 0x4010_0000;
const MAIN: u32 = VECBASE + 0x100;
const OUT: u32 = 0x3FFE_8000;
const ETS_DELAY_US: u32 = 0x4000_2ecc;
const TIMER: u32 = 1 << 6;

/// The level-1 handler at VECBASE + 0x50 (user vector) stores CCOUNT at OUT and moves CCOMPARE0
/// away; the main code calls `ets_delay_us(1000)` and stores CCOUNT at OUT + 4.
fn machine(cpi: u32) -> (Machine, u32) {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    if cpi > 1 { m.set_approximate_jit_timing(cpi, 64).unwrap(); }
    let handler = [0x80, 0xea, 0x03, /* rsr.ccount a8 */ 0x82, 0x69, 0x00, /* s32i a8, a9, 0 */
                   0xa0, 0xf0, 0x13, /* wsr.ccompare0 a10 */ 0x00, 0x30, 0x00 /* rfe */];
    let main = [0xc0, 0x03, 0x00, /* callx0 a3 */ 0x80, 0xea, 0x03, /* rsr.ccount a8 */
                0x82, 0x69, 0x01, /* s32i a8, a9, 4 */ 0x06, 0xff, 0xff /* j . */];
    m.bus.load_bytes(VECBASE + 0x50, &handler).unwrap();
    m.bus.load_bytes(MAIN, &main).unwrap();
    let c = &mut m.cores[0];
    let listing: Vec<String> = (0..4).map(|i| c.disasm(MAIN + 3 * i, [main[3 * i as usize], main[3 * i as usize + 1], main[3 * i as usize + 2], 0])).collect();
    assert!(listing[0].starts_with("callx0") && listing[1].starts_with("rsr") && listing[2].starts_with("s32i") && listing[3].starts_with('j'), "{listing:?}");
    let t0 = c.read_sr(sr::CCOUNT).unwrap();
    c.set_pc(MAIN);
    c.write_sr(sr::VECBASE, VECBASE).unwrap();
    c.write_sr(sr::PS, ps::UM).unwrap();
    c.write_sr(sr::INTENABLE, TIMER).unwrap();
    c.write_sr(sr::CCOMPARE0, t0.wrapping_add(40_000)).unwrap();
    c.set_ar(1, 0x3FFF_C000);
    c.set_ar(2, 1000);
    c.set_ar(3, ETS_DELAY_US);
    c.set_ar(9, OUT);
    c.set_ar(10, t0.wrapping_sub(1));
    m.bus.rom.boot_pending = false;
    (m, t0)
}

fn check(cpi: u32) {
    let (mut m, t0) = machine(cpi);
    let (insns0, cycles0) = (m.insns(), m.bus.cycles);
    // run in slices until the main code has stored its second CCOUNT
    while m.bus.read32(OUT + 4).unwrap() == 0 && m.insns() - insns0 < 200_000 { m.run(1000); }
    let at_irq = m.bus.read32(OUT).unwrap().wrapping_sub(t0);
    let done = m.bus.read32(OUT + 4).unwrap().wrapping_sub(t0);
    let insns = m.insns() - insns0;
    let ccount = u64::from(m.cores[0].read_sr(sr::CCOUNT).unwrap().wrapping_sub(t0));
    println!("cpi {cpi}: interrupt at +{at_irq}, delay done at +{done}, {insns} steps, CCOUNT +{ccount}, device time +{}", m.bus.cycles - cycles0);
    // the timer interrupt is taken on the cycle CCOMPARE0 matches, inside the wait
    assert_eq!(at_irq, 40_000, "cpi {cpi}");
    assert_eq!(m.interrupts, 1);
    // the wait ends 1000 us (80 000 cycles) after the call (one step in), the handler's time
    // included; then one step returns and `rsr.ccount` reads
    assert!((80_000 + cpi..=80_000 + 4 * cpi).contains(&done), "cpi {cpi}: done at +{done}");
    // the steps are cycles / cpi (plus up to one 1000-step slice after the store), not cycles
    let want = u64::from(80_000 / cpi);
    assert!((want..want + 1100).contains(&insns), "cpi {cpi}: {insns} steps");
    // device time is cycles: it follows CCOUNT (taking an interrupt is a step that charges the
    // device clock, not CCOUNT, in every LX106 run; hence the one-step slack)
    assert!((m.bus.cycles - cycles0).abs_diff(ccount) <= u64::from(cpi), "device time follows CCOUNT");
    assert_eq!(m.cores[0].pc(), MAIN + 9, "spinning on `j .` after the store");
}

#[test]
fn ets_delay_us_waits_cycles_and_takes_a_timer_interrupt_inside_the_wait() { check(1); }

#[test]
fn ets_delay_us_waits_cycles_under_an_approximate_cpi() { check(2); }

/// CPU2X (DPORT 0x14 bit 0, esp8266_peri.h:40) doubles the core's clock and nothing else: 1000
/// CPU cycles of a `j .` loop are 12.5 us of device time at 80 MHz and 6.25 us at 160 MHz, while
/// the timers keep the 80 MHz APB clock. `ets_delay_us` at the 160 MHz the core then announces
/// (`ets_update_cpu_frequency(160)`, cores/esp8266/core_esp8266_main.cpp:96-102) still waits its
/// microseconds: twice the CCOUNT cycles, the same device time.
#[test]
fn cpu2x_halves_the_device_time_of_a_cpu_cycle() {
    for (cpu2x, apb) in [(false, 1000), (true, 500)] {
        let (mut m, _) = machine(1);
        m.cores[0].write_sr(sr::INTENABLE, 0).unwrap();
        m.bus.load_bytes(MAIN, &[0x06, 0xff, 0xff]).unwrap();       // j .
        m.bus.write32(0x3FF0_0014, u32::from(cpu2x)).unwrap();
        let (t0, c0) = (m.bus.cycles, m.cores[0].read_sr(sr::CCOUNT).unwrap());
        m.run(1000);
        let ccount = m.cores[0].read_sr(sr::CCOUNT).unwrap().wrapping_sub(c0);
        // `run` stops at a quantum's end: scale to 1000 CPU cycles
        let per_1000 = (m.bus.cycles - t0) * 1000 / u64::from(ccount);
        println!("CPU2X {cpu2x}: CCOUNT +{ccount}, device time +{} cycles: {} us per 1000 CPU cycles", m.bus.cycles - t0, per_1000 as f64 / 80.0);
        assert!(ccount >= 1000);
        assert_eq!(per_1000, apb);
    }
    let (mut m, t0) = machine(1);
    m.cores[0].write_sr(sr::INTENABLE, 0).unwrap();
    m.bus.write32(0x3FF0_0014, 1).unwrap();
    m.bus.rom.cpu_mhz = 160;
    let cycles0 = m.bus.cycles;
    while m.bus.read32(OUT + 4).unwrap() == 0 && m.insns() < 400_000 { m.run(1000); }
    let done = m.bus.read32(OUT + 4).unwrap().wrapping_sub(t0);
    println!("ets_delay_us(1000) at 160 MHz: CCOUNT +{done}, device time +{}", m.bus.cycles - cycles0);
    assert!((160_000..160_010).contains(&done), "{done}");
    assert!((m.bus.cycles - cycles0).abs_diff(80_000 + 500) <= 500, "about 1000 us: {}", m.bus.cycles - cycles0);
}
