use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use emu_core::ClockDomain;

// ------------------------------------------------------------------ RTC controller
/// Reset causes (RTC_CNTL_RESET_CAUSE_PROCPU), as the ROM prints them.
pub const RST_POWERON: u32 = 1; pub const RST_SW_SYS: u32 = 3; pub const RST_RTCWDT_SYS: u32 = 9; pub const RST_SW_CPU: u32 = 12;
pub const RST_RTCWDT_CPU: u32 = 13; pub const RST_RTCWDT_RTC: u32 = 16;
pub fn reset_cause_name(c: u32) -> &'static str {
    match c { 1 => "POWERON", 3 => "RTC_SW_SYS_RESET", 5 => "DEEPSLEEP", 7 => "TG0WDT_SYS_RESET", 8 => "TG1WDT_SYS_RESET", 9 => "RTCWDT_SYS_RESET", 11 => "TG0WDT_CPU_RESET",
            12 => "RTC_SW_CPU_RESET", 13 => "RTCWDT_CPU_RESET", 15 => "RTCWDT_BROWN_OUT_RESET", 16 => "RTCWDT_RTC_RESET", 17 => "TG1WDT_CPU_RESET", 18 => "SUPER_WDT_RESET", _ => "?" }
}

/// RTC_CNTL: reset control, slow-clock time and the RTC watchdog.
/// WDTCONFIG0..WDTWPROTECT live at 0x98..0xb0 on S3 and 0x90..0xa8 on C3.
/// `esp_restart()` on ESP-IDF 5.x arms this watchdog and spins until it resets the chip.
pub struct RtcCntl { pub ram: RegRam, pub slow_ticks: u64, pub time_latch: u64, pub sw_reset: bool, pub reset_cause: u32,
                     wdt_base: u32, wdt_count: u64, wdt_stage: usize, wdt_unlocked: bool,
                     /// S3 only: the SENS block at +0x800 converts on SAR_MEAS1_START (see `sens_meas`); the chip refreshes
                     /// `now_cycles` before each access so a waveform source is sampled at the right emulated time.
                     pub sens_adc: bool, pub analog: crate::analog::AnalogInputs, pub now_cycles: u64,
                     /// Jumperly patch J5: convert pad voltages linearly against the attenuation's full scale instead of
                     /// upstream's calibrated curve, so a potentiometer swept 0..3.3 V reads 0..4095 (see `linear_adc_code`).
                     pub adc_linear: bool }
impl RtcCntl {
    pub fn preset_after_bootloader(&mut self) { self.ram.write(0xc0, 0xFFD7_0028); self.ram.write(0xc4, 0xFF0F_00F0); }
    fn request_reset(&mut self, cause: u32) { if !self.sw_reset { self.sw_reset = true; self.reset_cause = cause; } }
    /// Advance the watchdog by RTC slow-clock ticks.
    pub fn wdt_tick(&mut self, ticks: u64) {
        let conf0 = self.ram.read(self.wdt_base);
        if conf0 & (1 << 31) == 0 { return; }
        self.wdt_count += ticks;
        while self.wdt_stage < 4 {
            let timeout = self.ram.read(self.wdt_base + 4 + 4 * self.wdt_stage as u32) as u64;
            let action = (conf0 >> (28 - 3 * self.wdt_stage as u32)) & 7;
            if action == 0 { self.wdt_stage += 1; continue; }              // stage disabled: skip
            if self.wdt_count < timeout { break; }
            self.wdt_count = 0; self.wdt_stage += 1;
            match action {
                1 => { self.ram.write(0x44, self.ram.read(0x44) | (1 << 3)); }   // INT_RAW.WDT
                2 => self.request_reset(RST_RTCWDT_CPU),
                3 => self.request_reset(RST_RTCWDT_SYS),
                4 => self.request_reset(RST_RTCWDT_RTC),
                _ => {}
            }
            if self.sw_reset { break; }
        }
        if self.wdt_stage >= 4 { self.wdt_stage = 0; }
    }
    pub fn new() -> Self { let mut r = Self::with_wdt_base(0x98); r.sens_adc = true; r }
    pub fn new_c3() -> Self { Self::with_wdt_base(0x90) }
    pub fn with_wdt_base(wdt_base: u32) -> Self {
        let mut r = RtcCntl { ram: RegRam::new(), slow_ticks: 0, time_latch: 0, sw_reset: false, reset_cause: RST_POWERON, wdt_base, wdt_count: 0, wdt_stage: 0, wdt_unlocked: false,
                              sens_adc: false, analog: crate::analog::AnalogInputs::new(240_000_000), now_cycles: 0, adc_linear: false };
        r.ram.write(0x38, 1 | (1 << 6));           // RESET_STATE: reset cause POWERON for both CPUs
        r.ram.write(0x74, 0);                        // CLK_CONF
        r
    }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x10 => self.time_latch as u32, 0x14 => (self.time_latch >> 32) as u32,
            0xc => self.ram.read(off) | (1 << 30),  // TIME_UPDATE: valid
            0x1fc => 0x2007270,
            0x850 => (self.ram.read(off) & !0x1ff) | (1 << 8) | 0x80,   // SENS_SAR_TSENS_CTRL (SENS block at +0x800): TSENS_READY, raw ~ room temperature
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        let wdt = self.wdt_base;
        match off {
            0x0 => { if v & (1 << 31) != 0 { self.request_reset(RST_SW_SYS); } else if v & (1 << 5) != 0 { self.request_reset(RST_SW_CPU); } self.ram.write(off, v & !((1 << 31) | (1 << 5))); }   // OPTIONS0.SW_SYS_RST / SW_PROCPU_RST
            0xc => { if v & (1 << 31) != 0 { self.time_latch = self.slow_ticks; } self.ram.write(off, v); }
            0x80c | 0x830 if self.sens_adc => self.sens_meas(off, v),
            _ if off == wdt + 0x18 => { self.wdt_unlocked = v == 0x50D8_3AA1; self.ram.write(off, v); }
            _ if (wdt..=wdt + 0x10).contains(&off) => { if self.wdt_unlocked { if off == wdt && (v ^ self.ram.read(wdt)) & (1 << 31) != 0 { self.wdt_count = 0; self.wdt_stage = 0; } self.ram.write(off, v); } }
            _ if off == wdt + 0x14 => { if self.wdt_unlocked && v & (1 << 31) != 0 { self.wdt_count = 0; self.wdt_stage = 0; } }   // WDTFEED
            _ => self.ram.write(off, v),
        }
    }
}

impl RtcCntl {
    /// The 12-bit ADC1 code a typical S3 gives for `volts` at `atten`: the inverse of the IDF 4.4
    /// calibration in `sar_adc` (the code whose calibrated reading is nearest), saturating at 4095, so
    /// analogReadMilliVolts() on the firmware reports the injected voltage to within a code.
    /// Noise, INL beyond Espressif's fitted curve and chip-to-chip spread are not modelled.
    pub fn s3_adc_code(volts: f32, atten: u32) -> u32 {
        crate::sar_adc::voltage_code(volts, atten, crate::sar_adc::Calibration::S3Adc1)
    }
    /// Jumperly patch J5: the code for `volts` at attenuation code `atten` (0, 2.5, 6, 11 dB) as a straight
    /// line to the ESP32-S3 datasheet full scale (950, 1250, 1750, 3100 mV), clamped to 4095, with the
    /// millivolts truncated to whole numbers.
    pub fn linear_adc_code(volts: f32, atten: u32) -> u32 {
        const FULL_SCALE_MV: [u32; 4] = [950, 1250, 1750, 3100];
        let mv = (volts * 1000.0).round().clamp(0.0, 65535.0) as u32;
        (mv * 4095 / FULL_SCALE_MV[(atten & 3) as usize]).min(4095)
    }
    /// SENS_SAR_MEAS1_CTRL2 (IDF adc_ll_rtc_start_convert / _convert_is_done / _get_convert_value):
    /// EN_PAD [30:19] picks the ADC1 channel, START [17] rising runs one conversion, DONE [16]
    /// and DATA [15:0] carry the result. ADC1 channel n is GPIO n+1 on the S3; the channel's
    /// attenuation is SENS_SAR_ATTEN1 (+0x814) bits [2n+1:2n]. ADC2 uses CTRL2 +0x830,
    /// ATTEN2 +0x838 and GPIO n+11. Both convert instantaneously.
    fn sens_meas(&mut self, off: u32, v: u32) {
        let prev = self.ram.read(off);
        let out = crate::sar_adc::sens_oneshot(prev, v & !0xffff, || {
            let pads = (v >> 19) & 0xfff;
            let code = if pads == 0 { 0 } else {
                let ch = pads.trailing_zeros();
                let adc2 = off == 0x830;
                let atten = (self.ram.read(if adc2 { 0x838 } else { 0x814 }) >> (2 * ch)) & 3;
                if ch >= 10 { 0 } else {
                    let linear = self.adc_linear;   // Jumperly patch J5
                    self.analog.convert(ch as u8 + if adc2 { 11 } else { 1 }, self.now_cycles, |v| {
                        if linear { Self::linear_adc_code(v, atten) }
                        else if adc2 { crate::sar_adc::voltage_code(v, atten, crate::sar_adc::Calibration::S3Adc2) }
                        else { Self::s3_adc_code(v, atten) }
                    })
                }
            };
            (1 << 16) | code
        });
        self.ram.write(off, out);
    }
}

impl Default for RtcCntl { fn default() -> Self { Self::new() } }

impl Device for RtcCntl {
    fn read(&mut self, off: u32) -> u32 { RtcCntl::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { RtcCntl::write(self, off, v); WriteEffect::NONE }
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::RtcSlow) }
    fn tick(&mut self, ticks: u64) { self.slow_ticks += ticks; self.wdt_tick(ticks); }
}

#[cfg(test)]
mod linear_adc_tests {
    use super::*;

    /// Jumperly patch J5: with `adc_linear` the SENS one-shot reads the pad as a straight line to the
    /// attenuation's full scale; without it upstream's calibrated curve applies.
    fn read_ch0(r: &mut RtcCntl) -> u32 {
        r.write(0x80c, 1 << 19);                              // EN_PAD = ADC1 CH0, START = 0
        r.write(0x80c, (1 << 19) | (1 << 17));                // START = 1: convert
        let v = r.read(0x80c);
        assert_ne!(v & (1 << 16), 0, "DONE not set");
        v & 0xffff
    }

    #[test]
    fn linear_transfer_clamps_at_full_scale() {
        let mut r = RtcCntl::new();
        r.adc_linear = true;
        r.write(0x814, 3);                                    // ADC1 CH0: 11 dB (3100 mV)
        r.analog.set(1, crate::analog::AnalogSource::Const(1.55));
        assert_eq!(read_ch0(&mut r), 1550 * 4095 / 3100);
        r.write(0x80c, 0);
        r.analog.set(1, crate::analog::AnalogSource::Const(3.3));
        assert_eq!(read_ch0(&mut r), 4095);                   // above full scale
        r.write(0x80c, 0);
        r.write(0x814, 0);                                    // 0 dB (950 mV)
        r.analog.set(1, crate::analog::AnalogSource::Const(0.475));
        assert_eq!(read_ch0(&mut r), 475 * 4095 / 950);
    }

    #[test]
    fn upstream_curve_is_used_without_the_flag() {
        let mut r = RtcCntl::new();
        r.write(0x814, 3);
        r.analog.set(1, crate::analog::AnalogSource::Const(1.55));
        assert_eq!(read_ch0(&mut r), RtcCntl::s3_adc_code(1.55, 3));
        assert_ne!(read_ch0(&mut r), 1550 * 4095 / 3100);
    }
}