//! Jumperly patch J28: the host tap on the classic ESP32's LEDC (the C3 twin is `esp32c3::pwm`,
//! patch J4). The classic has eight high-speed and eight low-speed channels and no MCPWM tap yet;
//! their matrix signals are `LEDC_HS_SIG_OUT0..7` = 71..78 and `LEDC_LS_SIG_OUT0..7` = 79..86
//! (IDF v5.5.4 `soc/esp32/include/soc/gpio_sig_map.h`). `PwmTap` queues a change when a channel's
//! frequency or duty differs from the last report.
use crate::periph::Peripherals;

/// One PWM output as the circuit sees it. Same fields the S3 tap and `esp32sim_pwm_take` use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PwmOut {
    pub signal: u32,
    pub enabled: bool,
    pub idle_high: bool,
    pub freq: f64,
    pub duty: f64,
}

/// `LEDC_HS_SIG_OUT0` on the classic; the low-speed channels follow at 79.
pub const LEDC_SIG_OUT0: u32 = 71;
const CHANNELS: usize = 16;

fn off(signal: u32, idle_high: bool) -> PwmOut {
    PwmOut { signal, enabled: false, idle_high, freq: 0.0, duty: 0.0 }
}

pub struct PwmTap {
    last: [PwmOut; CHANNELS],
    pub events: Vec<(u64, PwmOut)>,
}

impl Default for PwmTap {
    fn default() -> Self { Self::new() }
}

impl PwmTap {
    pub fn new() -> Self {
        PwmTap {
            last: std::array::from_fn(|i| off(LEDC_SIG_OUT0 + i as u32, false)),
            events: Vec::new(),
        }
    }

    /// Record every channel whose output changed since the last check; true if any did.
    pub fn check(&mut self, p: &Peripherals, cycle: u64) -> bool {
        let mut changed = false;
        for (i, last) in self.last.iter_mut().enumerate() {
            let signal = LEDC_SIG_OUT0 + i as u32;
            let idle_high = p.ledc.channel_idle_high(i);
            let now = match p.ledc.tap_output(i) {
                Some((freq, duty)) => PwmOut { signal, enabled: true, idle_high, freq, duty },
                None => off(signal, idle_high),
            };
            if *last != now {
                *last = now;
                self.events.push((cycle, now));
                changed = true;
            }
        }
        changed
    }

    /// A chip reset: every running output stops (queued as off); true if any was running.
    pub fn reset(&mut self, cycle: u64) -> bool {
        let mut any = false;
        for last in &mut self.last {
            if last.enabled {
                self.events.push((cycle, off(last.signal, false)));
                any = true;
            }
            *last = off(last.signal, false);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPORT_PERIP_CLK_EN: u32 = 0x3ff0_00c0;
    const DPORT_PERIP_RST_EN: u32 = 0x3ff0_00c4;
    const LEDC: u32 = 0x3ff5_9000;
    /// DPORT_LEDC_CLK_EN / DPORT_LEDC_RST (IDF v5.5.4 register/soc/dport_reg.h).
    const LEDC_BIT: u32 = 1 << 11;
    /// 8-bit resolution, divider 16000 / 256 = 62.5, APB: 80 MHz / 62.5 / 256 = 5 kHz.
    const TIMER: u32 = 8 | (16000 << 5) | (1 << 25);

    fn clocked() -> Peripherals {
        let mut p = Peripherals::new([0; 6]);
        p.write32(DPORT_PERIP_CLK_EN, LEDC_BIT);
        assert!(p.ledc.clock_enabled);
        p
    }

    fn assert_quarter_at_5khz(o: &PwmOut, signal: u32) {
        assert_eq!(o.signal, signal);
        assert!(o.enabled, "{o:?}");
        assert!((o.freq - 5000.0).abs() < 1e-6, "{o:?}");
        assert!((o.duty - 0.25).abs() < 1e-9, "{o:?}");
    }

    #[test]
    fn high_speed_channel_0_reports_on_signal_71() {
        let mut p = clocked();
        let mut t = PwmTap::new();
        p.write32(LEDC + 0x140, TIMER);                                  // HSTIMER0_CONF
        p.write32(LEDC + 0x08, 64 << 4);                                 // HSCH0_DUTY
        assert!(!t.check(&p, 5));
        p.write32(LEDC + 0x0c, 1 << 31);                                 // HSCH0_CONF1.DUTY_START
        p.write32(LEDC + 0x00, 4 | (1 << 4));                            // HSCH0_CONF0: timer 0, SIG_OUT_EN
        assert!(t.check(&p, 10));
        let (cycle, o) = t.events.pop().expect("no change");
        assert_eq!(cycle, 10);
        assert_quarter_at_5khz(&o, LEDC_SIG_OUT0);
        assert!(!t.check(&p, 11));
    }

    #[test]
    fn low_speed_channel_0_reports_on_signal_79_after_para_up() {
        let mut p = clocked();
        let mut t = PwmTap::new();
        p.write32(LEDC + 0x190, 1);                                      // LEDC_CONF.APB_CLK_SEL: LS slow clock = APB
        p.write32(LEDC + 0x160, TIMER | (1 << 26));                      // LSTIMER0_CONF with PARA_UP
        p.write32(LEDC + 0xa8, 64 << 4);                                 // LSCH0_DUTY
        p.write32(LEDC + 0xac, 1 << 31);                                 // LSCH0_CONF1.DUTY_START
        p.write32(LEDC + 0xa0, 4);                                       // SIG_OUT_EN, no PARA_UP yet
        assert!(!t.check(&p, 5), "the low-speed shadow is not latched");
        p.write32(LEDC + 0xa0, 4 | (1 << 4));                            // PARA_UP
        assert!(t.check(&p, 10));
        let (_, o) = t.events.pop().expect("no change");
        assert_quarter_at_5khz(&o, LEDC_SIG_OUT0 + 8);
    }

    #[test]
    fn ledc_reset_and_chip_reset_turn_the_outputs_off() {
        let mut p = clocked();
        let mut t = PwmTap::new();
        p.write32(LEDC + 0x140, TIMER);
        p.write32(LEDC + 0x08, 64 << 4);
        p.write32(LEDC + 0x0c, 1 << 31);
        p.write32(LEDC + 0x00, 4 | 8);                                   // SIG_OUT_EN, IDLE_LV high
        assert!(t.check(&p, 10));
        t.events.clear();
        p.write32(DPORT_PERIP_RST_EN, LEDC_BIT);                         // DPORT_LEDC_RST
        assert!(t.check(&p, 20));
        assert_eq!(t.events.pop().map(|(_, o)| (o.signal, o.enabled)), Some((LEDC_SIG_OUT0, false)));
        p.write32(DPORT_PERIP_RST_EN, 0);
        p.write32(LEDC + 0x140, TIMER);
        p.write32(LEDC + 0x08, 64 << 4);
        p.write32(LEDC + 0x0c, 1 << 31);
        p.write32(LEDC + 0x00, 4);
        assert!(t.check(&p, 30));
        t.events.clear();
        assert!(t.reset(40));
        assert_eq!(t.events, vec![(40, PwmOut { signal: LEDC_SIG_OUT0, enabled: false, idle_high: false, freq: 0.0, duty: 0.0 })]);
        assert!(!t.reset(50));
    }

    #[test]
    fn idle_level_of_a_stopped_channel_is_reported() {
        let mut p = clocked();
        let mut t = PwmTap::new();
        p.write32(LEDC + 0x00, 8);                                       // HSCH0: output off, IDLE_LV high
        assert!(t.check(&p, 10));
        let (_, o) = t.events.pop().expect("no change");
        assert_eq!((o.signal, o.enabled, o.idle_high), (LEDC_SIG_OUT0, false, true));
    }
}
