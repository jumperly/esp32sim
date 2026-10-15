//! Jumperly patch J28: the pin view a circuit simulator needs, for the classic ESP32 (the twins are
//! `esp32s3::jumperly`, patch J2, and `esp32c3::jumperly`, patch J9). For every watched pin the
//! bus records its drive state (GPIO out/enable, IO_MUX pulls and input enable, open drain, matrix
//! signal) with the cycle it changed at, and asks `Machine::run` to end the slice so the host can
//! answer before the guest reads the pin again. The state word has the S3 layout, so the host
//! reads every chip the same way.
use crate::periph::Peripherals;

/// GPIO0..GPIO39 (GPIO20, 24 and 28..31 are not bonded and read as plain, idle pads).
pub const PINS: usize = 40;

/// GPIO34..39 are input-only: no output driver and no pull resistors (ESP32 TRM §4.10).
const FIRST_INPUT_ONLY: usize = 34;

/// Bits of a pin state word: the same as `esp32s3::jumperly::state`.
pub mod state {
    pub const OUT: u32 = 1 << 0;
    pub const ENABLE: u32 = 1 << 1;
    pub const PULL_UP: u32 = 1 << 2;
    pub const PULL_DOWN: u32 = 1 << 3;
    pub const OPEN_DRAIN: u32 = 1 << 4;
    pub const INPUT_ENABLE: u32 = 1 << 5;
    /// GPIO matrix output signal in bits 16..25; `PLAIN_GPIO` = the pad's own GPIO registers.
    pub const SIGNAL_SHIFT: u32 = 16;
    /// `GPIO_FUNCn_OUT_SEL_CFG.OUT_SEL` of a plain GPIO; the classic uses the S3's value, 256.
    pub const PLAIN_GPIO: u32 = 0x100;
}

/// The drive state of `pin`. An input-only pin (34..39) reports only its input enable.
pub fn pin_state(p: &Peripherals, pin: usize) -> u32 {
    let g = &p.gpio.gpio;
    let mux = p.gpio.mux(pin);                                   // IO_MUX_GPIOn_REG (0 for an unbonded pad)
    let mut s = 0;
    if mux & (1 << 9) != 0 { s |= state::INPUT_ENABLE; }         // FUN_IE
    if pin >= FIRST_INPUT_ONLY { return s | (state::PLAIN_GPIO << state::SIGNAL_SHIFT); }
    let bit = 1u64 << pin;
    if g.out & bit != 0 { s |= state::OUT; }
    if g.enable & bit != 0 { s |= state::ENABLE; }
    if mux & (1 << 8) != 0 { s |= state::PULL_UP; }              // FUN_WPU
    if mux & (1 << 7) != 0 { s |= state::PULL_DOWN; }            // FUN_WPD
    if g.pin[pin] & (1 << 2) != 0 { s |= state::OPEN_DRAIN; }    // GPIO_PINn.PAD_DRIVER
    s | ((g.func_out_sel[pin] & 0x1ff) << state::SIGNAL_SHIFT)   // OUT_SEL; 256 = PLAIN_GPIO
}

/// Last reported state of every pin and the changes not yet taken by the host.
pub struct PinWatch {
    pub mask: u64,
    last: [u32; PINS],
    /// (cycle, pin, state) in the order they happened
    pub events: Vec<(u64, u8, u32)>,
}

impl PinWatch {
    pub fn new(p: &Peripherals, mask: u64) -> Self {
        let mut last = [0; PINS];
        for (pin, s) in last.iter_mut().enumerate() { *s = pin_state(p, pin); }
        PinWatch { mask, last, events: Vec::new() }
    }

    /// Record every watched pin whose state changed since the last check; true if any did.
    pub fn check(&mut self, p: &Peripherals, cycle: u64) -> bool {
        let mut changed = false;
        for pin in 0..PINS {
            if self.mask & (1u64 << pin) == 0 { continue; }
            let s = pin_state(p, pin);
            if s != self.last[pin] {
                self.last[pin] = s;
                self.events.push((cycle, pin as u8, s));
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn periph() -> Peripherals { Peripherals::new([0; 6]) }
    // IDF v5.5.4 register/soc/gpio_reg.h and io_mux_reg.h: GPIO at 0x3ff44000, IO_MUX at 0x3ff49000.
    const GPIO: u32 = 0x3ff4_4000;
    const IO_MUX: u32 = 0x3ff4_9000;

    #[test]
    fn pin_state_reflects_gpio_iomux_and_matrix() {
        let mut p = periph();
        assert_eq!(pin_state(&p, 2) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
        p.write32(GPIO + 0x24, 1 << 2);                                  // ENABLE_W1TS
        p.write32(GPIO + 0x08, 1 << 2);                                  // OUT_W1TS
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE);
        p.write32(IO_MUX + 0x48, (1 << 8) | (1 << 9) | (2 << 12));       // GPIO4: FUN_WPU | FUN_IE, MCU_SEL 2 (GPIO)
        assert_eq!(pin_state(&p, 4) & 0xffff, state::PULL_UP | state::INPUT_ENABLE);
        p.write32(IO_MUX + 0x6c, 1 << 7);                                // GPIO5: FUN_WPD
        assert_eq!(pin_state(&p, 5) & 0xffff, state::PULL_DOWN);
        p.write32(GPIO + 0x88 + 4 * 16, 1 << 2);                         // GPIO_PIN16.PAD_DRIVER (classic PINn bank at 0x88)
        assert_eq!(pin_state(&p, 16) & 0xffff, state::OPEN_DRAIN);
        p.write32(GPIO + 0x530 + 4 * 18, 87);                            // GPIO18 <- RMT_SIG_OUT0 (FUNC18_OUT_SEL_CFG)
        assert_eq!(pin_state(&p, 18) >> state::SIGNAL_SHIFT, 87);
        p.write32(GPIO + 0x530 + 4 * 18, 256);                           // back to the GPIO registers
        assert_eq!(pin_state(&p, 18) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
    }

    #[test]
    fn input_only_pins_report_no_pulls_or_drive() {
        let mut p = periph();
        p.write32(GPIO + 0x24, 0);                                       // nothing enabled
        // IO_MUX_GPIO34_REG is at 0x14 (io_mux_reg.h; 0x1c is GPIO32): pulls are not wired on 34..39.
        p.write32(IO_MUX + 0x14, (1 << 8) | (1 << 7) | (1 << 9));
        assert_eq!(pin_state(&p, 34) & 0xffff, state::INPUT_ENABLE);
        p.write32(GPIO + 0x14, (1 << 2) | (1 << 7));                     // OUT1_W1TS: GPIO34, GPIO39
        p.write32(GPIO + 0x30, (1 << 2) | (1 << 7));                     // ENABLE1_W1TS: GPIO34, GPIO39
        p.write32(GPIO + 0x88 + 4 * 39, 1 << 2);                         // GPIO_PIN39.PAD_DRIVER
        assert_eq!(pin_state(&p, 34) & 0xffff, state::INPUT_ENABLE);
        assert_eq!(pin_state(&p, 39) & 0xffff, 0);
        assert_eq!(pin_state(&p, 39) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
    }

    #[test]
    fn pin_watch_records_changes_of_watched_pins_only() {
        let mut p = periph();
        let mut w = PinWatch::new(&p, (1 << 2) | (1 << 4));
        assert!(!w.check(&p, 10));                                       // nothing changed yet
        p.write32(GPIO + 0x24, (1 << 2) | (1 << 3));                     // enable GPIO2 and (unwatched) GPIO3
        assert!(w.check(&p, 20));
        assert_eq!(w.events, vec![(20, 2, state::ENABLE | (state::PLAIN_GPIO << state::SIGNAL_SHIFT))]);
        w.events.clear();
        // releasing the line keeps the visible level (out = 0) but must still be reported
        p.write32(GPIO + 0x28, 1 << 2);                                  // ENABLE_W1TC
        p.write32(IO_MUX + 0x40, 1 << 8);                                // GPIO2: pull-up on
        assert!(w.check(&p, 30));
        assert_eq!(w.events, vec![(30, 2, state::PULL_UP | (state::PLAIN_GPIO << state::SIGNAL_SHIFT))]);
        assert!(!w.check(&p, 40));
    }
}
