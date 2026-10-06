//! Jumperly patch J4: the host tap on upstream's LEDC and MCPWM. Upstream models both blocks
//! (`esp_periph::Ledc`, `esp_periph::Mcpwm`) but only answers "what does pin N carry now"; a circuit
//! simulator needs the changes: frequency + duty per GPIO-matrix output signal, with the cycle they
//! happened at. `PwmTap` watches the upstream devices and queues a `PwmOut` whenever a signal's
//! output differs from the last one reported. Without a tap nothing is watched.
use crate::periph::Peripherals;

/// One PWM output as the circuit sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PwmOut { pub signal: u32, pub enabled: bool, pub idle_high: bool, pub freq: f64, pub duty: f64 }

/// GPIO matrix signals: LEDC_LS_SIG_OUT0..7 = 73..80; the MCPWM units' generators follow
/// `Mcpwm::signal_base` (PWM0_OUT0A..2B = 160..165, PWM1 = 166..171).
pub const LEDC_SIG_OUT0: u32 = 73;
/// PWM0_OUT0A and PWM1_OUT0A, the first generator of each MCPWM unit.
pub const MCPWM_SIG_OUT0: [u32; 2] = [160, 166];
const LEDC_CHANNELS: usize = 8;
const MCPWM_CHANNELS: usize = 6;
const OUTPUTS: usize = LEDC_CHANNELS + 2 * MCPWM_CHANNELS;

/// The idle state of a signal nobody drives.
fn off(signal: u32, idle_high: bool) -> PwmOut { PwmOut { signal, enabled: false, idle_high, freq: 0.0, duty: 0.0 } }

/// The current output of every LEDC channel and MCPWM generator, in signal order.
fn outputs(p: &Peripherals) -> [PwmOut; OUTPUTS] {
    std::array::from_fn(|i| {
        if i < LEDC_CHANNELS {
            let signal = LEDC_SIG_OUT0 + i as u32;
            match p.ledc.channel_output(i) {
                Some((freq, duty)) => PwmOut { signal, enabled: true, idle_high: p.ledc.channel_idle_high(i), freq, duty },
                None => off(signal, p.ledc.channel_idle_high(i)),
            }
        } else {
            let (unit, channel) = ((i - LEDC_CHANNELS) / MCPWM_CHANNELS, (i - LEDC_CHANNELS) % MCPWM_CHANNELS);
            let signal = p.mcpwm[unit].signal_base() + channel as u32;
            match p.mcpwm[unit].channel_output(channel) {
                Some((freq, duty)) => PwmOut { signal, enabled: true, idle_high: false, freq, duty },
                None => off(signal, false),
            }
        }
    })
}

/// Last reported output of every signal and the changes not yet taken by the host.
pub struct PwmTap {
    last: [PwmOut; OUTPUTS],
    /// (cycle, output) in the order they happened
    pub events: Vec<(u64, PwmOut)>,
}

impl Default for PwmTap { fn default() -> Self { Self::new() } }

impl PwmTap {
    pub fn new() -> Self {
        let last = std::array::from_fn(|i| {
            if i < LEDC_CHANNELS { off(LEDC_SIG_OUT0 + i as u32, false) }
            else {
                let unit = (i - LEDC_CHANNELS) / MCPWM_CHANNELS;
                off(MCPWM_SIG_OUT0[unit] + ((i - LEDC_CHANNELS) % MCPWM_CHANNELS) as u32, false)
            }
        });
        PwmTap { last, events: Vec::new() }
    }

    /// Queue every output that changed since the last check; true if any did.
    pub fn check(&mut self, p: &Peripherals, cycle: u64) -> bool {
        let mut changed = false;
        for (last, now) in self.last.iter_mut().zip(outputs(p)) {
            if *last != now { *last = now; self.events.push((cycle, now)); changed = true; }
        }
        changed
    }

    /// A chip reset clears LEDC and MCPWM: report the outputs that were running as disabled at the
    /// reset cycle; true if there were any.
    pub fn reset(&mut self, cycle: u64) -> bool {
        let mut any = false;
        for last in &mut self.last {
            if last.enabled { self.events.push((cycle, off(last.signal, false))); any = true; }
            *last = off(last.signal, false);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use esp_periph::Device;

    fn periph() -> Peripherals { Peripherals::new([0; 6]) }

    /// Queue a LEDC duty update the way the driver does: DUTY_START, then PARA_UP on channel 0.
    fn latch_ledc(p: &mut Peripherals) {
        p.ledc.write(0x0c, 1 << 31);                       // CH0_CONF1.DUTY_START
        p.ledc.write(0x00, (1 << 2) | (1 << 4));           // ch0 CONF0: timer 0, SIG_OUT_EN, PARA_UP
    }

    #[test]
    fn ledc_reports_frequency_and_duty_at_para_up_not_at_the_timer_overflow() {
        let mut p = periph();
        let mut t = PwmTap::new();
        p.ledc.write(0xd0, 3);                             // LEDC_CONF.APB_CLK_SEL = XTAL (40 MHz)
        p.ledc.write(0xa0, (40000 << 4) | 8 | (1 << 25));  // timer 0: div 40000 (156.25), 8 bit, PARA_UP
        p.ledc.write(0x08, 64 << 4);                       // ch0 DUTY = 64
        assert!(!t.check(&p, 5));                          // not enabled yet
        latch_ledc(&mut p);
        assert!(t.check(&p, 10));
        let (cycle, o) = t.events.pop().expect("no change");
        assert_eq!(cycle, 10);
        assert_eq!(o.signal, 73);
        assert!(o.enabled && !o.idle_high);
        assert!((o.freq - 1000.0).abs() < 1e-6, "{o:?}");
        assert!((o.duty - 0.25).abs() < 1e-9, "{o:?}");
        assert!(!t.check(&p, 20));                         // unchanged: no duplicate
    }

    #[test]
    fn ledc_full_duty_clamps_and_a_timer_update_reaches_bound_channels() {
        let mut p = periph();
        let mut t = PwmTap::new();
        p.ledc.write(0xd0, 3);
        p.ledc.write(0xa0, (40000 << 4) | 8 | (1 << 25));
        p.ledc.write(0x08, 256 << 4);                      // ledcWrite(full) writes 2^res
        latch_ledc(&mut p);
        t.check(&p, 1);
        assert_eq!(t.events.pop().map(|(_, o)| o.duty), Some(1.0));
        p.ledc.write(0xa0, (20000 << 4) | 8 | (1 << 25));  // timer 0 re-divided: 2 kHz
        assert!(t.check(&p, 2));
        assert!((t.events.pop().expect("timer change").1.freq - 2000.0).abs() < 1e-6);
    }

    #[test]
    fn mcpwm_servo_registers_give_50_hz_and_pulse_duty() {
        let mut p = periph();
        let mut t = PwmTap::new();
        p.mcpwm[0].clock_enabled = true;
        p.mcpwm[0].write(0x0, 15);                         // CLK_CFG.PRESCALE: 160 MHz / 16
        p.mcpwm[0].write(0x4, (19999 << 8) | 9);           // TIMER0_CFG0: prescale /10, period 19999
        p.mcpwm[0].write(0x3c, 1);                         // GEN0_STMP_CFG: load compare A on write
        p.mcpwm[0].write(0x40, 1484);                      // GEN0_TSTMP_A
        p.mcpwm[0].write(0x50, 2 | (1 << 4));              // GEN0_A: TEZ high, TEA low
        p.mcpwm[0].write(0x8, 2 | (1 << 3));               // TIMER0_CFG1: START=2, MOD=up
        t.check(&p, 7);
        t.check(&p, 8);   // the compare value counts before the timer latches it
        assert_eq!(p.mcpwm[0].output(&p.gpio, 0), None);
        let a = t.events.iter().rev().find(|(_, o)| o.signal == 160).map(|&(_, o)| o).expect("no OUT0A");
        assert!(a.enabled);
        assert!((a.freq - 50.0).abs() < 1e-9, "{a:?}");
        assert!((a.duty - 0.0742).abs() < 1e-9, "{a:?}");
        assert!(t.events.iter().all(|(_, o)| o.signal != 161), "OUT0B is not configured");
    }

    #[test]
    fn mcpwm_frozen_timer_is_disabled() {
        let mut p = periph();
        let mut t = PwmTap::new();
        p.mcpwm[0].clock_enabled = true;
        p.mcpwm[0].write(0x4, (19999 << 8) | 9);
        p.mcpwm[0].write(0x50, 2 | (1 << 4));
        p.mcpwm[0].write(0x40, 1000);
        assert!(!t.check(&p, 1));
    }

    #[test]
    fn a_reset_reports_running_outputs_as_disabled_once() {
        let mut p = periph();
        let mut t = PwmTap::new();
        p.ledc.write(0xd0, 3);
        p.ledc.write(0xa0, (40000 << 4) | 8 | (1 << 25));
        p.ledc.write(0x08, 64 << 4);
        latch_ledc(&mut p);
        t.check(&p, 1);
        t.events.clear();
        assert!(t.reset(99));
        assert_eq!(t.events.len(), 1);
        assert_eq!((t.events[0].0, t.events[0].1.signal, t.events[0].1.enabled), (99, 73, false));
        assert!(!t.reset(100));
    }
}
