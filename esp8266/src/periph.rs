//! ESP8266 peripherals. Register bases: Arduino ESP8266 3.1.2 tools/sdk/include/eagle_soc.h and
//! cores/esp8266/esp8266_peri.h; the UART map: tools/sdk/include/uart_register.h.
//!
//! Modelled: UART0 and UART1 (the shared esp-periph UART; transmit goes to the host console,
//! receive raises interrupt 5), the two timers FRC1 (interrupt 9, or the NMI) and FRC2 (interrupt
//! 10), GPIO output/enable/status with the strapping bits of GPIO_IN, the RTC block's wake-up
//! status and counter, DPORT (edge-interrupt enable, NMI routing, CPU2X, the OTP words) and WDEV
//! (the microsecond counter, the TSF and the random number register). Every other register in the
//! peripheral page, DPORT, WDEV and the RF block is register memory: it reads what was last
//! written, 0 after a reset. Device time is the bus's cycle count (`now`), CPU cycles at 80 MHz,
//! which is also the APB clock the timers count (CPU2X is recorded, not modelled).
use esp_periph::uart::{Uart, UartLayout};
use esp_periph::Misc;
use std::collections::{HashMap, HashSet};

/// The CPU clock with CPU2X clear, and the APB clock the UART divisors count (esp8266_peri.h:32
/// ESP8266_CLOCK).
pub const CPU_HZ: u64 = 80_000_000;
/// esp8266_peri.h:30 ESP8266_REG: UART0 0x000, SPI1 0x100, SPI0 0x200, GPIO 0x300, timers 0x600,
/// RTC 0x700, IO_MUX 0x800, WDT 0x900, SLC 0xB00, I2S 0xE00, UART1 0xF00.
pub const PERIPH_BASE: u32 = 0x6000_0000;
pub const PERIPH_END: u32 = 0x6000_1000;
/// RTC memory (eagle_soc.h RTC_USER_MEM 0x60001200 is inside it); survives a chip reset.
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
/// FRC1's edge interrupt reaches the CPU.
pub const DPORT_TEIE: u32 = DPORT_BASE + 0x04;
const TEIE1: u32 = 1 << 1;
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
pub const INUM_FRC1: u32 = 9;
pub const INUM_FRC2: u32 = 10;
pub const INUM_NMI: u32 = 14;

/// The factory MAC as the OTP words hold it, in the form the SDK decodes (the MAC reader that
/// libmain's `call_user_start_local` calls, read from the Blink image's disassembly at
/// 0x40105430..0x401054cd): CHIPID bit 15 must be set or the SDK prints an error and stops;
/// with CHIPID bit 12 clear the address is 18:fe:34 (the SDK's fixed prefix), then MAC1 bits
/// 15..8, MAC1 bits 7..0, MAC0 bits 31..24. (Bit 12 set takes the first three bytes from the
/// fourth word, checked by a CRC-8 in CHIPID bits 31..24, which this model does not write.)
/// So only the last three bytes of `mac` reach the guest.
pub fn otp_words(mac: [u8; 6]) -> [u32; 4] {
    [u32::from(mac[5]) << 24, u32::from(mac[3]) << 8 | u32::from(mac[4]), 1 << 15, 0]
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
#[derive(Default)]
pub struct RegMem(HashMap<u32, u32>);
impl RegMem {
    pub fn read(&self, addr: u32) -> u32 { self.0.get(&addr).copied().unwrap_or(0) }
    pub fn write(&mut self, addr: u32, v: u32) { self.0.insert(addr, v); }
}

/// The FRC timers' prescaler (esp8266_peri.h:166 TCPD, 2 bits at 2): 0 = /1, 1 = /16, 2 and 3 =
/// /256 of the 80 MHz APB clock: CPU cycles per count.
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

/// GPIO (esp8266_peri.h:48-87, 0x300..0x370): output and enable with their W1TS/W1TC aliases,
/// the interrupt status, the per-pin configuration words. GPIO_IN (0x318) reads the driven level
/// of an output pin, the host's level of an input pin, and the strapping in bits 16 and up, as
/// the boot mode number GPIO15 << 2 | GPIO0 << 1 | GPIO2 (cores/esp8266/Updater.cpp:84 reads
/// `(GPI >> 16) & 0xf`; digitake esp8266-emulator src/soc.c:216 returns 3, a flash boot).
#[derive(Default)]
pub struct Gpio {
    pub out: u32,
    pub enable: u32,
    pub status: u32,
    pub pin: [u32; 16],
    /// host-driven input levels, by pin
    pub host_in: u32,
    /// the boot mode the strapping pins select (`rom::boot::STRAP_*`)
    pub strap: u32,
    /// record output changes for `SocBus::take_gpio_events`
    pub observe: bool,
    pub events: Vec<(u64, u8, bool)>,
    rtc_calib_sync: u32,
}

/// GPIO_RTC_CALIB_SYNC 0x36C and GPIO_RTC_CALIB_VALUE 0x370: libmain's `pm_rtc_clock_cali`
/// (Blink image 0x4010107c) starts a calibration through 0x36C and polls 0x370 until bit 31
/// (ready) is set, then takes bits 0..19. Reads ready at once with digitake esp8266-emulator's
/// value (src/soc.c:218: `(1u << 31) | 0x4E20`).
const RTC_CALIB_VALUE: u32 = (1 << 31) | 0x4E20;

impl Gpio {
    pub fn input(&self) -> u32 { (self.out & self.enable) | (self.host_in & !self.enable & 0xffff) | (self.strap << 16) }
    fn set_out(&mut self, v: u32, now: u64) {
        let v = v & 0xffff;
        let changed = v ^ self.out;
        if self.observe && changed != 0 {
            for p in 0..16 { if changed & (1 << p) != 0 { self.events.push((now, p as u8, v & (1 << p) != 0)); } }
        }
        self.out = v;
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
        match off {
            0x00 => self.set_out(v, now),
            0x04 => self.set_out(self.out | v, now),
            0x08 => self.set_out(self.out & !v, now),
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
    }
}

/// The RTC block (0x700..0x7FF; esp8266_peri.h:170-174). Register memory, except: 0x71C, the
/// RTC counter, counts at about 150 kHz (digitake esp8266-emulator src/soc.c:558), and 0x728
/// reads with bit 0 set: libmain's `pm_wait4wakeup` (Blink image 0x40224e3c) polls bits 1..0 of
/// it for the wake-up after the PHY start (soc.c:559 forces bit 0 the same way).
#[derive(Default)]
pub struct Rtc { pub regs: RegMem }
const RTC_COUNTER: u32 = 0x71C;
const RTC_WAKE: u32 = 0x728;
impl Rtc {
    fn read(&self, off: u32, now: u64) -> u32 {
        match off {
            RTC_COUNTER => (now_us(now) * 150 / 1000) as u32,
            RTC_WAKE => self.regs.read(off) | 1,
            _ => self.regs.read(off),
        }
    }
}

/// Microseconds of device time at `cycles` (80 MHz).
pub fn now_us(cycles: u64) -> u64 { cycles / (CPU_HZ / 1_000_000) }

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
    /// WDT 0x900..0x9FF: register memory (no reset yet)
    pub wdt: RegMem,
    pub rtc: Rtc,
    pub gpio: Gpio,
    /// IO_MUX 0x800..0x8FF: register memory
    pub io_mux: RegMem,
    /// SPI0 (the flash controller) 0x200..0x2FF: register memory; the ROM's SPI functions do the work
    pub spi0: RegMem,
    /// SPI1 (HSPI) 0x100..0x1FF: register memory
    pub spi1: RegMem,
    pub dport: Dport,
    pub wdev: Wdev,
    /// the RF/PHY block: register memory
    pub rf: RegMem,
    /// the rest of the peripheral page (SLC, I2S, ...)
    pub other: RegMem,
    pub misc: Misc,
    /// device time in CPU cycles, as the bus last counted it
    pub now: u64,
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
        Peripherals {
            uart: [Uart::new(UART_LAYOUT), Uart::new(UART_LAYOUT)], frc1: Frc1::new(), frc2: Frc2::new(), wdt: RegMem::default(),
            rtc: Rtc::default(), gpio: Gpio { strap: crate::rom::boot::STRAP_FLASH, ..Default::default() }, io_mux: RegMem::default(),
            spi0: RegMem::default(), spi1: RegMem::default(), dport: Dport { regs: RegMem::default(), otp: otp_words(mac) },
            wdev: Wdev { regs: RegMem::default(), rng: seed }, rf: RegMem::default(), other: RegMem::default(), misc: Misc::new(),
            now: 0, pulses: 0, presented: 0, seen: HashSet::new(),
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
            0x6000_0100..0x6000_0200 => self.spi1.read(addr),
            0x6000_0200..0x6000_0300 => self.spi0.read(addr),
            0x6000_0300..0x6000_0400 => self.gpio.read(off - 0x300),
            0x6000_0600..0x6000_0620 => self.frc1.read(off - 0x600, now),
            0x6000_0620..0x6000_0640 => self.frc2.read(off - 0x600, now),
            0x6000_0700..0x6000_0800 => self.rtc.read(off, now),
            0x6000_0800..0x6000_0900 => self.io_mux.read(addr),
            0x6000_0900..0x6000_0A00 => self.wdt.read(addr),
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
            0x6000_0100..0x6000_0200 => self.spi1.write(addr, v),
            0x6000_0200..0x6000_0300 => self.spi0.write(addr, v),
            0x6000_0300..0x6000_0400 => self.gpio.write(off - 0x300, v, now),
            0x6000_0600..0x6000_0620 => self.frc1.write(off - 0x600, v, now),
            0x6000_0620..0x6000_0640 => self.frc2.write(off - 0x600, v, now),
            0x6000_0700..0x6000_0800 => self.rtc.regs.write(off, v),
            0x6000_0800..0x6000_0900 => self.io_mux.write(addr, v),
            0x6000_0900..0x6000_0A00 => self.wdt.write(addr, v),
            DPORT_OTP..=DPORT_OTP_LAST | UIS | WDEV_US | WDEV_TSF_LO | WDEV_TSF_HI | WDEV_RNG => {}
            _ => { self.note(addr, true, v); self.mem(addr).write(addr, v); }
        }
    }

    /// The first access to a register without a model, under `--log-periph`.
    fn note(&mut self, addr: u32, write: bool, v: u32) {
        if self.misc.log_unknown && self.seen.insert(addr) {
            eprintln!("[periph] {} {addr:#010x}{} pc={:#010x}", if write { "W" } else { "R" }, if write { format!(" = {v:#x}") } else { String::new() }, self.misc.cur_pc);
        }
    }

    /// DPORT CPU2X bit 0: the CPU clock is doubled to 160 MHz.
    pub fn cpu2x(&self) -> bool { self.dport.regs.read(DPORT_CPU2X) & 1 != 0 }

    /// The cycle of the next timer event (u64::MAX: none).
    pub fn next_event(&self) -> u64 { self.frc1.next.min(self.frc2.next) }

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
        true
    }

    /// The interrupt lines (bit n = line n, the NMI at 14): the level sources, and each edge
    /// raised since the last call high for this one presentation. An edge that would follow a
    /// presentation in which its line was already high waits one presentation, so the core sees a
    /// low before it. Returns the lines and whether an edge is still waiting.
    pub fn refresh_lines(&mut self) -> (u32, bool) {
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
        assert_eq!(p.read32(DPORT_BASE + 0x58), 1 << 15);
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
}
