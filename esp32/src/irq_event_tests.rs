//! Event-presentation regression probes; register meanings reuse the classic adapters' citations.
use super::*;

#[test]
fn irq_events_board_and_chip_reset_invalidate() {
    let mut b = SocBus::new(0, [0; 6]);
    b.attach_board_devices();
    assert!(b.irq_dirty);
    b.irq_dirty = false;
    esp_soc::SocBus::reboot(&mut b, [0; 6]);
    assert!(b.irq_dirty, "reset/restore must re-present sources");
}

#[test]
fn irq_events_rtc_dispatch_transition_reaches_bus_dirty() {
    let mut b = SocBus::new(0, [0; 6]);
    b.write32(0x3ff4_80a4, 0x50d8_3aa1).unwrap();
    b.write32(0x3ff4_803c, 1 << 3).unwrap();
    b.write32(0x3ff4_8090, 2).unwrap();
    b.write32(0x3ff4_808c, (1 << 31) | (1 << 28)).unwrap();
    b.irq_dirty = false;
    assert_ne!(b.tick(1600), 0);
    assert_ne!(b.periph.rtc.irq_sources(), 0);
    assert!(b.irq_dirty);
}

#[test]
fn irq_events_manual_apb_ledc_and_rmt_transitions() {
    let mut b = SocBus::new(0, [0; 6]);
    b.write32(0x3ff0_00c0, (1 << 11) | (1 << 9)).unwrap();
    b.write32(0x3ff0_00c4, 0).unwrap();
    for (off, v) in [(0x140, 8 | (16000 << 5) | (1 << 25)), (8, 64 << 4),
        (12, (1 << 31) | (1 << 30) | (1 << 20) | (1 << 10)), (0, 4), (0x188, 0x101)] {
        b.write32(0x3ff5_9000 + off, v).unwrap();
    }
    b.irq_dirty = false;
    assert_ne!(b.tick(16000), 0);
    assert_ne!(b.periph.ledc.irq_sources(), 0);
    b.write32(0x3ff5_918c, 0x101).unwrap();
    assert_eq!(b.periph.ledc.irq_sources(), 0);
    // Stop LEDC to isolate the next source.
    b.write32(0x3ff0_00c0, 1 << 9).unwrap();
    for (off, v) in [(0xf0, 1), (0x20, (1 << 24) | 1), (0x800, 10 | (1 << 15) | (10 << 16)),
        (0x804, 0), (0xa8, 1), (0x24, (1 << 17) | (1 << 19) | 1 | (1 << 3) | (1 << 4))] {
        b.write32(0x3ff5_6000 + off, v).unwrap();
    }
    b.irq_dirty = false;
    assert_ne!(b.tick(100), 0);
    assert_ne!(b.periph.rmt.irq_sources(), 0);
}

#[test]
fn irq_events_gpio_output_transition_without_peripheral_irq() {
    let mut b = SocBus::new(0, [0; 6]);
    b.write32(0x3ff4_9048, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_4098, (5 << 7) | (1 << 15)).unwrap();
    b.write32(0x3ff4_4540, 71).unwrap();
    b.write32(0x3ff4_4024, 1 << 4).unwrap();
    b.write32(0x3ff0_00c0, 1 << 11).unwrap();
    b.write32(0x3ff0_00c4, 0).unwrap();
    for (off, v) in [(0x140, 8 | (16000 << 5) | (1 << 25)), (8, 64 << 4),
        (12, (1 << 31) | (1 << 30) | (1 << 20) | (1 << 10)), (0, 4)] {
        b.write32(0x3ff5_9000 + off, v).unwrap();
    }
    b.write32(0x3ff4_404c, 1 << 4).unwrap();
    assert!(!b.periph.gpio.irq(0, false));
    b.irq_dirty = false;
    assert_ne!(b.tick(16000), 0, "PWM pin edge alone must invalidate presentation");
    assert_eq!(b.periph.ledc.irq_sources(), 0);
    assert!(b.periph.gpio.irq(0, false));
}
