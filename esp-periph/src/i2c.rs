//! I2C master controller (I2C0/I2C1) and the `I2cDevice` trait the board's bus devices implement.
//! The controller executes the command list (RSTART/WRITE/READ/STOP/END) written by the driver
//! at `trans_start`, moving bytes between the FIFOs and the addressed device, and raises the
//! NACK / END_DETECT / TRANS_COMPLETE interrupts the IDF `i2c_master` driver waits for.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use std::collections::VecDeque;

pub trait I2cDevice {
    /// Optional physical (SDA, SCL) attachment. None keeps controller-only addressing.
    fn pins(&self) -> Option<(u8, u8)> { None }
    /// Address phase: the master addressed this device for a read (`read`) or a write. Return ACK.
    fn start(&mut self, _read: bool) -> bool { true }
    /// One data byte from the master. Return ACK.
    fn write(&mut self, b: u8) -> bool;
    /// One data byte to the master.
    fn read(&mut self) -> u8;
    fn stop(&mut self) {}
}

/// Jumperly patch J6: the target of a transaction for an address no attached device answers.
pub trait I2cHost {
    fn start(&mut self, addr: u8, read: bool) -> bool;
    fn write(&mut self, b: u8) -> bool;
    fn read(&mut self) -> u8;
    fn stop(&mut self);
}

pub const INT_END_DETECT: u32 = 1 << 3;
pub const INT_TRANS_COMPLETE: u32 = 1 << 7;
pub const INT_NACK: u32 = 1 << 10;

pub struct I2c {
    pub regs: RegRam,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    pub int_raw: u32,
    pub int_ena: u32,
    cmd: [u32; 8],
    devices: Vec<(u8, Box<dyn I2cDevice>)>,
    cur: Option<usize>,
    pins: Option<(u8, u8)>,
    expect_addr: bool,
    nack: bool,
    pub log: bool,
    pub transactions: u64,
    /// Jumperly patch J6: with `defer`, TRANS_START only sets `deferred`; `service` runs the
    /// transaction later with a host for unknown addresses (`host_cur`: the host owns this one).
    pub defer: bool,
    pub deferred: bool,
    host_cur: bool,
}

impl I2c {
    pub fn new() -> Self {
        I2c { regs: RegRam::new(), tx: VecDeque::new(), rx: VecDeque::new(), int_raw: 0, int_ena: 0, cmd: [0; 8], devices: Vec::new(), cur: None, pins: None, expect_addr: false, nack: false,
              log: false, transactions: 0, defer: false, deferred: false, host_cur: false }
    }
    /// A device attached at an occupied address and pin pair replaces the one there: a board swapped before
    /// boot (`esp32sim_set_measured_te`) must not leave the old board's devices answering.
    pub fn attach(&mut self, addr: u8, dev: Box<dyn I2cDevice>) {
        match self.devices.iter_mut().find(|(attached, old)| *attached == addr && old.pins() == dev.pins()) {
            Some(slot) => slot.1 = dev,
            None => self.devices.push((addr, dev)),
        }
    }
    pub fn has_pinned_devices(&self) -> bool { self.devices.iter().any(|(_, d)| d.pins().is_some()) }
    /// Current controller route, supplied by the SoC before starting a command list.
    pub fn set_pins(&mut self, pins: Option<(u8, u8)>) {
        if self.pins != pins { self.cur = None; }
        self.pins = pins;
    }
    /// Remove the device at `addr` and return it. If it was selected mid-transfer it gets no
    /// `stop()`; ack-checked writes then NACK and reads return 0xff until the next address phase.
    pub fn detach(&mut self, addr: u8) -> Option<Box<dyn I2cDevice>> {
        let index = self.devices.iter().position(|(attached, _)| *attached == addr)?;
        self.cur = self.cur.and_then(|cur| if cur == index { None } else { Some(cur - usize::from(cur > index)) });
        Some(self.devices.remove(index).1)
    }
    /// Remove every device without resetting registers, FIFOs or interrupt status.
    pub fn clear_devices(&mut self) {
        self.cur = None;
        self.devices.clear();
    }
    pub fn has_device(&self, addr: u8) -> bool { self.devices.iter().any(|(attached, _)| *attached == addr) }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x08 => (self.nack as u32) | ((self.rx.len() as u32 & 0x3f) << 8) | ((self.tx.len() as u32 & 0x3f) << 18),   // SR: resp_rec, rxfifo_cnt, txfifo_cnt
            0x14 => ((self.rx.len() as u32 & 0x1f) << 5) | ((self.tx.len() as u32 & 0x1f) << 15),                        // FIFO_ST: waddr = count, raddr = 0
            0x1c => self.rx.pop_front().unwrap_or(0) as u32,
            0x20 => self.int_raw,
            0x28 => self.int_ena,
            0x2c => self.int_raw & self.int_ena,
            0x58..=0x74 => self.cmd[((off - 0x58) / 4) as usize],
            _ => self.regs.read(off),
        }
    }

    /// FIFO reset bits are independent and may both be set by one write.
    #[allow(clippy::possible_missing_else)]
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x04 => { self.regs.write(off, v & !(1 << 5)); if v & (1 << 5) != 0 { if self.defer { self.deferred = true; } else { self.run(None); } } }   // CTR.TRANS_START (Jumperly patch J6)
            0x18 => { if v & (1 << 13) != 0 { self.tx.clear(); } if v & (1 << 12) != 0 { self.rx.clear(); } self.regs.write(off, v & !(3 << 12)); }
            0x1c => { if self.tx.len() < 32 { self.tx.push_back(v as u8); } }
            0x24 => self.int_raw &= !v,
            0x28 => self.int_ena = v,
            0x58..=0x74 => self.cmd[((off - 0x58) / 4) as usize] = v & !(1 << 31),
            _ => self.regs.write(off, v),
        }
    }

    /// Jumperly patch J6: turn deferral on or off. Turning it off runs a transaction still deferred
    /// without a host (unknown addresses NACK), so it never stays pending; true if one ran.
    pub fn set_defer(&mut self, on: bool) -> bool {
        self.defer = on;
        if on || !self.deferred { return false; }
        self.deferred = false;
        self.run(None);
        true
    }

    /// Jumperly patch J6: run a transaction deferred at TRANS_START, with `host` for unknown addresses.
    pub fn service(&mut self, host: &mut dyn I2cHost) -> bool {
        if !self.deferred { return false; }
        self.deferred = false;
        self.run(Some(host));
        true
    }

    /// Jumperly patch J6: `host` answers addresses no attached device has; `None` is the upstream path.
    fn run(&mut self, mut host: Option<&mut dyn I2cHost>) {
        self.nack = false;
        self.transactions += 1;
        for i in 0..8 {
            let c = self.cmd[i];
            let op = (c >> 11) & 7;
            let n = (c & 0xff) as usize;
            let ack_check = c & (1 << 8) != 0;
            match op {
                6 => self.expect_addr = true,                                            // RSTART
                1 => {                                                                   // WRITE n bytes
                    for _ in 0..n {
                        let b = self.tx.pop_front().unwrap_or(0);
                        let ack = if self.expect_addr {
                            self.expect_addr = false;
                            let addr = b >> 1; let rd = b & 1 != 0;
                            self.cur = self.devices.iter().position(|(a, d)| *a == addr && (d.pins().is_none() || d.pins() == self.pins));
                            self.host_cur = self.cur.is_none() && host.is_some();
                            if self.log { eprintln!("[i2c] start addr {:#04x} {}{}", addr, if rd { "R" } else { "W" }, if self.cur.is_none() { " (no device)" } else { "" }); }
                            match (self.cur, host.as_deref_mut()) { (Some(k), _) => self.devices[k].1.start(rd), (None, Some(h)) => h.start(addr, rd), (None, None) => false }
                        } else {
                            if self.log { eprintln!("[i2c]   write {:#04x}", b); }
                            match (self.cur, host.as_deref_mut()) { (Some(k), _) => self.devices[k].1.write(b), (None, Some(h)) if self.host_cur => h.write(b), _ => false }
                        };
                        if !ack && ack_check {
                            self.nack = true; self.int_raw |= INT_NACK; self.cmd[i] |= 1 << 31;
                            self.cur = None; self.host_cur = false;
                            return;
                        }
                    }
                }
                3 => {                                                                   // READ n bytes
                    for _ in 0..n {
                        let b = match (self.cur, host.as_deref_mut()) { (Some(k), _) => self.devices[k].1.read(), (None, Some(h)) if self.host_cur => h.read(), _ => 0xff };
                        if self.log { eprintln!("[i2c]   read  {:#04x}", b); }
                        if self.rx.len() < 32 { self.rx.push_back(b); }
                    }
                }
                2 => {                                                                   // STOP
                    match (self.cur, host.as_deref_mut()) { (Some(k), _) => self.devices[k].1.stop(), (None, Some(h)) if self.host_cur => h.stop(), _ => {} }
                    self.cur = None; self.host_cur = false; self.cmd[i] |= 1 << 31; self.int_raw |= INT_TRANS_COMPLETE;
                    return;
                }
                4 => { self.cmd[i] |= 1 << 31; self.int_raw |= INT_END_DETECT; return; } // END: driver continues later
                _ => { self.cmd[i] |= 1 << 31; return; }
            }
            self.cmd[i] |= 1 << 31;
        }
    }
}

impl Default for I2c { fn default() -> Self { Self::new() } }

// ------------------------------------------------------------------ devices

/// Generic 8-bit-register device (audio codecs etc.): first written byte selects the register,
/// following bytes / reads auto-increment.
pub struct Reg8Device { pub name: &'static str, pub regs: [u8; 256], ptr: u8, first: bool }
impl Reg8Device {
    pub fn new(name: &'static str, defaults: &[(u8, u8)]) -> Self { let mut d = Reg8Device { name, regs: [0; 256], ptr: 0, first: true }; for &(r, v) in defaults { d.regs[r as usize] = v; } d }
}
impl I2cDevice for Reg8Device {
    fn start(&mut self, read: bool) -> bool { if !read { self.first = true; } true }
    fn write(&mut self, b: u8) -> bool { if self.first { self.ptr = b; self.first = false; } else { self.regs[self.ptr as usize] = b; self.ptr = self.ptr.wrapping_add(1); } true }
    fn read(&mut self) -> u8 { let v = self.regs[self.ptr as usize]; self.ptr = self.ptr.wrapping_add(1); v }
}

/// Waveshare's CH32V003 IO expander: regs 0x02 direction, 0x03 output, 0x04 input, 0x05 PWM, 0x06 ADC, 0x07 RTC.
impl Device for I2c {
    fn read(&mut self, off: u32) -> u32 { I2c::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2c::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn debug(&mut self, on: bool) { self.log = on; }
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[derive(Default)]
    struct Mem { log: Vec<String>, reg: u8, data: [u8; 4], first: bool }
    impl I2cHost for Mem {
        fn start(&mut self, addr: u8, read: bool) -> bool { self.log.push(format!("S{addr:02x}{}", if read { 'R' } else { 'W' })); self.first = !read; addr == 0x50 }
        fn write(&mut self, b: u8) -> bool { if self.first { self.reg = b; self.first = false; } else { self.data[self.reg as usize & 3] = b; self.reg += 1; } true }
        fn read(&mut self) -> u8 { let b = self.data[self.reg as usize & 3]; self.reg += 1; b }
        fn stop(&mut self) { self.log.push("P".into()); }
    }
    const RSTART: u32 = 6 << 11; const WRITE: u32 = 1 << 11; const READ: u32 = 3 << 11; const STOP: u32 = 2 << 11; const ACK_CHECK: u32 = 1 << 8;

    fn program(i: &mut I2c, cmds: &[u32], tx: &[u8]) {
        for (k, c) in cmds.iter().enumerate() { i.write(0x58 + 4 * k as u32, *c); }
        for b in tx { i.write(0x1c, u32::from(*b)); }
        i.write(0x04, 1 << 5);                                           // TRANS_START
    }

    #[test]
    fn deferred_transaction_runs_against_the_host_on_service() {
        let mut i = I2c::new();
        i.defer = true;
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 3, STOP], &[0x50 << 1, 0x01, 0x42]);
        assert!(i.deferred);
        assert_eq!(i.int_raw & INT_TRANS_COMPLETE, 0);                   // not run yet
        let mut m = Mem::default();
        assert!(i.service(&mut m));
        assert!(!i.deferred);
        assert_eq!(i.int_raw & INT_TRANS_COMPLETE, INT_TRANS_COMPLETE);
        assert_eq!(m.data[1], 0x42);
        assert_eq!(m.log, ["S50W", "P"]);
        // register 1 again, repeated START, read one byte back
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 2, RSTART, WRITE | ACK_CHECK | 1, READ | 1, STOP], &[0x50 << 1, 0x01, (0x50 << 1) | 1]);
        assert!(i.service(&mut m));
        assert_eq!(i.read(0x1c), 0x42);
        assert_eq!(m.log, ["S50W", "P", "S50W", "S50R", "P"]);
        assert!(!i.service(&mut m));                                     // nothing pending
    }

    #[test]
    fn a_host_nack_aborts_with_int_nack() {
        let mut i = I2c::new();
        i.defer = true;
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 1, STOP], &[0x3c << 1]);
        let mut m = Mem::default();
        i.service(&mut m);
        assert_ne!(i.int_raw & INT_NACK, 0);
        assert_eq!(m.log, ["S3cW"]);
    }

    /// Records every host call: start, written bytes, stop.
    #[derive(Default)]
    struct Recorder { log: Vec<String> }
    impl I2cHost for Recorder {
        fn start(&mut self, addr: u8, read: bool) -> bool { self.log.push(format!("S{addr:02x}{}", if read { 'R' } else { 'W' })); true }
        fn write(&mut self, b: u8) -> bool { self.log.push(format!("W{b:02x}")); true }
        fn read(&mut self) -> u8 { 0xff }
        fn stop(&mut self) { self.log.push("P".into()); }
    }
    const END: u32 = 4 << 11;

    #[test]
    fn an_end_split_write_stays_one_host_transaction() {
        // > 32-byte writes (Adafruit_SSD1306 display()): chunk 1 ends with END, chunk 2 continues and STOPs
        let (n, m) = (31usize, 20usize);
        let mut i = I2c::new();
        i.defer = true;
        let mut h = Recorder::default();
        let first: Vec<u8> = std::iter::once(0x3c << 1).chain(1..n as u8).collect();
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | n as u32, END], &first);
        assert!(i.service(&mut h));
        assert_eq!(i.int_raw & INT_END_DETECT, INT_END_DETECT);
        assert_eq!(i.int_raw & INT_TRANS_COMPLETE, 0);
        assert!(!h.log.iter().any(|e| e == "P"));                        // no stop between the chunks
        i.write(0x24, INT_END_DETECT);                                   // INT_CLR, as the driver does
        let second: Vec<u8> = (0..m as u8).map(|k| 0x80 + k).collect();
        program(&mut i, &[WRITE | ACK_CHECK | m as u32, STOP], &second);
        assert!(i.service(&mut h));
        assert_eq!(i.int_raw & INT_TRANS_COMPLETE, INT_TRANS_COMPLETE);
        assert_eq!(i.int_raw & INT_NACK, 0);
        let mut expected = vec!["S3cW".to_string()];
        expected.extend(first[1..].iter().chain(&second).map(|b| format!("W{b:02x}")));
        expected.push("P".into());
        assert_eq!(expected.len(), 1 + (n - 1) + m + 1);
        assert_eq!(h.log, expected);
    }

    #[test]
    fn turning_defer_off_runs_a_deferred_transaction_without_a_host() {
        let mut i = I2c::new();
        i.set_defer(true);
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 1, STOP], &[0x50 << 1]);
        assert!(i.deferred);
        assert!(i.set_defer(false));                                     // it ran
        assert!(!i.defer && !i.deferred);
        assert_ne!(i.int_raw & INT_NACK, 0);                             // no device, no host: NACK
        assert!(!i.set_defer(false));                                    // nothing left to run
        assert!(!i.service(&mut Mem::default()));
    }

    #[test]
    fn without_defer_the_upstream_path_is_unchanged() {
        let mut i = I2c::new();
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 1, STOP], &[0x50 << 1]);
        assert!(!i.deferred);
        assert_ne!(i.int_raw & INT_NACK, 0);                             // no attached device, no host: NACK at once
    }
}
