//! The ROM's interrupt and exception layer (Tensilica XTOS as the ESP8266 ROM exports it,
//! eagle.rom.addr.v6.ld) and the mechanism by which a modelled ROM function calls into the guest.
//!
//! The image's own vectors (VECBASE 0x40100000) take every exception: the user vector reserves a
//! 256-byte frame below a1, stores a2, a3 and a4 in it and jumps through
//! `_xtos_exc_handler_table[EXCCAUSE]` with a1 = the frame, a2 = EXCCAUSE and a0 restored (Blink
//! image 0x40100050 `_UserExceptionVector`, 0x40106140 `_UserExceptionVector_1`). The handlers it
//! jumps to are here: `_xtos_l1int_handler` (cause 4, interrupts) and `_xtos_c_wrapper_handler`
//! (a C handler of `_xtos_set_exception_handler`'s). Both complete the frame the way XTOS does
//! (`struct __exception_frame`, cores/esp8266/esp8266_undocumented.h and the frame offsets
//! `exc-c-wrapper-handler.S` checks: +0 epc1, +4 ps, +8 sar, +12 vpri, +16 a0, +20 a2 .. +72 a15,
//! +76 exccause), call the guest's handler on the stack below the frame, then restore every
//! register from the frame and return from the exception (RFE), so the interrupted code finds
//! a0 and a2-a15 as it left them and a handler that edits the frame (the core's
//! `core_esp8266_non32xfer.cpp` writes the loaded value into it) is honoured.
//!
//! A call into the guest (`call_guest`) sets a0 to a private return address, 0x4000FD00 + 4 *
//! depth, and records a `GuestCall`; when the guest returns there, `dispatch` hands the call to
//! its `resume`, which continues the ROM function's work. Calls nest (an interrupt during a
//! character function, an exception in an interrupt handler) and return in order.
use super::{arg, ret, Lx106, SocBus};
use emu_core::{Bus, Core};
use xtensa_lx7::state::{ps, sr};

/// The return addresses of guest calls: one word per nesting depth.
pub const GUEST_RET: u32 = 0x4000_FD00;
pub const GUEST_RET_END: u32 = 0x4000_FE00;
const MAX_DEPTH: usize = ((GUEST_RET_END - GUEST_RET) / 4) as usize;

/// `_xtos_exc_handler_table` and `_xtos_c_handler_table` (eagle.rom.addr.v6.ld).
pub const EXC_HANDLER_TABLE: u32 = 0x3fff_c000;
pub const C_HANDLER_TABLE: u32 = 0x3fff_c100;
/// XCHAL_EXCCAUSE_NUM, the table length (exc-sethandler.cpp:29 bounds the cause by it).
const EXCCAUSE_NUM: u32 = 64;

/// The frame offsets (exc-c-wrapper-handler.S, UEXC_*).
const F_PC: u32 = 0;
const F_PS: u32 = 4;
const F_SAR: u32 = 8;
const F_A0: u32 = 16;
const F_A2: u32 = 20;
const F_CAUSE: u32 = 76;
/// The frame sits this far below the interrupted code's a1.
const FRAME_SIZE: u32 = 256;

/// The LX106's level-1 interrupts are lines 0..13; the NMI (14) has its own vector (core-isa.h,
/// xtensa_lx7::lx106). The edge-triggered lines and the software interrupt are cleared through
/// INTCLEAR before their handler runs, as XTOS's dispatcher does; level lines stay until the
/// device drops them.
const LEVEL1_LINES: u32 = 0x3fff;

/// A call the ROM made into the guest, waiting for its return.
pub struct GuestCall {
    /// where the guest returns: `GUEST_RET + 4 * depth`
    pub ret_slot: u32,
    /// a0..a15 when the call was made
    pub regs: [u32; 16],
    /// the ROM's work after the return (the guest's result is in a2)
    pub resume: fn(&mut Lx106, &mut SocBus, GuestCall),
    /// what `resume` needs: the exception frame, an index
    pub data: [u32; 2],
    /// characters still to go through a guest character function
    pub text: Vec<u8>,
}

/// Call the guest's function `f` with `args` (a2..), to continue in `resume` when it returns.
/// The registers at the call are kept in the record; the stack is the caller's a1.
pub fn call_guest(core: &mut Lx106, bus: &mut SocBus, f: u32, args: &[u32], resume: fn(&mut Lx106, &mut SocBus, GuestCall), data: [u32; 2], text: Vec<u8>) {
    let depth = bus.rom.calls.len();
    if depth >= MAX_DEPTH {
        eprintln!("[emu] rom: calls from the ROM into the guest nest {depth} deep (at {f:#010x}): stopping");
        bus.rom.request_stop();
        return;
    }
    let ret_slot = GUEST_RET + 4 * depth as u32;
    let regs = std::array::from_fn(|i| core.get_ar(i as u8));
    bus.rom.calls.push(GuestCall { ret_slot, regs, resume, data, text });
    for (i, &a) in args.iter().take(6).enumerate() { core.set_ar(2 + i as u8, a); }
    core.set_ar(0, ret_slot);
    core.set_pc(f);
    core.spin(1);
}

/// The guest returned to `pc` in the guest-call range: resume the call it ends.
pub fn returned(core: &mut Lx106, bus: &mut SocBus, pc: u32) {
    let top = bus.rom.calls.last().map(|c| c.ret_slot);
    if top != Some(pc) {
        eprintln!("[emu] rom: the guest returned to {pc:#010x}, but the open call from the ROM returns to {}: stopping",
                  top.map_or("nothing".into(), |t| format!("{t:#010x}")));
        bus.rom.request_stop();
        return;
    }
    let call = bus.rom.calls.pop().unwrap();
    (call.resume)(core, bus, call);
}

/// Put back the registers of the call's maker, return `v` to its a0.
pub fn finish(core: &mut Lx106, bus: &mut SocBus, call: &GuestCall, v: u32) {
    for (i, &r) in call.regs.iter().enumerate() { core.set_ar(i as u8, r); }
    ret(core, bus, v);
}

fn rd(bus: &mut SocBus, a: u32) -> u32 { bus.read32(a).unwrap_or(0) }
fn wr(bus: &mut SocBus, a: u32, v: u32) { let _ = bus.write32(a, v); }
fn sreg(core: &mut Lx106, n: u32) -> u32 { core.read_sr(n).unwrap_or(0) }

/// What XTOS stores on top of what the user vector stored (a2..a4): the interrupted pc, PS, SAR,
/// a0, a5..a15 and the cause.
fn save_frame(core: &mut Lx106, bus: &mut SocBus, frame: u32) {
    let (epc1, ps, sar, cause) = (sreg(core, sr::EPC1), sreg(core, sr::PS), sreg(core, sr::SAR), sreg(core, sr::EXCCAUSE));
    wr(bus, frame + F_PC, epc1);
    wr(bus, frame + F_PS, ps);
    wr(bus, frame + F_SAR, sar);
    wr(bus, frame + F_A0, core.get_ar(0));
    for r in 5..16u8 { wr(bus, frame + F_A2 + 4 * (u32::from(r) - 2), core.get_ar(r)); }
    wr(bus, frame + F_CAUSE, cause);
}

/// `_xtos_return_from_exc`: every register from the frame, a1 above it, then RFE (PS.EXCM
/// cleared, pc = EPC1).
fn return_from_exception(core: &mut Lx106, bus: &mut SocBus, frame: u32) {
    core.set_ar(0, rd(bus, frame + F_A0));
    for r in 2..16u8 { let v = rd(bus, frame + F_A2 + 4 * (u32::from(r) - 2)); core.set_ar(r, v); }
    core.set_ar(1, frame.wrapping_add(FRAME_SIZE));
    let (pc, ps_saved, sar) = (rd(bus, frame + F_PC), rd(bus, frame + F_PS), rd(bus, frame + F_SAR));
    core.write_sr(sr::SAR, sar);
    core.write_sr(sr::EPC1, pc);
    core.write_sr(sr::PS, ps_saved & !ps::EXCM);
    core.set_pc(pc);
    core.spin(1);
}

/// The PS a handler runs with: the interrupted mode bits, EXCM clear, INTLEVEL 1 (XCHAL_EXCM_LEVEL),
/// so level-1 interrupts wait and the NMI does not (exc-c-wrapper-handler.S: "INTLEVEL =
/// EXCM_LEVEL = 1"; `ETS_INTR_WITHINISR` in ets_sys.h tests INTLEVEL != 0).
fn handler_ps(frame_ps: u32) -> u32 { (frame_ps & !(ps::EXCM | ps::INTLEVEL_MASK)) | 1 }

// ------------------------------------------------------------------ interrupts

/// `_xtos_l1int_handler`: the level-1 interrupt dispatcher. Serves every pending, enabled line in
/// turn (lowest number first, a model choice: XTOS's own order follows its priority table),
/// calling the handler `ets_isr_attach` gave it with (arg, frame), then returns from the exception.
pub fn l1int_handler(core: &mut Lx106, bus: &mut SocBus) {
    let frame = core.get_ar(1);
    save_frame(core, bus, frame);
    serve_next(core, bus, frame);
}

fn serve_next(core: &mut Lx106, bus: &mut SocBus, frame: u32) {
    loop {
        let pending = sreg(core, sr::INTERRUPT) & sreg(core, sr::INTENABLE) & LEVEL1_LINES;
        if pending == 0 { return_from_exception(core, bus, frame); return; }
        let n = pending.trailing_zeros();
        core.write_sr(sr::INTCLEAR, 1 << n);
        let (f, a) = bus.rom.isr[n as usize];
        if f == 0 {
            // XTOS's default handler is `_xtos_unhandled_interrupt`; the line is masked so the
            // run goes on
            bus.rom.log_once("no-isr", || format!("interrupt {n} is pending and enabled with no handler: masked"));
            let en = sreg(core, sr::INTENABLE);
            core.write_sr(sr::INTENABLE, en & !(1 << n));
            continue;
        }
        core.set_ar(1, frame);
        let p = handler_ps(rd(bus, frame + F_PS));
        core.write_sr(sr::PS, p);
        call_guest(core, bus, f, &[a, frame], isr_done, [frame, 0], Vec::new());
        return;
    }
}

fn isr_done(core: &mut Lx106, bus: &mut SocBus, call: GuestCall) { serve_next(core, bus, call.data[0]); }

/// `ets_isr_attach(n, handler, arg)` (ets_sys.h:68-74: the handler gets (arg, frame)).
pub fn ets_isr_attach(core: &mut Lx106, bus: &mut SocBus) {
    let (n, f, a) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2));
    bus.rom.isr[(n & 15) as usize] = (f, a);
    ret(core, bus, 0);
}

/// `ets_isr_mask(mask)` / `ets_isr_unmask(mask)` (ets_sys.h:101-105 ETS_INTR_DISABLE/ENABLE):
/// INTENABLE &= !mask / |= mask.
pub fn ets_isr_mask(core: &mut Lx106, bus: &mut SocBus) { let m = arg(core, bus, 0); ints(core, bus, m, false); }
pub fn ets_isr_unmask(core: &mut Lx106, bus: &mut SocBus) { let m = arg(core, bus, 0); ints(core, bus, m, true); }

/// `_xtos_ints_on(mask)` / `_xtos_ints_off(mask)` (Xtensa XTOS xtruntime.h): the same change of
/// INTENABLE, returning the previous INTENABLE.
pub fn xtos_ints_on(core: &mut Lx106, bus: &mut SocBus) { let m = arg(core, bus, 0); ints(core, bus, m, true); }
pub fn xtos_ints_off(core: &mut Lx106, bus: &mut SocBus) { let m = arg(core, bus, 0); ints(core, bus, m, false); }

fn ints(core: &mut Lx106, bus: &mut SocBus, mask: u32, on: bool) {
    let old = sreg(core, sr::INTENABLE);
    core.write_sr(sr::INTENABLE, if on { old | mask } else { old & !mask });
    ret(core, bus, old);
}

/// `_xtos_set_intlevel(level)` (xtruntime.h XTOS_SET_INTLEVEL): PS.INTLEVEL = level, returning
/// the previous PS.
pub fn xtos_set_intlevel(core: &mut Lx106, bus: &mut SocBus) {
    let (level, old) = (arg(core, bus, 0), sreg(core, sr::PS));
    core.write_sr(sr::PS, (old & !ps::INTLEVEL_MASK) | (level & 0xf));
    ret(core, bus, old);
}

// ------------------------------------------------------------------ exceptions

/// `_xtos_set_exception_handler(cause, fn)`: as the Arduino core's own copy of it
/// (cores/esp8266/exc-sethandler.cpp:24-46): the C handler goes into `_xtos_c_handler_table`,
/// the cause's entry of `_xtos_exc_handler_table` becomes `_xtos_c_wrapper_handler` (or
/// `_xtos_unhandled_exception` for none), and the previous C handler is returned (0 for none).
pub fn set_exception_handler(core: &mut Lx106, bus: &mut SocBus) {
    let (cause, f) = (arg(core, bus, 0), arg(core, bus, 1));
    if cause >= EXCCAUSE_NUM { ret(core, bus, 0); return; }
    let none = super::addr_of("_xtos_p_none");
    let f = if f == 0 { none } else { f };
    let old = rd(bus, C_HANDLER_TABLE + 4 * cause);
    let entry = super::addr_of(if f == none { "_xtos_unhandled_exception" } else { "_xtos_c_wrapper_handler" });
    wr(bus, EXC_HANDLER_TABLE + 4 * cause, entry);
    wr(bus, C_HANDLER_TABLE + 4 * cause, f);
    ret(core, bus, if old == none { 0 } else { old });
}

/// `_xtos_c_wrapper_handler`: calls `_xtos_c_handler_table[cause]` with (frame, cause) at
/// INTLEVEL 1 (exc-c-wrapper-handler.S; the ROM's original then lowers INTLEVEL to 0 with
/// `rsil a13, 0`, which the core's copy removed; this model keeps 1, so an interrupt waits for
/// the handler's return), then returns from the exception through the frame, which the handler
/// may have changed.
pub fn c_wrapper_handler(core: &mut Lx106, bus: &mut SocBus) {
    let frame = core.get_ar(1);
    save_frame(core, bus, frame);
    let cause = sreg(core, sr::EXCCAUSE) & (EXCCAUSE_NUM - 1);
    let f = rd(bus, C_HANDLER_TABLE + 4 * cause);
    if f == 0 { return_from_exception(core, bus, frame); return; }
    let p = handler_ps(rd(bus, frame + F_PS));
    core.write_sr(sr::PS, p);
    call_guest(core, bus, f, &[frame, cause], c_handler_done, [frame, 0], Vec::new());
}

fn c_handler_done(core: &mut Lx106, bus: &mut SocBus, call: GuestCall) { return_from_exception(core, bus, call.data[0]); }

/// `_xtos_p_none`: XTOS's empty handler.
pub fn p_none(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

/// `_xtos_unhandled_exception`: no handler for the cause (a2). The ROM would spin until the
/// watchdog resets the chip; this model stops the run.
pub fn unhandled_exception(core: &mut Lx106, bus: &mut SocBus) {
    let c = core.state();
    eprintln!("[emu] rom: unhandled exception: EXCCAUSE {} at {:#010x} (EXCVADDR {:#010x}); stopping", core.get_ar(2), c.epc[1], c.excvaddr);
    bus.rom.request_stop();
}

// ------------------------------------------------------------------ character output

/// Send `text` through the guest's character function `f`, one call per character, then return
/// `v` to the caller of the ROM function.
pub fn put_chars(core: &mut Lx106, bus: &mut SocBus, f: u32, text: Vec<u8>, v: u32) {
    if text.is_empty() { ret(core, bus, v); return; }
    call_guest(core, bus, f, &[u32::from(text[0])], next_char, [f, v], text);
}

fn next_char(core: &mut Lx106, bus: &mut SocBus, mut call: GuestCall) {
    call.text.remove(0);
    let [f, v] = call.data;
    if call.text.is_empty() { finish(core, bus, &call, v); return; }
    // the next character from the caller's registers, as the first was sent
    for (i, &r) in call.regs.iter().enumerate() { core.set_ar(i as u8, r); }
    let c = u32::from(call.text[0]);
    call_guest(core, bus, f, &[c], next_char, [f, v], std::mem::take(&mut call.text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_is_the_one_exc_c_wrapper_handler_s_checks() {
        // struct __exception_frame: epc, ps, sar, vpri, a0, a2..a15, exccause (19 words + 1)
        assert_eq!((F_PC, F_PS, F_SAR, F_A0, F_A2, F_CAUSE), (0, 4, 8, 16, 20, 76));
        assert_eq!(F_A2 + 4 * 13, 72, "a15");
        assert_eq!(MAX_DEPTH, 64);
    }
}
