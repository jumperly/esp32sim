use esp_soc::board::{BoardEdge, BoardModel, VirtualCycle};

pub const PIN_AMOLED_SDIO0: u8 = 4;
pub const PIN_AMOLED_SDIO1: u8 = 5;
pub const PIN_AMOLED_SDIO2: u8 = 6;
pub const PIN_AMOLED_SDIO3: u8 = 7;
pub const PIN_AMOLED_SCLK: u8 = 11;
pub const PIN_AMOLED_CS: u8 = 12;
pub const PIN_AMOLED_TE: u8 = 13;
pub const PIN_AMOLED_I2C_SCL: u8 = 14;
pub const PIN_AMOLED_I2C_SDA: u8 = 15;
pub const PIN_AMOLED_TOUCH_INT: u8 = 21;

pub use esp_soc::devices::co5300::Co5300;

/// Waveshare ESP32-S3-Touch-AMOLED-1.8 V2 with the CO5300 and CST820 device set.
pub struct WaveshareAmoled18V2 {
    pub gpio_events: u64,
    pub panel: Co5300,
    pub touch_state: std::sync::Arc<std::sync::Mutex<crate::i2c::TouchState>>,
    /// Board clock and handling-script selection seen by the QMI8658.
    pub imu: std::sync::Arc<crate::i2c::ImuMotion>,
    smooth_display: bool,
    cycle: VirtualCycle,
    next_te_cycle: Option<VirtualCycle>,
    te_level: bool,
    te_high_cycles: VirtualCycle,
    te_low_cycles: VirtualCycle,
    touch_irq_level: bool,
    /// At most two touch IRQ transitions: the first observable edge and a coalesced final level.
    pending_touch_irq: Option<(VirtualCycle, bool)>,
    pending_touch_final: Option<bool>,
    edges: Vec<BoardEdge>,
}

impl WaveshareAmoled18V2 {
    pub const WIDTH: usize = 368;
    pub const HEIGHT: usize = 448;
    pub const X_OFFSET: u16 = 16;

    const APPROXIMATE_TE_HALF_PERIOD: VirtualCycle = crate::periph::CPU_HZ / 120;

    pub fn new() -> Self {
        Self {
            gpio_events: 0,
            panel: Co5300::new(Self::WIDTH, Self::HEIGHT, Self::X_OFFSET),
            touch_state: Default::default(),
            imu: Default::default(),
            smooth_display: false,
            cycle: 0,
            next_te_cycle: Some(Self::APPROXIMATE_TE_HALF_PERIOD),
            te_level: true,
            te_high_cycles: Self::APPROXIMATE_TE_HALF_PERIOD,
            te_low_cycles: Self::APPROXIMATE_TE_HALF_PERIOD,
            touch_irq_level: true,
            pending_touch_irq: None,
            pending_touch_final: None,
            edges: Vec::new(),
        }
    }

    /// Fixed waveform from TinyDraw's 2026-08-15 CO5300 hardware receipt:
    /// 16,773 us period, 578 us high. Phase starts high at reset; no jitter model.
    pub fn with_measured_te() -> Self {
        let mut board = Self::new();
        board.te_high_cycles = 578 * (crate::periph::CPU_HZ / 1_000_000);
        board.te_low_cycles = (16_773 - 578) * (crate::periph::CPU_HZ / 1_000_000);
        board.next_te_cycle = Some(board.te_high_cycles);
        board
    }

    fn queue_touch(&mut self, cycle: VirtualCycle, x: u16, y: u16, down: bool) {
        self.touch_state.lock().expect("AMOLED touch state mutex poisoned")
            .update(x.min(Self::WIDTH as u16 - 1), y.min(Self::HEIGHT as u16 - 1), down);
        let level = !down;
        let queued_level = self.pending_touch_final.or(self.pending_touch_irq.map(|(_, level)| level)).unwrap_or(self.touch_irq_level);
        if level != queued_level {
            match self.pending_touch_irq {
                Some((_, first_level)) if level == first_level => self.pending_touch_final = None,
                Some(_) => self.pending_touch_final = Some(level),
                None => self.pending_touch_irq = Some((cycle.max(self.cycle).saturating_add(1), level)),
            }
        }
    }
}

impl Default for WaveshareAmoled18V2 { fn default() -> Self { Self::new() } }

impl BoardModel for WaveshareAmoled18V2 {
    fn name(&self) -> &'static str { "waveshare-amoled18-v2" }
    fn gpio_changes(&mut self, changes: &[(u8, bool)]) { self.gpio_events += changes.len() as u64; }
    fn gpio_events(&self) -> u64 { self.gpio_events }
    fn spi_transfer(&mut self, host: u8, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if host == 2 { self.panel.transaction(tx); }
        vec![0xff; rx_len]
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn crate::i2c::I2cDevice>)> {
        use crate::i2c::*;
        vec![
            (0, 0x15, Box::new(Cst820::new(self.touch_state.clone()))),
            (0, 0x20, Box::new(Tca9554::register_ram_stub())),
            (0, 0x34, Box::new(Reg8Device::new("axp2101-pmic-initialization-stub", &[(0x03, 0x4a)]))),
            (0, 0x51, Box::new(Reg8Device::new("pcf85063a-rtc-initialization-stub", &[]))),
            (0, 0x6b, Box::new(Qmi8658::new(self.imu.clone()))),
        ]
    }
    fn display(&self) -> Option<(u32, u32, Vec<u16>, u64)> {
        Some((Self::WIDTH as u32, Self::HEIGHT as u32, self.panel.frame.clone(), self.panel.pixels_written))
    }
    fn display_version(&self) -> u64 { self.panel.pixels_written }
    fn display_frames(&self) -> u64 { self.panel.frames }
    fn display_quiet_push(&self) -> bool { !self.smooth_display }
    fn display_push_hz(&self) -> u64 { if self.smooth_display { 120 } else { 50 } }
    fn set_smooth_display(&mut self, on: bool) -> bool { self.smooth_display = on; true }
    fn set_imu_motion(&mut self, mode: u32) -> bool { self.imu.mode.store(mode, std::sync::atomic::Ordering::Relaxed); true }
    fn imu_motion(&self) -> u32 { self.imu.mode.load(std::sync::atomic::Ordering::Relaxed) }
    fn input_levels(&self) -> Vec<(u8, bool)> {
        vec![(PIN_AMOLED_TE, self.te_level), (PIN_AMOLED_TOUCH_INT, self.touch_irq_level)]
    }
    fn touch(&mut self, x: u16, y: u16, down: bool) { self.queue_touch(self.cycle, x, y, down); }
    fn touch_at(&mut self, cycle: VirtualCycle, x: u16, y: u16, down: bool) { self.queue_touch(cycle, x, y, down); }
    fn next_deadline(&self) -> Option<VirtualCycle> {
        match (self.next_te_cycle, self.pending_touch_irq) {
            (Some(te), Some((touch, _))) => Some(te.min(touch)),
            (Some(te), None) => Some(te),
            (None, Some((touch, _))) => Some(touch),
            (None, None) => None,
        }
    }
    fn advance_to(&mut self, cycle: VirtualCycle) {
        assert!(cycle >= self.cycle, "board time moved backwards from {} to {}", self.cycle, cycle);
        while self.next_deadline().is_some_and(|deadline| deadline <= cycle) {
            let deadline = self.next_deadline().expect("due board transition must have a deadline");
            if self.next_te_cycle == Some(deadline) {
                self.te_level = !self.te_level;
                self.edges.push(BoardEdge { cycle: deadline, pin: PIN_AMOLED_TE, level: self.te_level });
                self.next_te_cycle = deadline.checked_add(if self.te_level { self.te_high_cycles } else { self.te_low_cycles });
            }
            if self.pending_touch_irq.is_some_and(|(touch_cycle, _)| touch_cycle == deadline) {
                let (_, level) = self.pending_touch_irq.take().expect("due touch interrupt must remain pending");
                self.touch_irq_level = level;
                self.edges.push(BoardEdge { cycle: deadline, pin: PIN_AMOLED_TOUCH_INT, level });
                if self.pending_touch_final.is_some_and(|final_level| final_level != level) {
                    let final_level = self.pending_touch_final.take().expect("different final touch level must remain pending");
                    self.pending_touch_irq = Some((deadline.saturating_add(1), final_level));
                } else {
                    self.pending_touch_final = None;
                }
            }
        }
        self.cycle = cycle;
        self.imu.cycle.store(cycle, std::sync::atomic::Ordering::Relaxed);
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> { std::mem::take(&mut self.edges) }
}


#[cfg(test)]
mod amoled_tests {
    use super::*;

    #[test]
    fn smooth_publication_is_an_explicit_opt_in() {
        for mut board in [WaveshareAmoled18V2::new(), WaveshareAmoled18V2::with_measured_te()] {
            assert_eq!(board.display_push_hz(), 50);
            assert!(board.display_quiet_push());
            assert!(board.set_smooth_display(true));
            assert_eq!(board.display_push_hz(), 120);
            assert!(!board.display_quiet_push());
            assert!(board.set_smooth_display(false));
            assert_eq!(board.display_push_hz(), 50);
            assert!(board.display_quiet_push());
        }
    }

    #[test]
    fn board_exposes_the_v2_device_addresses_on_i2c0() {
        let mut board = WaveshareAmoled18V2::new();
        let addresses: Vec<_> = board.i2c_devices().into_iter().map(|(bus, address, _)| (bus, address)).collect();
        assert_eq!(addresses, [(0, 0x15), (0, 0x20), (0, 0x34), (0, 0x51), (0, 0x6b)]);
    }

    #[test]
    fn board_name_selects_only_the_v2_model() {
        assert_eq!(crate::board::make_board("waveshare-amoled18-v2").unwrap().name(), "waveshare-amoled18-v2");
        assert_eq!(crate::board::make_board("amoled18-v2").unwrap().name(), "waveshare-amoled18-v2");
        assert!(crate::board::make_board("waveshare-amoled18").is_none());
    }

    #[test]
    fn measured_te_matches_hardware_period_and_high_width() {
        let mut board = WaveshareAmoled18V2::with_measured_te();
        let us = crate::periph::CPU_HZ / 1_000_000;
        board.advance_to(16_773 * us + 578 * us);
        assert_eq!(board.take_edges(), [
            BoardEdge { cycle: 578 * us, pin: PIN_AMOLED_TE, level: false },
            BoardEdge { cycle: 16_773 * us, pin: PIN_AMOLED_TE, level: true },
            BoardEdge { cycle: (16_773 + 578) * us, pin: PIN_AMOLED_TE, level: false },
        ]);
    }

    #[test]
    fn approximate_te_signal_preserves_edge_timestamps() {
        let mut board = WaveshareAmoled18V2::new();
        let half_period = WaveshareAmoled18V2::APPROXIMATE_TE_HALF_PERIOD;
        assert_eq!(board.next_deadline(), Some(half_period));
        board.advance_to(half_period - 1);
        assert!(board.take_edges().is_empty());
        board.advance_to(half_period);
        assert_eq!(board.take_edges(), [BoardEdge { cycle: half_period, pin: PIN_AMOLED_TE, level: false }]);
        board.advance_to(half_period * 2);
        assert_eq!(board.take_edges(), [BoardEdge { cycle: half_period * 2, pin: PIN_AMOLED_TE, level: true }]);
    }

    #[test]
    fn panel_tear_commands_do_not_change_the_legacy_board_waveform() {
        for mut board in [WaveshareAmoled18V2::new(), WaveshareAmoled18V2::with_measured_te()] {
            let deadline = board.next_deadline().unwrap();
            for command in [0x34, 0x44, 0x35, 0x01] {
                board.spi_transfer(2, &[0x02, 0, command, 0, 1, 64], 0);
                assert_eq!(board.next_deadline(), Some(deadline));
            }
            board.advance_to(deadline);
            assert_eq!(board.take_edges(), [BoardEdge { cycle: deadline, pin: PIN_AMOLED_TE, level: false }]);
        }
    }

    #[test]
    fn touch_signal_preserves_active_low_edge_timestamps() {
        let mut board = WaveshareAmoled18V2::new();
        board.touch(100, 200, true);
        board.touch(100, 200, false);
        board.advance_to(2);
        assert_eq!(board.take_edges(), [
            BoardEdge { cycle: 1, pin: PIN_AMOLED_TOUCH_INT, level: false },
            BoardEdge { cycle: 2, pin: PIN_AMOLED_TOUCH_INT, level: true },
        ]);
    }

    #[test]
    fn touch_burst_keeps_the_first_edge_and_coalesces_to_the_final_level() {
        let mut board = WaveshareAmoled18V2::new();
        for index in 0..1000 { board.touch(100, 200, index % 2 == 0); }
        assert_eq!(board.pending_touch_irq, Some((1, false)));
        assert_eq!(board.pending_touch_final, Some(true));
        board.advance_to(2);
        assert_eq!(board.take_edges(), [
            BoardEdge { cycle: 1, pin: PIN_AMOLED_TOUCH_INT, level: false },
            BoardEdge { cycle: 2, pin: PIN_AMOLED_TOUCH_INT, level: true },
        ]);
        assert_eq!(board.input_levels(), [(PIN_AMOLED_TE, true), (PIN_AMOLED_TOUCH_INT, true)]);
    }

    #[test]
    fn te_deadline_overflow_disables_future_transitions() {
        let mut board = WaveshareAmoled18V2::new();
        board.cycle = VirtualCycle::MAX - 1;
        board.next_te_cycle = Some(VirtualCycle::MAX);
        board.advance_to(VirtualCycle::MAX);
        assert_eq!(board.take_edges(), [BoardEdge { cycle: VirtualCycle::MAX, pin: PIN_AMOLED_TE, level: false }]);
        assert_eq!(board.next_deadline(), None);
    }

}
