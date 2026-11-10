//! The ROM's console: `ets_printf` and friends, the character functions behind them, and the
//! ROM's UART set-up calls. Semantics: Arduino ESP8266 3.1.2 tools/sdk/include/ets_sys.h:30-45,
//! 221-254 and cores/esp8266/esp8266_undocumented.h:36-78. Characters go into the UART model's
//! transmit FIFO register, which is where the host console reads them.
use super::{arg, ret, SocBus};
use emu_core::{Bus, Core};
use xtensa_lx7::Lx106;

/// ets_uart_putc1: the "cooked" character function, which `ets_install_uart_printf` installs as
/// `ets_printf`'s (ets_sys.h:242-252), so it is the one in place after a reset.
pub const ETS_UART_PUTC1: u32 = 0x4000_1dcc;
/// ets_putc: the "raw" one (ets_sys.h:230-237).
const ETS_PUTC: u32 = 0x4000_2be8;
/// uart_tx_one_char: raw, like `ets_putc` (ets_sys.h:34-36: returns 0).
const UART_TX_ONE_CHAR: u32 = 0x4000_3b30;
/// uart_register.h:60: UART_CLKDIV(i).
const UART_CLKDIV: u32 = 0x14;
/// uart_register.h:81-83: UART_CONF0(i), RXFIFO_RST.
const UART_CONF0: u32 = 0x20;
const RXFIFO_RST: u32 = 1 << 17;
/// The longest format or `%s` string read before giving up on its terminator.
const MAX_STR: u32 = 4096;

/// One byte into UART `n`'s transmit FIFO.
fn uart_raw(bus: &mut SocBus, n: usize, b: u8) { bus.periph.uart[n & 1].write(0, b as u32); }

/// ets_uart_putc1's "cooked" newline: '\r' is dropped, '\n' goes out as "\r\n".
fn uart_cooked(bus: &mut SocBus, n: usize, b: u8) {
    match b {
        b'\r' => {}
        b'\n' => { uart_raw(bus, n, b'\r'); uart_raw(bus, n, b'\n'); }
        _ => uart_raw(bus, n, b),
    }
}

/// Text from the ROM itself (the boot banner) on UART0, cooked.
pub fn rom_print(bus: &mut SocBus, text: &str) { for b in text.bytes() { uart_cooked(bus, 0, b); } }

/// `b` through the ROM's character function at `f`, to the UART that `uart_buff_switch` selected
/// (0: none installed, nothing is printed).
fn put_via(bus: &mut SocBus, f: u32, b: u8) {
    let n = bus.rom.uart_sel;
    match f {
        ETS_UART_PUTC1 => uart_cooked(bus, n, b),
        ETS_PUTC | UART_TX_ONE_CHAR => uart_raw(bus, n, b),
        _ => {}
    }
}

/// Whether `f` is one of the ROM's character functions (or none), which `put_via` runs here.
fn rom_putc(f: u32) -> bool { matches!(f, ETS_UART_PUTC1 | ETS_PUTC | UART_TX_ONE_CHAR | 0) }

/// `text` through the character function `f`, then return `v` to the caller: the ROM's run here,
/// a function of the guest's is called once per character (`xtos::put_chars`).
fn emit(core: &mut Lx106, bus: &mut SocBus, f: u32, text: Vec<u8>, v: u32) {
    if !rom_putc(f) { super::xtos::put_chars(core, bus, f, text, v); return; }
    for b in text { put_via(bus, f, b); }
    ret(core, bus, v);
}

/// A NUL-terminated string at `addr`, at most `MAX_STR` bytes.
fn c_string(bus: &mut SocBus, addr: u32) -> Vec<u8> {
    let mut s = Vec::new();
    for a in addr..addr.saturating_add(MAX_STR) {
        match bus.peek8(a) { Some(0) | None => break, Some(b) => s.push(b) }
    }
    s
}

/// The subset of printf the ROM's callers use: `%d %i %u %x %X %c %s %p %%`, with the flags `-`
/// and `0`, a width, and `l`/`h` length modifiers (all arguments are 32-bit). `%p` prints like
/// `%x`. A NULL `%s` prints "(null)". Anything else is copied as written.
pub fn format(bus: &mut SocBus, fmt: u32, mut next: impl FnMut(&mut SocBus) -> u32) -> Vec<u8> {
    let f = c_string(bus, fmt);
    let mut out = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let c = f[i];
        i += 1;
        if c != b'%' { out.push(c); continue; }
        let start = i - 1;
        let (mut left, mut zero, mut width) = (false, false, 0usize);
        while i < f.len() && matches!(f[i], b'-' | b'0') { if f[i] == b'-' { left = true } else { zero = true } i += 1; }
        while i < f.len() && f[i].is_ascii_digit() { width = width * 10 + (f[i] - b'0') as usize; i += 1; }
        while i < f.len() && matches!(f[i], b'l' | b'h') { i += 1; }
        let Some(&conv) = f.get(i) else { out.extend_from_slice(&f[start..]); break };
        i += 1;
        let body: Vec<u8> = match conv {
            b'%' => { out.push(b'%'); continue; }
            b'd' | b'i' => (next(bus) as i32).to_string().into_bytes(),
            b'u' => next(bus).to_string().into_bytes(),
            b'x' | b'p' => format!("{:x}", next(bus)).into_bytes(),
            b'X' => format!("{:X}", next(bus)).into_bytes(),
            b'c' => vec![next(bus) as u8],
            b's' => match next(bus) { 0 => b"(null)".to_vec(), p => c_string(bus, p) },
            _ => { out.extend_from_slice(&f[start..i]); continue; }
        };
        let pad = width.saturating_sub(body.len());
        if left { out.extend_from_slice(&body); out.resize(out.len() + pad, b' '); continue; }
        if zero && conv != b's' && conv != b'c' {
            // zeros go after a sign
            let (sign, digits) = if body.first() == Some(&b'-') { (&body[..1], &body[1..]) } else { (&body[..0], &body[..]) };
            out.extend_from_slice(sign); out.resize(out.len() + pad, b'0'); out.extend_from_slice(digits);
        } else { out.resize(out.len() + pad, b' '); out.extend_from_slice(&body); }
    }
    out
}

/// `ets_printf(fmt, ...)`: through the installed character function; returns the length.
pub fn ets_printf(core: &mut Lx106, bus: &mut SocBus) {
    let (fmt, mut n) = (arg(core, bus, 0), 0);
    let text = format(bus, fmt, |bus| { n += 1; core.arg(bus, n) });
    let (f, n) = (bus.rom.putc1, text.len() as u32);
    emit(core, bus, f, text, n);
}

/// `ets_uart_printf(fmt, ...)`: cooked, on the selected UART, whatever `ets_install_putc1` did
/// (esp8266_undocumented.h:62-71).
pub fn ets_uart_printf(core: &mut Lx106, bus: &mut SocBus) {
    let (fmt, mut n) = (arg(core, bus, 0), 0);
    let text = format(bus, fmt, |bus| { n += 1; core.arg(bus, n) });
    let uart = bus.rom.uart_sel;
    for &b in &text { uart_cooked(bus, uart, b); }
    ret(core, bus, text.len() as u32);
}

/// `ets_vprintf(putc, fmt, va_list)` (ets_sys.h:228). The Xtensa `va_list` is a three-word struct
/// passed by value (a4..a6): the stack arguments less 32, the saved register arguments, and the
/// byte index of the next argument; an argument past the six register words is on the stack
/// (GCC gcc/config/xtensa/xtensa.cc, xtensa_gimplify_va_arg_expr).
pub fn ets_vprintf(core: &mut Lx106, bus: &mut SocBus) {
    let (putc, fmt) = (arg(core, bus, 0), arg(core, bus, 1));
    let (stk, reg, mut ndx) = (arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4));
    let text = format(bus, fmt, |bus| {
        let orig = ndx;
        ndx = orig.wrapping_add(4);
        let at = if ndx <= 24 { reg.wrapping_add(ndx - 4) } else {
            if orig <= 24 { ndx = 36; }
            stk.wrapping_add(ndx - 4)
        };
        bus.read32(at).unwrap_or(0)
    });
    let n = text.len() as u32;
    emit(core, bus, putc, text, n);
}

/// `ets_putc(c)`: raw, on the selected UART; returns 0 (ets_sys.h:230-237).
pub fn ets_putc(core: &mut Lx106, bus: &mut SocBus) {
    let (c, n) = (arg(core, bus, 0) as u8, bus.rom.uart_sel);
    uart_raw(bus, n, c);
    ret(core, bus, 0);
}

/// `uart_tx_one_char(c)`: raw, on the selected UART; returns 0.
pub fn uart_tx_one_char(core: &mut Lx106, bus: &mut SocBus) { ets_putc(core, bus) }

/// `ets_uart_putc1(c)`: cooked, on the selected UART; returns the character (ets_sys.h:242-252).
pub fn ets_uart_putc1(core: &mut Lx106, bus: &mut SocBus) {
    let (c, n) = (arg(core, bus, 0), bus.rom.uart_sel);
    uart_cooked(bus, n, c as u8);
    ret(core, bus, c & 0xff);
}

/// `ets_write_char(c)`: one character of `ets_printf`, through the installed character function.
pub fn ets_write_char(core: &mut Lx106, bus: &mut SocBus) {
    let (c, f) = (arg(core, bus, 0) as u8, bus.rom.putc1);
    emit(core, bus, f, vec![c], 0);
}

/// `ets_install_putc1(f)` (ets_sys.h:221).
pub fn ets_install_putc1(core: &mut Lx106, bus: &mut SocBus) { bus.rom.putc1 = arg(core, bus, 0); ret(core, bus, 0); }

/// `ets_install_uart_printf()`: installs the ROM's UART character function and returns it
/// (esp8266_undocumented.h:55-59).
pub fn ets_install_uart_printf(core: &mut Lx106, bus: &mut SocBus) { bus.rom.putc1 = ETS_UART_PUTC1; ret(core, bus, ETS_UART_PUTC1); }

/// `rom_uart_div_modify(uart_no, div)` (esp8266_undocumented.h:41): the UART's CLKDIV.
pub fn rom_uart_div_modify(core: &mut Lx106, bus: &mut SocBus) {
    let (n, div) = (arg(core, bus, 0) as usize, arg(core, bus, 1));
    bus.periph.uart[n & 1].write(UART_CLKDIV, div);
    ret(core, bus, 0);
}

/// `Uart_Init(uart_no)` (esp8266_undocumented.h:76): the line settings the ROM keeps already hold.
pub fn uart_init(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

/// `uart_buff_switch(n)`: selects the UART of `ets_putc`/`ets_uart_putc1`; 0 also clears UART0's
/// receive FIFO (esp8266_undocumented.h:43-53).
pub fn uart_buff_switch(core: &mut Lx106, bus: &mut SocBus) {
    let n = arg(core, bus, 0) as usize & 1;
    bus.rom.uart_sel = n;
    if n == 0 {
        let u = &mut bus.periph.uart[0];
        let conf0 = u.read(UART_CONF0);
        u.write(UART_CONF0, conf0 | RXFIFO_RST);
        u.write(UART_CONF0, conf0);
    }
    ret(core, bus, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_LOW;

    fn put(bus: &mut SocBus, at: u32, s: &[u8]) { bus.load_bytes(at, s).unwrap(); bus.load_bytes(at + s.len() as u32, &[0]).unwrap(); }

    #[test]
    fn the_printf_subset() {
        let mut bus = SocBus::new(1 << 20, [0; 6]);
        put(&mut bus, DRAM_LOW, b"v%08x %d|%5u|%-4x|%c%s%% %p %q %05d");
        put(&mut bus, DRAM_LOW + 0x100, b"ok");
        let mut args = [0x1234, -42i32 as u32, 7, 0xab, b'!' as u32, DRAM_LOW + 0x100, 0x3ffe8000, -5i32 as u32].into_iter();
        let out = format(&mut bus, DRAM_LOW, |_| args.next().unwrap());
        assert_eq!(String::from_utf8(out).unwrap(), "v00001234 -42|    7|ab  |!ok% 3ffe8000 %q -0005");
    }

    #[test]
    fn cooked_output_turns_newlines_into_crlf_and_raw_keeps_them() {
        let mut bus = SocBus::new(1 << 20, [0; 6]);
        rom_print(&mut bus, "a\r\nb\n");
        put_via(&mut bus, ETS_PUTC, b'\n');
        assert_eq!(bus.periph.uart[0].tx_out, b"a\r\nb\r\n\n");
        bus.rom.uart_sel = 1;
        put_via(&mut bus, ETS_UART_PUTC1, b'x');
        put_via(&mut bus, 0x4010_0000, b'y');
        assert_eq!(bus.periph.uart[1].tx_out, b"x");
    }
}
