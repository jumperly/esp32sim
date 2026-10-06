//! Jumperly patch J4 on the ESP32-C3: the host tap on upstream's LEDC. The C3 has six low-speed
//! channels and no MCPWM; matrix signals are `LEDC_LS_SIG_OUT0..5` = 45..50. `PwmTap` queues a
//! change when a channel's frequency or duty differs from the last report.
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

/// `LEDC_LS_SIG_OUT0` on the C3 (`soc/gpio_sig_map.h`).
pub const LEDC_SIG_OUT0: u32 = 45;
const CHANNELS: usize = 6;

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

    pub fn check(&mut self, p: &Peripherals, cycle: u64) -> bool {
        let mut changed = false;
        for (i, last) in self.last.iter_mut().enumerate() {
            let signal = LEDC_SIG_OUT0 + i as u32;
            let now = match p.ledc.channel_output(i) {
                Some((freq, duty)) => PwmOut {
                    signal,
                    enabled: true,
                    idle_high: p.ledc.channel_idle_high(i),
                    freq,
                    duty,
                },
                None => off(signal, p.ledc.channel_idle_high(i)),
            };
            if *last != now {
                *last = now;
                self.events.push((cycle, now));
                changed = true;
            }
        }
        changed
    }

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
    use esp_periph::Device;

    fn latch(p: &mut Peripherals) {
        p.ledc.write(0x0c, 1 << 31);
        p.ledc.write(0x00, (1 << 2) | (1 << 4));
    }

    #[test]
    fn ledc_reports_channel_0_on_signal_45() {
        let mut p = Peripherals::new([0; 6]);
        let mut t = PwmTap::new();
        p.ledc.write(0xd0, 3);
        p.ledc.write(0xa0, (40000 << 4) | 8 | (1 << 25));
        p.ledc.write(0x08, 64 << 4);
        assert!(!t.check(&p, 5));
        latch(&mut p);
        assert!(t.check(&p, 10));
        let (cycle, o) = t.events.pop().expect("no change");
        assert_eq!(cycle, 10);
        assert_eq!(o.signal, 45);
        assert!(o.enabled);
        assert!((o.freq - 1000.0).abs() < 1e-6, "{o:?}");
        assert!((o.duty - 0.25).abs() < 1e-9, "{o:?}");
    }
}
