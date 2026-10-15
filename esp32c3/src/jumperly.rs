//! Jumperly patch J9: the pin view a circuit simulator needs, for the ESP32-C3 (the S3 twin is
//! `esp32s3::jumperly`, patch J2). For every watched pin the bus records its drive state (GPIO
//! out/enable, IO_MUX pulls and input enable, open drain, matrix signal) with the cycle it changed
//! at, and asks `Machine::run` to end the slice so the host can answer before the guest reads the
//! pin again. The state word has the S3 layout, so the host reads both chips the same way.
use crate::periph::Peripherals;

/// GPIO0..GPIO21 (GPIO12..17 are the flash pins and stay inside the module).
pub const PINS: usize = 22;

/// Bits of a pin state word: the same as `esp32s3::jumperly::state`.
pub mod state {
    pub const OUT: u32 = 1 << 0;
    pub const ENABLE: u32 = 1 << 1;
    pub const PULL_UP: u32 = 1 << 2;
    pub const PULL_DOWN: u32 = 1 << 3;
    pub const OPEN_DRAIN: u32 = 1 << 4;
    pub const INPUT_ENABLE: u32 = 1 << 5;
    /// GPIO matrix output signal in bits 16..24; `PLAIN_GPIO` = the pad's own GPIO registers.
    pub const SIGNAL_SHIFT: u32 = 16;
    /// The value the host expects for a plain GPIO (the S3's `0x100`).
    pub const PLAIN_GPIO: u32 = 0x100;
}

/// `GPIO_FUNCn_OUT_SEL_CFG.OUT_SEL` of a plain GPIO on the C3 (8 bits wide).
const C3_PLAIN_GPIO: u32 = 128;

/// The drive state of `pin`.
pub fn pin_state(p: &Peripherals, pin: usize) -> u32 {
    let g = &p.gpio;
    let bit = 1u64 << pin;
    let mux = p.io_mux.read(0x4 + 4 * pin as u32);          // IO_MUX_GPIOn_REG
    let mut s = 0;
    if g.out & bit != 0 { s |= state::OUT; }
    if g.enable & bit != 0 { s |= state::ENABLE; }
    if mux & (1 << 8) != 0 { s |= state::PULL_UP; }        // FUN_WPU
    if mux & (1 << 7) != 0 { s |= state::PULL_DOWN; }      // FUN_WPD
    if mux & (1 << 9) != 0 { s |= state::INPUT_ENABLE; }   // FUN_IE
    if g.pin[pin] & (1 << 2) != 0 { s |= state::OPEN_DRAIN; } // GPIO_PINn.PAD_DRIVER
    let sel = g.func_out_sel[pin] & 0xff;
    let signal = if sel == C3_PLAIN_GPIO { state::PLAIN_GPIO } else { sel };
    s | (signal << state::SIGNAL_SHIFT)
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

    #[test]
    fn pin_state_reflects_gpio_iomux_and_matrix() {
        let mut p = periph();
        assert_eq!(pin_state(&p, 2), state::PLAIN_GPIO << state::SIGNAL_SHIFT);   // reset: plain GPIO, input, no pulls
        p.gpio.write(0x24, 1 << 2);                                      // ENABLE_W1TS
        p.gpio.write(0x8, 1 << 2);                                       // OUT_W1TS
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE);
        p.io_mux.write(0x4 + 4 * 4, (1 << 8) | (1 << 9));                // GPIO4: FUN_WPU | FUN_IE
        assert_eq!(pin_state(&p, 4) & 0xffff, state::PULL_UP | state::INPUT_ENABLE);
        p.io_mux.write(0x4 + 4 * 5, 1 << 7);                             // GPIO5: FUN_WPD
        assert_eq!(pin_state(&p, 5) & 0xffff, state::PULL_DOWN);
        p.gpio.write(0x74 + 4 * 6, 1 << 2);                              // GPIO_PIN6.PAD_DRIVER
        assert_eq!(pin_state(&p, 6) & 0xffff, state::OPEN_DRAIN);
        p.gpio.write(0x554 + 4 * 8, 51);                                 // GPIO8 <- matrix signal 51 (RMT out 0)
        assert_eq!(pin_state(&p, 8) >> state::SIGNAL_SHIFT, 51);
        p.gpio.write(0x554 + 4 * 8, 128);                                // back to the GPIO registers
        assert_eq!(pin_state(&p, 8) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
    }

    #[test]
    fn pin_watch_records_changes_of_watched_pins_only() {
        let mut p = periph();
        let mut w = PinWatch::new(&p, (1 << 2) | (1 << 4));
        assert!(!w.check(&p, 10));                                       // nothing changed yet
        p.gpio.write(0x24, (1 << 2) | (1 << 3));                         // enable GPIO2 and (unwatched) GPIO3
        assert!(w.check(&p, 20));
        assert_eq!(w.events, vec![(20, 2, state::ENABLE | (state::PLAIN_GPIO << state::SIGNAL_SHIFT))]);
        w.events.clear();
        // releasing the line keeps the visible level (out = 0) but must still be reported
        p.gpio.write(0x28, 1 << 2);                                      // ENABLE_W1TC
        p.io_mux.write(0x4 + 4 * 2, 1 << 8);                             // pull-up on
        assert!(w.check(&p, 30));
        assert_eq!(w.events, vec![(30, 2, state::PULL_UP | (state::PLAIN_GPIO << state::SIGNAL_SHIFT))]);
        assert!(!w.check(&p, 40));
    }
}
