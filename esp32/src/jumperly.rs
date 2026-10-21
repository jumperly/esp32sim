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

/// Pull resistors of the pads with RTC functions: on the ESP32 they are set through RTC_IO, not
/// IO_MUX (IDF `soc_caps.h`: "those PADs which have RTC functions must set pullup/down/capability
/// via RTC register"; `gpio_pullup_en` -> `rtc_gpio_pullup_en`). (GPIO, offset from
/// DR_REG_RTCIO_BASE 0x3ff48400, RUE bit, RDE bit), from ESP-IDF
/// `soc/esp32/register/soc/rtc_io_reg.h` (arduino-esp32 libs 3.3.7):
/// - RTC_IO_PAD_DAC1_REG +0x84 PDAC1_RUE 27 / RDE 28 (:752-776), GPIO25 (`rtc_io_channel.h` channel 6);
/// - RTC_IO_PAD_DAC2_REG +0x88 PDAC2_RUE 27 / RDE 28 (:834-858), GPIO26 (channel 7);
/// - RTC_IO_XTAL_32K_PAD_REG +0x8c: X32N_RUE 27 / RDE 28 (:929-940), X32P_RUE 22 / RDE 23 (:953-964);
///   `io_mux_reg.h:93-94`: XTAL32K_P = GPIO32, XTAL32K_N = GPIO33;
/// - RTC_IO_TOUCH_PAD0..7_REG +0x94..+0xb0 RUE 27 / RDE 28 (:1095-1760); `touch_sensor_channel.h`:
///   pads 0..7 = GPIO4, 0, 2, 15, 13, 12, 14, 27. TOUCH_PAD8/9 (+0xb4/+0xb8, GPIO33/32) have no
///   pull bits, and the ADC pads (SENSOR_PADS, ADC_PAD: GPIO34..39) have none either.
const RTC_PULLS: [(usize, u32, u32, u32); 12] = [
    (25, 0x84, 27, 28), (26, 0x88, 27, 28), (32, 0x8c, 22, 23), (33, 0x8c, 27, 28),
    (4, 0x94, 27, 28), (0, 0x98, 27, 28), (2, 0x9c, 27, 28), (15, 0xa0, 27, 28),
    (13, 0xa4, 27, 28), (12, 0xa8, 27, 28), (14, 0xac, 27, 28), (27, 0xb0, 27, 28),
];

/// (pull-up, pull-down) of `pin` from RTC_IO; (false, false) for a pad without RTC functions.
fn rtc_pulls(p: &Peripherals, pin: usize) -> (bool, bool) {
    RTC_PULLS.iter().find(|&&(gpio, ..)| gpio == pin).map_or((false, false), |&(_, off, rue, rde)| {
        let reg = p.adc.rtc_io.read(off);
        (reg & (1 << rue) != 0, reg & (1 << rde) != 0)
    })
}

/// The drive state of `pin`. An input-only pin (34..39) reports only its input enable. Pulls are
/// IO_MUX FUN_WPU/WPD ORed with the RTC_IO pulls of an RTC pad (`RTC_PULLS`).
pub fn pin_state(p: &Peripherals, pin: usize) -> u32 {
    let g = &p.gpio.gpio;
    let mux = p.gpio.mux(pin);                                   // IO_MUX_GPIOn_REG (0 for an unbonded pad)
    let mut s = 0;
    if mux & (1 << 9) != 0 { s |= state::INPUT_ENABLE; }         // FUN_IE
    if pin >= FIRST_INPUT_ONLY { return s | (state::PLAIN_GPIO << state::SIGNAL_SHIFT); }
    let bit = 1u64 << pin;
    // OUT / ENABLE as on the S3: the GPIO function (MCU_SEL 2, translated) and the plain-GPIO
    // selector (256) report the drive `PinRoutes::software_output` resolves (enable and output
    // inversion).
    let routes = p.gpio.routes();
    let (out, enable) = if g.func_out_sel[pin] & 0x1ff == state::PLAIN_GPIO && routes.function(pin, 1) {
        routes.software_output(pin as u8).map_or((g.out & bit != 0, false), |level| (level, true))
    } else { (g.out & bit != 0, g.enable & bit != 0) };
    if out { s |= state::OUT; }
    if enable { s |= state::ENABLE; }
    let (rtc_up, rtc_down) = rtc_pulls(p, pin);
    if mux & (1 << 8) != 0 || rtc_up { s |= state::PULL_UP; }    // FUN_WPU | RTC_IO RUE
    if mux & (1 << 7) != 0 || rtc_down { s |= state::PULL_DOWN; } // FUN_WPD | RTC_IO RDE
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
    const RTC_IO: u32 = 0x3ff4_8400;                                     // DR_REG_RTCIO_BASE (reg_base.h:22)
    const RUE: u32 = 1 << 27;
    const RDE: u32 = 1 << 28;

    #[test]
    fn pin_state_reflects_gpio_iomux_and_matrix() {
        let mut p = periph();
        assert_eq!(pin_state(&p, 2) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
        p.write32(GPIO + 0x24, 1 << 2);                                  // ENABLE_W1TS
        p.write32(GPIO + 0x08, 1 << 2);                                  // OUT_W1TS
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE);
        // An RTC pad's pulls are in RTC_IO (gpio_pullup_en -> rtc_gpio_pullup_en): GPIO4 = TOUCH_PAD0.
        p.write32(IO_MUX + 0x48, (1 << 9) | (2 << 12));                  // GPIO4: FUN_IE, MCU_SEL 2 (GPIO)
        p.write32(RTC_IO + 0x94, RUE);                                   // RTC_IO_TOUCH_PAD0.RUE
        assert_eq!(pin_state(&p, 4) & 0xffff, state::PULL_UP | state::INPUT_ENABLE);
        p.write32(RTC_IO + 0x9c, RDE);                                   // GPIO2 = TOUCH_PAD2.RDE
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE | state::PULL_DOWN);
        // A digital-only pad keeps its pulls in IO_MUX.
        p.write32(IO_MUX + 0x6c, 1 << 7);                                // GPIO5: FUN_WPD
        assert_eq!(pin_state(&p, 5) & 0xffff, state::PULL_DOWN);
        p.write32(IO_MUX + 0x4c, 1 << 8);                                // GPIO16: FUN_WPU
        assert_eq!(pin_state(&p, 16) & 0xffff, state::PULL_UP);
        p.write32(IO_MUX + 0x4c, 0);
        p.write32(GPIO + 0x88 + 4 * 16, 1 << 2);                         // GPIO_PIN16.PAD_DRIVER (classic PINn bank at 0x88)
        assert_eq!(pin_state(&p, 16) & 0xffff, state::OPEN_DRAIN);
        p.write32(GPIO + 0x530 + 4 * 18, 87);                            // GPIO18 <- RMT_SIG_OUT0 (FUNC18_OUT_SEL_CFG)
        assert_eq!(pin_state(&p, 18) >> state::SIGNAL_SHIFT, 87);
        p.write32(GPIO + 0x530 + 4 * 18, 256);                           // back to the GPIO registers
        assert_eq!(pin_state(&p, 18) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
    }

    #[test]
    fn rtc_pads_report_the_pulls_of_their_own_rtc_io_register() {
        let mut p = periph();
        // (GPIO, RTC_IO offset, RUE, RDE) — rtc_io_reg.h: PAD_DAC1/2, XTAL_32K_PAD (X32P = GPIO32 at
        // bits 22/23, X32N = GPIO33 at 27/28; io_mux_reg.h:93-94), TOUCH_PAD0..7.
        let pads = [(25, 0x84, RUE, RDE), (26, 0x88, RUE, RDE), (32, 0x8c, 1 << 22, 1 << 23), (33, 0x8c, RUE, RDE),
            (4, 0x94, RUE, RDE), (0, 0x98, RUE, RDE), (2, 0x9c, RUE, RDE), (15, 0xa0, RUE, RDE),
            (13, 0xa4, RUE, RDE), (12, 0xa8, RUE, RDE), (14, 0xac, RUE, RDE), (27, 0xb0, RUE, RDE)];
        for (pin, off, rue, rde) in pads {
            p.write32(RTC_IO + off, rue);
            assert_eq!(pin_state(&p, pin) & 0xffff, state::PULL_UP, "GPIO{pin}");
            p.write32(RTC_IO + off, rde);
            assert_eq!(pin_state(&p, pin) & 0xffff, state::PULL_DOWN, "GPIO{pin}");
            p.write32(RTC_IO + off, 0);
            assert_eq!(pin_state(&p, pin) & 0xffff, 0, "GPIO{pin}");
        }
        p.write32(RTC_IO + 0x8c, 1 << 22);                               // X32P only: GPIO32, not GPIO33
        assert_eq!(pin_state(&p, 33) & 0xffff, 0);
        p.write32(RTC_IO + 0xb4, RUE | RDE);                             // TOUCH_PAD8 (GPIO33) has no pull bits
        assert_eq!(pin_state(&p, 33) & 0xffff, 0);
        p.write32(RTC_IO + 0x80, u32::MAX);                              // ADC_PAD (GPIO34/35): no pulls on 34..39
        assert_eq!(pin_state(&p, 34) & 0xffff, 0);
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

    /// As on the S3: a pad on the GPIO function (MCU_SEL 2) reports the drive
    /// `PinRoutes::software_output` resolves, OUT_INV_SEL bit 9 and OEN_INV_SEL bit 11 (IDF v5.5.4
    /// components/soc/esp32/register/soc/gpio_reg.h:7065-7092); off the GPIO function the raw latch.
    #[test]
    fn a_gpio_function_pad_reports_the_inverted_drive() {
        let mut p = periph();
        p.write32(GPIO + 0x24, 1 << 2);                                  // ENABLE_W1TS
        p.write32(GPIO + 0x08, 1 << 2);                                  // OUT_W1TS
        p.write32(GPIO + 0x530 + 4 * 2, 256 | (1 << 9));                 // GPIO2: plain GPIO, OUT_INV_SEL
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE, "MCU_SEL 0: the raw latch");
        p.write32(IO_MUX + 0x40, 2 << 12);                               // GPIO2: MCU_SEL 2 (GPIO)
        assert_eq!(pin_state(&p, 2) & 0xffff, state::ENABLE, "OUT_INV_SEL: driven low");
        p.write32(GPIO + 0x530 + 4 * 2, 256 | (1 << 11));                // OEN_INV_SEL
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT, "OEN_INV_SEL with ENABLE: not driven");
        p.write32(GPIO + 0x28, 1 << 2);                                  // ENABLE_W1TC
        assert_eq!(pin_state(&p, 2) & 0xffff, state::OUT | state::ENABLE, "OEN_INV_SEL without ENABLE: driven high");
        assert_eq!(pin_state(&p, 2) >> state::SIGNAL_SHIFT, state::PLAIN_GPIO);
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
        p.write32(RTC_IO + 0x9c, RUE);                                   // GPIO2 (TOUCH_PAD2): pull-up on
        assert!(w.check(&p, 30));
        assert_eq!(w.events, vec![(30, 2, state::PULL_UP | (state::PLAIN_GPIO << state::SIGNAL_SHIFT))]);
        assert!(!w.check(&p, 40));
    }
}
