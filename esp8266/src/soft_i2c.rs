//! Jumperly patch J29: I2C for the host from the ESP8266's software master. The chip has no I2C
//! controller: Arduino's `Wire` bit-bangs SDA and SCL as open-drain lines by switching the pins'
//! output enable with the output latch at 0 (Arduino ESP8266 3.1.2
//! cores/esp8266/core_esp8266_si2c.cpp: `SDA_LOW` = `GPES`, `SDA_HIGH` = `GPEC`, `SDA_READ` =
//! `GPI`; `write_start`, `write_stop`, `write_bit`, `read_bit`, `twi_scl_valley`). The host
//! registers the pin pairs whose nets carry I2C targets in its circuit (`set_pair`); `SoftI2c`
//! decodes the guest's drive of each pair and plays the targets' part through the same
//! `host_i2c(bus, op, arg)` calls the hardware bridge makes (J6): op 0 START with
//! `addr << 1 | read` (returns the ACK), 1 write a byte (ACK), 2 read a byte, 3 STOP.
//!
//! The lines: a pin the guest drives low (`ENABLE` set, `OUT` clear) is low; otherwise it is
//! released and the circuit's pull-ups hold it high. Conditions and bits, as the I2C
//! specification (NXP UM10204 3.1.4, 3.1.6) defines them on the lines:
//! - START: SDA falls while SCL is high (also a repeated START); the next byte is the address;
//! - STOP: SDA rises while SCL is high; a transaction that began calls op 3;
//! - a bit the master writes is SDA at the rising edge of SCL; after eight, the ninth clock is
//!   the ACK slot: the address goes to op 0 and a data byte to op 1 at the eighth rising edge,
//!   and the target's ACK pulls SDA low from the falling edge that ends the eighth bit to the
//!   falling edge that ends the ACK clock;
//! - after an acknowledged read address the target sends: op 2 fetches a byte at the falling
//!   edge that ends the ACK clock, and each bit is on SDA from the falling edge before its clock
//!   to the falling edge after it; the master's ACK (SDA low at the ninth rising edge) asks for
//!   another byte, its NACK ends the reads until STOP or a repeated START.
//!
//! The target's drive reaches the guest's `digitalRead` / `GPI` through `SocBus::soft_i2c_input`,
//! combined with the level the circuit applies to the pin. Not modelled: clock stretching by a
//! target (the project's parts do not stretch; `WAIT_CLOCK_STRETCH` always finds SCL high), a
//! second master, pins above GPIO15 (`Wire` addresses its pins through `GPES`/`GPEC`, 0..15).

/// `host_i2c(bus, op, arg)`: the circuit's targets, as the Jumperly ABI's import answers them.
pub type HostFn = Box<dyn FnMut(u32, u32, u32) -> u32>;

/// One call to the host, as the decoder made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cOp {
    Start { addr: u8, read: bool, ack: bool },
    Write { byte: u8, ack: bool },
    Read(u8),
    Stop,
}

/// Where a transaction is (see the module comment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// the master clocks out a byte; `n` bits sampled
    Write { first: bool, n: u8, byte: u8 },
    /// eight bits in; the ACK (if `ack`) goes on SDA at the next falling edge of SCL
    AckWait { ack: bool, read: bool },
    /// the ACK clock: the target holds SDA low if `ack` until SCL falls
    AckClock { ack: bool, read: bool },
    /// the target sends `byte`; `n` bits clocked
    Read { n: u8, byte: u8 },
    /// eight bits read: the master's ACK clock; `ack` as SDA was at its rising edge
    MasterAck { ack: bool },
    /// the target took no more: until STOP or a repeated START
    Done,
}

/// One registered pair and its decoder.
#[derive(Clone, Copy, Debug)]
pub struct Pair {
    pub sda: u8,
    pub scl: u8,
    /// the line levels as the guest leaves them: false = it drives the line low
    sda_hi: bool,
    scl_hi: bool,
    phase: Phase,
    /// the target pulls SDA low
    pull: bool,
}

impl Pair {
    /// The `bus` argument of the host calls for this pair: `sda | scl << 8`.
    pub fn bus(&self) -> u32 { u32::from(self.sda) | u32::from(self.scl) << 8 }
}

/// The registered pairs; their decoders run while `host` is set (`SocBus::bus_host`'s I2C).
#[derive(Default)]
pub struct SoftI2c {
    pub pairs: Vec<Pair>,
    pub host: Option<HostFn>,
}

impl SoftI2c {
    /// Register (`on`) or drop the pair `sda`/`scl`; false for a pin above 15, `sda == scl`, or
    /// a pin another pair has. `low` is the guest's current drive (bit n: GPIOn driven low).
    pub fn set_pair(&mut self, sda: u8, scl: u8, on: bool, low: u32) -> bool {
        if !on {
            self.pairs.retain(|p| (p.sda, p.scl) != (sda, scl));
            return true;
        }
        if sda > 15 || scl > 15 || sda == scl { return false; }
        if self.pairs.iter().any(|p| (p.sda, p.scl) == (sda, scl)) { return true; }
        if self.pairs.iter().any(|p| [p.sda, p.scl].iter().any(|&q| q == sda || q == scl)) { return false; }
        self.pairs.push(Pair { sda, scl, sda_hi: low >> sda & 1 == 0, scl_hi: low >> scl & 1 == 0, phase: Phase::Idle, pull: false });
        true
    }

    /// Whether the decoders run.
    pub fn active(&self) -> bool { self.host.is_some() && !self.pairs.is_empty() }

    /// The pins whose drive the decoders follow.
    pub fn pins(&self) -> u32 { self.pairs.iter().fold(0, |m, p| m | 1 << p.sda | 1 << p.scl) }

    /// Whether a target pulls `pin` low now.
    pub fn pulls_low(&self, pin: u8) -> bool { self.pairs.iter().any(|p| p.sda == pin && p.pull) }

    /// The SDA pins the targets pull low, bit n = GPIOn.
    pub fn pulled(&self) -> u32 { self.pairs.iter().fold(0, |m, p| m | u32::from(p.pull) << p.sda) }

    /// Back to idle (a chip reset): nothing pulled, the lines as `low` has them.
    pub fn reset(&mut self, low: u32) {
        for p in &mut self.pairs {
            (p.phase, p.pull) = (Phase::Idle, false);
            (p.sda_hi, p.scl_hi) = (low >> p.sda & 1 == 0, low >> p.scl & 1 == 0);
        }
    }

    /// The guest's drive changed to `low` (bit n: GPIOn driven low): run the decoders of the
    /// pairs whose lines moved. When both lines of a pair move in one register write, a falling
    /// SCL goes first and a rising SCL last, so the data line changes while the clock is low.
    /// The host calls made, in order.
    pub fn update(&mut self, low: u32) -> Vec<I2cOp> {
        let mut ops = Vec::new();
        let Some(host) = self.host.as_mut() else { return ops };
        for p in &mut self.pairs {
            let (sda_hi, scl_hi) = (low >> p.sda & 1 == 0, low >> p.scl & 1 == 0);
            let (sda, scl) = (sda_hi != p.sda_hi, scl_hi != p.scl_hi);
            if scl && !scl_hi { ops.extend(p.on_pin_change(p.scl, true, host)); }
            if sda { ops.extend(p.on_pin_change(p.sda, !sda_hi, host)); }
            if scl && scl_hi { ops.extend(p.on_pin_change(p.scl, false, host)); }
        }
        ops
    }
}

impl Pair {
    /// One line of the pair changed: `pin` is now driven low (`driven_low`) or released. The
    /// host call it made, if any.
    pub fn on_pin_change(&mut self, pin: u8, driven_low: bool, host: &mut HostFn) -> Option<I2cOp> {
        let bus = self.bus();
        let mut call = |op: u32, arg: u32| host(bus, op, arg);
        if pin == self.sda {
            self.sda_hi = !driven_low;
            if !self.scl_hi { return None; }
            if driven_low {
                // START, or a repeated START: whatever was going on, the next byte is an address
                self.pull = false;
                self.phase = Phase::Write { first: true, n: 0, byte: 0 };
                return None;
            }
            let began = self.phase != Phase::Idle;
            (self.phase, self.pull) = (Phase::Idle, false);
            if began { call(3, 0); return Some(I2cOp::Stop); }
            return None;
        }
        if pin != self.scl { return None; }
        self.scl_hi = !driven_low;
        if self.scl_hi { self.rising(&mut call) } else { self.falling(&mut call) }
    }

    fn rising(&mut self, call: &mut impl FnMut(u32, u32) -> u32) -> Option<I2cOp> {
        match self.phase {
            Phase::Write { first, n, byte } => {
                let byte = byte << 1 | u8::from(self.sda_hi);
                if n + 1 < 8 { self.phase = Phase::Write { first, n: n + 1, byte }; return None; }
                let (op, ack, read) = if first {
                    let (addr, read) = (byte >> 1, byte & 1 != 0);
                    let ack = call(0, u32::from(byte)) != 0;
                    (I2cOp::Start { addr, read, ack }, ack, read)
                } else {
                    let ack = call(1, u32::from(byte)) != 0;
                    (I2cOp::Write { byte, ack }, ack, false)
                };
                self.phase = Phase::AckWait { ack, read };
                Some(op)
            }
            Phase::Read { n, byte } => { self.phase = Phase::Read { n: n + 1, byte }; None }
            Phase::MasterAck { .. } => { self.phase = Phase::MasterAck { ack: !self.sda_hi }; None }
            _ => None,
        }
    }

    fn falling(&mut self, call: &mut impl FnMut(u32, u32) -> u32) -> Option<I2cOp> {
        match self.phase {
            Phase::AckWait { ack, read } => { self.pull = ack; self.phase = Phase::AckClock { ack, read }; None }
            Phase::AckClock { ack, read } => {
                self.pull = false;
                self.phase = match (ack, read) {
                    (false, _) => Phase::Done,
                    (true, false) => Phase::Write { first: false, n: 0, byte: 0 },
                    (true, true) => return Some(self.fetch(call)),
                };
                None
            }
            Phase::Read { n, .. } if n >= 8 => { self.pull = false; self.phase = Phase::MasterAck { ack: false }; None }
            Phase::Read { n, byte } => { self.pull = byte >> (7 - n) & 1 == 0; None }
            Phase::MasterAck { ack: true } => Some(self.fetch(call)),
            Phase::MasterAck { ack: false } => { self.phase = Phase::Done; None }
            _ => None,
        }
    }

    /// The target's next byte: its first bit goes on SDA now.
    fn fetch(&mut self, call: &mut impl FnMut(u32, u32) -> u32) -> I2cOp {
        let byte = call(2, 0) as u8;
        self.phase = Phase::Read { n: 0, byte };
        self.pull = byte & 0x80 == 0;
        I2cOp::Read(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const SDA: u8 = 4;
    const SCL: u8 = 5;

    /// The guest's side as core_esp8266_si2c.cpp drives it: each line released or driven low,
    /// SDA read back as the line (released and not pulled by the target = high).
    struct Master { i2c: SoftI2c, sda_low: bool, scl_low: bool, log: Rc<RefCell<Vec<(u32, u32, u32)>>> }

    impl Master {
        fn new(read: u8, ack: bool) -> Master {
            let log = Rc::new(RefCell::new(Vec::new()));
            let l = log.clone();
            let mut i2c = SoftI2c::default();
            assert!(i2c.set_pair(SDA, SCL, true, 0));
            i2c.host = Some(Box::new(move |bus, op, arg| {
                l.borrow_mut().push((bus, op, arg));
                match op { 0 | 1 => u32::from(ack), 2 => u32::from(read), _ => 0 }
            }));
            Master { i2c, sda_low: false, scl_low: false, log }
        }
        fn drive(&mut self) { self.i2c.update(u32::from(self.sda_low) << SDA | u32::from(self.scl_low) << SCL); }
        fn sda(&mut self, high: bool) { self.sda_low = !high; self.drive(); }
        fn scl(&mut self, high: bool) { self.scl_low = !high; self.drive(); }
        fn read_sda(&self) -> bool { !self.sda_low && !self.i2c.pulls_low(SDA) }
        // the core's routines
        fn start(&mut self) { self.scl(true); self.sda(true); assert!(self.read_sda(), "line busy"); self.sda(false); self.scl(false); }
        fn stop(&mut self) { self.scl(false); self.sda(false); self.scl(true); self.sda(true); }
        fn write_bit(&mut self, bit: bool) { self.scl(false); self.sda(bit); self.scl(true); }
        fn read_bit(&mut self) -> bool { self.scl(false); self.sda(true); self.scl(true); self.read_sda() }
        fn write_byte(&mut self, byte: u8) -> bool { for i in (0..8).rev() { self.write_bit(byte >> i & 1 != 0); } !self.read_bit() }
        fn read_byte(&mut self, nack: bool) -> u8 {
            let mut b = 0;
            for _ in 0..8 { b = b << 1 | u8::from(self.read_bit()); }
            self.write_bit(nack);
            b
        }
        fn valley(&mut self) { self.scl(false); self.scl(true); }
        fn ops(&self) -> Vec<(u32, u32)> { self.log.borrow().iter().map(|&(_, op, arg)| (op, arg)).collect() }
    }

    #[test]
    fn write_then_read_transaction_with_repeated_start() {
        let mut m = Master::new(0x60, true);
        // Wire.beginTransmission(0x76); write(0xD0); endTransmission(false); requestFrom(0x76, 1)
        m.start();
        assert!(m.write_byte(0xec), "the address is acknowledged");
        assert!(m.write_byte(0xd0), "the register is acknowledged");
        m.valley();                                  // twi_scl_valley before the repeated start
        assert!(m.read_sda(), "the target lets SDA go after the ACK clock");
        m.start();
        assert!(m.write_byte(0xed));
        assert_eq!(m.read_byte(true), 0x60, "the target's bits are on SDA at each rising edge");
        assert!(m.read_sda(), "after the last byte the target lets SDA go");
        m.stop();
        assert_eq!(m.ops(), vec![(0, 0x76 << 1), (1, 0xd0), (0, 0x76 << 1 | 1), (2, 0), (3, 0)]);
        assert!(m.log.borrow().iter().all(|&(bus, ..)| bus == u32::from(SDA) | u32::from(SCL) << 8));
        assert!(!m.i2c.pulls_low(SDA));
    }

    #[test]
    fn two_bytes_read_with_a_master_ack_between() {
        let mut m = Master::new(0xa5, true);
        m.start();
        assert!(m.write_byte(0x77 << 1 | 1));
        assert_eq!(m.read_byte(false), 0xa5);
        assert_eq!(m.read_byte(true), 0xa5);
        m.stop();
        assert_eq!(m.ops(), vec![(0, 0x77 << 1 | 1), (2, 0), (2, 0), (3, 0)]);
    }

    #[test]
    fn nack_from_host_keeps_sda_high_in_the_ack_slot() {
        let mut m = Master::new(0, false);
        m.start();
        // the eighth bit: the slot follows
        for i in (0..8).rev() { m.write_bit((0x3c << 1) >> i & 1 != 0); }
        m.scl(false);
        m.sda(true);
        assert!(m.read_sda() && !m.i2c.pulls_low(SDA), "no ACK on SDA");
        m.scl(true);
        assert!(m.read_sda(), "NACK at the ACK clock");
        m.stop();
        assert_eq!(m.ops(), vec![(0, 0x3c << 1), (3, 0)]);
        // the next transaction starts on a free line
        m.start();
        assert!(!m.write_byte(0x3c << 1));
        m.stop();
        assert_eq!(m.ops().len(), 4);
    }

    #[test]
    fn pair_off_ignores_edges() {
        let mut m = Master::new(0x60, true);
        assert!(m.i2c.set_pair(SDA, SCL, false, 0));
        assert!(!m.i2c.active());
        m.start();
        assert!(!m.write_byte(0xec), "nobody acknowledges");
        m.stop();
        assert!(m.ops().is_empty());
        // without the host the decoders do not run either
        let mut m = Master::new(0x60, true);
        m.i2c.host = None;
        m.start();
        assert!(!m.write_byte(0xec));
        m.stop();
        assert!(m.ops().is_empty());
    }

    #[test]
    fn pairs_are_checked() {
        let mut i = SoftI2c::default();
        assert!(i.set_pair(4, 5, true, 0));
        assert!(i.set_pair(4, 5, true, 0), "again: kept");
        assert!(!i.set_pair(5, 12, true, 0), "GPIO5 is taken");
        assert!(!i.set_pair(16, 2, true, 0) && !i.set_pair(2, 2, true, 0));
        assert!(i.set_pair(12, 14, true, 0));
        assert_eq!(i.pins(), 1 << 4 | 1 << 5 | 1 << 12 | 1 << 14);
        assert!(i.set_pair(4, 5, false, 0));
        assert_eq!(i.pins(), 1 << 12 | 1 << 14);
    }
}
