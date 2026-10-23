//! I2C master controller (I2C0/I2C1) and the `I2cDevice` trait the board's bus devices implement.
//! The controller executes the command list (RSTART/WRITE/READ/STOP/END) written by the driver
//! after `trans_start`, clocking bytes between the FIFOs and addressed devices, and raises the
//! NACK / END_DETECT / TRANS_COMPLETE interrupts the IDF `i2c_master` driver waits for.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use std::collections::VecDeque;
use emu_core::ClockDomain;

pub mod m5;

pub trait I2cDevice {
    /// Current 7-bit address, sampled after each data write. Defaults to the attached address.
    fn address(&self, configured: u8) -> u8 { configured }
    /// Match the address and start the transfer; override for aliases or general calls.
    fn start_address(&mut self, configured: u8, address: u8, read: bool) -> bool {
        self.address(configured) == address && self.start(read)
    }
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
pub const INT_TIMEOUT: u32 = 1 << 8;
pub const INT_NACK: u32 = 1 << 10;

pub struct I2c<const N: usize = 8> {
    pub regs: RegRam,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    pub int_raw: u32,
    pub int_ena: u32,
    /// Boxed so the controller keeps main's size and field layout.
    program: Box<Program<N>>,
    devices: Vec<(u8, Box<dyn I2cDevice>)>,
    cur: Vec<usize>,
    /// Controller clock ticks until the current command step completes; `None` while idle.
    remaining: Option<u64>,
    pins: Option<(u8, u8)>,
    expect_addr: bool,
    nack: bool,
    pub log: bool,
    pub transactions: u64,
    command_index: u8,
    byte_index: u8,
    /// Jumperly patch J6: with `defer`, TRANS_START only sets `deferred`; `service` runs the
    /// transaction later with a host for unknown addresses (`host_cur`: the host owns this one).
    pub defer: bool,
    pub deferred: bool,
    host_cur: bool,
}

struct Program<const N: usize> {
    cmd: [u32; N],
    /// C6 supplies its PCR divider in the S3/C3 CLK_CONF layout.
    external_clock_config: Option<u32>,
}

impl I2c { pub fn new() -> Self { Self::with_slots() } }
/// IDF v5.5.4 esp32 i2c_reg.h: sixteen slots; i2c_ll.h: opcodes 0/1/2/3/4.
impl I2c<16> { pub fn new_classic() -> Self { Self::with_slots() } }
// Non-generic method bodies keep one modern implementation across S3/C3/C6.
macro_rules! i2c_impl { ($n:literal) => {
impl I2c<$n> {
    fn with_slots() -> Self {
        I2c { regs: RegRam::new(), tx: VecDeque::new(), rx: VecDeque::new(), int_raw: 0, int_ena: 0, program: Box::new(Program { cmd: [0; $n], external_clock_config: None }),
              devices: Vec::new(), cur: Vec::new(), remaining: None, pins: None, expect_addr: false, nack: false, log: false, transactions: 0,
              command_index: 0, byte_index: 0, defer: false, deferred: false, host_cur: false }
    }
    /// C6 supplies its PCR divider in the S3/C3 CLK_CONF layout.
    pub fn set_external_clock_config(&mut self, config: Option<u32>) { self.program.external_clock_config = config; }
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
        if self.pins != pins { self.cur.clear(); }
        self.pins = pins;
    }
    /// Remove a device, returning it so the host can move it to another bus or address.
    /// Other selected recipients remain active. With none left, writes NACK and reads return 0xff.
    pub fn detach(&mut self, addr: u8) -> Option<Box<dyn I2cDevice>> {
        let index = self.devices.iter().position(|(attached, _)| *attached == addr)?;
        self.cur.retain_mut(|cur| { if *cur == index { return false; } *cur -= usize::from(*cur > index); true });
        Some(self.devices.remove(index).1)
    }
    /// Remove every device without resetting registers, FIFOs or interrupt status.
    pub fn clear_devices(&mut self) {
        self.cur.clear();
        self.devices.clear();
    }
    pub fn has_device(&self, addr: u8) -> bool { self.devices.iter().any(|(attached, _)| *attached == addr) }
    #[inline(always)]
    pub fn is_active(&self) -> bool { self.remaining.is_some() }
    pub fn irq(&self) -> bool { self.int_raw & self.int_ena != 0 }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            // IDF v5.5.4 S3 i2c_reg.h:173-179: BUS_BUSY bit 4.
            0x08 => (self.nack as u32) | ((self.is_active() as u32) << 4) | (u32::from($n == 16 && self.int_raw & INT_TIMEOUT != 0) << 2) | ((self.rx.len() as u32 & 0x3f) << 8) | ((self.tx.len() as u32 & 0x3f) << 18),   // SR: resp_rec, bus_busy, rxfifo_cnt, txfifo_cnt
            0x14 => ((self.rx.len() as u32 & 0x1f) << 5) | ((self.tx.len() as u32 & 0x1f) << 15),                        // FIFO_ST: waddr = count, raddr = 0
            0x1c => self.rx.pop_front().unwrap_or(0) as u32,
            0x20 => self.int_raw,
            0x28 => self.int_ena,
            0x2c => self.int_raw & self.int_ena,
            0x58..=0x74 => self.program.cmd[((off - 0x58) / 4) as usize],
            0x78..=0x94 if $n == 16 => self.program.cmd[((off - 0x58) / 4) as usize],
            _ => self.regs.read(off),
        }
    }

    /// FIFO reset bits are independent and may both be set by one write.
    #[allow(clippy::possible_missing_else)]
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            // IDF v5.5.4 components/soc/esp32s3/register/soc/i2c_reg.h:
            // TRANS_START bit 5 (72-78), FSM_RST bit 10 (111-117).
            // Reset cancellation of a pending callback is inferred.
            0x04 => {
                // Classic IDF v5.5.4 components/soc/esp32/register/soc/i2c_reg.h:
                // CTR has no FSM_RST field. Preserve unmodeled bit 10 in register RAM;
                // this readback policy is inferred, not a silicon claim for a reserved bit.
                let reset_mask = if $n == 16 { 0 } else { 1 << 10 };
                let reset = v & reset_mask != 0;
                self.regs.write(off, v & !((1 << 5) | reset_mask));
                if reset { self.remaining = None; self.cur.clear(); self.expect_addr = false; self.nack = false; self.deferred = false; self.host_cur = false; }
                if v & (1 << 5) != 0 { if self.defer { self.deferred = true; } else { self.run(); } }   // Jumperly patch J6
            }               // CTR.TRANS_START
            0x18 => { if v & (1 << 13) != 0 { self.tx.clear(); } if v & (1 << 12) != 0 { self.rx.clear(); } self.regs.write(off, v & !(3 << 12)); }
            0x1c => { if self.tx.len() < 32 { self.tx.push_back(v as u8); } }
            0x24 => self.int_raw &= !v,
            0x28 => self.int_ena = v,
            0x58..=0x74 => self.program.cmd[((off - 0x58) / 4) as usize] = v & !(1 << 31),
            0x78..=0x94 if $n == 16 => self.program.cmd[((off - 0x58) / 4) as usize] = v & !(1 << 31),
            _ => self.regs.write(off, v),
        }
    }

    fn op(&self) -> u32 {
        { let op = (self.program.cmd[usize::from(self.command_index)] >> 11) & 7;
          if $n == 16 { match op { 0 => 6, 2 => 3, 3 => 2, op => op } } else { op } }
    }

    // IDF v5.5.4 components/hal/esp32s3/include/hal/i2c_ll.h:205-235:
    // divider = NUM + 1 + B/A (the register-header A/B prose is reversed).
    // components/soc/esp32s3/register/soc/i2c_reg.h:1043-1074; C3 has the same fields.
    // C6 uses PCR instead. clk_tree_defs.h:39 (C6:47) gives nominal 17.5 MHz
    // RC_FAST; XTAL is modeled at 40 MHz and APB at 80 MHz. IDF 4.4 uses a
    // nominal 20 MHz RC clock; this model does not reproduce oscillator calibration.
    fn clock_ticks(&self, cycles: u64) -> u64 {
        let c = self.program.external_clock_config.unwrap_or_else(|| self.regs.read(0x54));
        let a = u64::from((c >> 8) & 63);
        let b = u64::from((c >> 14) & 63);
        let denominator = a.max(1);
        let divisor = (u64::from(c & 255) + 1) * denominator + if a > 0 { b } else { 0 };
        let hz = if c & (1 << 20) != 0 { 17_500_000 } else { 40_000_000 };
        (cycles * divisor * 80_000_000).div_ceil(hz * denominator).max(1)
    }

    // Inferred byte/ACK timing, not measured on hardware. IDF v5.5.4
    // components/hal/{esp32s3,esp32c3,esp32c6}/include/hal/i2c_ll.h
    // master_set_bus_timing (S3:175-201, C6:173-200): low/setup/hold are
    // programmed minus one; high and wait-high are literal. IDF v4.4.7
    // S3:160-171 and C3:164-175 use the same convention.
    // S3 i2c_reg.h:16-26, 932-1006 gives 9-bit periods and 7-bit wait-high.
    fn schedule(&mut self) {
        if usize::from(self.command_index) >= $n { self.remaining = None; return; }
        let mask = 0x1ff;
        let cycles = match self.op() {
            1 | 3 if self.program.cmd[usize::from(self.command_index)] & 255 != 0 => {
                let high = self.regs.read(0x38);
                let extra = (high >> 9) & 127;
                ((self.regs.read(0) & mask) + 1 + (high & mask) + extra) * 9
            }
            op @ (6 | 2) => {
                let off = if op == 6 { 0x40 } else { 0x48 };
                (self.regs.read(off) & mask) + (self.regs.read(off + 4) & mask) + 2
            }
            _ => 0,
        };
        self.remaining = Some(self.clock_ticks(cycles.into()));
    }

    fn run(&mut self) {
        if self.is_active() { return; }
        self.begin();
        // Classic adapter retains its synchronous transaction model; modern controllers
        // retain #199's scheduled byte callbacks and clock selection.
        if $n == 16 { self.advance(u64::MAX); }
    }

    fn begin(&mut self) {
        self.nack = false;
        self.transactions += 1;
        self.command_index = 0;
        self.byte_index = 0;
        self.schedule();
    }

    /// Jumperly patch J6: turn deferral on or off. Turning it off runs a transaction still deferred
    /// without a host (unknown addresses NACK), so it never stays pending; true if one ran.
    pub fn set_defer(&mut self, on: bool) -> bool {
        self.defer = on;
        if on || !self.deferred { return false; }
        self.deferred = false;
        self.advance(u64::MAX);
        self.begin();
        self.advance(u64::MAX);
        true
    }

    /// Jumperly patch J6: run a transaction deferred at TRANS_START to completion, with `host` for
    /// addresses no attached device has; true if one ran. The host is reached between runs, so the
    /// bytes are not spread over the controller clock as a scheduled transaction's are.
    pub fn service(&mut self, host: &mut dyn I2cHost) -> bool {
        if !self.deferred { return false; }
        self.deferred = false;
        self.advance(u64::MAX);
        self.begin();
        self.advance_with(u64::MAX, Some(host));
        true
    }

    fn advance(&mut self, ticks: u64) { self.advance_with(ticks, None); }

    fn advance_with(&mut self, mut ticks: u64, mut host: Option<&mut dyn I2cHost>) {
        while let Some(remaining) = self.remaining {
            if ticks < remaining { self.remaining = Some(remaining - ticks); return; }
            ticks -= remaining;
            self.step(host.as_deref_mut());
            if self.is_active() { self.schedule(); }
        }
    }

    /// One command step; `host` (Jumperly patch J6) answers an address no attached device has.
    fn step(&mut self, mut host: Option<&mut (dyn I2cHost + '_)>) {
        let i = usize::from(self.command_index);
        let c = self.program.cmd[i];
        let n = c as u8;
        match self.op() {
            6 => self.expect_addr = true,
            1 if self.byte_index < n => {
                let b = self.tx.pop_front().unwrap_or(0);
                let ack = if self.expect_addr {
                    self.expect_addr = false;
                    let addr = b >> 1; let rd = b & 1 != 0;
                    self.cur.clear();
                    for (k, (configured, device)) in self.devices.iter_mut().enumerate() {
                        if (device.pins().is_none() || device.pins() == self.pins) && device.start_address(*configured, addr, rd) {
                            self.cur.push(k);
                        }
                    }
                    if self.log { eprintln!("[i2c] start addr {addr:#04x} read={rd}"); }
                    self.host_cur = false;
                    match host.as_deref_mut() {
                        Some(h) if self.cur.is_empty() => { self.host_cur = true; h.start(addr, rd) }
                        _ => !self.cur.is_empty(),
                    }
                } else {
                    if self.log { eprintln!("[i2c]   write {b:#04x}"); }
                    let mut ack = false;
                    for &k in &self.cur {
                        let (addr, device) = &mut self.devices[k];
                        ack |= device.write(b);
                        *addr = device.address(*addr);
                    }
                    if self.host_cur { if let Some(h) = host.as_deref_mut() { ack |= h.write(b); } }
                    ack
                };
                if !ack && c & (1 << 8) != 0 {
                    self.nack = true; self.int_raw |= INT_NACK; self.program.cmd[i] |= 1 << 31;
                    self.cur.clear(); self.host_cur = false; self.remaining = None;
                    return;
                }
                self.byte_index += 1;
                if self.byte_index < n { return; }
            }
            3 if self.byte_index < n => {
                let mut b = 0xff;
                for &k in &self.cur { b &= self.devices[k].1.read(); }
                if self.host_cur { if let Some(h) = host.as_deref_mut() { b &= h.read(); } }
                if self.log { eprintln!("[i2c]   read  {b:#04x}"); }
                if self.rx.len() < 32 { self.rx.push_back(b); }
                self.byte_index += 1;
                if self.byte_index < n { return; }
            }
            1 | 3 => {}
            2 => {
                for &k in &self.cur { self.devices[k].1.stop(); }
                if self.host_cur { if let Some(h) = host.as_deref_mut() { h.stop(); } }
                self.cur.clear(); self.host_cur = false; self.int_raw |= INT_TRANS_COMPLETE; self.remaining = None;
            }
            4 => { self.int_raw |= INT_END_DETECT; self.remaining = None; }
            _ => self.remaining = None,
        }
        self.program.cmd[i] |= 1 << 31;
        self.command_index += 1;
        self.byte_index = 0;
    }

}

impl Device for I2c<$n> {
    fn read(&mut self, off: u32) -> u32 { I2c::<$n>::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { I2c::<$n>::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
    fn clock(&self) -> Option<ClockDomain> { self.is_active().then_some(ClockDomain::Apb) }
    fn tick(&mut self, ticks: u64) { self.advance(ticks); }
    fn has_deadline(&self) -> bool { self.is_active() }
    fn next_deadline(&self) -> Option<u64> { self.remaining }
    fn debug(&mut self, on: bool) { self.log = on; }
}

} }
i2c_impl!(8);
i2c_impl!(16);

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


#[cfg(test)]
mod classic_tests {
    use super::*;
    #[test]
    fn classic_executes_sixteen_slots_and_old_stop_opcode() {
        let mut classic = I2c::new_classic();
        for i in 0..15 { classic.write(0x58 + 4 * i, 0); }
        classic.write(0x94, 3 << 11);
        classic.write(4, 1 << 5);
        assert_ne!(classic.read(0x94) & (1 << 31), 0);
        assert_ne!(classic.int_raw & INT_TRANS_COMPLETE, 0);
        let mut modern = I2c::new();
        modern.write(0x94, 3 << 11);
        modern.write(0x58, 2 << 11);
        modern.write(4, 1 << 5);
        modern.advance(u64::MAX);
        assert_eq!(modern.read(0x94), 3 << 11);
        assert_ne!(modern.int_raw & INT_TRANS_COMPLETE, 0);
    }
}

/// Jumperly patch J6: deferred transactions and the host for unknown addresses.
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
        assert!(!i.is_active(), "nothing scheduled: the host runs it");
        assert_eq!(i.int_raw & INT_TRANS_COMPLETE, 0);                   // not run yet
        let mut m = Mem::default();
        assert!(i.service(&mut m));
        assert!(!i.deferred && !i.is_active());
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

    #[test]
    fn an_attached_device_keeps_its_address_from_the_host() {
        let mut i = I2c::new();
        i.defer = true;
        i.attach(0x50, Box::new(Reg8Device::new("eeprom", &[(1, 0x99)])));
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 2, RSTART, WRITE | ACK_CHECK | 1, READ | 1, STOP], &[0x50 << 1, 0x01, (0x50 << 1) | 1]);
        let mut m = Mem::default();
        assert!(i.service(&mut m));
        assert_eq!(i.read(0x1c), 0x99, "the device answered");
        assert!(m.log.is_empty(), "the host never saw the address");
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
    fn an_fsm_reset_drops_a_deferred_transaction() {
        let mut i = I2c::new();
        i.defer = true;
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 1, STOP], &[0x50 << 1]);
        i.write(0x04, 1 << 10);
        assert!(!i.deferred);
        assert!(!i.service(&mut Mem::default()));
    }

    #[test]
    fn without_defer_the_upstream_path_is_unchanged() {
        let mut i = I2c::new();
        program(&mut i, &[RSTART, WRITE | ACK_CHECK | 1, STOP], &[0x50 << 1]);
        assert!(!i.deferred);
        assert!(i.is_active(), "scheduled on the controller clock");
        i.advance(u64::MAX);
        assert_ne!(i.int_raw & INT_NACK, 0);                             // no attached device, no host: NACK
    }
}
