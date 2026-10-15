//! Synthetic classic GPIO/I2C correctness fixture; see docs/evidence/classic-gpio-irq.
use esp_soc::Soc;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "gpio_irq_fixture <gpio|i2c> <fixture.elf>");
    let mut m = esp32::machine([0; 6], 4 << 20);
    let elf = esp_soc::elf::parse(&std::fs::read(&args[2]).unwrap()).unwrap();
    for segment in &elf.segments {
        m.bus.load_bytes(segment.vaddr, &segment.data).unwrap();
    }
    esp32::Esp32::boot_core(&mut m.cores[0], elf.entry);
    // Mask CPU interrupt delivery, not source presentation; no synthetic ISR is installed.
    m.cores[0].ps |= 15;
    m.console.capture = true;
    m.console.mask = 2;
    let p = &mut m.bus.periph;
    p.write32(0x3ff4_8070, 1 << 27); // disable ROM watchdog, like classic tests
    if args[1] == "gpio" {
        p.write32(0x3ff4_9048, (2 << 12) | (1 << 9));
        p.write32(0x3ff4_4098, (5 << 7) | (15 << 13));
        p.write32(0x3ff4_4024, 1 << 4);
        p.write32(0x3ff0_0104 + 4 * 22, 7);
    } else {
        assert_eq!(args[1], "i2c");
        // A synthetic display-like byte sink, not an OLED controller or private sketch.
        p.i2c[0].attach(
            0x3c,
            Box::new(esp_periph::i2c::Reg8Device::new("byte-sink", &[])),
        );
        for (pin, mux, signal) in [(21u32, 0x7c, 30u32), (22, 0x80, 29)] {
            p.write32(0x3ff4_9000 + mux, (2 << 12) | (1 << 9) | (1 << 8));
            p.write32(0x3ff4_4130 + 4 * signal, (1 << 7) | pin);
            p.write32(0x3ff4_4530 + 4 * pin, signal);
        }
    }
    m.max_cycles = 2_400_000;
    let stop = m.run(10_000_000);
    assert_eq!(m.exceptions, 0);
    println!(
        "stop={stop:?} insns={} cycles={} interrupts={} transactions={} console={:?}",
        m.insns(),
        m.bus.cycles,
        m.interrupts,
        m.bus.periph.i2c[0].transactions,
        m.console.all
    );
    assert!(!m.console.all.is_empty());
    if args[1] == "i2c" {
        assert!(m.bus.periph.i2c[0].transactions > 100);
    }
}
