//! ESP8266 peripherals. Register bases: Arduino ESP8266 3.1.2 tools/sdk/include/eagle_soc.h and
//! cores/esp8266/esp8266_peri.h; the UART map: tools/sdk/include/uart_register.h.
//!
//! Modelled: UART0 and UART1 (the shared esp-periph UART; transmit goes to the host console,
//! receive raises interrupt 5), the two timers FRC1 (interrupt 9, or the NMI) and FRC2 (interrupt
//! 10), GPIO 0..15 (output, enable, open drain, input levels with the IO_MUX pull-ups, the
//! interrupt types, interrupt 4) and GPIO16 (the RTC pad), the strapping bits of GPIO_IN, the
//! hardware watchdog, the RTC block (software reset, the STORE registers, the sleep counter and
//! deep sleep with its GPIO16 wake pulse), SPI1 (HSPI) as a master, DPORT (edge-interrupt
//! enables, NMI routing, CPU2X, the watchdog pause, the OTP words) and WDEV (the microsecond
//! counter, the TSF and the random number register). Every other register in the peripheral page,
//! DPORT, WDEV and the RF block is register memory: it reads what was last written, 0 after a
//! reset. Device time is the bus's cycle count (`now`) at the 80 MHz APB clock the timers count;
//! CPU2X doubles only the core's clock (`SocBus::tick`).
use crate::rom::boot::{RST_SW, RST_WDT};
use esp_periph::uart::{Uart, UartLayout};
use esp_periph::Misc;
use std::collections::{HashMap, HashSet};

/// The APB clock: the timers, the UART divisors and the watchdog count it (esp8266_peri.h:32
/// ESP8266_CLOCK), and the CPU clock with CPU2X clear.
pub const CPU_HZ: u64 = 80_000_000;
/// esp8266_peri.h:30 ESP8266_REG: UART0 0x000, SPI1 0x100, SPI0 0x200, GPIO 0x300, timers 0x600,
/// RTC 0x700, IO_MUX 0x800, WDT 0x900, SLC 0xB00, I2S 0xE00, UART1 0xF00.
pub const PERIPH_BASE: u32 = 0x6000_0000;
pub const PERIPH_END: u32 = 0x6000_1000;
/// RTC memory (eagle_soc.h RTC_USER_MEM 0x60001200 is inside it); survives every reset but
/// power-on. libmain's `system_rtc_mem_write` (Blink image 0x40105fc8) addresses its 768 bytes
/// from 0x60001100 (0x60000E00 + 0x300 + 4 x block, 192 blocks).
pub const RTC_MEM_LOW: u32 = 0x6000_1000;
pub const RTC_MEM_HIGH: u32 = 0x6000_2000;
/// The RF/PHY register block the SDK's binary blobs program even with Wi-Fi off.
pub const RF_LOW: u32 = 0x6000_2000;
pub const RF_HIGH: u32 = 0x6010_0000;
/// eagle_soc.h:84 PERIPHS_DPORT_BASEADDR (esp8266_peri.h:31 ESP8266_DREG).
pub const DPORT_BASE: u32 = 0x3FF0_0000;
pub const DPORT_END: u32 = 0x3FF1_0000;
/// The Wi-Fi MAC (WDEV) registers, also the microsecond counter and the RNG.
pub const WDEV_BASE: u32 = 0x3FF2_0000;
pub const WDEV_END: u32 = 0x3FF4_0000;
/// uart_register.h:9: REG_UART_BASE(i) = 0x60000000 + i * 0xf00.
pub const UART1_BASE: u32 = PERIPH_BASE + 0xF00;
/// uart_register.h:12-126: FIFO 0x00 ... DATE 0x78, ID 0x7C.
const UART_SPAN: u32 = 0x80;
/// esp8266_peri.h:40: CPU2X, bit 0 set = the CPU runs at 160 MHz.
pub const DPORT_CPU2X: u32 = DPORT_BASE + 0x14;
/// esp8266_peri.h:42-45: the OTP words MAC0 0x50, MAC1 0x54, CHIPID 0x58, and 0x5C after them.
pub const DPORT_OTP: u32 = DPORT_BASE + 0x50;
const DPORT_OTP_LAST: u32 = DPORT_OTP + 0xF;
/// DPORT 0x00, bit 0: FRC1's interrupt goes to the NMI instead of interrupt 9. The SDK's
/// `NmiTimSetFunc` writes the low five bits as 15, its `NMI_Handler` as 14 on entry and the NMI
/// vector 15 again on exit (Blink image 0x40105398, 0x401053b8, 0x40106134); digitake
/// esp8266-emulator src/soc.c:780 routes FRC1 by this bit.
pub const DPORT_NMI: u32 = DPORT_BASE;
/// eagle_soc.h:92-94 EDGE_INT_ENABLE_REG, TM1_EDGE_INT_ENABLE = bit 1 (esp8266_peri.h:152 TEIE):
/// FRC1's edge interrupt reaches the CPU. Bit 0 does the same for the watchdog's (digitake
/// esp8266-emulator src/soc.c:389).
pub const DPORT_TEIE: u32 = DPORT_BASE + 0x04;
const TEIE1: u32 = 1 << 1;
const TEIE_WDT: u32 = 1;
/// DPORT 0x18 bit 3: the watchdog is paused while set (digitake esp8266-emulator src/soc.c:526-530;
/// the SDK sets it on its way into deep sleep, `0x3ff00018 <- 0xfc700008` in the sleep fixture).
const DPORT_WDT_PAUSE: u32 = DPORT_BASE + 0x18;
/// esp8266_peri.h:189-191: UIS, the UART interrupt status, bit 0 UART0, bit 2 UART1.
const UIS: u32 = DPORT_BASE + 0x20020;
/// WDEV 0x3FF20C00: the free-running microsecond counter `system_get_time` reads (digitake
/// esp8266-emulator src/soc.c:500); 0x3FF21048/4C: the 64-bit TSF in microseconds (soc.c:503-504).
pub const WDEV_US: u32 = WDEV_BASE + 0xC00;
const WDEV_TSF_LO: u32 = WDEV_BASE + 0x1048;
const WDEV_TSF_HI: u32 = WDEV_BASE + 0x104C;
/// esp8266_peri.h:851 RANDOM_REG32.
pub const WDEV_RNG: u32 = WDEV_BASE + 0xE44;

/// RF 0x60009B60 bit 1: a noise-floor measurement is running. libphy's `do_noisefloor` (Blink
/// image 0x402277e0) sets it, then polls it with a timeout on the WDEV
/// microsecond counter until the hardware clears it; with no radio it reads clear, the
/// measurement done at once, instead of every call spending its whole timeout.
const RF_NOISEFLOOR: u32 = 0x6000_9B60;
const NOISEFLOOR_BUSY: u32 = 1 << 1;

/// Interrupt numbers (ets_sys.h:77-86; FRC2 is the SDK's 10, the NMI the LX106's line 14).
pub const INUM_GPIO: u32 = 4;
pub const INUM_UART: u32 = 5;
pub const INUM_WDT: u32 = 8;
pub const INUM_FRC1: u32 = 9;
pub const INUM_FRC2: u32 = 10;
pub const INUM_NMI: u32 = 14;

/// CRC-8 as libmain's `esp_crc8` computes it (Blink image 0x40106a28): reflected polynomial 0x8C,
/// initial value 0, no final XOR (CRC-8/MAXIM).
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut c, &b| {
        c ^= b;
        for _ in 0..8 { c = if c & 1 != 0 { (c >> 1) ^ 0x8c } else { c >> 1 }; }
        c
    })
}

/// The factory MAC as the OTP words hold it, in the form the SDK decodes (the MAC reader that
/// libmain's `call_user_start_local` calls, Blink image 0x40105434..0x401054cd): CHIPID bit 15
/// must be set or the SDK prints an error and stops; the last three bytes are MAC1 bits 15..8,
/// MAC1 bits 7..0, MAC0 bits 31..24; with CHIPID bit 12 set the first three are bits 23..16,
/// 15..8 and 7..0 of the fourth word, accepted when CHIPID bits 31..24 hold `crc8` of them, last
/// byte first (with bit 12 clear the SDK uses 18:fe:34). `ESP.getChipId()` is then the MAC's last
/// three bytes (libmain `system_get_chip_id`, 0x40235b5c).
pub fn otp_words(mac: [u8; 6]) -> [u32; 4] {
    let crc = u32::from(crc8(&[mac[2], mac[1], mac[0]]));
    [u32::from(mac[5]) << 24, u32::from(mac[3]) << 8 | u32::from(mac[4]), crc << 24 | 1 << 15 | 1 << 12,
     u32::from(mac[0]) << 16 | u32::from(mac[1]) << 8 | u32::from(mac[2])]
}

/// uart_register.h: STATUS RXFIFO_CNT 8 bits at 0 (:78), CONF0 RXFIFO_RST bit 17 (:83), CONF1
/// RXFIFO_FULL_THRHD 7 bits at 0 (:111). The offsets 0x00-0x24 are the ESP32 family's. The
/// autobaud detector (AUTOBAUD 0x18, LOWPULSE 0x28) is not modelled.
pub const UART_LAYOUT: UartLayout = UartLayout { thrhd_mask: 0x7f, rxfifo_rst: 1 << 17, rxfifo_cnt_mask: 0xff, autobaud_en: 0, autobaud_regs: 0x28, autobaud_clock_hz: 0 };
/// uart_register.h:17,102-103: RXFIFO_TOUT is interrupt bit 8, and it only exists when CONF1
/// RX_TOUT_EN (bit 31) is set. The shared UART raises it whenever bytes wait; the Arduino core
/// leaves RX_TOUT_EN clear (uart.cpp:457 `USC1 = INTRIGG << UCFFT`) and treats a timeout as an
/// error without draining the FIFO (uart.cpp:430-433), so here it is masked unless enabled.
const UART_INT_TOUT: u32 = 1 << 8;
const UART_RX_TOUT_EN: u32 = 1 << 31;

/// Register memory without a model: what was last written, 0 before.
#[derive(Default, Clone)]
pub struct RegMem(HashMap<u32, u32>);
impl RegMem {
    pub fn read(&self, addr: u32) -> u32 { self.0.get(&addr).copied().unwrap_or(0) }
    pub fn write(&mut self, addr: u32, v: u32) { self.0.insert(addr, v); }
}

/// The FRC timers' prescaler (esp8266_peri.h:166 TCPD, 2 bits at 2): 0 = /1, 1 = /16, 2 and 3 =
/// /256 of the 80 MHz APB clock: APB cycles per count.
fn prescale(ctrl: u32) -> u64 { match (ctrl >> 2) & 3 { 0 => 1, 1 => 16, _ => 256 } }
/// esp8266_peri.h:163-167: CTRL bit 8 interrupt status (read), 7 enable, 6 auto-reload, 0
/// interrupt type (0 edge, 1 level).
const TCIS: u32 = 1 << 8;
const TCTE: u32 = 1 << 7;
const TCAR: u32 = 1 << 6;
const TCIT: u32 = 1;
/// FRC1's 23-bit counter (esp8266_peri.h:147 T1L "23bit (0-8388607)").
const FRC1_MASK: u32 = 0x7F_FFFF;

/// FRC1 (esp8266_peri.h:147-150, 0x600..0x60C): counts down from LOAD at the prescaled APB clock;
/// at 0 it raises its interrupt and, with auto-reload, starts again from LOAD, else it stops
/// (digitake esp8266-emulator src/soc.c:258-269 and :778-791 behave the same).
pub struct Frc1 {
    pub load: u32,
    pub ctrl: u32,
    pub status: bool,
    /// the count at `t0` (a count boundary)
    count0: u32,
    t0: u64,
    /// the cycle of the next 0, or u64::MAX when stopped
    pub next: u64,
}

impl Frc1 {
    fn new() -> Self { Frc1 { load: 0, ctrl: 0, status: false, count0: 0, t0: 0, next: u64::MAX } }
    pub fn count(&self, now: u64) -> u32 {
        if self.next == u64::MAX { return self.count0; }
        let ticks = now.saturating_sub(self.t0) / prescale(self.ctrl);
        self.count0.saturating_sub(ticks.min(u64::from(u32::MAX)) as u32)
    }
    /// Count down from `count0` at `now`.
    fn start(&mut self, now: u64) {
        self.t0 = now;
        self.next = if self.ctrl & TCTE != 0 && self.count0 > 0 { now + u64::from(self.count0) * prescale(self.ctrl) } else { u64::MAX };
    }
    fn write(&mut self, off: u32, v: u32, now: u64) {
        match off {
            0x00 => { self.load = v & FRC1_MASK; self.count0 = self.load; self.start(now); }
            0x08 => { self.count0 = self.count(now); self.ctrl = v & 0xff; self.start(now); }
            0x0C => self.status = false,
            _ => {}
        }
    }
    fn read(&self, off: u32, now: u64) -> u32 {
        match off {
            0x00 => self.load,
            0x04 => self.count(now),
            0x08 => self.ctrl | if self.status { TCIS } else { 0 },
            0x0C => u32::from(self.status),
            _ => 0,
        }
    }
    /// The zero crossings up to `now`; whether one happened.
    fn advance(&mut self, now: u64) -> bool {
        let mut fired = false;
        while self.next <= now {
            fired = true;
            self.status = true;
            if self.ctrl & TCAR != 0 && self.load > 0 {
                self.count0 = self.load;
                self.t0 = self.next;
                self.next += u64::from(self.load) * prescale(self.ctrl);
            } else {
                self.count0 = 0;
                self.next = u64::MAX;
            }
        }
        fired
    }
}

/// FRC2 (esp8266_peri.h:156-160, 0x620..0x630): counts up from LOAD at the prescaled APB clock;
/// when the count reaches ALARM it raises its interrupt (and again 2^32 counts later).
pub struct Frc2 {
    pub ctrl: u32,
    pub alarm: u32,
    pub status: bool,
    load: u32,
    count0: u32,
    t0: u64,
    /// the cycle of the next alarm, or u64::MAX when stopped
    pub next: u64,
}

impl Frc2 {
    fn new() -> Self { Frc2 { ctrl: 0, alarm: 0, status: false, load: 0, count0: 0, t0: 0, next: u64::MAX } }
    pub fn count(&self, now: u64) -> u32 {
        if self.ctrl & TCTE == 0 { return self.count0; }
        self.count0.wrapping_add((now.saturating_sub(self.t0) / prescale(self.ctrl)) as u32)
    }
    /// Re-base at the last count boundary and find the alarm's cycle. An alarm equal to the count
    /// raises the interrupt on this count, as digitake esp8266-emulator src/soc.c:283-293 does
    /// (`frc2_sched`: an alarm already due fires at once).
    fn schedule(&mut self, now: u64) {
        let p = prescale(self.ctrl);
        let elapsed = if self.ctrl & TCTE == 0 { 0 } else { now.saturating_sub(self.t0) / p };
        self.count0 = self.count(now);
        self.t0 = if self.ctrl & TCTE == 0 { now } else { self.t0 + elapsed * p };
        self.next = if self.ctrl & TCTE == 0 { u64::MAX } else { self.t0 + u64::from(self.alarm.wrapping_sub(self.count0)) * p };
    }
    fn write(&mut self, off: u32, v: u32, now: u64) {
        match off {
            0x20 => { self.load = v; self.count0 = v; self.t0 = now; self.schedule(now); }
            0x28 => { self.schedule(now); self.ctrl = v & 0xff; self.t0 = now; self.schedule(now); }
            0x2C => self.status = false,
            0x30 => { self.alarm = v; self.schedule(now); }
            _ => {}
        }
    }
    fn read(&self, off: u32, now: u64) -> u32 {
        match off {
            0x20 => self.load,
            0x24 => self.count(now),
            0x28 => self.ctrl | if self.status { TCIS } else { 0 },
            0x2C => u32::from(self.status),
            0x30 => self.alarm,
            _ => 0,
        }
    }
    fn advance(&mut self, now: u64) -> bool {
        if self.next > now { return false; }
        self.status = true;
        while self.next <= now { self.next += (1u64 << 32) * prescale(self.ctrl); }
        true
    }
}

/// esp8266_peri.h:84-86: GPIO_PINn bit 2 open drain (GPCD), bits 7..9 the interrupt type (GPCI:
/// 0 none, 1 rising, 2 falling, 3 either, 4 low, 5 high).
const PIN_OPEN_DRAIN: u32 = 1 << 2;
/// esp8266_peri.h:91-106 GPF0..GPF15: each pin's IO_MUX register offset from 0x800.
pub const IO_MUX_OFFSET: [u32; 16] = [0x34, 0x18, 0x38, 0x14, 0x3C, 0x40, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30, 0x04, 0x08, 0x0C, 0x10];
/// esp8266_peri.h:119 GPFPU: an IO_MUX pin register's pull-up enable.
const IO_MUX_PULLUP: u32 = 1 << 7;
/// esp8266_peri.h:141 GP16FPD: GPIO16's pull-down, in its function register GP16F.
const GP16_PULLDOWN: u32 = 1 << 3;
/// The pads with a pull-up after reset: GPIO0 and GPIO2, which the ROM pulls up so that a
/// floating strapping pin selects the flash boot (design §3); GPIO15 has no pull.
const RESET_PULLUPS: u32 = 1 << 0 | 1 << 2;

/// GPIO (esp8266_peri.h:48-87, 0x300..0x370) and the pads: GPIO0..15 with their output, enable,
/// open-drain and interrupt settings, GPIO16 through its RTC registers (esp8266_peri.h:126-141,
/// 0x768 OUT, 0x774 ENABLE, 0x78C IN, 0x790 CONF, 0x7A0 FUNC), and each pad's level: the chip's
/// drive, else the host's, else the pull-up (GPIO16: the pull-down), else the last level.
/// GPIO_IN (0x318) reads the levels of GPIO0..15 and, in bits 16 and up, the strapping latched at
/// reset as the boot mode number GPIO15 << 2 | GPIO0 << 1 | GPIO2 (cores/esp8266/Updater.cpp:84
/// reads `(GPI >> 16) & 0xf`; digitake esp8266-emulator src/soc.c:216 returns 3, a flash boot).
#[derive(Default)]
pub struct Gpio {
    pub out: u32,
    pub enable: u32,
    pub status: u32,
    pub pin: [u32; 16],
    /// GPIO16: GP16O, GP16E, GP16C, GP16F
    pub gp16: [u32; 4],
    /// deep sleep: the RTC drives GPIO16 as XPD_DCDC at this level
    pub xpd: Option<bool>,
    /// pins 0..16 the host drives, and its levels on them; the others float
    pub host_driven: u32,
    pub host_level: u32,
    /// pins 0..15 with the IO_MUX pull-up enabled
    pub pullup: u32,
    /// the pads' levels, pins 0..16, as last evaluated
    pub level: u32,
    /// the boot mode the strapping pins selected at reset (`rom::boot::STRAP_*`)
    pub strap: u32,
    /// record output changes for `SocBus::take_gpio_events`
    pub observe: bool,
    pub events: Vec<(u64, u8, bool)>,
    rtc_calib_sync: u32,
}

/// GPIO_RTC_CALIB_SYNC 0x36C and GPIO_RTC_CALIB_VALUE 0x370 (eagle_soc.h:116-121): libmain's
/// `pm_rtc_clock_cali` (Blink image 0x4010107c) starts a calibration through 0x36C, polls 0x370
/// until bit 31 (ready) is set and takes bits 0..19 as the 26 MHz crystal's cycles over 256 RTC
/// cycles: it computes the RTC period in microseconds as a Q12 number, value << 4 / 26 (26 is the
/// crystal's MHz from its table). Ready at once, with the value of an RTC clock of `RTC_HZ`, so
/// that the sleep times the SDK converts with it come out right on the modelled counter.
const RTC_CALIB_VALUE: u32 = (1 << 31) | (26 * 256 * 1_000_000 / RTC_HZ) as u32;

impl Gpio {
    /// The levels the chip drives: (pins driven, levels). An open-drain pin drives only its 0.
    fn drive(&self) -> (u32, u32) {
        let od = self.pin.iter().enumerate().fold(0, |m, (p, &c)| m | u32::from(c & PIN_OPEN_DRAIN != 0) << p);
        let mut driven = self.enable & 0xffff & !(od & self.out);
        let mut level = self.out & 0xffff;
        match self.xpd {
            Some(l) => { driven |= 1 << 16; level |= u32::from(l) << 16; }
            None if self.gp16[1] & 1 != 0 => { driven |= 1 << 16; level |= (self.gp16[0] & 1) << 16; }
            None => {}
        }
        (driven, level & driven)
    }
    /// The output levels the chip drives high: GPIO_OUT & GPIO_ENABLE for GPIO0..15, bit 16 for
    /// GPIO16 (its RTC output when enabled, or the XPD_DCDC level in deep sleep).
    pub fn output(&self) -> u32 {
        let gp16 = self.xpd.unwrap_or(self.gp16[1] & 1 != 0 && self.gp16[0] & 1 != 0);
        (self.out & self.enable & 0xffff) | u32::from(gp16) << 16
    }
    /// Re-derive the pad levels; edges and levels set the interrupt status of their pins.
    pub(crate) fn update_levels(&mut self) {
        let (driven, out) = self.drive();
        let host = self.host_driven & !driven;
        let floating = !(driven | host) & 0x1_ffff;
        let up = self.pullup & 0xffff;
        let down = if self.gp16[3] & GP16_PULLDOWN != 0 { 1 << 16 } else { 0 };
        let level = out | (self.host_level & host) | (floating & up) | (self.level & floating & !up & !down);
        let (rose, fell) = (level & !self.level, !level & self.level);
        for p in 0..16 {
            let hit = match (self.pin[p] >> 7) & 7 { 1 => rose, 2 => fell, 3 => rose | fell, 4 => !level, 5 => level, _ => 0 };
            self.status |= hit & 1 << p;
        }
        self.level = level;
    }
    /// The strapping as the ROM reads it, GPIO15 << 2 | GPIO0 << 1 | GPIO2.
    fn boot_mode(&self) -> u32 { (self.level >> 15 & 1) << 2 | (self.level & 1) << 1 | (self.level >> 2 & 1) }
    fn input(&self) -> u32 { (self.level & 0xffff) | self.strap << 16 }
    /// Record a change of the levels the chip drives high (`output`).
    pub(crate) fn note(&mut self, before: u32, now: u64) {
        let changed = before ^ self.output();
        if self.observe && changed != 0 {
            for p in 0..17 { if changed & (1 << p) != 0 { self.events.push((now, p as u8, self.output() & (1 << p) != 0)); } }
        }
    }
    fn read(&self, off: u32) -> u32 {
        match off {
            0x00 => self.out,
            0x0C => self.enable,
            0x18 => self.input(),
            0x1C => self.status,
            0x28..=0x64 => self.pin[((off - 0x28) / 4) as usize],
            0x6C => self.rtc_calib_sync,
            0x70 => RTC_CALIB_VALUE,
            _ => 0,
        }
    }
    fn write(&mut self, off: u32, v: u32, now: u64) {
        let before = self.output();
        match off {
            0x00 => self.out = v & 0xffff,
            0x04 => self.out |= v & 0xffff,
            0x08 => self.out &= !v,
            0x0C => self.enable = v & 0xffff,
            0x10 => self.enable |= v & 0xffff,
            0x14 => self.enable &= !v,
            0x1C => self.status = v & 0xffff,
            0x20 => self.status |= v & 0xffff,
            0x24 => self.status &= !v,
            0x28..=0x64 => self.pin[((off - 0x28) / 4) as usize] = v,
            0x6C => self.rtc_calib_sync = v,
            _ => {}
        }
        self.update_levels();
        self.note(before, now);
    }
    /// GPIO16's registers at RTC offset `off` (0x768.. as an offset from 0x60000000).
    fn gp16_index(off: u32) -> Option<usize> { match off { 0x768 => Some(0), 0x774 => Some(1), 0x790 => Some(2), 0x7A0 => Some(3), _ => None } }
    fn gp16_write(&mut self, i: usize, v: u32, now: u64) {
        let before = self.output();
        self.gp16[i] = v;
        self.update_levels();
        self.note(before, now);
    }
    /// The RTC's drive of GPIO16 in deep sleep (`None`: the pad is the GPIO again).
    fn set_xpd(&mut self, l: Option<bool>, now: u64) {
        let before = self.output();
        self.xpd = l;
        self.update_levels();
        self.note(before, now);
    }
}

/// The RTC block's slow clock: the RTC counter 0x71C counts at about 150 kHz (digitake
/// esp8266-emulator src/soc.c:558); `RTC_CALIB_VALUE` tells the SDK the same rate.
pub const RTC_HZ: u64 = 150_000;
/// The RTC counter at device time `cycles` (80 MHz).
pub fn rtc_count(cycles: u64) -> u32 { (u128::from(cycles) * u128::from(RTC_HZ) / u128::from(CPU_HZ)) as u32 }
/// The first cycle at or after `cycles` at which the counter has moved on by `ticks`.
fn rtc_cycles(cycles: u64, ticks: u32) -> u64 {
    let start = u128::from(rtc_count(cycles)) + u128::from(ticks);
    (start * u128::from(CPU_HZ)).div_ceil(u128::from(RTC_HZ)).max(u128::from(cycles)) as u64
}
/// RTC 0x00 bit 31: a software reset of the digital core (ROM SW_RESET), the bit the ROM's
/// `software_reset` stands for (model choice: the ESP8266's RTC register map is not published;
/// the bit is the RTC_CNTL software system reset of the ESP32, whose RTC block descends from it).
const RTC_SWRST: u32 = 1 << 31;
/// RTC 0x04 RTCSV (esp8266_peri.h:170): the sleep timer's target; 0x08 bit 20: start a sleep;
/// 0x18 bit 3: the timer wakes it (libmain's deep-sleep path writes 8, or 0 for `deepSleep(0)`:
/// Blink image 0x4023572b/0x40235720); 0x40: libmain writes all ones there before a deep sleep
/// (0x402356ea, its power-down mask) and 0 before the short sleeps of its start-up and modem
/// sleep (`pm_sleep_opt` 0x40224b89, `sleep_opt_8266` 0x402255ec), so here a sleep with 0x40 all
/// ones is a deep sleep and the others are not modelled.
const RTC_SLEEP_TARGET: u32 = 0x704;
const RTC_SLEEP: u32 = 0x708;
const RTC_SLEEP_EN: u32 = 1 << 20;
const RTC_WAKE_ENABLE: u32 = 0x718;
const RTC_WAKE_TIMER: u32 = 1 << 3;
const RTC_POWER_DOWN: u32 = 0x740;
const RTC_COUNTER: u32 = 0x71C;
const RTC_WAKE: u32 = 0x728;
/// How long the RTC pulls GPIO16 (XPD_DCDC) low when the deep-sleep timer runs out: model
/// choice (design §3); wired to RST, the pulse resets the chip.
pub const WAKE_PULSE_CYCLES: u64 = 100 * CPU_HZ / 1_000_000;

/// A deep sleep: the digital domain is off; the RTC timer ends it with a low pulse on GPIO16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeepSleep {
    /// the cycle the timer runs out (`None`: no timer, `deepSleep(0)`)
    pub until: Option<u64>,
    /// the end of the GPIO16 pulse, while it lasts
    pub pulse_end: Option<u64>,
}

/// The RTC block (0x700..0x7FF; esp8266_peri.h:170-174). Register memory, except: 0x71C, the
/// RTC counter, counts at `RTC_HZ`; 0x728 reads with bit 0 set: libmain's `pm_wait4wakeup`
/// (Blink image 0x40224e3c) polls bits 1..0 of it for the wake-up after the PHY start (soc.c:559
/// forces bit 0 the same way); 0x700 bit 31 resets; 0x708 bit 20 sleeps (deep sleep only);
/// GPIO16's registers are `Gpio`'s. STORE0..3 (eagle_soc.h:204-207, 0x730..0x73C) and the rest
/// are memory that survives every reset but an external one and power-on (`SocBus::reboot`).
#[derive(Default, Clone)]
pub struct Rtc { pub regs: RegMem, pub sleep: Option<DeepSleep> }
impl Rtc {
    fn read(&self, off: u32, now: u64) -> u32 {
        match off {
            RTC_COUNTER => rtc_count(now),
            RTC_WAKE => self.regs.read(off) | 1,
            _ => self.regs.read(off),
        }
    }
}

/// Microseconds of device time at `cycles` (80 MHz).
pub fn now_us(cycles: u64) -> u64 { cycles / (CPU_HZ / 1_000_000) }

/// The hardware watchdog (0x900..0x918). Register meaning as digitake esp8266-emulator
/// src/soc.c:324-392 has it from Cesanta's measured esp_hw_wdt_register.h: CTRL 0x00 (bit 0
/// enable, bit 1 stage 1 reloads stage 0 instead of resetting, bit 2 no stage 0), OP 0x04 and
/// OP_ND 0x08 (stage 0 and stage 1 last 2^(16 + n) APB cycles), 0x0C the count, 0x10 the stage,
/// RST 0x14 (0x73 feeds, esp8266_peri.h:293-294 WDT_FEED), 0x18 (0x73 back to stage 0). Stage 0
/// running out raises the WDT edge (line 8) with DPORT 0x04 bit 0; stage 1 resets the chip.
#[derive(Default)]
pub struct Wdt { ctrl: u32, reload: [u32; 2], stage: u32, count: u64, paused: bool, due: Option<u64> }
const WDT_FEED_KEY: u32 = 0x73;
impl Wdt {
    fn length(&self, stage: usize) -> u64 { 1 << (16 + (self.reload[stage] & 15)) }
    fn count(&self, now: u64) -> u64 { self.due.map_or(self.count, |d| d.saturating_sub(now)) }
    fn feed(&mut self, now: u64) {
        self.stage = 0;
        self.count = self.length(0);
        self.due = (self.ctrl & 1 != 0 && !self.paused).then(|| now + self.count);
    }
    fn pause(&mut self, on: bool, now: u64) {
        if on == self.paused { return; }
        if on { self.count = self.count(now); self.due = None; } else if self.ctrl & 1 != 0 { self.due = Some(now + self.count); }
        self.paused = on;
    }
    fn read(&self, off: u32, now: u64) -> u32 {
        match off { 0x00 => self.ctrl, 0x04 => self.reload[0], 0x08 => self.reload[1], 0x0C => self.count(now) as u32, 0x10 => self.stage, _ => 0 }
    }
    fn write(&mut self, off: u32, v: u32, now: u64) {
        match off {
            0x00 => {
                let was = self.ctrl & 1 != 0;
                self.count = self.count(now);
                self.ctrl = v;
                if v & 1 == 0 { self.due = None; } else if !was { self.feed(now); }
            }
            0x04 => self.reload[0] = v & 15,
            0x08 => self.reload[1] = v & 15,
            0x14 if v & 0xff == WDT_FEED_KEY => self.feed(now),
            0x18 if v & 0xff == WDT_FEED_KEY => self.stage = 0,
            _ => {}
        }
    }
    fn next(&self) -> u64 { self.due.unwrap_or(u64::MAX) }
    /// The stages that ran out up to `now`: (an edge was raised, the chip resets).
    fn advance(&mut self, now: u64) -> (bool, bool) {
        let mut edge = false;
        while let Some(due) = self.due.filter(|&d| d <= now) {
            if self.ctrl & 4 != 0 || self.stage != 0 && self.ctrl & 2 == 0 { self.due = None; return (edge, true); }
            if self.stage == 0 { self.stage = 1; edge = true; }
            self.count = self.length(if self.ctrl & 2 != 0 { 0 } else { 1 });
            self.due = Some(due + self.count);
        }
        (edge, false)
    }
}

/// One complete SPI1 transaction for the host: the bytes on MOSI (command, address, data), and how
/// many bytes it reads back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpiTransfer { pub tx: Vec<u8>, pub rx_len: usize }

/// esp8266_peri.h:336-373 (SPI1 0x100..0x17C) and the bits at :396-506: CMD 0x00 bit 18 USR
/// starts a user transaction and reads 1 until it is done; CTRL 0x08 bits 26/25 send/receive each
/// byte LSB first; USER 0x1C bits 31 command, 30 address, 28 MISO, 27 MOSI, 25/24 the MOSI/MISO
/// data in W8.. instead of W0.., 0 full duplex (MISO during MOSI); USER1 0x20 bits 26..31 the
/// address bits - 1, 17..25 the MOSI bits - 1, 8..16 the MISO bits - 1; USER2 0x24 bits 28..31
/// the command bits - 1 and its value in bits 0..15; W0..W15 0x40..0x7C, low byte first.
pub struct Hspi {
    regs: [u32; 64],
    /// a transaction USR started that the bus has not handed on
    pending: Option<SpiTransfer>,
    /// a transaction waiting for the host (`SocBus::spi_host`); the guest sees USR until
    /// `finish_transfer`
    pub host_wait: Option<SpiTransfer>,
    pub transfers: u64,
}
impl Default for Hspi { fn default() -> Self { Hspi { regs: [0; 64], pending: None, host_wait: None, transfers: 0 } } }
const SPI_USR: u32 = 1 << 18;
const SPI_WR_BIT_ORDER: u32 = 1 << 26;
const SPI_RD_BIT_ORDER: u32 = 1 << 25;
const SPI_USR_COMMAND: u32 = 1 << 31;
const SPI_USR_ADDR: u32 = 1 << 30;
const SPI_USR_MISO: u32 = 1 << 28;
const SPI_USR_MOSI: u32 = 1 << 27;
const SPI_MOSI_HIGHPART: u32 = 1 << 25;
const SPI_MISO_HIGHPART: u32 = 1 << 24;
const SPI_DOUTDIN: u32 = 1;

impl Hspi {
    fn reg(&self, off: u32) -> u32 { self.regs[(off / 4) as usize] }
    fn read(&self, off: u32) -> u32 {
        let busy = self.pending.is_some() || self.host_wait.is_some();
        if off == 0 { self.reg(0) | if busy { SPI_USR } else { 0 } } else { self.reg(off) }
    }
    fn write(&mut self, off: u32, v: u32) {
        if off == 0 {
            self.regs[0] = v & !SPI_USR;
            if v & SPI_USR != 0 && self.pending.is_none() && self.host_wait.is_none() { self.pending = Some(self.assemble()); }
        } else {
            self.regs[(off / 4) as usize] = v;
        }
    }
    /// The bytes a transaction shifts out, from the registers USR found.
    fn assemble(&self) -> SpiTransfer {
        let (ctrl, user, user1, user2) = (self.reg(0x08), self.reg(0x1C), self.reg(0x20), self.reg(0x24));
        let order = |b: u8| if ctrl & SPI_WR_BIT_ORDER != 0 { b.reverse_bits() } else { b };
        let mut tx = Vec::new();
        if user & SPI_USR_COMMAND != 0 {
            let n = ((user2 >> 28) + 1).div_ceil(8);
            for i in 0..n { tx.push(order((user2 >> (8 * i)) as u8)); }
        }
        if user & SPI_USR_ADDR != 0 {
            let bits = (user1 >> 26) + 1;
            let a = self.reg(0x04);
            for i in 0..bits.div_ceil(8) { tx.push(order((a >> (24 - 8 * i)) as u8)); }
        }
        let mosi_bytes = ((((user1 >> 17) & 0x1ff) + 1).div_ceil(8)) as usize;
        if user & SPI_USR_MOSI != 0 {
            let base = if user & SPI_MOSI_HIGHPART != 0 { 0x60 } else { 0x40 };
            for i in 0..mosi_bytes.min(if base == 0x40 { 64 } else { 32 }) {
                tx.push(order((self.reg(base + 4 * (i as u32 / 4)) >> (8 * (i % 4))) as u8));
            }
        }
        let rx_len = if user & SPI_USR_MISO != 0 { ((((user1 >> 8) & 0x1ff) + 1).div_ceil(8)) as usize }
                     else if user & SPI_DOUTDIN != 0 && user & SPI_USR_MOSI != 0 { mosi_bytes } else { 0 };
        SpiTransfer { tx, rx_len }
    }
    /// The transaction USR started, for the bus to hand to the host or the board.
    pub fn take_transfer(&mut self) -> Option<SpiTransfer> { self.pending.take() }
    /// Complete a transaction with the bytes read back: they go to W0.. (W8.. with the MISO high
    /// part), and USR clears.
    pub fn finish_transfer(&mut self, t: SpiTransfer, rx: &[u8]) {
        let (ctrl, user) = (self.reg(0x08), self.reg(0x1C));
        let base = if user & SPI_MISO_HIGHPART != 0 { 0x60 } else { 0x40 };
        for i in 0..t.rx_len.min(if base == 0x40 { 64 } else { 32 }) {
            let b = rx.get(i).copied().unwrap_or(0xff);
            let b = if ctrl & SPI_RD_BIT_ORDER != 0 { b.reverse_bits() } else { b };
            let w = ((base + 4 * (i as u32 / 4)) / 4) as usize;
            let shift = 8 * (i % 4);
            self.regs[w] = (self.regs[w] & !(0xff << shift)) | u32::from(b) << shift;
        }
        self.transfers += 1;
    }
}

/// DPORT (0x3FF00000..0x3FF10000): register memory with the OTP words read-only.
pub struct Dport { pub regs: RegMem, pub otp: [u32; 4] }
/// WDEV (0x3FF20000..0x3FF40000): register memory with the counters and the random number
/// register; the MAC's queues are plain memory, so no frame completes and no MAC interrupt rises.
pub struct Wdev { pub regs: RegMem, rng: u64 }

impl Wdev {
    /// A deterministic sequence seeded from the MAC: a 64-bit LCG (Knuth's MMIX constants), the
    /// high word of each step.
    fn random(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.rng >> 32) as u32
    }
}

pub struct Peripherals {
    pub uart: [Uart; 2],
    pub frc1: Frc1,
    pub frc2: Frc2,
    pub wdt: Wdt,
    pub rtc: Rtc,
    pub gpio: Gpio,
    /// IO_MUX 0x800..0x8FF: register memory; the pins' pull-ups go to `gpio`
    pub io_mux: RegMem,
    /// SPI0 (the flash controller) 0x200..0x2FF: register memory; the bus runs its commands
    pub spi0: RegMem,
    /// SPI1 (HSPI) 0x100..0x1FF
    pub spi1: Hspi,
    pub dport: Dport,
    pub wdev: Wdev,
    /// the RF/PHY block: register memory
    pub rf: RegMem,
    /// the rest of the peripheral page (SLC, I2S, ...)
    pub other: RegMem,
    pub misc: Misc,
    /// device time in APB cycles, as the bus last counted it
    pub now: u64,
    /// a reset the watchdog or the RTC asked for, in the ROM's numbering (`rom::boot::RST_*`)
    pub reset: Option<u32>,
    /// the core's clock in half APB cycles per CPU cycle: 2, or 1 with CPU2X (`SocBus::tick`)
    pub cpu_scale_half: u64,
    /// edge interrupts raised and not yet presented (a line goes high for one presentation)
    pulses: u32,
    /// the edge lines of the last presentation
    presented: u32,
    /// register words already reported by `--log-periph`
    seen: HashSet<u32>,
}

impl Peripherals {
    pub fn new(mac: [u8; 6]) -> Self {
        let seed = mac.iter().fold(0u64, |s, &b| s << 8 | u64::from(b)) | 1;
        let mut io_mux = RegMem::default();
        for (p, off) in IO_MUX_OFFSET.iter().enumerate() { if RESET_PULLUPS & 1 << p != 0 { io_mux.write(PERIPH_BASE + 0x800 + off, IO_MUX_PULLUP); } }
        let mut gpio = Gpio { pullup: RESET_PULLUPS, ..Default::default() };
        gpio.update_levels();
        gpio.strap = gpio.boot_mode();
        Peripherals {
            uart: [Uart::new(UART_LAYOUT), Uart::new(UART_LAYOUT)], frc1: Frc1::new(), frc2: Frc2::new(), wdt: Wdt::default(),
            rtc: Rtc::default(), gpio, io_mux, spi0: RegMem::default(), spi1: Hspi::default(),
            dport: Dport { regs: RegMem::default(), otp: otp_words(mac) },
            wdev: Wdev { regs: RegMem::default(), rng: seed }, rf: RegMem::default(), other: RegMem::default(), misc: Misc::new(),
            now: 0, reset: None, cpu_scale_half: 2, pulses: 0, presented: 0, seen: HashSet::new(),
        }
    }

    /// Whether `addr` is a register of this file's (the bus maps RTC memory itself).
    pub fn contains(addr: u32) -> bool {
        matches!(addr, PERIPH_BASE..PERIPH_END | RF_LOW..RF_HIGH | DPORT_BASE..DPORT_END | WDEV_BASE..WDEV_END)
    }

    /// The UART at `addr`, and the register offset in it.
    fn uart_at(addr: u32) -> Option<(usize, u32)> {
        if (PERIPH_BASE..PERIPH_BASE + UART_SPAN).contains(&addr) { Some((0, addr - PERIPH_BASE)) }
        else if (UART1_BASE..UART1_BASE + UART_SPAN).contains(&addr) { Some((1, addr - UART1_BASE)) }
        else { None }
    }

    /// The interrupt bits of UART `n` that exist (RXFIFO_TOUT only with RX_TOUT_EN).
    fn uart_int_mask(&mut self, n: usize) -> u32 {
        if self.uart[n].read(0x24) & UART_RX_TOUT_EN != 0 { u32::MAX } else { !UART_INT_TOUT }
    }

    fn uart_read(&mut self, n: usize, off: u32) -> u32 {
        let mask = if matches!(off, 0x04 | 0x08) { self.uart_int_mask(n) } else { u32::MAX };
        self.uart[n].read(off) & mask
    }

    /// Whether UART `n` asks for interrupt 5.
    fn uart_irq(&mut self, n: usize) -> bool { let m = self.uart_int_mask(n); self.uart[n].int_raw & self.uart[n].int_ena & m != 0 }

    /// The register memory behind `addr`.
    fn mem(&mut self, addr: u32) -> &mut RegMem {
        match addr {
            RF_LOW..RF_HIGH => &mut self.rf,
            DPORT_BASE..DPORT_END => &mut self.dport.regs,
            WDEV_BASE..WDEV_END => &mut self.wdev.regs,
            _ => &mut self.other,
        }
    }

    /// A register's value; `peek` reads without side effects (a FIFO is not popped, nothing is
    /// noted, the RNG does not step).
    fn value(&mut self, addr: u32, peek: bool) -> u32 {
        let now = self.now;
        if let Some((n, off)) = Self::uart_at(addr) { return if peek && off == 0 { 0 } else { self.uart_read(n, off) }; }
        let off = addr.wrapping_sub(PERIPH_BASE);
        match addr {
            0x6000_0100..0x6000_0200 => self.spi1.read(off - 0x100),
            0x6000_0200..0x6000_0300 => self.spi0.read(addr),
            0x6000_0300..0x6000_0400 => self.gpio.read(off - 0x300),
            0x6000_0600..0x6000_0620 => self.frc1.read(off - 0x600, now),
            0x6000_0620..0x6000_0640 => self.frc2.read(off - 0x600, now),
            0x6000_078C => self.gpio.level >> 16 & 1,
            0x6000_0700..0x6000_0800 => match Gpio::gp16_index(off) { Some(i) => self.gpio.gp16[i], None => self.rtc.read(off, now) },
            0x6000_0800..0x6000_0900 => self.io_mux.read(addr),
            0x6000_0900..0x6000_0A00 => self.wdt.read(off - 0x900, now),
            DPORT_OTP..=DPORT_OTP_LAST => self.dport.otp[((addr - DPORT_OTP) / 4) as usize],
            UIS => u32::from(self.uart_irq(0)) | u32::from(self.uart_irq(1)) << 2,
            WDEV_US | WDEV_TSF_LO => now_us(now) as u32,
            WDEV_TSF_HI => (now_us(now) >> 32) as u32,
            WDEV_RNG if !peek => self.wdev.random(),
            RF_NOISEFLOOR => self.rf.read(addr) & !NOISEFLOOR_BUSY,
            _ => {
                if !peek { self.note(addr, false, 0); }
                self.mem(addr).read(addr)
            }
        }
    }

    pub fn read32(&mut self, addr: u32) -> u32 {
        let v = self.value(addr, false);
        let m = &mut self.misc;
        if m.log_all { eprintln!("[rd] {addr:#010x} -> {v:#010x} pc={:#010x}", m.cur_pc); }
        if let Some(l) = &mut m.mmio_log { l.push((m.cur_pc, addr, v, false)); }
        v
    }

    /// A register's value as a narrower store merges into it: no log line, no `mmio_log` entry and
    /// no `--log-periph` note, since the guest made no read. (A UART FIFO is not read here: the
    /// bus takes a narrow store to it as stored.)
    pub fn peek32(&mut self, addr: u32) -> u32 { self.value(addr, true) }

    pub fn write32(&mut self, addr: u32, v: u32) {
        let m = &mut self.misc;
        if m.log_all { eprintln!("[wr] {addr:#010x} <- {v:#010x} pc={:#010x}", m.cur_pc); }
        if let Some(l) = &mut m.mmio_log { l.push((m.cur_pc, addr, v, true)); }
        let now = self.now;
        if let Some((n, off)) = Self::uart_at(addr) { self.uart[n].write(off, v); return; }
        let off = addr.wrapping_sub(PERIPH_BASE);
        match addr {
            0x6000_0100..0x6000_0200 => self.spi1.write(off - 0x100, v),
            0x6000_0200..0x6000_0300 => self.spi0.write(addr, v),
            0x6000_0300..0x6000_0400 => self.gpio.write(off - 0x300, v, now),
            0x6000_0600..0x6000_0620 => self.frc1.write(off - 0x600, v, now),
            0x6000_0620..0x6000_0640 => self.frc2.write(off - 0x600, v, now),
            0x6000_078C => {}
            0x6000_0700..0x6000_0800 => match Gpio::gp16_index(off) { Some(i) => self.gpio.gp16_write(i, v, now), None => self.rtc_write(off, v) },
            0x6000_0800..0x6000_0900 => {
                self.io_mux.write(addr, v);
                if let Some(p) = IO_MUX_OFFSET.iter().position(|&o| o == off - 0x800) {
                    self.gpio.pullup = (self.gpio.pullup & !(1 << p)) | u32::from(v & IO_MUX_PULLUP != 0) << p;
                    self.gpio.update_levels();
                }
            }
            0x6000_0900..0x6000_0A00 => self.wdt.write(off - 0x900, v, now),
            DPORT_OTP..=DPORT_OTP_LAST | UIS | WDEV_US | WDEV_TSF_LO | WDEV_TSF_HI | WDEV_RNG => {}
            _ => {
                self.note(addr, true, v);
                self.mem(addr).write(addr, v);
                match addr {
                    DPORT_CPU2X => self.cpu_scale_half = if v & 1 != 0 { 1 } else { 2 },
                    DPORT_WDT_PAUSE => self.wdt.pause(v & 1 << 3 != 0, now),
                    _ => {}
                }
            }
        }
    }

    /// A store to the RTC block at `off` (from 0x60000000).
    fn rtc_write(&mut self, off: u32, v: u32) {
        let now = self.now;
        self.rtc.regs.write(off, v);
        match off {
            0x700 if v & RTC_SWRST != 0 => self.reset = Some(RST_SW),
            RTC_SLEEP if v & RTC_SLEEP_EN != 0 && self.rtc.regs.read(RTC_POWER_DOWN) == u32::MAX => {
                let until = (self.rtc.regs.read(RTC_WAKE_ENABLE) & RTC_WAKE_TIMER != 0)
                    .then(|| rtc_cycles(now, self.rtc.regs.read(RTC_SLEEP_TARGET).wrapping_sub(rtc_count(now))));
                self.enter_deep_sleep(until);
            }
            _ => {}
        }
    }

    /// The digital domain powers down: its timers and the watchdog stop, and the RTC drives
    /// GPIO16 (XPD_DCDC) high until its timer runs out at `until`.
    pub fn enter_deep_sleep(&mut self, until: Option<u64>) {
        self.frc1.next = u64::MAX;
        self.frc2.next = u64::MAX;
        self.wdt.due = None;
        self.pulses = 0;
        self.rtc.sleep = Some(DeepSleep { until, pulse_end: None });
        self.gpio.set_xpd(Some(true), self.now);
    }

    /// The first access to a register without a model, under `--log-periph`.
    fn note(&mut self, addr: u32, write: bool, v: u32) {
        if self.misc.log_unknown && self.seen.insert(addr) {
            eprintln!("[periph] {} {addr:#010x}{} pc={:#010x}", if write { "W" } else { "R" }, if write { format!(" = {v:#x}") } else { String::new() }, self.misc.cur_pc);
        }
    }

    /// DPORT CPU2X bit 0: the CPU clock is doubled to 160 MHz.
    pub fn cpu2x(&self) -> bool { self.cpu_scale_half == 1 }

    /// The pins the chip drives high (`Gpio::output`).
    pub fn gpio_output(&self) -> u32 { self.gpio.output() }

    /// The host's level on `pin` (0..=16): driven, or `None` to let it float (the pull-up, GPIO16's
    /// pull-down, or the last level).
    pub fn gpio_set_input(&mut self, pin: u8, level: Option<bool>) {
        if pin > 16 { return; }
        let bit = 1 << pin;
        match level {
            Some(l) => { self.gpio.host_driven |= bit; self.gpio.host_level = (self.gpio.host_level & !bit) | u32::from(l) << pin; }
            None => self.gpio.host_driven &= !bit,
        }
        self.gpio.update_levels();
    }

    /// The strapping pins' levels now, as the ROM reads them at reset: (GPIO15, GPIO0, GPIO2).
    pub fn strap(&self) -> (bool, bool, bool) { (self.gpio.level >> 15 & 1 != 0, self.gpio.level & 1 != 0, self.gpio.level >> 2 & 1 != 0) }

    /// Latch the strapping into GPIO_IN at reset release; the boot mode number.
    pub fn latch_strap(&mut self) -> u32 { self.gpio.strap = self.gpio.boot_mode(); self.gpio.strap }

    /// The cycle of the next timer, watchdog or deep-sleep event (u64::MAX: none).
    pub fn next_event(&self) -> u64 {
        let sleep = self.rtc.sleep.map_or(u64::MAX, |s| s.pulse_end.or(s.until).unwrap_or(u64::MAX));
        self.frc1.next.min(self.frc2.next).min(self.wdt.next()).min(sleep)
    }

    /// Device time is `now`: run the timers up to it. Whether an interrupt may have changed.
    pub fn advance(&mut self, now: u64) -> bool {
        self.now = now;
        if now < self.next_event() { return false; }
        if self.frc1.advance(now) {
            // an edge reaches the CPU with TM1_EDGE_INT_ENABLE, as interrupt 9 or the NMI; level
            // mode is a line (`refresh_lines`), except to the NMI, which only takes edges
            let nmi = self.dport.regs.read(DPORT_NMI) & 1 != 0;
            let edge = self.frc1.ctrl & TCIT == 0;
            if edge && self.dport.regs.read(DPORT_TEIE) & TEIE1 != 0 || !edge && nmi {
                self.pulses |= 1 << if nmi { INUM_NMI } else { INUM_FRC1 };
            }
        }
        if self.frc2.advance(now) { self.pulses |= 1 << INUM_FRC2; }
        let (edge, reset) = self.wdt.advance(now);
        if edge && self.dport.regs.read(DPORT_TEIE) & TEIE_WDT != 0 { self.pulses |= 1 << INUM_WDT; }
        if reset { self.reset = Some(RST_WDT); }
        if let Some(mut s) = self.rtc.sleep {
            if let Some(end) = s.pulse_end.filter(|&e| e <= now) {
                s.pulse_end = None;
                self.gpio.set_xpd(Some(true), end);
            }
            if let Some(at) = s.until.filter(|&t| t <= now) {
                s.until = None;
                self.gpio.set_xpd(Some(false), at);
                s.pulse_end = Some(at + WAKE_PULSE_CYCLES);
                if s.pulse_end.unwrap() <= now { s.pulse_end = None; self.gpio.set_xpd(Some(true), at + WAKE_PULSE_CYCLES); }
            }
            self.rtc.sleep = Some(s);
        }
        true
    }

    /// The interrupt lines (bit n = line n, the NMI at 14): the level sources, and each edge
    /// raised since the last call high for this one presentation. An edge that would follow a
    /// presentation in which its line was already high waits one presentation, so the core sees a
    /// low before it. Returns the lines and whether an edge is still waiting. In deep sleep there
    /// are none.
    pub fn refresh_lines(&mut self) -> (u32, bool) {
        if self.rtc.sleep.is_some() { return (0, false); }
        let mut level = 0;
        if self.uart_irq(0) || self.uart_irq(1) { level |= 1 << INUM_UART; }
        if self.gpio.status != 0 { level |= 1 << INUM_GPIO; }
        if self.frc1.status && self.frc1.ctrl & TCIT != 0 && self.dport.regs.read(DPORT_NMI) & 1 == 0 { level |= 1 << INUM_FRC1; }
        let edges = self.pulses & !self.presented;
        self.pulses &= !edges;
        self.presented = edges;
        (level | edges, self.pulses != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_registers_follow_uart_register_h() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(PERIPH_BASE, b'A' as u32);                  // UART_FIFO(0)
        p.write32(UART1_BASE, b'B' as u32);                   // UART_FIFO(1)
        assert_eq!(p.uart[0].tx_out, b"A");
        assert_eq!(p.uart[1].tx_out, b"B");
        p.write32(PERIPH_BASE + 0x14, 694);                   // UART_CLKDIV(0)
        assert_eq!(p.read32(PERIPH_BASE + 0x14), 694);
        p.uart[0].host_input(b"xy");
        assert_eq!(p.read32(PERIPH_BASE + 0x1c) & 0xff, 2);   // UART_STATUS RXFIFO_CNT
        assert_eq!((p.read32(PERIPH_BASE + 0x1c) >> 16) & 0xff, 0);   // TXFIFO_CNT: always drained
        assert_eq!(p.read32(PERIPH_BASE), b'x' as u32);
        p.write32(PERIPH_BASE + 0x20, 1 << 17);               // CONF0 RXFIFO_RST
        assert_eq!(p.read32(PERIPH_BASE + 0x1c) & 0xff, 0);
    }

    #[test]
    fn uart_receive_raises_interrupt_5_at_the_threshold_and_tout_only_when_enabled() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(PERIPH_BASE + 0x24, 4);                     // CONF1 RXFIFO_FULL_THRHD 4
        p.write32(PERIPH_BASE + 0x0c, 1 | 1 << 8);            // INT_ENA RXFIFO_FULL | RXFIFO_TOUT
        p.uart[0].host_input(b"abc");
        assert_eq!(p.refresh_lines().0 & 1 << INUM_UART, 0, "below the threshold, timeout disabled");
        assert_eq!(p.read32(PERIPH_BASE + 0x04) & 1 << 8, 0);
        p.uart[0].host_input(b"d");
        assert_ne!(p.refresh_lines().0 & 1 << INUM_UART, 0);
        assert_eq!(p.read32(UIS), 1);
        for _ in 0..4 { p.read32(PERIPH_BASE); }
        assert_eq!(p.refresh_lines().0 & 1 << INUM_UART, 0, "drained");
        // UART1 shares the line
        p.write32(UART1_BASE + 0x24, 1 << 31 | 100);          // CONF1 RX_TOUT_EN
        p.write32(UART1_BASE + 0x0c, 1 << 8);
        p.uart[1].host_input(b"z");
        assert_ne!(p.refresh_lines().0 & 1 << INUM_UART, 0);
        assert_eq!(p.read32(UIS), 4);
    }

    #[test]
    fn frc2_counts_up_and_raises_one_edge_per_alarm() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(0x6000_0628, TCTE | 2 << 2);                // enabled, /256
        p.write32(0x6000_0630, 100);                          // ALARM
        assert_eq!(p.next_event(), 100 * 256);
        assert!(!p.advance(100 * 256 - 1));
        assert_eq!(p.read32(0x6000_0624), 99);
        assert!(p.advance(100 * 256));
        assert_eq!(p.refresh_lines(), (1 << INUM_FRC2, false));
        assert_eq!(p.refresh_lines(), (0, false), "an edge is one presentation");
        assert_ne!(p.read32(0x6000_0628) & TCIS, 0);
        p.write32(0x6000_062C, 1);
        assert_eq!(p.read32(0x6000_0628) & TCIS, 0);
        // the next alarm, 50 counts on; a pulse right after a presentation waits for a low
        p.write32(0x6000_0630, 150);
        assert_eq!(p.next_event(), 150 * 256);
        assert!(p.advance(150 * 256));
        p.presented = 1 << INUM_FRC2;
        assert_eq!(p.refresh_lines(), (0, true));
        assert_eq!(p.refresh_lines(), (1 << INUM_FRC2, false));
    }

    #[test]
    fn frc1_counts_down_reloads_and_goes_to_the_nmi_by_dport_0_bit_0() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(DPORT_TEIE, TEIE1);
        p.write32(0x6000_0608, TCTE | TCAR);                  // /1, edge, auto-reload
        p.write32(0x6000_0600, 1000);
        assert_eq!(p.read32(0x6000_0604), 1000);
        assert!(p.advance(1000));
        assert_eq!(p.refresh_lines().0, 1 << INUM_FRC1);
        assert_eq!(p.next_event(), 2000);
        p.write32(DPORT_NMI, 15);
        assert!(p.advance(2000));
        assert_eq!(p.refresh_lines().0, 1 << INUM_NMI);
    }

    #[test]
    fn unmodelled_registers_are_memory_and_cpu2x_is_dport_0x14() {
        let mut p = Peripherals::new([0; 6]);
        assert!(!p.cpu2x());
        p.write32(DPORT_BASE + 0x14, 1);
        assert!(p.cpu2x());
        p.write32(0x6000_0B00, 0x55);
        p.write32(WDEV_BASE + 0x100, 7);
        assert_eq!((p.read32(0x6000_0B00), p.read32(WDEV_BASE + 0x100), p.read32(RF_LOW + 0x100)), (0x55, 7, 0));
        // the OTP words are fuses
        p.write32(DPORT_BASE + 0x58, 0);
        assert_eq!(p.read32(DPORT_BASE + 0x58), otp_words([0; 6])[2]);
        assert_ne!(p.read32(DPORT_BASE + 0x58) & 1 << 15, 0);
        assert!(Peripherals::contains(RF_HIGH - 4) && !Peripherals::contains(RF_HIGH) && !Peripherals::contains(RTC_MEM_LOW));
    }

    #[test]
    fn counters_follow_device_time_and_the_rng_is_seeded_from_the_mac() {
        let mut p = Peripherals::new([1, 2, 3, 4, 5, 6]);
        p.advance(80 * 1234);
        assert_eq!(p.read32(WDEV_US), 1234);
        assert_eq!(p.read32(0x6000_0728) & 1, 1);
        assert_eq!(p.read32(0x6000_0370) >> 31, 1);
        assert_eq!(p.read32(0x6000_0318) >> 16, 3, "flash boot strapping");
        let a = (p.read32(WDEV_RNG), p.read32(WDEV_RNG));
        let mut q = Peripherals::new([1, 2, 3, 4, 5, 6]);
        assert_eq!(a, (q.read32(WDEV_RNG), q.read32(WDEV_RNG)));
        assert_ne!(a.0, a.1);
    }

    const FRC1: u32 = PERIPH_BASE + 0x600;
    const GPIO: u32 = PERIPH_BASE + 0x300;
    const RTC: u32 = PERIPH_BASE + 0x700;
    const IO_MUX: u32 = PERIPH_BASE + 0x800;
    const WDT: u32 = PERIPH_BASE + 0x900;
    const SPI1: u32 = PERIPH_BASE + 0x100;

    /// LOAD 5000 at /16 is 80 000 APB cycles: 1000 us at 80 MHz, then again with auto-reload.
    #[test]
    fn frc1_edge_interrupt_fires_at_load_div16_and_autoloads() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(DPORT_TEIE, TEIE1);
        p.write32(FRC1 + 8, TCTE | TCAR | 1 << 2);            // enabled, auto-reload, /16, edge
        p.write32(FRC1, 5000);
        assert_eq!(p.next_event(), 80_000);
        assert!(!p.advance(79_999));
        assert_eq!(p.read32(FRC1 + 4), 1, "4999 counts of 16 cycles done");
        assert!(p.advance(80_000));
        assert_eq!(p.refresh_lines().0, 1 << INUM_FRC1);
        assert_eq!(p.refresh_lines().0, 0, "an edge is one presentation");
        assert_eq!(p.read32(FRC1 + 4), 5000, "reloaded");
        assert_eq!(p.next_event(), 160_000);
        assert!(p.advance(160_000));
        assert_eq!(p.refresh_lines().0, 1 << INUM_FRC1);
        // TEIE bit 1 clear: the edge does not reach the CPU
        p.write32(DPORT_TEIE, 0);
        assert!(p.advance(240_000));
        assert_eq!(p.refresh_lines().0, 0);
    }

    /// With TEIE bit 1 the edge reaches the CPU; DPORT 0x00 bit 0 (the SDK's `NmiTimSetFunc`)
    /// sends it to the NMI, line 14, instead of line 9.
    #[test]
    fn frc1_nmi_when_dport_teie_bit1() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(DPORT_NMI, 15);
        p.write32(FRC1 + 8, TCTE);                            // /1, edge, single shot
        p.write32(FRC1, 800);
        assert!(p.advance(800));
        assert_eq!(p.refresh_lines().0, 0, "TEIE bit 1 clear");
        p.write32(DPORT_TEIE, TEIE1);
        p.write32(FRC1, 800);
        assert!(p.advance(1600));
        assert_eq!(p.refresh_lines().0, 1 << INUM_NMI);
        assert_eq!(p.next_event(), u64::MAX, "single shot: stopped at 0");
    }

    /// The SDK's settings (Blink run: CTRL 0x39, OP 11, OP_ND 13): stage 0 is 2^27 APB cycles,
    /// stage 1 2^29; a feed (RST = 0x73) starts stage 0 again.
    #[test]
    fn wdt_feed_0x73_defers_reset_and_timeout_resets_with_rom_cause_4() {
        let mut p = Peripherals::new([0; 6]);
        let (s0, s1) = (1u64 << 27, 1u64 << 29);
        p.write32(WDT + 4, 11);
        p.write32(WDT + 8, 13);
        p.write32(WDT, 0x39);
        assert_eq!(p.next_event(), s0);
        p.advance(s0 - 1000);
        p.write32(WDT + 0x14, 0x73);
        assert_eq!(p.next_event(), 2 * s0 - 1000, "fed");
        p.write32(WDT + 0x14, 0x72);
        assert_eq!(p.next_event(), 2 * s0 - 1000, "only 0x73 feeds");
        // DPORT 0x18 bit 3 pauses it
        p.advance(s0);
        p.write32(DPORT_BASE + 0x18, 1 << 3);
        assert_eq!(p.next_event(), u64::MAX);
        p.advance(3 * s0);
        p.write32(DPORT_BASE + 0x18, 0);
        assert_eq!(p.next_event(), 4 * s0 - 1000);
        // stage 0 runs out: the WDT edge (line 8) with DPORT 0x04 bit 0, no reset yet
        p.write32(DPORT_TEIE, 1);
        assert!(p.advance(4 * s0 - 1000));
        assert_eq!(p.refresh_lines().0, 1 << INUM_WDT);
        assert_eq!((p.reset, p.read32(WDT + 0x10)), (None, 1));
        // stage 1 runs out: a reset with the ROM's OWDT_RESET
        assert!(!p.advance(4 * s0 - 1000 + s1 - 1));
        assert_eq!(p.reset, None);
        p.advance(4 * s0 - 1000 + s1);
        assert_eq!(p.reset, Some(crate::rom::boot::RST_WDT));
    }

    #[test]
    fn rtc_swrst_bit31_resets_with_rom_cause_3() {
        let mut p = Peripherals::new([0; 6]);
        p.write32(RTC, 0x0019_c06a);
        assert_eq!(p.reset, None);
        assert_eq!(p.read32(RTC), 0x0019_c06a);
        p.write32(RTC, 1 << 31);
        assert_eq!(p.reset, Some(crate::rom::boot::RST_SW));
    }

    /// A jump to the reset vector and a digital-core reset keep the RTC registers and RTC memory;
    /// an external (or deep-sleep wake) reset clears the registers and keeps the memory; power-on
    /// clears both.
    #[test]
    fn rtc_store_and_user_memory_survive_reboot() {
        use crate::rom::boot::{RST_EXT, RST_POWERON, RST_WDT};
        use emu_core::Bus;
        use esp_soc::SocBus as _;
        let mut b = crate::bus::SocBus::new(1 << 20, [0; 6]);
        let (store0, user) = (RTC + 0x30, RTC_MEM_LOW + 0x200);
        let set = |b: &mut crate::bus::SocBus| {
            b.write32(store0, 4).unwrap();
            b.write32(RTC + 0x3C, 7).unwrap();
            b.write32(user, 0x4A4D_5052).unwrap();
            b.write32(GPIO, 1).unwrap();
        };
        let get = |b: &mut crate::bus::SocBus| (b.read32(store0).unwrap(), b.read32(RTC + 0x3C).unwrap(), b.read32(user).unwrap(), b.read32(GPIO).unwrap());
        set(&mut b);
        b.restart_from_reset_vector();
        assert_eq!(b.reboot([0; 6]), RST_POWERON, "a jump keeps the cause");
        assert_eq!(get(&mut b), (4, 7, 0x4A4D_5052, 0), "RTC kept, GPIO reset");
        b.request_reset(RST_WDT);
        assert_eq!(b.reboot([0; 6]), RST_WDT);
        assert_eq!(get(&mut b), (4, 7, 0x4A4D_5052, 0));
        b.request_reset(RST_EXT);
        assert_eq!(b.reboot([0; 6]), RST_EXT);
        assert_eq!(get(&mut b), (0, 0, 0x4A4D_5052, 0), "RTC memory kept, its registers cleared");
        set(&mut b);
        b.request_reset(RST_POWERON);
        assert_eq!(b.reboot([0; 6]), RST_POWERON);
        assert_eq!(get(&mut b), (0, 0, 0, 0));
    }

    #[test]
    fn gpio_w1ts_w1tc_and_open_drain_pin_register() {
        let mut p = Peripherals::new([0; 6]);
        let level = |p: &mut Peripherals, pin: u32| p.read32(GPIO + 0x18) >> pin & 1;
        p.write32(GPIO + 0x10, 1 << 4 | 1 << 5);              // ENABLE_W1TS
        p.write32(GPIO + 0x04, 1 << 4 | 1 << 5);              // OUT_W1TS
        assert_eq!(p.gpio_output() & 0x30, 0x30);
        p.write32(GPIO + 0x08, 1 << 5);                       // OUT_W1TC
        assert_eq!((p.gpio_output() & 0x30, p.read32(GPIO) & 0x30), (0x10, 0x10));
        assert_eq!(p.read32(GPIO + 0x18) & 0x30, 0x10);
        p.write32(GPIO + 0x14, 1 << 4);                       // ENABLE_W1TC
        assert_eq!((p.gpio_output() & 0x30, p.read32(GPIO + 0x0C) & 0x30), (0, 0x20));
        // a push-pull output drives against the host
        p.gpio_set_input(5, Some(true));
        assert_eq!(level(&mut p, 5), 0);
        // open drain (PIN5 bit 2): low is driven, high releases the line to the host
        p.write32(GPIO + 0x28 + 4 * 5, 1 << 2);
        p.write32(GPIO + 0x04, 1 << 5);
        assert_eq!(level(&mut p, 5), 1);
        p.gpio_set_input(5, Some(false));
        assert_eq!(level(&mut p, 5), 0, "a device holds the released line low");
        p.gpio_set_input(5, Some(true));
        p.write32(GPIO + 0x08, 1 << 5);
        assert_eq!(level(&mut p, 5), 0, "driven low against the host's high");
        // interrupts: PIN12 INT_TYPE 1 (rising) sets STATUS and line 4
        p.write32(GPIO + 0x28 + 4 * 12, 1 << 7);
        p.gpio_set_input(12, Some(false));
        assert_eq!(p.refresh_lines().0 & 1 << INUM_GPIO, 0);
        p.gpio_set_input(12, Some(true));
        assert_eq!(p.read32(GPIO + 0x1C), 1 << 12);
        assert_ne!(p.refresh_lines().0 & 1 << INUM_GPIO, 0);
        p.write32(GPIO + 0x24, 1 << 12);                      // STATUS_W1TC
        assert_eq!(p.refresh_lines().0 & 1 << INUM_GPIO, 0);
        p.gpio_set_input(12, Some(false));
        assert_eq!(p.read32(GPIO + 0x1C), 0, "a falling edge is not a rising one");
        // INT_TYPE 4 (low level) stays asserted while the level holds
        p.write32(GPIO + 0x28 + 4 * 12, 4 << 7);
        assert_eq!(p.read32(GPIO + 0x1C), 1 << 12);
        p.write32(GPIO + 0x24, 1 << 12);
        assert_eq!(p.read32(GPIO + 0x1C), 1 << 12, "re-asserted at once");
        p.write32(GPIO + 0x20, 1 << 3);                       // STATUS_W1TS
        assert_eq!(p.read32(GPIO + 0x1C), 1 << 12 | 1 << 3);
    }

    #[test]
    fn iomux_pullup_bit7_levels_a_floating_input_high() {
        let mut p = Peripherals::new([0; 6]);
        let level = |p: &mut Peripherals, pin: u32| p.read32(GPIO + 0x18) >> pin & 1;
        // GPIO4's IO_MUX register is 0x83C (esp8266_peri.h:99)
        p.gpio_set_input(4, Some(false));
        p.gpio_set_input(4, None);
        assert_eq!(level(&mut p, 4), 0, "floating without a pull: the last level");
        p.write32(IO_MUX + 0x3C, 1 << 7);
        assert_eq!(level(&mut p, 4), 1);
        p.gpio_set_input(4, Some(false));
        assert_eq!(level(&mut p, 4), 0, "a driven input wins over the pull-up");
        p.write32(IO_MUX + 0x3C, 0);
        p.gpio_set_input(4, None);
        assert_eq!(level(&mut p, 4), 0);
        // the ROM's pulls at reset: GPIO0 and GPIO2 up, GPIO15 none (floating reads low here)
        let mut q = Peripherals::new([0; 6]);
        assert_eq!(q.strap(), (false, true, true));
        q.gpio_set_input(0, Some(false));
        assert_eq!(q.strap(), (false, false, true), "GPIO0 low: UART download");
        q.gpio_set_input(15, Some(true));
        assert_eq!(q.strap(), (true, false, true));
    }

    #[test]
    fn gpio16_output_through_rtc_registers() {
        let mut p = Peripherals::new([0; 6]);
        p.gpio.observe = true;
        p.write32(RTC + 0xA0, 1);                             // GP16F: function 1, GPIO
        p.write32(RTC + 0x90, 0);                             // GP16C
        p.write32(RTC + 0x74, 1);                             // GP16E
        p.write32(RTC + 0x68, 1);                             // GP16O
        assert_eq!(p.gpio_output() >> 16, 1);
        assert_eq!(p.read32(RTC + 0x8C) & 1, 1);              // GP16I
        p.write32(RTC + 0x68, 0);
        assert_eq!((p.gpio_output() >> 16, p.read32(RTC + 0x8C) & 1), (0, 0));
        assert_eq!(p.gpio.events.iter().map(|e| (e.1, e.2)).collect::<Vec<_>>(), [(16, true), (16, false)]);
        // an input: the host's level, or with GP16F bit 3 (pull-down) low when floating
        p.write32(RTC + 0x74, 0);
        p.gpio_set_input(16, Some(true));
        assert_eq!(p.read32(RTC + 0x8C) & 1, 1);
        p.gpio_set_input(16, None);
        assert_eq!(p.read32(RTC + 0x8C) & 1, 1, "floating: the last level");
        p.write32(RTC + 0xA0, 1 | 1 << 3);
        assert_eq!(p.read32(RTC + 0x8C) & 1, 0);
        assert_eq!(p.read32(GPIO + 0x18) >> 16, 3, "GPIO_IN bits 16.. are the strapping, not GPIO16");
    }

    #[test]
    fn hspi_usr_builds_a_transfer_from_w0_and_user1_bits() {
        let mut p = Peripherals::new([0; 6]);
        // as SPI.begin() and SPI.transfer(0x55) leave them (libraries/SPI/SPI.cpp:84-85,306-313)
        p.write32(SPI1 + 0x1C, 1 << 27 | 1 << 6 | 1);         // USER: MOSI, slave edge, DOUTDIN
        p.write32(SPI1 + 0x20, 7 << 17 | 7 << 8);             // USER1: 8 MOSI bits, 8 MISO bits
        p.write32(SPI1 + 0x40, 0x55);                         // W0
        p.write32(SPI1, 1 << 18);                             // CMD: USR
        assert_ne!(p.read32(SPI1) & 1 << 18, 0, "busy until the transfer is done");
        let t = p.spi1.take_transfer().unwrap();
        assert_eq!((t.tx.as_slice(), t.rx_len), (&[0x55][..], 1), "full duplex: MISO during MOSI");
        p.spi1.finish_transfer(t, &[0xa5]);
        assert_eq!((p.read32(SPI1) & 1 << 18, p.read32(SPI1 + 0x40) & 0xff), (0, 0xa5));
        // 16 bits LSB first (CTRL WR/RD_BIT_ORDER, SPI.cpp:186-188), MOSI only
        p.write32(SPI1 + 0x08, 1 << 26 | 1 << 25);
        p.write32(SPI1 + 0x1C, 1 << 27);
        p.write32(SPI1 + 0x20, 15 << 17);
        p.write32(SPI1 + 0x40, 0x0201);
        p.write32(SPI1, 1 << 18);
        let t = p.spi1.take_transfer().unwrap();
        assert_eq!((t.tx.as_slice(), t.rx_len), (&[0x80, 0x40][..], 0));
        // a MISO-only read of 3 bytes after a 1-byte command phase (USER2 bits 28-31 length, 0-15 value)
        p.write32(SPI1 + 0x08, 0);
        p.write32(SPI1 + 0x1C, 1 << 31 | 1 << 28);
        p.write32(SPI1 + 0x20, 23 << 8);
        p.write32(SPI1 + 0x24, 7 << 28 | 0x9f);
        p.write32(SPI1, 1 << 18);
        let t = p.spi1.take_transfer().unwrap();
        assert_eq!((t.tx.as_slice(), t.rx_len), (&[0x9f][..], 3));
        p.spi1.finish_transfer(t, &[0xef, 0x40, 0x16]);
        assert_eq!(p.read32(SPI1 + 0x40), 0x0016_40ef);
    }

    /// The OTP words decode, the way libmain's MAC reader does (Blink image 0x40105434..0x401054cd),
    /// to the machine's MAC: CHIPID bit 12 selects the OUI from the fourth word, checked by the
    /// SDK's `esp_crc8` (0x40106a28) in CHIPID bits 31..24.
    #[test]
    fn otp_words_hold_the_whole_mac() {
        assert_eq!(crc8(b"123456789"), 0xa1, "CRC-8/MAXIM check value");
        let mac = [0x02, 0x12, 0x34, 0x56, 0x78, 0x9a];
        let [mac0, mac1, chipid, mac3] = otp_words(mac);
        assert_ne!(chipid & 1 << 15, 0);
        let mut m = [0u8; 6];
        m[5] = (mac0 >> 24) as u8;
        m[4] = mac1 as u8;
        m[3] = (mac1 >> 8) as u8;
        assert_ne!(chipid & 1 << 12, 0);
        m[2] = mac3 as u8;
        m[1] = (mac3 >> 8) as u8;
        m[0] = (mac3 >> 16) as u8;
        assert_eq!(m, mac);
        assert_eq!(crc8(&[m[2], m[1], m[0]]), (chipid >> 24) as u8);
        // ESP.getChipId(): the MAC's last three bytes (libmain `system_get_chip_id`, 0x40235b5c)
        assert_eq!((mac1 & 0xffff) << 8 | mac0 >> 24, 0x56_789a);
    }
}
