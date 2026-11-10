//! Jumperly patch J29: what a circuit simulator needs from the ESP8266 (the C3's twin is
//! `esp32c3::jumperly`, patch J9). Every watched pin's drive state (GPIO out/enable, open drain,
//! the IO_MUX pull-up, GPIO16's pull-down, the pad's function) with the cycle it changed at, and
//! the end of the slice so the host can answer before the guest reads the pin again; the PWM
//! detector (`crate::pwm`) and the soft I2C decoder (`crate::soft_i2c`) on the same register
//! writes; the circuit's input levels combined with the I2C targets' drive; the TOUT pad's
//! voltage for `analogRead(A0)`; the baud of each UART; the host log notes. The state word has
//! the S3 layout (`esp32s3::jumperly::state`), so the host reads every chip the same way.
use crate::bus::SocBus;
use crate::periph::{Peripherals, DPORT_BASE, IO_MUX_OFFSET, PERIPH_BASE};
use crate::pwm::PwmOut;
use crate::soft_i2c::{HostFn, SoftI2c};

/// GPIO0..GPIO16 (GPIO6..11 are the flash pins inside the module).
pub const PINS: usize = 17;
/// The pseudo pin of the TOUT pad (A0) for `esp32sim_adc_set`.
pub const ADC_PIN: u32 = 17;

/// Bits of a pin state word: the same as `esp32s3::jumperly::state`.
pub mod state {
    pub const OUT: u32 = 1 << 0;
    pub const ENABLE: u32 = 1 << 1;
    pub const PULL_UP: u32 = 1 << 2;
    pub const PULL_DOWN: u32 = 1 << 3;
    pub const OPEN_DRAIN: u32 = 1 << 4;
    pub const INPUT_ENABLE: u32 = 1 << 5;
    /// The pad's function in bits 16..24; `PLAIN_GPIO` = the pad's own GPIO registers.
    pub const SIGNAL_SHIFT: u32 = 16;
    /// The value the host expects for a plain GPIO (the S3's `0x100`).
    pub const PLAIN_GPIO: u32 = 0x100;
    // The ESP8266 has no output matrix: a pad on another IO_MUX function reports which one
    // (esp8266_peri.h:121-123 `GPFFS`, `GPFFS_GPIO`, `GPFFS_BUS`; uart.cpp `uart_init`/`uart_swap`
    // for the UART0 pads; `IOSWAP` bit 2 swaps U0TXD/U0RTS and U0RXD/U0CTS, esp8266_peri.h:179-183).
    /// U0TXD: GPIO1 function 0, GPIO2 function 4, GPIO15 function 4 with IOSWAPU0
    pub const SIG_U0TXD: u32 = 1;
    /// U0RXD: GPIO3 function 0, GPIO13 function 4 with IOSWAPU0
    pub const SIG_U0RXD: u32 = 2;
    /// U1TXD: GPIO2 function 2
    pub const SIG_U1TXD: u32 = 3;
    /// HSPI on GPIO12..15 function 2: MISO, MOSI, CLK, CS
    pub const SIG_HSPI_MISO: u32 = 4;
    pub const SIG_HSPI_MOSI: u32 = 5;
    pub const SIG_HSPI_CLK: u32 = 6;
    pub const SIG_HSPI_CS: u32 = 7;
    /// U0RTS: GPIO15 function 4 without IOSWAPU0; U0CTS: GPIO13 function 4 without it
    pub const SIG_U0RTS: u32 = 8;
    pub const SIG_U0CTS: u32 = 9;
    /// any other function `f` (SDIO/flash, clock out, I2S, ...): `SIG_OTHER | f`
    pub const SIG_OTHER: u32 = 0x80;
}

/// esp8266_peri.h:116-120: an IO_MUX pin register's function select (bits 4, 5 and 8) and pull-up.
const MUX_PULLUP: u32 = 1 << 7;
/// esp8266_peri.h:86 GPCD: GPIO_PINn bit 2, open drain.
const PIN_OPEN_DRAIN: u32 = 1 << 2;
/// esp8266_peri.h:141 GP16FPD: GPIO16's pull-down in GP16F.
const GP16_PULLDOWN: u32 = 1 << 3;
/// esp8266_peri.h:179,182: IOSWAP (DPORT 0x28) bit 2 swaps UART0's pads.
const IOSWAP: u32 = DPORT_BASE + 0x28;
const IOSWAPU0: u32 = 1 << 2;

/// The IO_MUX function of GPIO`pin` (0..15).
fn function(p: &Peripherals, pin: usize) -> u32 {
    let mux = p.io_mux.read(PERIPH_BASE + 0x800 + IO_MUX_OFFSET[pin]);
    (mux >> 4 & 3) | (mux >> 8 & 1) << 2
}

/// The pad function code of `pin` (`state::PLAIN_GPIO` or `state::SIG_*`).
pub fn signal(p: &Peripherals, pin: usize) -> u32 {
    use state::*;
    if pin >= 16 { return PLAIN_GPIO; }
    let f = function(p, pin);
    // esp8266_peri.h:122 GPFFS_GPIO: the GPIO function is 0 on GPIO0, 2, 4, 5 and 3 on the others
    if f == if matches!(pin, 0 | 2 | 4 | 5) { 0 } else { 3 } { return PLAIN_GPIO; }
    let swap = p.dport.regs.read(IOSWAP) & IOSWAPU0 != 0;
    match (pin, f) {
        (1, 0) | (2, 4) => SIG_U0TXD,
        (3, 0) => SIG_U0RXD,
        (2, 2) => SIG_U1TXD,
        (12, 2) => SIG_HSPI_MISO,
        (13, 2) => SIG_HSPI_MOSI,
        (14, 2) => SIG_HSPI_CLK,
        (15, 2) => SIG_HSPI_CS,
        (15, 4) => if swap { SIG_U0TXD } else { SIG_U0RTS },
        (13, 4) => if swap { SIG_U0RXD } else { SIG_U0CTS },
        _ => SIG_OTHER | f,
    }
}

/// The drive state of `pin` (0..=16). The pads have no input enable of their own (esp8266_peri.h
/// lists none): the input is always on. GPIO0..15 have no pull-down (the IO_MUX bit 6 is not a
/// pad pull on this chip; Arduino offers INPUT_PULLDOWN_16 only), GPIO16 has no pull-up. In deep
/// sleep the RTC drives GPIO16 (XPD_DCDC).
pub fn pin_state(p: &Peripherals, pin: usize) -> u32 {
    let g = &p.gpio;
    let mut s = state::INPUT_ENABLE;
    if pin < 16 {
        let bit = 1 << pin;
        if g.out & bit != 0 { s |= state::OUT; }
        if g.enable & bit != 0 { s |= state::ENABLE; }
        if p.io_mux.read(PERIPH_BASE + 0x800 + IO_MUX_OFFSET[pin]) & MUX_PULLUP != 0 { s |= state::PULL_UP; }
        if g.pin[pin] & PIN_OPEN_DRAIN != 0 { s |= state::OPEN_DRAIN; }
    } else if pin == 16 {
        let (out, enable) = match g.xpd { Some(l) => (l, true), None => (g.gp16[0] & 1 != 0, g.gp16[1] & 1 != 0) };
        if out { s |= state::OUT; }
        if enable { s |= state::ENABLE; }
        if g.gp16[3] & GP16_PULLDOWN != 0 { s |= state::PULL_DOWN; }
    } else {
        return 0;
    }
    s | signal(p, pin) << state::SIGNAL_SHIFT
}

/// Last reported state of every pin and the changes not yet taken by the host.
pub struct PinWatch {
    pub mask: u64,
    last: [u32; PINS],
    /// (cycle, pin, state) in the order they happened
    pub events: Vec<(u64, u8, u32)>,
}

impl PinWatch {
    pub fn new(p: &Peripherals, mask: u64) -> Self {
        let mut last = [0; PINS];
        for (pin, s) in last.iter_mut().enumerate() { *s = pin_state(p, pin); }
        PinWatch { mask, last, events: Vec::new() }
    }

    /// Record every watched pin outside `skip` whose state changed since the last check; the
    /// pins that did (bit n = GPIOn). A skipped pin keeps its last report, so it is reported when
    /// it comes back.
    pub fn check(&mut self, p: &Peripherals, cycle: u64, skip: u32) -> u32 {
        let mut changed = 0;
        for pin in 0..PINS {
            if self.mask & (1u64 << pin) == 0 || skip & (1 << pin) != 0 { continue; }
            let s = pin_state(p, pin);
            if s != self.last[pin] {
                self.last[pin] = s;
                self.events.push((cycle, pin as u8, s));
                changed |= 1 << pin;
            }
        }
        changed
    }
}

/// An output edge counts for the PWM detector when it comes this soon after FRC1 ran out: the
/// waveform's NMI writes its edges right after the timer fires (core_esp8266_waveform_pwm.cpp
/// `timer1Interrupt`). Measured from FRC1 running out to the GPIO write: `analogWrite` on GPIO4
/// 96..576 APB cycles (8024 edges), `tone` on GPIO5 150..704 (1762), `Servo` on GPIO14 160..256
/// (160), so at most 8.8 us, in the `esp8266-pwm` and `esp8266-servo` fixtures; the 20 us window
/// leaves room for an NMI that serves several pins. Blink's `digitalWrite` edges came 4 ms and
/// more after FRC1 (it never runs there).
pub const TIMER_EDGE_CYCLES: u64 = 20 * crate::periph::CPU_HZ / 1_000_000;

/// The WDEV register the SDK writes with bit 31 set to start a transmit queue (0x3FF20DC4: a
/// `WiFi.begin()` sends its first frame there, the `esp8266-wifi-off` fixture's only write to the
/// 0x3FF20DC0 block; Blink, Serial and every other fixture never write it). Found by tracing,
/// not documented.
pub const WDEV_TX_START: u32 = crate::periph::WDEV_BASE + 0xDC4;

/// The host log line when the guest starts its radio.
pub const WIFI_NOTE: &str = "[emu] wifi: ESP8266 Wi-Fi is not modelled: the sketch keeps running, WiFi.status() never connects";
/// The host log line when the chip goes to deep sleep (no "reset" in it: the adapter reads a
/// warning with that word as a chip reset).
pub const SLEEP_NOTE: &str = "[emu] deep sleep: the ESP8266 wakes only when GPIO16 pulls RST low (wire GPIO16 to RST)";

/// What the host side keeps beside the bus.
#[derive(Default)]
pub struct HostState {
    /// `GPIO_OUT & GPIO_ENABLE` as the PWM detector last saw it
    pub last_out: u32,
    /// the guest's open-drain drive (bit n: GPIOn driven low) as the I2C decoders last saw it
    pub last_low: u32,
    /// the APB cycle FRC1 last ran out at (`SocBus::tick`)
    pub frc1_fired: u64,
    /// the divider the guest wrote to UARTn's CLKDIV since the last reset
    pub uart_div: [Option<u32>; 2],
    /// the level the circuit applies to each pin; None: floating
    pub circuit: [Option<bool>; PINS],
    /// the SAR words for the TOUT pad's voltage (`sar_word`)
    pub adc_word: Option<u32>,
    pub wifi_noted: bool,
    pub sleep_noted: bool,
    pub notes: Vec<&'static str>,
}

/// The guest's open-drain drive: GPIO0..15 with ENABLE set and OUT clear are driven low.
fn driven_low(p: &Peripherals) -> u32 { p.gpio.enable & !p.gpio.out & 0xffff }

/// The SAR result words libphy's `read_sar_dout` reads (Blink image 0x4022c090: eight words at
/// 0x60000D80..0x60000D9C, each `v = !word`, a sample of `(v & 0x700) + min(255, max(0, (v & 0xff)
/// - 21) * 279 >> 8)`), `test_tout` (0x4022699c) averages as `(sum + 8) >> 4` and
/// `system_adc_read` (0x40234990) scales by 12 / 11 (`__divsi3`) and caps at 1024. The word
/// whose reading is closest to `code` (0..=1024).
pub fn sar_word(code: u32) -> u32 {
    let reading = |v: u32| {
        let sample = (v & 0x700) + ((((v & 0xff) as i32 - 21).max(0) as u32 * 279) >> 8).min(255);
        ((8 * sample + 8) >> 4) * 12 / 11
    };
    let v = (0..0x800).min_by_key(|&v| reading(v).min(1024).abs_diff(code)).unwrap_or(0);
    !v
}

/// The TOUT pad at `mv` millivolts as `analogRead(A0)` reads it: 0..=1000 mV -> 0..=1023,
/// saturating above (model choice: a straight line to the ADC's 1.0 V full scale, ESP8266EX
/// datasheet 4.6 "TOUT ... input voltage range 0 to 1.0 V").
pub fn adc_code(mv: u32) -> u32 { ((u64::from(mv) * 1023 + 500) / 1000).min(1023) as u32 }

/// uart_register.h:60 UART_CLKDIV: the divider in bits 0..19.
const UART_CLKDIV_MASK: u32 = 0xf_ffff;
/// The SAR result registers (`sar_word`).
const SAR_DOUT: u32 = PERIPH_BASE + 0xD80;

impl SocBus {
    /// Whether a J29 hook runs on register writes: Jumperly mode or soft I2C pairs.
    #[inline]
    pub(crate) fn jumperly_hooks(&self) -> bool { self.pin_watch.is_some() || self.soft_i2c.is_some() }

    /// Jumperly patch J29: the guest wrote `v` to the register at `addr` (`mmio_write`).
    pub(crate) fn note_pin_write(&mut self, addr: u32, v: u32) {
        match addr {
            // GPIO, the RTC block (GPIO16's registers; deep sleep drives GPIO16), IO_MUX
            0x6000_0300..0x6000_0370 | 0x6000_0700..0x6000_0800 | 0x6000_0800..0x6000_0900 => self.pins_changed(),
            // DPORT IOSWAP: Serial.swap() moves UART0 to GPIO15/13, which changes the pins' functions
            IOSWAP => self.pins_changed(),
            0x6000_0014 => self.jumperly.uart_div[0] = Some(v & UART_CLKDIV_MASK),
            0x6000_0F14 => self.jumperly.uart_div[1] = Some(v & UART_CLKDIV_MASK),
            WDEV_TX_START if v & 1 << 31 != 0 && !self.jumperly.wifi_noted => {
                self.jumperly.wifi_noted = true;
                self.jumperly.notes.push(WIFI_NOTE);
            }
            _ => {}
        }
    }

    /// The pins may have changed: edges to the PWM detector, the I2C decoders, the pin watch.
    pub(crate) fn pins_changed(&mut self) {
        let cycle = self.cycles;
        if let Some(d) = &mut self.pwm_tap {
            let out = self.periph.gpio.output();
            let changed = out ^ self.jumperly.last_out;
            if changed != 0 {
                let timed = self.jumperly.frc1_fired != 0 && cycle.saturating_sub(self.jumperly.frc1_fired) <= TIMER_EDGE_CYCLES;
                for pin in 0..PINS as u8 {
                    if changed & 1 << pin == 0 { continue; }
                    let level = out & 1 << pin != 0;
                    let event = if timed { d.note_edge(pin, level, cycle) } else { d.note_direct_edge(pin, level, cycle) };
                    if event { self.slice_yield = true; }
                }
                self.jumperly.last_out = out;
            }
        }
        let mut moved = 0;
        if let Some(i2c) = self.soft_i2c.as_mut().filter(|i| i.active()) {
            let low = driven_low(&self.periph) & i2c.pins();
            if low != self.jumperly.last_low {
                self.jumperly.last_low = low;
                let before = i2c.pulled();
                i2c.update(low);
                moved = before ^ i2c.pulled();
            }
        }
        for pin in 0..16 { if moved & 1 << pin != 0 { self.apply_input(pin); } }
        // a pin in `pwm` mode reports through the detector; the soft I2C pins report without
        // ending the slice, since the decoders play the targets inline
        let skip = self.pwm_tap.as_ref().map_or(0, |d| d.pwm_pins());
        let quiet = self.soft_i2c.as_ref().filter(|i| i.active()).map_or(0, |i| i.pins());
        if let Some(w) = &mut self.pin_watch {
            if w.check(&self.periph, cycle, skip) & !quiet != 0 { self.slice_yield = true; }
        }
    }

    /// The I2C targets' part of `pin`'s input: `Some(false)` while one pulls it low.
    pub fn soft_i2c_input(&self, pin: u8) -> Option<bool> {
        self.soft_i2c.as_ref().filter(|i| i.active() && i.pulls_low(pin)).map(|_| false)
    }

    /// The pad level the host side gives `pin`: an I2C target's pull, else the circuit's level.
    fn apply_input(&mut self, pin: u8) {
        let level = self.soft_i2c_input(pin).or(self.jumperly.circuit[usize::from(pin)]);
        self.periph.gpio_set_input(pin, level);
        self.irq_dirty = true;
    }

    /// The level the circuit applies to `pin` (0..=16; None: floating, the pad's pull decides).
    pub fn circuit_input(&mut self, pin: u8, level: Option<bool>) {
        if usize::from(pin) >= PINS { return; }
        self.jumperly.circuit[usize::from(pin)] = level;
        self.apply_input(pin);
    }

    /// Register (`on`) or drop the soft I2C pair `sda`/`scl` (`SoftI2c::set_pair`). Two different
    /// pads of GPIO0..15 or nothing: false, and nothing touched, for any other pair (on or off).
    pub fn set_soft_i2c(&mut self, sda: u8, scl: u8, on: bool) -> bool {
        if sda > 15 || scl > 15 || sda == scl { return false; }
        let low = driven_low(&self.periph);
        let i2c = self.soft_i2c.get_or_insert_with(SoftI2c::default);
        let pulled = i2c.pulled();
        let ok = i2c.set_pair(sda, scl, on, low);
        self.jumperly.last_low = low & i2c.pins();
        if pulled & 1 << sda != 0 { self.apply_input(sda); }
        ok
    }

    /// The I2C targets (`host_i2c`) for the soft I2C pairs, or none: the decoders stop and let go.
    pub fn set_soft_i2c_host(&mut self, host: Option<HostFn>) {
        let low = driven_low(&self.periph);
        let i2c = self.soft_i2c.get_or_insert_with(SoftI2c::default);
        // The bridge flag is set again by every host call; the decoders restart only when the
        // presence of the host changes, so a repeat keeps a transaction and its ACK pull.
        if i2c.host.is_some() == host.is_some() {
            if host.is_some() { i2c.host = host; }
            return;
        }
        let pulled = i2c.pulled();
        i2c.host = host;
        i2c.reset(low);
        self.jumperly.last_low = low & i2c.pins();
        for pin in 0..16 { if pulled & 1 << pin != 0 { self.apply_input(pin); } }
    }

    /// The TOUT pad's voltage for `analogRead(A0)` (`adc_code`, `sar_word`); kept across resets.
    pub fn adc_set_mv(&mut self, mv: u32) {
        self.jumperly.adc_word = Some(sar_word(adc_code(mv)));
        self.write_sar();
    }

    fn write_sar(&mut self) {
        let Some(w) = self.jumperly.adc_word else { return };
        for i in 0..8 { self.periph.other.write(SAR_DOUT + 4 * i, w); }
    }

    /// UART `port`'s baud (0, 1): the 80 MHz APB clock over the CLKDIV the guest wrote
    /// (`Serial.begin`: uart.cpp `uart_set_baudrate`, `USD(n) = ESP8266_CLOCK / baud`). Before
    /// that, UART0 has the ROM's divider 694 once the ROM has booted, which is 74880 baud: the
    /// ROM programs 115200 for a 40 MHz crystal and runs from the 26 MHz one before the SDK
    /// starts the PLL (`rom::boot::UART0_BOOT_DIV`); the same 694 from `Serial.begin(115200)` is
    /// 115200. None for a UART not set up.
    pub fn uart_baud(&self, port: u8) -> Option<u32> {
        match (port, self.jumperly.uart_div.get(usize::from(port)).copied().flatten()) {
            (_, Some(0)) | (2.., _) => None,
            (_, Some(div)) => Some((crate::periph::CPU_HZ / u64::from(div)) as u32),
            (0, None) => (!self.rom.boot_pending).then_some(74_880),
            (_, None) => None,
        }
    }

    /// UART `port`'s pads (tx, rx) from the IO_MUX functions: UART0 on GPIO1/3, TX on GPIO2 with
    /// function 4, GPIO15/13 after `Serial.swap()`; UART1 transmits only, on GPIO2.
    pub fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) {
        let on = |pins: &[u8], sig: u32| pins.iter().copied().find(|&p| signal(&self.periph, usize::from(p)) == sig);
        match port {
            0 => (on(&[1, 2, 15], state::SIG_U0TXD), on(&[3, 13], state::SIG_U0RXD)),
            1 => (on(&[2], state::SIG_U1TXD), None),
            _ => (None, None),
        }
    }

    /// The PWM changes since the last call, after the silences up to now.
    pub fn take_pwm(&mut self) -> Vec<(u64, PwmOut)> {
        let Some(d) = &mut self.pwm_tap else { return Vec::new() };
        if d.poll(self.cycles) {
            // a pin left `pwm` mode: its edges go to the pin watch again
            if let Some(w) = &mut self.pin_watch { w.check(&self.periph, self.cycles, d.pwm_pins()); }
        }
        d.take()
    }

    /// The host log lines since the last call: the modelled ROM's diagnostics as it wrote them
    /// (`[emu] rom: ...`, `rom::RomState::take_notes`: an unmodelled ROM entry, a ROM stop and its
    /// reason, ...), then the radio note once and a note at each deep sleep.
    pub fn take_notes(&mut self) -> Vec<String> {
        let sleeping = self.deep_sleep();
        if sleeping && !self.jumperly.sleep_noted { self.jumperly.notes.push(SLEEP_NOTE); }
        self.jumperly.sleep_noted = sleeping;
        let mut lines = self.rom.take_notes();
        lines.extend(self.jumperly.notes.drain(..).map(String::from));
        lines
    }

    /// After a chip reset (`Soc::reboot`): the PWM outputs are off, the I2C decoders idle, the
    /// circuit's levels and the TOUT voltage apply again, the UART dividers are the ROM's.
    pub(crate) fn jumperly_after_reboot(&mut self) {
        if let Some(d) = &mut self.pwm_tap { if d.reset(self.cycles) { self.slice_yield = true; } }
        self.jumperly.last_out = self.periph.gpio.output();
        self.jumperly.uart_div = [None; 2];
        let low = driven_low(&self.periph);
        let pins = self.soft_i2c.as_mut().map_or(0, |i2c| { i2c.reset(low); i2c.pins() });
        self.jumperly.last_low = low & pins;
        for pin in 0..16 { if pins & 1 << pin != 0 { self.apply_input(pin); } }
        self.write_sar();
        if let Some(w) = &mut self.pin_watch { if w.check(&self.periph, self.cycles, 0) != 0 { self.slice_yield = true; } }
    }

    /// The host's word on the chip's ROM: the strict mode stops at an unknown ROM entry; the host
    /// keeps it off, so such an entry returns 0 and the host log gets its line once
    /// (`[emu] rom: unimplemented <name> (<addr>) from <call site>: returns 0`, `take_notes`). The
    /// stops the ROM still makes (the image at flash 0 fails to load or returns, a pc in the ROM
    /// that is no entry, an exception without a handler) reach the host as its line and as
    /// `esp32sim_run`'s stop code 8 until a reset (`rom_halted`).
    pub fn set_rom_strict(&mut self, on: bool) { self.rom.strict = on; }

    /// Whether the modelled ROM stopped the chip (`rom::RomState::halted`): it stands still until
    /// a reset.
    pub fn rom_halted(&self) -> bool { self.rom.halted().is_some() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::Bus;

    fn periph() -> Peripherals { Peripherals::new([0; 6]) }
    const GPIO: u32 = PERIPH_BASE + 0x300;
    const MUX: u32 = PERIPH_BASE + 0x800;
    const PLAIN: u32 = state::PLAIN_GPIO << state::SIGNAL_SHIFT;

    #[test]
    fn pin_state_reflects_gpio_iomux_and_the_pad_function() {
        let mut p = periph();
        // reset: GPIO2 a plain GPIO input with the ROM's pull-up; GPIO1/3 on UART0
        assert_eq!(pin_state(&p, 2), state::INPUT_ENABLE | state::PULL_UP | PLAIN);
        assert_eq!(pin_state(&p, 1) >> state::SIGNAL_SHIFT, state::SIG_U0TXD);
        assert_eq!(pin_state(&p, 3) >> state::SIGNAL_SHIFT, state::SIG_U0RXD);
        p.write32(GPIO + 0x10, 1 << 4);                                  // ENABLE_W1TS
        p.write32(GPIO + 0x04, 1 << 4);                                  // OUT_W1TS
        assert_eq!(pin_state(&p, 4) & 0xffff, state::OUT | state::ENABLE | state::INPUT_ENABLE);
        p.write32(GPIO + 0x28 + 4 * 5, 1 << 2);                          // GPIO_PIN5: open drain
        assert_eq!(pin_state(&p, 5) & 0xffff, state::OPEN_DRAIN | state::INPUT_ENABLE);
        // pinMode(14, SPECIAL): function 2 = HSPI CLK; pinMode(14, OUTPUT): function 3 = GPIO
        p.write32(MUX + IO_MUX_OFFSET[14], 2 << 4);
        assert_eq!(pin_state(&p, 14) >> state::SIGNAL_SHIFT, state::SIG_HSPI_CLK);
        p.write32(MUX + IO_MUX_OFFSET[14], 3 << 4 | MUX_PULLUP);
        assert_eq!(pin_state(&p, 14), state::PULL_UP | state::INPUT_ENABLE | PLAIN);
        // Serial.swap(): GPIO15/13 function 4 with IOSWAPU0
        p.write32(MUX + IO_MUX_OFFSET[15], 1 << 8);
        assert_eq!(pin_state(&p, 15) >> state::SIGNAL_SHIFT, state::SIG_U0RTS);
        p.write32(IOSWAP, IOSWAPU0);
        assert_eq!(pin_state(&p, 15) >> state::SIGNAL_SHIFT, state::SIG_U0TXD);
        // GPIO16: RTC registers, pull-down in GP16F
        p.write32(PERIPH_BASE + 0x774, 1);
        p.write32(PERIPH_BASE + 0x768, 1);
        p.write32(PERIPH_BASE + 0x7A0, GP16_PULLDOWN);
        assert_eq!(pin_state(&p, 16), state::OUT | state::ENABLE | state::PULL_DOWN | state::INPUT_ENABLE | PLAIN);
        assert_eq!(pin_state(&p, 17), 0);
    }

    #[test]
    fn ioswap_write_updates_the_pin_view() {
        // Serial.swap() writes IOSWAP (DPORT 0x28, bit IOSWAPU0): GPIO15's function 4 turns from
        // U0RTS into U0TXD, and the pin watch reports it
        let mut b = SocBus::new(4 << 20, [0; 6]);
        b.pin_watch = Some(PinWatch::new(&b.periph, (1 << PINS) - 1));
        b.write32(MUX + IO_MUX_OFFSET[15], 1 << 8).unwrap();
        let w = b.pin_watch.as_mut().unwrap();
        assert_eq!(w.events.last().map(|e| (e.1, e.2 >> state::SIGNAL_SHIFT)), Some((15, state::SIG_U0RTS)));
        w.events.clear();
        b.write32(IOSWAP, IOSWAPU0).unwrap();
        let w = b.pin_watch.as_mut().unwrap();
        assert_eq!(w.events.len(), 1, "{:?}", w.events);
        assert_eq!((w.events[0].1, w.events[0].2 >> state::SIGNAL_SHIFT), (15, state::SIG_U0TXD));
    }

    /// Jumperly patch J29: a pair outside GPIO0..15 or of one pad is refused before anything is
    /// touched, on or off (before, `esp32sim_soft_i2c(e, 36, ...)` indexed past the pins and
    /// panicked); the soft I2C state stays as it was.
    #[test]
    fn soft_i2c_refuses_pads_outside_gpio0_15_before_touching_them() {
        let mut b = SocBus::new(4 << 20, [0; 6]);
        for (sda, scl) in [(36, 5), (4, 36), (16, 5), (255, 255), (4, 4)] {
            for on in [true, false] { assert!(!b.set_soft_i2c(sda, scl, on), "{sda}/{scl} {on}"); }
        }
        assert!(b.soft_i2c.is_none(), "nothing set up");
        assert!(b.set_soft_i2c(4, 5, true) && b.set_soft_i2c(4, 5, false));
    }

    #[test]
    fn a_repeated_host_keeps_the_decoder_state() {
        let host = || -> Option<HostFn> { Some(Box::new(|_, op, _| u32::from(op < 2))) };
        let mut b = SocBus::new(4 << 20, [0; 6]);
        assert!(b.set_soft_i2c(4, 5, true));
        b.set_soft_i2c_host(host());
        let (sda, scl) = (1u32 << 4, 1u32 << 5);
        let drive = |b: &mut SocBus, low: u32| b.soft_i2c.as_mut().unwrap().update(low);
        // START, then the address byte 0xEC; the target pulls SDA for the ACK slot
        drive(&mut b, 0);
        drive(&mut b, sda);
        drive(&mut b, sda | scl);
        for i in (0..8).rev() {
            let bit = 0xecu8 >> i & 1 != 0;
            drive(&mut b, scl);
            drive(&mut b, if bit { scl } else { scl | sda });
            drive(&mut b, if bit { 0 } else { sda });
        }
        drive(&mut b, scl);
        assert_ne!(b.soft_i2c.as_ref().unwrap().pulled() & sda, 0, "the ACK slot is pulled");
        // the host flag set again does not restart the decoders
        b.set_soft_i2c_host(host());
        assert_ne!(b.soft_i2c.as_ref().unwrap().pulled() & sda, 0, "a repeat keeps the ACK pull");
        // the host going away and back does
        b.set_soft_i2c_host(None);
        assert_eq!(b.soft_i2c.as_ref().unwrap().pulled(), 0);
        b.set_soft_i2c_host(host());
        assert_eq!(b.soft_i2c.as_ref().unwrap().pulled(), 0);
    }

    #[test]
    fn pin_watch_skips_pins_in_pwm_mode() {
        let mut p = periph();
        let mut w = PinWatch::new(&p, (1 << PINS) - 1);
        p.write32(GPIO + 0x10, 1 << 4 | 1 << 5);
        assert_eq!(w.check(&p, 10, 1 << 4 | 1 << 5), 0);
        assert_eq!(w.check(&p, 20, 1 << 5), 1 << 4);
        assert_eq!(w.events, vec![(20, 4, state::ENABLE | state::INPUT_ENABLE | PLAIN)]);
        assert_eq!(w.check(&p, 30, 0), 1 << 5, "GPIO5 comes back");
        assert_eq!(w.events[1], (30, 5, state::ENABLE | state::INPUT_ENABLE | PLAIN));
    }

    #[test]
    fn sar_words_read_back_as_the_code_through_libphys_arithmetic() {
        // the guest's arithmetic, as read_sar_dout, test_tout and system_adc_read do it
        let read = |word: u32| {
            let sum: u32 = (0..8).map(|_| {
                let v = !word;
                let low = (((v & 0xff) as i32 - 21).max(0) * 0x117) >> 8;
                (v & 0x7ff & 0xf00) + (low.min(255) as u32)
            }).sum();
            (((sum + 8) >> 4) * 12 / 11).min(0x400)
        };
        assert_eq!(read(0), 1024, "registers at 0 read full scale (the model before J29)");
        for code in [0, 1, 100, 511, 512, 1000, 1023] {
            assert!(read(sar_word(code)).abs_diff(code) <= 1, "code {code}: {}", read(sar_word(code)));
        }
        assert_eq!(read(sar_word(adc_code(500))), 512);
        assert_eq!(read(sar_word(adc_code(0))), 0);
        assert_eq!(adc_code(1000), 1023);
        assert_eq!(adc_code(3300), 1023);
    }
}
