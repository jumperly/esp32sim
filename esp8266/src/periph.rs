//! ESP8266 peripherals. Register bases: Arduino ESP8266 3.1.2 tools/sdk/include/eagle_soc.h and
//! cores/esp8266/esp8266_peri.h; the UART map: tools/sdk/include/uart_register.h.
//!
//! Modelled so far: UART0 and UART1 (the shared esp-periph UART; transmit goes to the host
//! console) and DPORT's CPU2X bit. Every other register in the peripheral page, DPORT, WDEV and
//! the RF block is register memory: it reads what was last written, 0 after a reset.
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

pub struct Peripherals {
    pub uart: [Uart; 2],
    /// every register without a model, by word address
    pub regs: HashMap<u32, u32>,
    /// DPORT 0x50..0x60, read-only (`otp_words`)
    pub otp: [u32; 4],
    /// register words already reported by `--log-periph`
    seen: HashSet<u32>,
    pub misc: Misc,
}

impl Peripherals {
    pub fn new(mac: [u8; 6]) -> Self {
        Peripherals { uart: [Uart::new(UART_LAYOUT), Uart::new(UART_LAYOUT)], regs: HashMap::new(), otp: otp_words(mac), seen: HashSet::new(), misc: Misc::new() }
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

    pub fn read32(&mut self, addr: u32) -> u32 {
        let v = match Self::uart_at(addr) {
            Some((n, off)) => self.uart[n].read(off),
            None if (DPORT_OTP..DPORT_OTP + 16).contains(&addr) => self.otp[((addr - DPORT_OTP) / 4) as usize],
            None => { self.note(addr, false, 0); self.regs.get(&addr).copied().unwrap_or(0) }
        };
        let m = &mut self.misc;
        if m.log_all { eprintln!("[rd] {addr:#010x} -> {v:#010x} pc={:#010x}", m.cur_pc); }
        if let Some(l) = &mut m.mmio_log { l.push((m.cur_pc, addr, v, false)); }
        v
    }

    /// A register's value as a narrower store merges into it: no log line, no `mmio_log` entry and
    /// no `--log-periph` note, since the guest made no read. (A UART FIFO is not read here: the
    /// bus takes a narrow store to it as stored.)
    pub fn peek32(&mut self, addr: u32) -> u32 {
        match Self::uart_at(addr) {
            Some((n, off)) => self.uart[n].read(off),
            None if (DPORT_OTP..DPORT_OTP + 16).contains(&addr) => self.otp[((addr - DPORT_OTP) / 4) as usize],
            None => self.regs.get(&addr).copied().unwrap_or(0),
        }
    }

    pub fn write32(&mut self, addr: u32, v: u32) {
        let m = &mut self.misc;
        if m.log_all { eprintln!("[wr] {addr:#010x} <- {v:#010x} pc={:#010x}", m.cur_pc); }
        if let Some(l) = &mut m.mmio_log { l.push((m.cur_pc, addr, v, true)); }
        match Self::uart_at(addr) {
            Some((n, off)) => self.uart[n].write(off, v),
            None if (DPORT_OTP..DPORT_OTP + 16).contains(&addr) => {}
            None => { self.note(addr, true, v); self.regs.insert(addr, v); }
        }
    }

    /// The first access to a register without a model, under `--log-periph`.
    fn note(&mut self, addr: u32, write: bool, v: u32) {
        if self.misc.log_unknown && self.seen.insert(addr) {
            eprintln!("[periph] {} {addr:#010x}{} pc={:#010x}", if write { "W" } else { "R" }, if write { format!(" = {v:#x}") } else { String::new() }, self.misc.cur_pc);
        }
    }

    /// DPORT CPU2X bit 0: the CPU clock is doubled to 160 MHz.
    pub fn cpu2x(&self) -> bool { self.regs.get(&DPORT_CPU2X).is_some_and(|v| v & 1 != 0) }
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
    fn unmodelled_registers_are_memory_and_cpu2x_is_dport_0x14() {
        let mut p = Peripherals::new([0; 6]);
        assert!(!p.cpu2x());
        p.write32(DPORT_BASE + 0x14, 1);
        assert!(p.cpu2x());
        p.write32(0x6000_0300, 0x55);
        p.write32(WDEV_BASE + 0xE44, 7);
        assert_eq!((p.read32(0x6000_0300), p.read32(WDEV_BASE + 0xE44), p.read32(RF_LOW + 0x100)), (0x55, 7, 0));
        // the OTP words are fuses
        p.write32(DPORT_BASE + 0x58, 0);
        assert_eq!(p.read32(DPORT_BASE + 0x58), 1 << 15);
        assert!(Peripherals::contains(RF_HIGH - 4) && !Peripherals::contains(RF_HIGH) && !Peripherals::contains(RTC_MEM_LOW));
    }
}
