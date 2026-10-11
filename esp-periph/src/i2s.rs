//! I2S TX: the clock tree that sets the frame rate, and the sample sink the SoC's DMA pump fills.
//! Jumperly patch J17: I2S RX in master mode — the RX clock tree, the frame format, and a queue of
//! slot words from outside the chip that the SoC's DMA pump turns into received samples.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use std::collections::VecDeque;

/// Jumperly patch J17: the most frames the outside may queue ahead of the RX pump.
pub const RX_FRAMES_MAX: usize = 1 << 16;


pub struct I2s {
    pub rx_conf: u32, pub tx_conf: u32, pub int_raw: u32, pub int_ena: u32,
    ram: RegRam,
    /// TX_CONF1 (0x2c: data/slot/frame widths, BCK divider), TX_CLKM_CONF (0x34: source, integer MCLK divider),
    /// TX_CLKM_DIV_CONF (0x3c: fractional MCLK divider x/y/z/yn1), TX_TDM_CTRL (0x54: slot count)
    pub tx_conf1: u32, pub tx_clkm_conf: u32, pub tx_clkm_div_conf: u32, pub tx_tdm_ctrl: u32,
    /// frame rate on the wire, derived from the clock registers exactly as the silicon divides
    /// its source clock; 44.1 kHz until firmware programs the clock
    pub sample_rate: u32,
    pub bytes_per_frame: u32,
    acc: u64,
    /// decoded left-channel samples (host sink)
    pub pcm: Vec<i16>,
    pub frames_out: u64,
    pub tx_started_log: bool,
    cpu_hz: u64,
    /// Jumperly patch J17: RX_CONF1 (0x28: data/slot/half-frame widths, BCK divider, MSB shift),
    /// RX_CLKM_CONF (0x30), RX_CLKM_DIV_CONF (0x38), RX_TDM_CTRL (0x50: slot count and enables),
    /// RX_EOF_NUM (0x64: received bytes per GDMA IN_SUC_EOF, as ESP-IDF programs it)
    pub rx_conf1: u32, pub rx_clkm_conf: u32, pub rx_clkm_div_conf: u32, pub rx_tdm_ctrl: u32, pub rx_eof_num: u32,
    /// RX frame rate derived from the RX clock registers; 44.1 kHz until firmware programs them
    pub rx_sample_rate: u32,
    rx_acc: u64,
    /// frames that fell due while the outside had none queued (host source only)
    pub rx_owed: u32,
    /// the starving pump asked the outside for frames; cleared by `rx_push`
    rx_asked: bool,
    /// slot words from outside the chip, per frame (left = WS low, right = WS high): MSB-aligned,
    /// bit 31 is the first bit after the WS edge and the I2S one-BCK delay
    pub rx_frames: VecDeque<[u32; 2]>,
    /// frames the RX pump has received (taken from the queue or read as silence)
    pub frames_in: u64,
}
impl I2s {
    pub fn new(cpu_hz: u64) -> Self { I2s { cpu_hz, rx_conf: 0, tx_conf: 0, int_raw: 0, int_ena: 0, ram: RegRam::new(), tx_conf1: 0, tx_clkm_conf: 0, tx_clkm_div_conf: 0, tx_tdm_ctrl: 0xffff, sample_rate: 44100, bytes_per_frame: 1, acc: 0, pcm: Vec::new(), frames_out: 0, tx_started_log: false,
        rx_conf1: 0, rx_clkm_conf: 0, rx_clkm_div_conf: 0, rx_tdm_ctrl: 0xffff, rx_eof_num: 0x40, rx_sample_rate: 44100, rx_acc: 0, rx_owed: 0, rx_asked: false, rx_frames: VecDeque::new(), frames_in: 0 } }
    pub fn tx_running(&self) -> bool { self.tx_conf & (1 << 2) != 0 }
    /// Packed DMA sample width, independent of padding in the wire's time slots.
    pub fn sample_bytes(&self) -> usize { (((self.tx_conf1 >> 13) & 0x1f) + 1).div_ceil(8) as usize }
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0xc => self.int_raw, 0x10 => self.int_raw & self.int_ena, 0x14 => self.int_ena,
            0x20 => self.rx_conf & !(1 << 8) & !3, 0x24 => self.tx_conf & !(1 << 8) & !3,   // update/reset bits self-clear
            0x6c => if self.tx_running() { 0 } else { 1 },                                   // STATE: tx_idle
            0x54 => self.tx_tdm_ctrl,
            0x50 => self.rx_tdm_ctrl, 0x64 => self.rx_eof_num,                                 // Jumperly patch J17
            0x80 => 0x2003070,
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x14 => self.int_ena = v, 0x18 => self.int_raw &= !v,
            0x20 => {
                // Jumperly patch J17: starting or stopping RX restarts the frame clock and drops
                // what the outside queued for the old run.
                let was = self.rx_running();
                self.rx_conf = v;
                if self.rx_running() != was { self.rx_acc = 0; self.rx_owed = 0; self.rx_asked = false; self.rx_frames.clear(); }
                self.update_rx_rate();
            }
            0x28 => { self.rx_conf1 = v; self.ram.write(off, v); self.update_rx_rate(); }        // Jumperly patch J17
            0x30 => { self.rx_clkm_conf = v; self.ram.write(off, v); self.update_rx_rate(); }
            0x38 => { self.rx_clkm_div_conf = v; self.ram.write(off, v); self.update_rx_rate(); }
            0x50 => { self.rx_tdm_ctrl = v; self.ram.write(off, v); }
            0x64 => self.rx_eof_num = v & 0xfff,
            0x24 => { self.tx_conf = v; self.update_rate(); }
            0x2c => { self.tx_conf1 = v; self.ram.write(off, v); self.update_rate(); }
            0x34 => { self.tx_clkm_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x3c => { self.tx_clkm_div_conf = v; self.ram.write(off, v); self.update_rate(); }
            0x54 => { self.tx_tdm_ctrl = v; self.ram.write(off, v); self.update_rate(); }
            _ => self.ram.write(off, v),
        }
    }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }
    /// The TX frame rate the clock tree produces:
    ///   MCLK = src / (div_num + b/a)   with b/a recovered from the x/y/z/yn1 fields the way
    ///                                  `i2s_ll_tx_set_mclk` encodes them (b = z, a = (x+1)·z + y;
    ///                                  with yn1 the fraction is 1 − b/a)
    ///   BCK  = MCLK / (bck_div_num + 1)
    ///   fs   = BCK / (2 · (half_sample_bits + 1))
    /// Returns None while the clock is off or unprogrammed, so the default stays in force.
    pub fn derive_rate(&self) -> Option<u32> { frame_rate(self.tx_clkm_conf, self.tx_clkm_div_conf, self.tx_conf1) }
    /// Jumperly patch J17: the RX frame rate, from the RX clock registers the same way (the RX
    /// CLKM_CONF / CLKM_DIV_CONF / CONF1 fields sit where the TX ones do).
    pub fn derive_rx_rate(&self) -> Option<u32> { frame_rate(self.rx_clkm_conf, self.rx_clkm_div_conf, self.rx_conf1) }
    fn update_rate(&mut self) {
        if let Some(fs) = self.derive_rate() { self.sample_rate = fs; }
        let slots = ((self.tx_tdm_ctrl >> 16) & 0xf) + 1;
        let active = (self.tx_tdm_ctrl & ((1 << slots) - 1)).count_ones();
        // SKIP_MSK consumes DMA data for inactive slots too. Otherwise only
        // enabled slots have data; mono reuses one sample across the frame.
        let dma_slots = if self.tx_conf & (1 << 5) != 0 { u32::from(active != 0) }
            else if self.tx_tdm_ctrl & (1 << 20) != 0 { slots } else { active };
        self.bytes_per_frame = self.sample_bytes() as u32 * dma_slots;
    }
    /// Number of frames due after `cycles` CPU cycles at the configured sample rate.
    pub fn frames_due(&mut self, cycles: u64) -> u32 {
        if !self.tx_running() { self.acc = 0; return 0; }
        self.acc += cycles * self.sample_rate as u64;
        let n = (self.acc / self.cpu_hz) as u32;
        self.acc %= self.cpu_hz;
        n
    }
}

/// The frame rate one direction's clock tree produces (see `I2s::derive_rate`): CLKM_CONF `c`,
/// CLKM_DIV_CONF `d`, CONF1 `conf1`.
fn frame_rate(c: u32, d: u32, conf1: u32) -> Option<u32> {
    if c & (1 << 26) == 0 { return None; }                                   // CLK_ACTIVE
    let src: u64 = match (c >> 27) & 3 { 0 => 40_000_000, 1 => 240_000_000, 2 => 160_000_000, _ => return None };   // XTAL / PLL240M / PLL160M / external
    let n = (c & 0xff) as u64;
    if n == 0 { return None; }
    let (z, y, x, yn1) = ((d & 0x1ff) as u64, ((d >> 9) & 0x1ff) as u64, ((d >> 18) & 0x1ff) as u64, d & (1 << 27) != 0);
    let (a, b) = if z == 0 { (1, 0) } else { let a = (x + 1) * z + y; (a, if yn1 { a - z } else { z }) };
    let bck = ((conf1 >> 7) & 0x3f) as u64 + 1;
    // WS_WIDTH controls the WS pulse, not the frame duration. IDF programs
    // HALF_SAMPLE_BITS = slot_bits * total_slots / 2 - 1 for standard/TDM TX.
    let frame_bits = 2 * (((conf1 >> 18) & 0x3f) as u64 + 1);
    let denom = (n * a + b) * bck * frame_bits;
    if denom == 0 { return None; }
    let fs = (src * a + denom / 2) / denom;
    if !(1_000..=400_000).contains(&fs) { return None; }
    Some(fs as u32)
}

/// Jumperly patch J17: the RX side. The SoC's DMA pump calls `rx_take` with the elapsed cycles and
/// writes `rx_encode` of the frames into the GDMA IN channel bound to this controller; a host
/// queues the slot words of the frames to come with `rx_push`, sized by `rx_frames_needed`.
impl I2s {
    /// RX_START (bit 2), master (RX_SLAVE_MOD bit 3 clear), standard/TDM (RX_TDM_EN bit 19, not
    /// RX_PDM_EN bit 20): the RX clock runs and frames arrive at `rx_sample_rate`. A slave waits
    /// for an outside BCK, and PDM RX is a different data path; neither is modelled.
    pub fn rx_running(&self) -> bool { self.rx_conf & ((1 << 2) | (1 << 3) | (1 << 19) | (1 << 20)) == (1 << 2) | (1 << 19) }
    /// BCK cycles per slot on the wire (RX_TDM_CHAN_BITS + 1).
    pub fn rx_slot_bits(&self) -> u32 { ((self.rx_conf1 >> 24) & 0x1f) + 1 }
    /// Valid data bits per slot (RX_BITS_MOD + 1).
    pub fn rx_data_bits(&self) -> u32 { ((self.rx_conf1 >> 13) & 0x1f) + 1 }
    /// Slots per frame (RX_TDM_TOT_CHAN_NUM + 1); standard mode programs 2.
    pub fn rx_slots(&self) -> u32 { ((self.rx_tdm_ctrl >> 16) & 0xf) + 1 }
    /// The slots that are received, bit n = slot n (RX_TDM_CHANn_EN within the frame).
    pub fn rx_slot_mask(&self) -> u32 { self.rx_tdm_ctrl & ((1 << self.rx_slots()) - 1) }
    /// Bytes one received sample takes in memory: 8/16/32-bit data in 1/2/4, 24-bit in 3, or in
    /// 4 with RX_24_FILL_EN (bit 16).
    pub fn rx_sample_bytes(&self) -> usize {
        let bits = self.rx_data_bits();
        if bits > 16 && bits <= 24 && self.rx_conf & (1 << 16) != 0 { 4 } else { bits.div_ceil(8) as usize }
    }
    /// Bytes after which the GDMA IN channel reports SUC_EOF (RX_EOF_NUM; ESP-IDF writes the DMA
    /// buffer size there, in bytes). None while it is 0.
    pub fn rx_eof_bytes(&self) -> Option<u32> { (self.rx_eof_num != 0).then_some(self.rx_eof_num) }
    /// BCK frequency on the wire, Hz.
    pub fn rx_bck_hz(&self) -> u32 { self.rx_sample_rate.saturating_mul(self.rx_slots() * self.rx_slot_bits()) }
    fn update_rx_rate(&mut self) {
        if let Some(fs) = self.derive_rx_rate() { self.rx_sample_rate = fs; }
    }
    /// Frames the outside should queue now so that the next `cycles` CPU cycles find them waiting:
    /// those falling due in that time and the ones already owed, less those queued. 0 while RX is off.
    pub fn rx_frames_needed(&self, cycles: u64) -> u32 {
        if !self.rx_running() { return 0; }
        let due = (self.rx_acc + cycles * self.rx_sample_rate as u64) / self.cpu_hz + 1;   // one frame of slack for a slice that overruns
        (due + self.rx_owed as u64).saturating_sub(self.rx_frames.len() as u64).min(RX_FRAMES_MAX as u64) as u32
    }
    /// Queue slot words (left, right) for the frames to come; returns how many fit (`RX_FRAMES_MAX`).
    pub fn rx_push(&mut self, frames: &[[u32; 2]]) -> usize {
        let n = frames.len().min(RX_FRAMES_MAX.saturating_sub(self.rx_frames.len()));
        self.rx_frames.extend(&frames[..n]);
        if n > 0 { self.rx_asked = false; }
        n
    }
    /// The frames received in the next `cycles` CPU cycles. Without a `host` every frame is silence
    /// (nothing drives SD). With one, frames come from the queue; those it lacks are owed and the
    /// second value asks the caller once to fetch more (end the slice). Owed frames beyond 100 ms
    /// are given up as silence, so a host that never answers cannot stall the receiver.
    pub fn rx_take(&mut self, cycles: u64, host: bool) -> (Vec<[u32; 2]>, bool) {
        if !self.rx_running() { self.rx_acc = 0; self.rx_owed = 0; return (Vec::new(), false); }
        self.rx_acc += cycles * self.rx_sample_rate as u64;
        let due = (self.rx_acc / self.cpu_hz) as usize + self.rx_owed as usize;
        self.rx_acc %= self.cpu_hz;
        if !host {
            self.rx_owed = 0;
            self.frames_in += due as u64;
            return (vec![[0, 0]; due], false);
        }
        let n = due.min(self.rx_frames.len());
        let mut frames: Vec<[u32; 2]> = self.rx_frames.drain(..n).collect();
        let mut owed = due - n;
        let limit = (self.rx_sample_rate / 10).max(1) as usize;
        if owed > limit { frames.resize(frames.len() + owed - limit, [0, 0]); owed = limit; }
        self.rx_owed = owed as u32;
        let ask = owed > 0 && !self.rx_asked;
        if ask { self.rx_asked = true; }
        self.frames_in += frames.len() as u64;
        (frames, ask)
    }
    /// The bytes the receiver writes to memory for `frames`: per frame, one sample per received
    /// slot in slot order (slots past the second read silence), little-endian unless RX_BIG_ENDIAN
    /// (bit 7). A sample is the first RX_BITS_MOD + 1 bits of the slot with RX_LEFT_ALIGN (bit 15,
    /// ESP-IDF's default), else its last ones; RX_BIT_ORDER (bit 18) takes them LSB first. Without
    /// RX_MSB_SHIFT (Philips one-BCK delay) the receiver samples one BCK early, so the word arrives
    /// shifted right by one bit.
    pub fn rx_encode(&self, frames: &[[u32; 2]]) -> Vec<u8> {
        let (slots, mask, data, slot_bits) = (self.rx_slots(), self.rx_slot_mask(), self.rx_data_bits(), self.rx_slot_bits().max(self.rx_data_bits()));
        let bytes = self.rx_sample_bytes();
        let (left_align, big_endian, lsb_first, philips) = (self.rx_conf & (1 << 15) != 0, self.rx_conf & (1 << 7) != 0, self.rx_conf & (1 << 18) != 0, self.rx_conf1 & (1 << 29) != 0);
        let data_mask = if data >= 32 { u32::MAX } else { (1u32 << data) - 1 };
        let mut out = Vec::with_capacity(frames.len() * bytes * mask.count_ones() as usize);
        for f in frames {
            for slot in 0..slots {
                if mask & (1 << slot) == 0 { continue; }
                let mut word = f.get(slot as usize).copied().unwrap_or(0);
                if !philips { word >>= 1; }
                let mut sample = if left_align { word.checked_shr(32 - data).unwrap_or(0) } else { word.checked_shr(32 - slot_bits).unwrap_or(0) & data_mask };
                if lsb_first { sample = sample.reverse_bits().checked_shr(32 - data).unwrap_or(0); }
                let le = sample.to_le_bytes();
                if big_endian { out.extend(le[..bytes].iter().rev()); } else { out.extend_from_slice(&le[..bytes]); }
            }
        }
        out
    }
}

impl Device for I2s {
    fn read(&mut self, off: u32) -> u32 { I2s::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2s::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
}

#[cfg(test)]
mod rx_tests {
    //! Jumperly patch J17: the RX side as ESP-IDF v5.5 `i2s_std` programs it for the Arduino
    //! `ESP_I2S` library (master RX, Philips, 16 kHz, 32-bit mono left, PLL160M, MCLK = 256 fs).
    use super::*;

    const CPU_HZ: u64 = 240_000_000;
    /// RX_CONF: RX_START, RX_TDM_EN, RX_LEFT_ALIGN, RX_MONO + RX_MONO_FST_VLD.
    const RX_CONF_STD_MONO: u32 = (1 << 2) | (1 << 19) | (1 << 15) | (1 << 5) | (1 << 9);

    /// 160 MHz / (39 + 1/16) = 4.096 MHz MCLK, / 4 = 1.024 MHz BCK, / 64 = 16 kHz.
    fn std_rx(data_bits: u32, slot_bits: u32, slots_mask: u32) -> I2s {
        let mut i2s = I2s::new(CPU_HZ);
        i2s.write(0x30, 39 | (1 << 26) | (2 << 27));                             // RX_CLKM_CONF: div 39, active, PLL160M
        i2s.write(0x38, 1 | (15 << 18));                                          // z = 1, x = 15, y = 0: fraction 1/16
        let conf1 = (slot_bits - 1) | (3 << 7) | ((data_bits - 1) << 13) | ((slot_bits - 1) << 18) | ((slot_bits - 1) << 24) | (1 << 29);
        i2s.write(0x28, conf1);
        i2s.write(0x50, (1 << 16) | slots_mask);
        i2s.write(0x64, 960);
        i2s.write(0x20, RX_CONF_STD_MONO);
        i2s
    }

    #[test]
    fn idf_rx_clock_gives_16_khz_and_1024_khz_bck() {
        let i2s = std_rx(32, 32, 1);
        assert!(i2s.rx_running());
        assert_eq!(i2s.rx_sample_rate, 16_000);
        assert_eq!(i2s.rx_bck_hz(), 1_024_000);
        assert_eq!((i2s.rx_slot_bits(), i2s.rx_data_bits(), i2s.rx_slot_mask(), i2s.rx_sample_bytes()), (32, 32, 1, 4));
        assert_eq!(i2s.rx_eof_bytes(), Some(960));
        assert_eq!(i2s.read(0x64), 960);
    }

    #[test]
    fn slave_pdm_or_stopped_rx_receives_nothing() {
        let mut i2s = std_rx(32, 32, 1);
        for conf in [RX_CONF_STD_MONO & !(1 << 2), RX_CONF_STD_MONO | (1 << 3), (RX_CONF_STD_MONO & !(1 << 19)) | (1 << 20)] {
            i2s.write(0x20, conf);
            assert!(!i2s.rx_running());
            assert_eq!(i2s.rx_frames_needed(CPU_HZ), 0);
            assert_eq!(i2s.rx_take(CPU_HZ, false).0.len(), 0);
        }
    }

    #[test]
    fn without_a_host_frames_are_silence_at_the_frame_rate() {
        let mut i2s = std_rx(32, 32, 1);
        let (frames, ask) = i2s.rx_take(CPU_HZ / 1000, false);                    // 1 ms
        assert_eq!((frames.len(), ask), (16, false));
        assert!(frames.iter().all(|f| *f == [0, 0]));
        assert_eq!(i2s.rx_take(CPU_HZ / 32_000, false).0.len(), 0);               // half a frame
        assert_eq!(i2s.rx_take(CPU_HZ / 32_000, false).0.len(), 1);
        assert_eq!(i2s.frames_in, 17);
    }

    #[test]
    fn host_frames_are_taken_in_order_and_a_shortfall_asks_once() {
        let mut i2s = std_rx(32, 32, 1);
        let need = i2s.rx_frames_needed(CPU_HZ / 1000);
        assert_eq!(need, 17);                                                     // 16 due + one of slack
        let frames: Vec<[u32; 2]> = (0..10).map(|i| [i, !i]).collect();
        assert_eq!(i2s.rx_push(&frames), 10);
        let (got, ask) = i2s.rx_take(CPU_HZ / 1000, true);
        assert_eq!(got, frames);
        assert!(ask);
        assert_eq!(i2s.rx_owed, 6);
        assert!(!i2s.rx_take(CPU_HZ / 16_000, true).1, "asked once until the host answers");
        assert_eq!(i2s.rx_owed, 7);
        assert_eq!(i2s.rx_frames_needed(0), 8);
        i2s.rx_push(&[[7, 7]; 8]);
        let (got, ask) = i2s.rx_take(0, true);
        assert_eq!((got.len(), ask, i2s.rx_owed), (7, false, 0));
        assert_eq!(i2s.rx_frames.len(), 1);
    }

    #[test]
    fn a_silent_host_costs_at_most_100_ms_of_owed_frames() {
        let mut i2s = std_rx(32, 32, 1);
        let (got, ask) = i2s.rx_take(CPU_HZ, true);                               // 1 s, nothing queued
        assert!(ask);
        assert_eq!(got.len(), 16_000 - 1_600);
        assert_eq!(i2s.rx_owed, 1_600);
    }

    #[test]
    fn stopping_rx_drops_the_queue() {
        let mut i2s = std_rx(32, 32, 1);
        i2s.rx_push(&[[1, 2]; 5]);
        i2s.write(0x20, RX_CONF_STD_MONO & !(1 << 2));
        assert!(i2s.rx_frames.is_empty());
        assert_eq!(i2s.rx_push(&vec![[0, 0]; RX_FRAMES_MAX + 3]), RX_FRAMES_MAX);
    }

    #[test]
    fn encodes_32_bit_mono_left_as_the_slot_word() {
        let i2s = std_rx(32, 32, 1);
        let bytes = i2s.rx_encode(&[[0x1234_5600, 0xdead_beef], [0xff00_0100, 0]]);
        assert_eq!(bytes, [0x00, 0x56, 0x34, 0x12, 0x00, 0x01, 0x00, 0xff]);
    }

    #[test]
    fn encodes_16_bit_stereo_from_the_top_of_each_slot() {
        let i2s = std_rx(16, 16, 3);
        assert_eq!(i2s.rx_sample_rate, 32_000);                                   // same clocks, half the frame
        assert_eq!(i2s.rx_encode(&[[0x8001_ffff, 0x7ffe_0000]]), [0x01, 0x80, 0xfe, 0x7f]);
    }

    #[test]
    fn encodes_24_bit_in_three_bytes_or_four_with_fill() {
        let mut i2s = std_rx(24, 32, 2);
        assert_eq!(i2s.rx_encode(&[[0, 0xabcd_ef00]]), [0xef, 0xcd, 0xab]);
        i2s.write(0x20, RX_CONF_STD_MONO | (1 << 16));
        assert_eq!(i2s.rx_encode(&[[0, 0xabcd_ef00]]), [0xef, 0xcd, 0xab, 0x00]);
    }

    #[test]
    fn without_the_philips_shift_the_word_arrives_one_bit_late() {
        let mut i2s = std_rx(32, 32, 1);
        i2s.write(0x28, i2s.rx_conf1 & !(1 << 29));
        assert_eq!(i2s.rx_encode(&[[0x8000_0002, 0]]), 0x4000_0001u32.to_le_bytes());
    }
}
