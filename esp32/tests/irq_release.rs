//! Public synthetic scheduler probes. DPORT meanings reuse the adapter's ESP-IDF
//! v5.5.4 register/soc/dport_reg.h citation; no firmware or hardware timing claim.
use esp32::{machine, bus::SocBus};
use esp_soc::SocBus as _;
use xtensa_lx7::bus::Bus;

#[test]
fn irq_events_held_release_republishes_level_and_edge_after_reset() {
    for line in [9u32, 10] {
        let mut m = machine([0; 6], 0);
        m.cores[0].waiting = true;
        m.cores[0].ps = 15;
        m.bus.write32(0x3ff0_0104 + 4 * 24, line).unwrap();
        m.bus.write32(0x3ff0_0218 + 4 * 24, line).unwrap();
        m.bus.write32(0x3ff0_00dc, 1).unwrap();
        m.run(1); // sample reset-held APP; tick presents to both cores
        assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 1 << line);
        m.bus.write32(0x3ff0_0030, 1).unwrap();
        m.bus.write32(0x3ff0_002c, 0).unwrap();
        m.run(1); // pre-round presentation, then APP reset/release, then post-round
        assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 1 << line);
        assert_eq!(m.cores[1].interrupt & (1 << line), 1 << line);
        m.bus.write32(0x3ff0_00dc, 0).unwrap();
        m.run(1);
        assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 0);
        if line == 9 { assert_eq!(m.cores[1].interrupt & (1 << line), 0); }
        m.reboot();
        assert!(m.bus.irq_dirty);
        m.run(0);
        assert_eq!(m.cores[0].ext_irq_lines, 0);
        assert_eq!(m.cores[1].ext_irq_lines, 0);
    }
}

#[test]
fn irq_events_guest_release_level() { irq_events_guest_release_case(9); }
#[test]
fn irq_events_guest_release_edge() { irq_events_guest_release_case(10); }
fn irq_events_guest_release_case(line: u32) {
    for mode in 0..4 {
        let mut m = release_machine(mode);
        m.bus.load_bytes(0x40080000, &[0x39, 0x02, 0x06, 0xff, 0xff]).unwrap();
        m.cores[0].pc = 0x40080000; m.cores[0].ps = 15;
        m.cores[0].set_ar(2, 0x3ff0002c); m.cores[0].set_ar(3, 0);
        m.bus.write32(0x3ff0_0104 + 4 * 24, line).unwrap();
        m.bus.write32(0x3ff0_0218 + 4 * 24, line).unwrap();
        m.bus.write32(0x3ff0_00dc, 1).unwrap();
        m.bus.write32(0x3ff0_0030, 1).unwrap(); // clock enabled, reset still held
        m.run(1); // guest clears APP reset in round 1, APP not yet scheduled
        assert_eq!(m.bus.periph.dport.core1_control(), (true, false, false));
        assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 1 << line);
        m.run(1); // round 2 resets APP, no new device event
        assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 1 << line,
            "guest release must republish after scheduler reset, line={line}, mode={mode}");
        assert_eq!(m.cores[1].interrupt & (1 << line), 1 << line);
        // One reset must not re-latch an acknowledged edge on later clean rounds.
        m.cores[1].ps = 15;
        m.cores[1].waiting = true;
        if line == 10 { m.cores[1].write_sr(227, 1 << line); }
        m.run(32);
        assert_eq!(m.cores[1].interrupt & (1 << line), if line == 10 { 0 } else { 1 << line });
        assert!(!m.bus.irq_dirty, "clean rounds consume reset invalidation");
    }
}

struct UnitCost;
impl emu_core::CostModel for UnitCost {
    fn lifecycle(&mut self, _: &emu_core::LifecycleFacts) -> Result<(), String> { Ok(()) }
    fn cycles(&mut self, _: &emu_core::ExecutionFacts<'_>) -> Result<u32, String> { Ok(1) }
}

fn release_machine(mode: u32) -> esp32::Machine {
    let mut m = machine([0; 6], 0);
    m.quantum = 1;
    match mode {
        0 => {},
        1 => m.set_cost_model(Box::new(UnitCost)).unwrap(),
        2 => m.set_approximate_jit_timing(1, 1).unwrap(),
        3 => {
            m.set_approximate_jit_timing(1, 1).unwrap();
            m.set_approximate_jit_frontiers(true).unwrap();
        },
        _ => unreachable!(),
    }
    m
}

#[test]
fn irq_events_repeated_reset_clock_hold_stall_and_script_release() {
    for mode in 0..4 {
        for line in [9u32, 10] {
            let mut m = release_machine(mode);
            m.bus.load_bytes(0x40080000, &[0x06, 0xff, 0xff]).unwrap(); // j .
            m.cores[0].pc = 0x40080000;
            m.cores[0].ps = 15;
            m.bus.write32(0x3ff0_0218 + 4 * 24, line).unwrap();
            m.bus.write32(0x3ff0_00dc, 1).unwrap();
            for _ in 0..3 {
                m.bus.write32(0x3ff0_002c, 1).unwrap(); // sample asserted reset
                m.run(1);
                m.bus.write32(0x3ff0_0030, 0).unwrap(); // clock hold
                m.bus.write32(0x3ff0_002c, 0).unwrap();
                m.run(1);
                m.bus.write32(0x3ff0_0034, 1).unwrap(); // stall, then clock on
                m.bus.write32(0x3ff0_0030, 1).unwrap();
                m.run(1);
                let marker = 0x1234_5678;
                m.cores[1].cpenable = marker;
                m.script.events.push((m.bus.cycles, esp_soc::ScriptAction::Poke(0x3ff0_0034, 0)));
                m.run(1); // script write is before IRQ refresh and actual reset
                assert_ne!(m.cores[1].cpenable, marker, "release performs real core reset");
                assert_eq!(m.cores[1].ext_irq_lines & (1 << line), 1 << line, "mode={mode}");
                assert_eq!(m.cores[1].interrupt & (1 << line), 1 << line);
                m.cores[1].ps = 15;
                m.cores[1].waiting = true;
                if line == 10 { m.cores[1].write_sr(227, 1 << line); }
                m.cores[1].cpenable = marker;
                m.bus.write32(0x3ff0_0034, 1).unwrap(); // stall without reset
                m.run(1);
                m.bus.write32(0x3ff0_0034, 0).unwrap();
                m.run(1);
                assert_eq!(m.cores[1].cpenable, marker, "stall release must not reset");
                assert_eq!(m.cores[1].interrupt & (1 << line), if line == 10 { 0 } else { 1 << line });
                assert!(!m.bus.irq_dirty);
            }
        }
    }
}

#[test]
fn irq_events_acknowledged_edge_stays_clear_without_source_fall() {
    let mut m = machine([0; 6], 0);
    m.cores[0].waiting = true; m.cores[0].ps = 15;
    m.bus.write32(0x3ff0_0104 + 4 * 24, 10).unwrap();
    m.bus.write32(0x3ff0_00dc, 1).unwrap();
    m.run(1);
    assert_ne!(m.cores[0].interrupt & (1 << 10), 0);
    m.cores[0].write_sr(227, 1 << 10);
    m.run(512);
    assert_eq!(m.cores[0].interrupt & (1 << 10), 0);
    m.bus.write32(0x3ff0_00dc, 0).unwrap(); m.run(1);
    m.bus.write32(0x3ff0_00dc, 1).unwrap(); m.run(1);
    assert_ne!(m.cores[0].interrupt & (1 << 10), 0);
}

#[test]
fn irq_events_live_line_change_requires_event_seeded_trace() {
    let mut b = SocBus::new(0, [0; 6]);
    b.write32(0x3ff0_0104 + 4 * 22, 9).unwrap();
    b.write32(0x3ff0_0218 + 4 * 22, 10).unwrap();
    b.write32(0x3ff0_0104 + 4 * 34, 9).unwrap();
    b.write32(0x3ff4_9048, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_000c, 1).unwrap(); b.write32(0x3ff4_0024, 1).unwrap();
    let mut seed = 0x12345678u32;
    for _ in 0..10000 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut old = [0; 2]; <esp32::Esp32 as esp_soc::Soc>::irqs(&b, &mut old); b.irq_dirty = false;
        match seed % 8 {
            0 => b.gpio_set_input(4, seed & 0x100 != 0),
            1 => b.gpio_release_input(4),
            2 => b.write32(0x3ff4_4098, (((seed >> 8) & 7) << 7) | (((seed >> 16) & 31) << 13)).unwrap(),
            3 => b.write32(0x3ff4_4048, 1 << 4).unwrap(),
            4 => b.write32(0x3ff4_404c, 1 << 4).unwrap(),
            5 => b.serial_input(b"x"),
            6 => { b.read8(0x6000_0000).unwrap(); },
            _ => b.write32(0x3ff4_0010, 1).unwrap(),
        }
        let mut new = [0; 2]; <esp32::Esp32 as esp_soc::Soc>::irqs(&b, &mut new);
        assert!(old == new || b.irq_dirty, "line changed without event seed={seed:x}");
        b.irq_dirty = false; let ticked = b.tick((seed & 0xfff) + 1);
        let mut after = [0; 2]; <esp32::Esp32 as esp_soc::Soc>::irqs(&b, &mut after);
        assert!(new == after || ticked != 0, "tick changed without event seed={seed:x}");
    }
}
