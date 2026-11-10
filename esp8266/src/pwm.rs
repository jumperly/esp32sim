//! Jumperly patch J29: PWM for the host from the ESP8266's software waveform. The chip has no
//! PWM block: Arduino's `analogWrite`, `tone` and `Servo` toggle the pins from the FRC1 NMI
//! (Arduino ESP8266 3.1.2 cores/esp8266/core_esp8266_waveform_pwm.cpp: `timer1Interrupt`,
//! attached with `ETS_FRC_TIMER1_NMI_INTR_ATTACH`). A host that took every edge would end a slice
//! per edge and still not see a brightness; `PwmDetector` measures the edges instead and reports
//! a steady waveform as a `PwmOut` (frequency, duty), the event the LEDC tap of the other chips
//! sends (`esp32c3::pwm::PwmOut`), with the pin number as the signal.
//!
//! The rules (design ruling 2; the tolerances are fractions of the period):
//! - a period runs from a rising edge to the next; it matches another when its length is within
//!   1 % of the other's and its duty within 0.01 (1 % of the period) of the other's;
//! - a pin becomes `pwm` after `STABLE` (4) matching periods in a row whose frequency is within
//!   40 Hz ..= 100 kHz: one `PwmOut { enabled: true }` with the average of those periods;
//! - in `pwm` mode a waveform that changes (duty or frequency) is reported again after `CHANGE`
//!   (2) matching periods at the new values; `LOST` (4) periods in a row that do not match the
//!   running waveform and settle on nothing,
//!   or a new frequency outside the band, end `pwm` mode;
//! - no edge for more than 2 periods ends `pwm` mode (`poll`): `PwmOut { enabled: false }` with
//!   the pin's level in `idle_high`, stamped where the silence began counting as such;
//! - an edge the bus does not attribute to the waveform timer (`note_direct_edge`: written by the
//!   sketch, not by the FRC1 NMI) ends `pwm` mode at once and resets the measurement, so OneWire
//!   slots, DHT start pulses, `shiftOut`, NeoPixel bit-banging and other `digitalWrite` patterns
//!   stay edges whatever their timing (`SocBus::note_pin_write` classifies the edges).
//!
//! While a pin is in `pwm` mode the bus does not hand its edges to the pin watch.
use crate::jumperly::PINS;

/// One PWM output as the circuit sees it; the fields of the other chips' `PwmOut`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PwmOut {
    /// the GPIO number (the ESP8266 has no output matrix)
    pub signal: u32,
    pub enabled: bool,
    /// with `enabled: false`, the level the pin stays at
    pub idle_high: bool,
    pub freq: f64,
    pub duty: f64,
}

/// The clock the edge cycles count in: the 80 MHz APB clock (`SocBus::cycles`), also with CPU2X.
const HZ: f64 = crate::periph::CPU_HZ as f64;
const MIN_HZ: f64 = 40.0;
const MAX_HZ: f64 = 100_000.0;
/// Matching periods in a row before a pin becomes `pwm`.
pub const STABLE: u32 = 4;
/// Matching periods in a row at new values before a change is reported.
pub const CHANGE: u32 = 2;
/// Periods in a row that settle on nothing before `pwm` mode ends.
pub const LOST: u32 = 4;
/// Silence, in periods, that ends `pwm` mode.
const SILENCE: u64 = 2;

/// One period: its length and how long the pin was high in it, in cycles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Period { len: u64, high: u64 }

impl Period {
    fn duty(&self) -> f64 { self.high as f64 / self.len as f64 }
    /// Within 1 % in length and 0.01 in duty of `other`.
    fn matches(&self, other: &Period) -> bool {
        self.len.abs_diff(other.len) * 100 <= other.len && (self.duty() - other.duty()).abs() <= 0.01
    }
    fn freq(&self) -> f64 { HZ / self.len as f64 }
    fn in_band(&self) -> bool { (MIN_HZ..=MAX_HZ).contains(&self.freq()) }
}

/// Periods that match the first of them.
#[derive(Clone, Copy, Debug)]
struct Run { first: Period, n: u32, len: u64, high: u64 }

impl Run {
    fn new(p: Period) -> Self { Run { first: p, n: 1, len: p.len, high: p.high } }
    fn mean(&self) -> Period { Period { len: self.len / u64::from(self.n), high: self.high / u64::from(self.n) } }
}

#[derive(Clone, Copy, Debug, Default)]
struct Pin {
    level: bool,
    last_edge: u64,
    /// the rising edge that began the current period, and the falling edge in it
    rise: Option<u64>,
    fall: Option<u64>,
    run: Option<Run>,
    /// `pwm` mode: the waveform last reported
    on: Option<Period>,
    /// periods in `pwm` mode that matched neither `on` nor a settled change
    misses: u32,
}

/// The measurement of every GPIO's output edges and the `PwmOut` changes for the host.
pub struct PwmDetector {
    pins: [Pin; PINS],
    pub events: Vec<(u64, PwmOut)>,
}

impl Default for PwmDetector {
    fn default() -> Self { Self::new() }
}

impl PwmDetector {
    pub fn new() -> Self { PwmDetector { pins: [Pin::default(); PINS], events: Vec::new() } }

    /// The pins in `pwm` mode, bit n = GPIOn.
    pub fn pwm_pins(&self) -> u32 {
        self.pins.iter().enumerate().fold(0, |m, (i, p)| m | u32::from(p.on.is_some()) << i)
    }

    /// An output edge of `pin` driven by the waveform timer: true if an event was queued.
    pub fn note_edge(&mut self, pin: u8, level: bool, cycle: u64) -> bool {
        let Some(i) = self.index(pin) else { return false };
        let before = self.events.len();
        self.check_silence(i, cycle);
        let p = &mut self.pins[i];
        p.level = level;
        p.last_edge = cycle;
        if level {
            if let (Some(r), Some(f)) = (p.rise, p.fall) { self.period(i, Period { len: cycle - r, high: f - r }, cycle); }
            let p = &mut self.pins[i];
            p.rise = Some(cycle);
            p.fall = None;
        } else if p.rise.is_some() {
            p.fall = Some(cycle);
        }
        self.events.len() > before
    }

    /// An output edge of `pin` the sketch wrote itself: it is no waveform. Ends `pwm` mode (with
    /// the new level) and starts the measurement again. True if an event was queued.
    pub fn note_direct_edge(&mut self, pin: u8, level: bool, cycle: u64) -> bool {
        let Some(i) = self.index(pin) else { return false };
        let before = self.events.len();
        self.check_silence(i, cycle);
        let p = &mut self.pins[i];
        p.level = level;
        p.last_edge = cycle;
        if p.on.is_some() { self.leave(i, cycle); }
        let p = &mut self.pins[i];
        (p.rise, p.fall, p.run) = (None, None, None);
        self.events.len() > before
    }

    /// End `pwm` mode on the pins that have been silent for more than 2 periods by `now`.
    pub fn poll(&mut self, now: u64) -> bool {
        let before = self.events.len();
        for i in 0..PINS { self.check_silence(i, now); }
        self.events.len() > before
    }

    /// The chip reset: every output in `pwm` mode is off, the measurements start again.
    pub fn reset(&mut self, cycle: u64) -> bool {
        let mut any = false;
        for (i, p) in self.pins.iter_mut().enumerate() {
            if p.on.is_some() {
                self.events.push((cycle, off(i, false)));
                any = true;
            }
            *p = Pin::default();
        }
        any
    }

    /// The changes since the last call, in cycle order within this take (a silence-off event is
    /// stamped where the silence began counting, but pushed when `poll` runs, so the pushes of
    /// different pins can be out of order; the sort is stable: equal cycles keep push order).
    pub fn take(&mut self) -> Vec<(u64, PwmOut)> {
        let mut events = std::mem::take(&mut self.events);
        events.sort_by_key(|e| e.0);
        events
    }

    fn index(&self, pin: u8) -> Option<usize> { (usize::from(pin) < PINS).then_some(usize::from(pin)) }

    fn check_silence(&mut self, i: usize, now: u64) {
        let p = &self.pins[i];
        let Some(on) = p.on else { return };
        let end = p.last_edge + SILENCE * on.len;
        if now > end {
            self.leave(i, end);
            let p = &mut self.pins[i];
            (p.rise, p.fall, p.run) = (None, None, None);
        }
    }

    /// `pwm` mode ends at `cycle` with the pin's level.
    fn leave(&mut self, i: usize, cycle: u64) {
        let p = &mut self.pins[i];
        p.on = None;
        p.misses = 0;
        self.events.push((cycle, off(i, p.level)));
    }

    fn report(&mut self, i: usize, w: Period, cycle: u64) {
        let p = &mut self.pins[i];
        p.on = Some(w);
        p.misses = 0;
        self.events.push((cycle, PwmOut { signal: i as u32, enabled: true, idle_high: false, freq: w.freq(), duty: w.duty() }));
    }

    /// A period of pin `i` ended at `cycle`.
    fn period(&mut self, i: usize, w: Period, cycle: u64) {
        let p = &mut self.pins[i];
        let run = match p.run {
            Some(mut r) if w.matches(&r.first) => { r.n += 1; r.len += w.len; r.high += w.high; r }
            _ => Run::new(w),
        };
        p.run = Some(run);
        match p.on {
            None => {
                if run.n >= STABLE && run.mean().in_band() { self.report(i, run.mean(), cycle); }
            }
            Some(on) if w.matches(&on) => p.misses = 0,
            Some(on) => {
                p.misses += 1;
                let settled = run.n >= CHANGE && !run.first.matches(&on);
                if settled && run.mean().in_band() {
                    self.report(i, run.mean(), cycle);
                } else if settled || p.misses >= LOST {
                    self.leave(i, cycle);
                }
            }
        }
    }
}

fn off(pin: usize, level: bool) -> PwmOut { PwmOut { signal: pin as u32, enabled: false, idle_high: level, freq: 0.0, duty: 0.0 } }

#[cfg(test)]
mod tests {
    use super::*;

    /// 80 MHz cycles per microsecond.
    const US: u64 = 80;

    /// `n` periods of a square wave on `pin` from `start`: rising edges `period` apart, each high
    /// for `high` cycles, fed as timer edges. The cycle after the last falling edge's period.
    fn square(d: &mut PwmDetector, pin: u8, start: u64, period: u64, high: u64, n: u64) -> u64 {
        for k in 0..n {
            d.note_edge(pin, true, start + k * period);
            d.note_edge(pin, false, start + k * period + high);
        }
        start + n * period
    }

    #[test]
    fn a_1khz_25_percent_square_becomes_pwm_after_four_periods() {
        let mut d = PwmDetector::new();
        // four rising edges close three periods: not yet
        let t = square(&mut d, 4, 1000, 1000 * US, 250 * US, 4);
        assert!(d.take().is_empty(), "three periods are not enough");
        // the fifth rising edge closes the fourth period
        assert!(d.note_edge(4, true, t));
        let ev = d.take();
        assert_eq!(ev.len(), 1, "{ev:?}");
        let (cycle, o) = ev[0];
        assert_eq!(cycle, t);
        assert_eq!((o.signal, o.enabled, o.idle_high), (4, true, false));
        assert!((o.freq - 1000.0).abs() <= 10.0, "{o:?}");
        assert!((o.duty - 0.25).abs() <= 0.01, "{o:?}");
        assert_eq!(d.pwm_pins(), 1 << 4);
        // more of the same reports nothing
        d.note_edge(4, false, t + 250 * US);
        square(&mut d, 4, t + 1000 * US, 1000 * US, 250 * US, 10);
        assert!(d.take().is_empty());
    }

    #[test]
    fn servo_50hz_1500us_is_pwm() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 14, 0, 20_000 * US, 1500 * US, 5);
        d.note_edge(14, true, t);
        let ev = d.take();
        assert_eq!(ev.len(), 1, "{ev:?}");
        let o = ev[0].1;
        assert!(o.enabled && (o.freq - 50.0).abs() <= 0.5, "{o:?}");
        assert!((o.duty - 0.075).abs() <= 0.001, "{o:?}");
    }

    #[test]
    fn eight_shiftout_clock_pulses_at_500khz_are_not_pwm() {
        // shiftOut's clock: 8 regular pulses at 500 kHz, far above the band (even as timer edges)
        let mut d = PwmDetector::new();
        square(&mut d, 14, 0, 2 * US, US, 8);
        assert!(d.take().is_empty());
        assert_eq!(d.pwm_pins(), 0);
    }

    #[test]
    fn neopixel_800khz_is_not_pwm() {
        // WS2812 bits: 1.25 us each, a 1 high for 0.8 us, a 0 for 0.4 us; 0xff 0x00 0xff 0xff
        let mut d = PwmDetector::new();
        let mut t = 0;
        for byte in [0xffu8, 0x00, 0xff, 0xff] {
            for bit in (0..8).rev() {
                let high = if byte >> bit & 1 != 0 { 64 } else { 32 };
                d.note_edge(2, true, t);
                d.note_edge(2, false, t + high);
                t += 100;
            }
        }
        assert!(d.take().is_empty());
        assert_eq!(d.pwm_pins(), 0);
    }

    #[test]
    fn duty_change_reports_after_two_periods() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 4, 0, 1000 * US, 250 * US, 6);
        assert_eq!(d.take().len(), 1);
        // 75 %: the first period at the new duty is a change, not yet a report
        let t = square(&mut d, 4, t, 1000 * US, 750 * US, 1);
        d.note_edge(4, true, t);
        assert!(d.take().is_empty(), "one period at 75 %");
        d.note_edge(4, false, t + 750 * US);
        assert!(d.note_edge(4, true, t + 1000 * US), "the second period at 75 % reports");
        let ev = d.take();
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(ev[0].1.enabled && (ev[0].1.duty - 0.75).abs() <= 0.01, "{ev:?}");
        assert!((ev[0].1.freq - 1000.0).abs() <= 10.0, "{ev:?}");
        assert_eq!(d.pwm_pins(), 1 << 4);
    }

    #[test]
    fn silence_reports_disabled_with_level() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 5, 0, 1000 * US, 600 * US, 5);
        d.note_edge(5, true, t);                    // the waveform stops high
        assert_eq!(d.take().len(), 1);
        assert!(!d.poll(t + 1500 * US), "1.5 periods of silence");
        assert!(d.poll(t + 2500 * US));
        let ev = d.take();
        assert_eq!(ev, vec![(t + 2000 * US, PwmOut { signal: 5, enabled: false, idle_high: true, freq: 0.0, duty: 0.0 })]);
        assert_eq!(d.pwm_pins(), 0);
        // the next waveform needs its four periods again
        let t2 = square(&mut d, 5, t + 10_000 * US, 1000 * US, 600 * US, 4);
        assert!(d.take().is_empty());
        assert!(d.note_edge(5, true, t2));
    }

    /// DS18B20 traffic as OneWire bit-bangs it (OneWire 2.3.x write_bit: a 1 is 10 us low then
    /// 55 us high, a 0 is 65 us low then 5 us high; read slots release the pin, so their
    /// `GPIO_OUT & GPIO_ENABLE` stays low): reset, skip ROM 0xCC, convert 0x44. The slot pattern
    /// never repeats four periods, even fed as timer edges.
    #[test]
    fn lost_ends_pwm_after_exactly_lost_misses() {
        // 1 kHz 25 %; then rising edges 1.5 ms apart (inside the silence limit, never two periods
        // alike): the LOST-th period that matches nothing ends pwm mode, not the one after it
        let mut d = PwmDetector::new();
        let t = square(&mut d, 4, 0, 1000 * US, 250 * US, 6);
        assert_eq!(d.take().len(), 1);
        d.note_edge(4, true, t);
        let mut now = t;
        for k in 1..=LOST {
            now += (1000 + 300 * u64::from(k)) * US;
            d.note_edge(4, true, now);
            let left = d.pwm_pins() == 0;
            assert_eq!(left, k == LOST, "miss {k}");
        }
        let ev = d.take();
        assert!(ev.iter().any(|e| !e.1.enabled), "{ev:?}");
    }

    #[test]
    fn take_orders_events_by_cycle_across_pins() {
        let mut d = PwmDetector::new();
        // pin 5 stops first; `poll` visits pin 4 first, so its later event is pushed first
        let t5 = square(&mut d, 5, 0, 1000 * US, 500 * US, 6);
        d.take();
        let t4 = square(&mut d, 4, 1000 * US, 1000 * US, 500 * US, 6);
        d.take();
        assert!(t4 > t5);
        assert!(d.poll(t4 + 20_000 * US));
        let ev = d.take();
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert!(ev[0].0 <= ev[1].0, "{ev:?}");
        assert_eq!(ev[0].1.signal, 5, "pin 5 went silent first: {ev:?}");
    }

    #[test]
    fn irregular_onewire_edges_are_not_pwm() {
        fn slot_bits(d: &mut PwmDetector, t: &mut u64, byte: u8, direct: bool) {
            for bit in 0..8 {
                let low = if byte >> bit & 1 != 0 { 10 } else { 65 };
                let high = if byte >> bit & 1 != 0 { 55 } else { 5 };
                if direct { d.note_direct_edge(4, false, *t); } else { d.note_edge(4, false, *t); }
                *t += low * US;
                if direct { d.note_direct_edge(4, true, *t); } else { d.note_edge(4, true, *t); }
                *t += high * US;
            }
        }
        let mut d = PwmDetector::new();
        let mut t = 0;
        d.note_edge(4, true, t);
        t += 100 * US;
        d.note_edge(4, false, t);                   // reset pulse: 480 us low, 480 us released
        t += 480 * US;
        d.note_edge(4, true, t);
        t += 480 * US;
        for byte in [0xcc, 0x44] { slot_bits(&mut d, &mut t, byte, false); }
        assert!(d.take().is_empty() && d.pwm_pins() == 0);
        // Read scratchpad 0xbe has five 1 slots in a row: four matching 70 us periods at 14 kHz.
        // As timer edges that is a waveform; the bus hands the sketch's own writes over as
        // direct edges, which never are.
        let mut timed = PwmDetector::new();
        let mut t1 = 0;
        slot_bits(&mut timed, &mut t1, 0xbe, false);
        assert_eq!(timed.pwm_pins(), 1 << 4, "the run of ones is PWM-shaped");
        slot_bits(&mut d, &mut t, 0xbe, true);
        assert!(d.take().is_empty() && d.pwm_pins() == 0);
    }

    #[test]
    fn a_direct_edge_ends_pwm_mode_at_once() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 4, 0, 1000 * US, 250 * US, 5);
        d.note_edge(4, true, t);
        assert_eq!(d.take().len(), 1);
        // analogWrite(4, 0): the NMI stops, digitalWrite(4, LOW) comes from the sketch
        assert!(d.note_direct_edge(4, false, t + 100 * US));
        assert_eq!(d.take(), vec![(t + 100 * US, PwmOut { signal: 4, enabled: false, idle_high: false, freq: 0.0, duty: 0.0 })]);
        assert_eq!(d.pwm_pins(), 0);
    }

    #[test]
    fn a_reset_turns_running_outputs_off() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 4, 0, 1000 * US, 250 * US, 5);
        d.note_edge(4, true, t);
        d.take();
        assert!(d.reset(t + 5));
        assert_eq!(d.take(), vec![(t + 5, PwmOut { signal: 4, enabled: false, idle_high: false, freq: 0.0, duty: 0.0 })]);
        assert!(!d.reset(t + 6));
    }

    #[test]
    fn a_tone_below_40_hz_stays_edges() {
        let mut d = PwmDetector::new();
        let t = square(&mut d, 5, 0, 1_000_000 * US / 31, 500_000 * US / 31, 8);
        d.note_edge(5, true, t);
        assert!(d.take().is_empty());
    }
}
