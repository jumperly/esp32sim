//! The ROM's interrupt dispatcher and C exception wrapper around a guest handler: the guest's
//! user exception vector (the SDK's, as in the Blink image at 0x40100050 and 0x40106140) jumps
//! through `_xtos_exc_handler_table`; the handler runs on the stack below the exception frame and
//! may clobber every register; afterwards the interrupted code finds a0 and a2..a15, a1, SAR and
//! PS as it left them, or as the handler rewrote them in the frame.
use emu_core::{Bus, Core};
use esp8266::{rom, Machine};
use xtensa_lx7::state::{ps, sr};

const VECBASE: u32 = 0x4010_0000;
const MAIN: u32 = VECBASE + 0x100;
const HANDLER: u32 = VECBASE + 0x200;
const OUT: u32 = 0x3FFE_8000;
const SP: u32 = 0x3FFF_0000;
const EXC_TABLE: u32 = 0x3FFF_C000;

/// `movi.n aR, v` (0 <= v < 16).
fn movi_n(r: u8, v: u8) -> [u8; 2] { [0x0c, v << 4 | r] }

/// The SDK's user exception vector and its continuation (Blink image 0x40100050, 0x40106140):
/// excsave1 = a0; a1 -= 256; frame a2, a3, a4; jump through the table by EXCCAUSE.
fn vectors(m: &mut Machine) {
    m.bus.load_bytes(VECBASE + 0x50, &[0x00, 0xd1, 0x13, /* wsr.excsave1 a0 */ 0xc5, 0x02, 0x00 /* call0 VECBASE+0x80 */]).unwrap();
    m.bus.load_bytes(VECBASE + 0x7c, &EXC_TABLE.to_le_bytes()).unwrap();
    m.bus.load_bytes(VECBASE + 0x80, &[
        0x00, 0xd1, 0x03, // rsr.excsave1 a0
        0x12, 0xd1, 0xff, // addmi a1, a1, -256
        0x29, 0x51,       // s32i.n a2, a1, 20
        0x39, 0x61,       // s32i.n a3, a1, 24
        0x31, 0xfc, 0xff, // l32r a3, VECBASE+0x7c
        0x20, 0xe8, 0x03, // rsr.exccause a2
        0x30, 0x32, 0xa0, // addx4 a3, a2, a3
        0x38, 0x03,       // l32i.n a3, a3, 0
        0x49, 0x71,       // s32i.n a4, a1, 28
        0xa0, 0x03, 0x00, // jx a3
    ]).unwrap();
}

/// A machine with the vectors, the table's interrupt entry, and a0..a15 = 0x1000 + n (a1 = SP).
fn machine() -> Machine {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.bus.rom.boot_pending = false;
    vectors(&mut m);
    m.bus.write32(EXC_TABLE + 4 * 4, rom::addr_of("_xtos_l1int_handler")).unwrap();
    let c = &mut m.cores[0];
    c.write_sr(sr::VECBASE, VECBASE).unwrap();
    c.write_sr(sr::PS, ps::UM).unwrap();
    c.write_sr(sr::SAR, 13).unwrap();
    for r in 0..16 { c.set_ar(r, 0x1000 + u32::from(r)); }
    c.set_ar(1, SP);
    m
}

/// The registers are as `machine` set them, except `a5`.
fn assert_preserved(m: &Machine, a5: u32) {
    let c = &m.cores[0];
    for r in (0..16).filter(|&r| r != 1 && r != 5) { assert_eq!(c.get_ar(r), 0x1000 + u32::from(r), "a{r}"); }
    assert_eq!((c.get_ar(1), c.get_ar(5)), (SP, a5));
    assert_eq!(c.state().sar, 13);
    assert_eq!(c.state().ps & (ps::EXCM | ps::INTLEVEL_MASK | ps::UM), ps::UM);
}

/// Handler code that clobbers a2..a15 (but a0, its return) and returns.
fn clobber_and_return() -> Vec<u8> {
    let mut v: Vec<u8> = (2..16).flat_map(|r| movi_n(r, 15)).collect();
    v.extend([0x0d, 0xf0]); // ret.n
    v
}

#[test]
fn an_interrupt_handler_gets_arg_and_frame_and_the_interrupted_code_its_registers() {
    let mut m = machine();
    // the handler: OUT = a2 (arg), OUT + 4 = a3 (frame), then clobber everything
    let mut isr = vec![0x81, 0xff, 0xff, /* l32r a8, HANDLER-4 */ 0x22, 0x68, 0x00, /* s32i a2, a8, 0 */ 0x32, 0x68, 0x01 /* s32i a3, a8, 4 */];
    isr.extend(clobber_and_return());
    m.bus.load_bytes(HANDLER - 4, &OUT.to_le_bytes()).unwrap();
    m.bus.load_bytes(HANDLER, &isr).unwrap();
    m.bus.load_bytes(MAIN, &[0x06, 0xff, 0xff /* j . */]).unwrap();
    m.bus.rom.isr[7] = (HANDLER, 0xabcd);
    let c = &mut m.cores[0];
    c.set_pc(MAIN);
    c.write_sr(sr::INTENABLE, 1 << 7).unwrap();
    c.write_sr(sr::INTSET, 1 << 7).unwrap();   // the software interrupt
    m.run(200);
    assert_eq!(m.bus.read32(OUT).unwrap(), 0xabcd, "the handler's argument");
    assert_eq!(m.bus.read32(OUT + 4).unwrap(), SP - 256, "the frame");
    assert_eq!(m.interrupts, 1);
    assert_eq!(m.cores[0].state().interrupt & 1 << 7, 0, "the dispatcher cleared the software interrupt");
    assert_eq!(m.cores[0].pc(), MAIN);
    assert!(m.bus.rom.calls.is_empty());
    assert_preserved(&m, 0x1005);
    // the frame as XTOS lays it out: epc1, ps, sar, a0, a2..a15, cause
    let frame = SP - 256;
    assert_eq!(m.bus.read32(frame).unwrap(), MAIN);
    assert_eq!(m.bus.read32(frame + 4).unwrap() & ps::EXCM, ps::EXCM);
    assert_eq!(m.bus.read32(frame + 8).unwrap(), 13);
    assert_eq!(m.bus.read32(frame + 16).unwrap(), 0x1000);
    assert_eq!(m.bus.read32(frame + 20).unwrap(), 0x1002);
    assert_eq!(m.bus.read32(frame + 72).unwrap(), 0x100f);
    assert_eq!(m.bus.read32(frame + 76).unwrap(), 4, "LEVEL1_INTERRUPT");
}

#[test]
fn a_c_exception_handler_rewrites_the_frame_and_the_faulting_code_resumes_from_it() {
    let mut m = machine();
    // the handler (frame, cause): epc += 3, a5 = 0x55 in the frame, clobber everything
    let mut h = vec![0x42, 0x22, 0x00, /* l32i a4, a2, 0 */ 0x42, 0xc4, 0x03, /* addi a4, a4, 3 */ 0x42, 0x62, 0x00, /* s32i a4, a2, 0 */
                     0x5c, 0x54, /* movi.n a4, 0x55 */ 0x42, 0x62, 0x08 /* s32i a4, a2, 32 (a5) */];
    h.extend(clobber_and_return());
    m.bus.load_bytes(HANDLER, &h).unwrap();
    // l8ui a5, a9, 0 from IRAM: a LoadStoreError (cause 3); then j .
    m.bus.load_bytes(MAIN, &[0x52, 0x09, 0x00, /* l8ui a5, a9, 0 */ 0x06, 0xff, 0xff /* j . */]).unwrap();
    // _xtos_set_exception_handler(3, HANDLER) returns 0 (none before) to MAIN, which faults
    let c = &mut m.cores[0];
    c.set_pc(rom::addr_of("_xtos_set_exception_handler"));
    c.set_ar(0, MAIN);
    c.set_ar(2, 3);
    c.set_ar(3, HANDLER);
    c.set_ar(9, VECBASE + 0x400);
    m.run(200);
    assert_eq!(m.bus.read32(EXC_TABLE + 12).unwrap(), rom::addr_of("_xtos_c_wrapper_handler"));
    assert_eq!(m.bus.read32(EXC_TABLE + 0x100 + 12).unwrap(), HANDLER, "_xtos_c_handler_table");

    assert_eq!(m.exceptions, 1);
    assert_eq!(m.cores[0].pc(), MAIN + 3, "resumed past the load");
    assert!(m.bus.rom.calls.is_empty());
    let c = &mut m.cores[0];
    assert_eq!((c.get_ar(0), c.get_ar(2), c.get_ar(3), c.get_ar(9)), (MAIN, 0, HANDLER, VECBASE + 0x400));
    c.set_ar(9, 0x1009);
    c.set_ar(2, 0x1002);
    c.set_ar(3, 0x1003);
    c.set_ar(0, 0x1000);
    assert_preserved(&m, 0x55);
    assert_eq!(m.bus.read32(SP - 256 + 76).unwrap(), 3, "LOAD_STORE_ERROR");
}
