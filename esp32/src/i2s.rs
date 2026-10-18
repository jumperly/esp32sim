//! Classic I2S RX register adapter and native descriptor DMA.
//! ESP-IDF v5.5.4 components/soc/esp32/register/soc/i2s_reg.h: CONF bits 21-128,
//! interrupt _S defines 142-184, RXEOF_NUM 676-682, CONF_CHAN 692-695, INLINK_ADDR 763-768,
//! LC_CONF 856-868, clocks 1340-1390. include/soc/gpio_sig_map.h:69-72,299,309-312,343
//! supplies the GPIO matrix signals; dport_reg.h:947,965 defines the clock gates.
//! Samples are packed at DMA boundaries; no bit-level clock model is claimed.
use esp_periph::{Device, RegRam, WriteEffect};
use esp_periph::i2s::{I2s, PcmSources};
use xtensa_lx7::bus::{Bus, Fault};
use crate::bus::{SocBus, dma_range};
#[cfg(test)]
use crate::bus::{DRAM_HIGH, DRAM_LOW};
use crate::periph::CPU_HZ;
use crate::periph::ClassicGpio;

/// I2S_CONF_REG reset: RX/TX_MSB_RIGHT (bits 17/16) and RX/TX_RIGHT_FIRST (bits 9/8) set.
const CONF_RESET: u32 = (1 << 17) | (1 << 16) | (1 << 9) | (1 << 8);
/// I2S_LC_CONF_REG reset: OUT_EOF_MODE (bit 8) set, CHECK_OWNER (bit 12) clear.
const LC_CONF_RESET: u32 = 1 << 8;
/// LC_CONF CHECK_OWNER, at the bit position the shared channel's CONF1 uses too.
const CHECK_OWNER: u32 = 1 << 12;

pub struct ClassicI2s {
    pub inner: I2s,
    ram: RegRam,
    dma: esp_periph::gdma::GdmaInCh,
    pub(crate) clock_enabled: bool,
    #[cfg(test)]
    pub(crate) step_calls: u64,
}
impl Default for ClassicI2s { fn default() -> Self { Self::new() } }
impl ClassicI2s {
    pub fn new() -> Self {
        let mut ram = RegRam::new();
        ram.write(8, CONF_RESET);
        ram.write(0x24, 64);
        ram.write(0x60, LC_CONF_RESET);
        Self { inner: I2s::new(CPU_HZ), ram, dma: esp_periph::gdma::GdmaInCh { conf1: LC_CONF_RESET & CHECK_OWNER, ..Default::default() }, clock_enabled: false,
            #[cfg(test)] step_calls: 0 }
    }
    /// Writes go through `Device::write`, as the other chips' dispatch does, so the shared
    /// inherent writer keeps its single caller and S3/C3/C6 code generation is unchanged.
    fn sync(&mut self) {
        let conf = self.ram.read(8);
        let clock = self.ram.read(0xac);
        let sample = self.ram.read(0xb0);
        let bits = (sample >> 18) & 63;
        let bck = (sample >> 6) & 63;
        let (a, b) = ((clock >> 14) & 63, (clock >> 8) & 63);
        let div = if b == 0 || a < b { 0 } else { b | ((a % b) << 9) | ((a / b - 1) << 18) };
        let supported = clock & (1 << 21) == 0 && bck != 0 && [16, 24, 32].contains(&bits);
        Device::write(&mut self.inner, 0x30, (clock & 255) | (2 << 27) | if supported { 1 << 26 } else { 0 });
        Device::write(&mut self.inner, 0x38, div);
        Device::write(&mut self.inner, 0x28, (bck.saturating_sub(1) << 7) | (bits.saturating_sub(1) << 13) | ((if bits <= 16 { 15 } else { 31 }) << 18));
        // RX_CHAN_MOD 1/2 selects one slot and RX_MSB_RIGHT (CONF bit 17) decides which:
        // IDF v5.5.4 hal/esp32/include/hal/i2s_ll.h:859-875 writes RIGHT as msb_right ? 1 : 2
        // and LEFT as msb_right ? 2 : 1. The shared TDM mask has left in bit 0, right in bit 1.
        let channel = (self.ram.read(0x2c) >> 3) & 3;
        let msb_right = conf & (1 << 17) != 0;
        let mask = match channel { 1 | 2 => if (channel == 1) == msb_right { 2 } else { 1 }, _ => 3 };
        Device::write(&mut self.inner, 0x50, (1 << 16) | mask);
        // Shared RX_CONF bit 0 RX_RESET <- CONF bit 1; bit 1 RX_FIFO_RESET <- CONF bit 3 (CONF
        // bit 2 is TX_FIFO_RESET); bit 2 RX_START <- bit 5; bit 3 RX_SLAVE_MOD <- bit 7.
        // Classic keeps a 24-bit sample in a 32-bit word (i2s_ll.h:676-680 sets RX_FIFO_MOD 2;
        // esp_driver_i2s/i2s_common.c:447-448 allocates 4 bytes), so 24-bit RX sets RX_24_FILL_EN.
        let fill24 = if bits == 24 { 1 << 16 } else { 0 };
        Device::write(&mut self.inner, 0x20, ((conf >> 1) & 1) | ((conf >> 2) & 2) | ((conf >> 3) & 4) | ((conf & (1 << 7)) >> 4) | fill24);
    }
    #[inline]
    pub(crate) fn data(&mut self, cycles: u64, now: u64, port: usize, gpio: &ClassicGpio, sources: Option<&mut PcmSources>) -> Vec<u8> {
        let (data, clock) = [(155, [27, 28]), (181, [164, 165])][port];
        let signals = esp_periph::i2s::RxSignals { data, clock, input_select_bit: 7, output_mask: 0x3ff };
        self.inner.receive(cycles, now, false, &gpio.gpio, signals, sources)
    }
    /// Map the shared channel's GDMA-layout interrupt bits onto classic INT_RAW: IN_DONE 0,
    /// IN_SUC_EOF 1, IN_DSCR_ERR 3 and IN_DSCR_EMPTY 4 become bits 8, 9, 13 and 15.
    #[inline]
    pub(crate) fn raise(&mut self, gdma: u32, eof_desc: u32) {
        if gdma & (1 << 1) != 0 { self.ram.write(0x3c, eof_desc); }
        self.inner.int_raw |= ((gdma & 3) << 8) | ((gdma & (1 << 3)) << 10) | ((gdma & (1 << 4)) << 11);
    }
}
impl Device for ClassicI2s {
    fn read(&mut self, off: u32) -> u32 {
        match off {
            0xc | 0x10 | 0x14 => self.inner.read(off),
            // IDF v5.5.4 i2s_reg.h:105-128: all four reset bits are plain R/W.
            8 => self.ram.read(off),
            0xbc => 1,
            _ => self.ram.read(off),
        }
    }
    fn write(&mut self, off: u32, value: u32) -> WriteEffect {
        self.ram.write(off, value);
        match off {
            0x14 | 0x18 => { Device::write(&mut self.inner, off, value); }
            0x34 => {
                if value & (1 << 28) != 0 { self.dma.running = false; }
                if value & (3 << 29) != 0 { self.dma.desc = 0x3ff0_0000 | (value & 0xfffff); self.dma.buf_pos = 0; self.dma.rx_eof_pos = 0; self.dma.running = true; }
            }
            0x60 => self.dma.conf1 = value & CHECK_OWNER,
            8 | 0x2c | 0xac | 0xb0 => self.sync(),
            _ => {}
        }
        WriteEffect::NONE
    }
    fn irq_sources(&self) -> u64 { self.inner.irq() as u64 }
}

/// Classic I2S DMA as the shared receive channel sees it: internal DRAM only. ESP-IDF v5.5.4
/// components/soc/esp32/include/soc/soc.h:203-205 (SOC_DMA_LOW/HIGH) bounds DMA-capable
/// memory to 0x3FFA_E000..0x4000_0000. Descriptor reads, buffer writes and descriptor
/// writeback anywhere else (registers, ROM, IRAM, RTC memory, flash) fault without access.
struct DramDma<'a>(&'a mut SocBus);
impl DramDma<'_> {
    fn reaches(addr: u32, len: u32) -> bool {
        dma_range(addr, len as usize).is_some()
    }
}
impl Bus for DramDma<'_> {
    fn read8(&mut self, _: u32) -> Result<u8, Fault> { Err(Fault::Prohibited) }
    fn read16(&mut self, _: u32) -> Result<u16, Fault> { Err(Fault::Prohibited) }
    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        if !Self::reaches(addr, 4) { return Err(Fault::Prohibited); }
        self.0.dma_read_word(addr).ok_or(Fault::Unmapped)
    }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> {
        if Self::reaches(addr, 1) && self.0.dma_write_byte(addr, v) { Ok(()) } else { Err(Fault::Prohibited) }
    }
    fn write16(&mut self, _: u32, _: u16) -> Result<(), Fault> { Err(Fault::Prohibited) }
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), Fault> {
        if Self::reaches(addr, 4) && self.0.dma_write_word(addr, v) { Ok(()) } else { Err(Fault::Prohibited) }
    }
    fn fetch(&mut self, _: u32) -> Result<[u8; 4], Fault> { Err(Fault::Prohibited) }
    fn note_code_page(&mut self, _: u32) {}
}

impl SocBus {
    #[inline]
    pub(crate) fn i2s_step(&mut self, cycles: u64) {
        for port in 0..2 {
            if !self.periph.i2s[port].clock_enabled || !self.periph.i2s[port].dma.running { continue; }
            #[cfg(test)] { self.periph.i2s[port].step_calls += 1; }
            // Pin-routed sources live on controller 0, as on the other chips.
            let mut bank = self.periph.i2s[0].inner.take_pcm_bank();
            let data = self.periph.i2s[port].data(cycles, self.cycles, port, &self.periph.gpio, bank.as_deref_mut());
            self.periph.i2s[0].inner.restore_pcm_bank(bank);
            let mut dma = self.periph.i2s[port].dma;
            let mut irq = false;
            // IDF v5.5.4 hal/esp32/include/hal/i2s_ll.h:639-643 counts RXEOF_NUM in words.
            if let Some(eof_bytes) = self.periph.i2s[port].ram.read(0x24).checked_mul(4) {
                dma.receive(&mut DramDma(self), &data, Some(eof_bytes), false, |a| !DramDma::reaches(a, 1), &mut irq);
            } else {
                dma.fail_receive(&mut irq);
            }
            self.periph.i2s[port].raise(dma.int_raw, dma.eof_desc);
            dma.int_raw = 0;
            self.periph.i2s[port].dma = dma;
            self.periph.i2s[port].inner.recycle_rx_buffer(data);
            self.irq_dirty |= irq;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classic_i2s_clock_reset_and_pending_gates() {
        for (port, bit) in [4, 21].into_iter().enumerate() {
            let mut b = SocBus::new(4 << 20, [0; 6]);
            b.periph.i2s[port].dma.running = true;
            b.i2s_step(64);
            assert_eq!(b.periph.i2s[port].step_calls, 0);
            b.write32(0x3ff0_00c0, 1 << bit).unwrap();
            b.write32(0x3ff0_00c4, 1 << bit).unwrap();
            b.i2s_step(64);
            assert_eq!(b.periph.i2s[port].step_calls, 0);
            b.write32(0x3ff0_00c4, 0).unwrap();
            b.i2s_step(64);
            assert_eq!(b.periph.i2s[port].step_calls, 1);
            b.write32(0x3ff0_00c0, 0).unwrap();
            b.i2s_step(64);
            assert_eq!(b.periph.i2s[port].step_calls, 1);
            b.periph.i2s[port].dma.running = false;
            b.write32(0x3ff0_00c0, 1 << bit).unwrap();
            b.i2s_step(64);
            assert_eq!(b.periph.i2s[port].step_calls, 1);
        }
    }
    #[test]
    fn classic_i2s_dma_reach_reads_and_writes() {
        for address in [DRAM_LOW, DRAM_HIGH - 4, crate::bus::ROM_DRAM_LOW,
            DRAM_LOW - 4, DRAM_HIGH - 3, 0x4008_0000, 0x3ff8_0000, 0x5000_0000, u32::MAX] {
            let allowed = address == DRAM_LOW || address == DRAM_HIGH - 4;
            let mut b = SocBus::new(0, [0; 6]);
            let mut dma = DramDma(&mut b);
            assert_eq!(dma.read32(address).is_ok(), allowed, "read {address:#x}");
            assert_eq!(dma.write32(address, 42).is_ok(), allowed, "write {address:#x}");
        }
    }

    /// 16-bit stereo at 7843 Hz (PLL160 / 25.5 / BCK 25 / 32): one frame takes 30,601 CPU cycles.
    fn receiver(bits: u32, conf: u32) -> ClassicI2s {
        let mut i = ClassicI2s::new();
        i.write(0xac, 25 | (1 << 8) | (2 << 14));
        i.write(0xb0, (25 << 6) | (bits << 18));
        i.write(8, conf);
        i
    }
    #[test]
    fn classic_fractional_clock_width_and_slave_selection() {
        let mut i = receiver(16, 0);
        assert_eq!(i.inner.rx_rate(), Some(7843));
        i.write(0xb0, (25 << 6) | (24 << 18));
        assert_eq!(i.inner.rx_rate(), Some(3922));
        i.write(0xac, 25 | (1 << 21));
        assert_eq!(i.inner.rx_rate(), None);
        i.write(0xac, 25);
        i.write(0xb0, 16 << 18);
        assert_eq!(i.inner.rx_rate(), None);
        i.write(0xb0, (25 << 6) | (8 << 18));
        assert_eq!(i.inner.rx_rate(), None);
        i.write(0xac, 25 | (1 << 8) | (2 << 14));
        i.write(0xb0, (25 << 6) | (16 << 18));
        i.write(8, (1 << 5) | (1 << 7));
        assert!(i.inner.rx_data(40_000, false, |_| [1, 2]).is_empty());
        i.write(8, 1 << 5);
        assert_eq!(i.inner.rx_data(40_000, false, |_| [1, 2]), [1, 0, 2, 0]);
    }

    /// IDF's slot encoding (i2s_ll.h:859-875) for both RX_MSB_RIGHT values; [left, right] = [1, 2].
    #[test]
    fn rx_chan_mod_selects_slot_by_msb_right() {
        assert_eq!(ClassicI2s::new().read(8), 0x0003_0300); // i2s_reg.h:21-125 defaults
        for (msb_right, left_mod, right_mod) in [(1 << 17, 2, 1), (0, 1, 2)] {
            let mut i = receiver(16, (1 << 5) | msb_right);
            i.write(0x2c, left_mod << 3);
            assert_eq!(i.inner.rx_data(40_000, false, |_| [1, 2]), [1, 0], "left, msb_right {msb_right:#x}");
            i.write(0x2c, right_mod << 3);
            assert_eq!(i.inner.rx_data(40_000, false, |_| [1, 2]), [2, 0], "right, msb_right {msb_right:#x}");
            i.write(0x2c, 0);
            assert_eq!(i.inner.rx_data(40_000, false, |_| [1, 2]), [1, 0, 2, 0]);
        }
    }

    /// CONF bit 3 is RX_FIFO_RESET and bit 2 TX_FIFO_RESET (i2s_reg.h:105-125): only the RX
    /// resets (bits 1 and 3) discard the receiver's fractional frame time.
    #[test]
    fn rx_fifo_reset_is_conf_bit_3_not_tx_bit_2() {
        for (reset, restarts) in [(1 << 3, true), (1 << 1, true), (1 << 2, false), (1 << 0, false)] {
            let mut i = receiver(16, 1 << 5);
            assert!(i.inner.rx_data(30_000, false, |_| [1, 2]).is_empty());
            i.write(8, (1 << 5) | reset);
            assert_eq!(i.read(8), (1 << 5) | reset, "reset bit {reset:#x} remains readable");
            i.write(8, 1 << 5);
            assert_eq!(i.inner.rx_data(1_000, false, |_| [1, 2]).is_empty(), restarts, "reset bit {reset:#x}");
        }
    }

    /// Classic keeps 24-bit samples in 32-bit words, as IDF sizes them; 16/32-bit are unchanged.
    #[test]
    fn classic_24_bit_samples_fill_words() {
        for (bits, cycles, frame) in [(24, 80_000, vec![0, 0, 1, 0, 0, 0, 0xfe, 0xff]), (32, 80_000, vec![0, 0, 1, 0, 0, 0, 0xfe, 0xff]), (16, 40_000, vec![1, 0, 0xfe, 0xff])] {
            let mut i = receiver(bits, 1 << 5);
            assert_eq!(i.inner.rx_data(cycles, false, |_| [1, -2]), frame, "{bits}-bit");
        }
    }
}
