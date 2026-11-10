//! A fault inside a modelled ROM function is the chip's own: an unmapped load or store in a ROM
//! function that walks guest memory raises LoadProhibited (28) / StoreProhibited (29) with EXCVADDR
//! the address and EPC1 the function's entry; an integer division by zero executes the ILL at
//! 0x4000dce5 inside `__divsi3` (EXCCAUSE 0), which the Arduino core's postmortem reports as
//! "Exception (6)" with the caller from EXCSAVE1 (cores/esp8266/core_esp8266_postmortem.cpp:168-178).
//! The guest's handler sees the exception as it sees one of its own instructions', and the machine
//! counts it.
use emu_core::{Bus, Core};
use esp8266::{rom, Machine};
use xtensa_lx7::state::{exc, ps, sr};

const VECBASE: u32 = 0x4010_0000;
const MAIN: u32 = VECBASE + 0x100;
const HANDLER: u32 = VECBASE + 0x200;
const OUT: u32 = 0x3FFE_8000;
const DATA: u32 = 0x3FFE_9000;
const SP: u32 = 0x3FFF_0000;
const EXC_TABLE: u32 = 0x3FFF_C000;
const C_TABLE: u32 = 0x3FFF_C100;
const UNMAPPED: u32 = 0x2000_0000;
/// The ILL in `__divsi3` that every 32-bit division helper of the ROM reaches on a zero divisor.
const DIV_ZERO_PC: u32 = 0x4000_dce5;

/// The SDK's user exception vector and its continuation (Blink image 0x40100050, 0x40106140), as
/// tests/xtos.rs has them: excsave1 = a0; a1 -= 256; frame a2, a3, a4; jump through the table.
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

/// A machine with the vectors and a C handler for causes 0, 28 and 29 that records what it sees
/// at OUT: the cause it is passed, EXCVADDR, the frame's EPC1 and EXCSAVE1; then it stays (`j .`).
fn machine() -> Machine {
    let mut m = esp8266::machine([0x5c, 0xcf, 0x7f, 0, 0x11, 0x22], 4 << 20);
    m.bus.rom.boot_pending = false;
    vectors(&mut m);
    for cause in [exc::ILLEGAL, exc::LOAD_PROHIBITED, exc::STORE_PROHIBITED] {
        m.bus.write32(EXC_TABLE + 4 * cause, rom::addr_of("_xtos_c_wrapper_handler")).unwrap();
        m.bus.write32(C_TABLE + 4 * cause, HANDLER).unwrap();
    }
    m.bus.load_bytes(HANDLER - 4, &OUT.to_le_bytes()).unwrap();
    m.bus.load_bytes(HANDLER, &[
        0x81, 0xff, 0xff, // l32r a8, HANDLER-4 (OUT)
        0x32, 0x68, 0x00, // s32i a3, a8, 0     the cause argument
        0x40, 0xee, 0x03, // rsr.excvaddr a4
        0x42, 0x68, 0x01, // s32i a4, a8, 4
        0x42, 0x22, 0x00, // l32i a4, a2, 0     the frame's EPC1
        0x42, 0x68, 0x02, // s32i a4, a8, 8
        0x40, 0xd1, 0x03, // rsr.excsave1 a4
        0x42, 0x68, 0x03, // s32i a4, a8, 12
        0x06, 0xff, 0xff, // j .
    ]).unwrap();
    m.bus.load_bytes(MAIN, &[0x06, 0xff, 0xff /* j . */]).unwrap();
    for w in 0..4 { m.bus.write32(OUT + 4 * w, 0xdead_0000 + w).unwrap(); }
    let c = &mut m.cores[0];
    c.write_sr(sr::VECBASE, VECBASE).unwrap();
    c.write_sr(sr::PS, ps::UM).unwrap();
    c.write_sr(sr::EXCVADDR, 0x0bad_0bad).unwrap();
    c.set_ar(1, SP);
    m
}

/// Call the ROM function `name` with `args` (a2..) from MAIN's `j .` and run a while.
fn call(m: &mut Machine, name: &str, args: &[u32]) {
    let c = &mut m.cores[0];
    c.set_pc(rom::addr_of(name));
    c.set_ar(0, MAIN);
    for (i, &a) in args.iter().enumerate() { c.set_ar(2 + i as u8, a); }
    m.run(300);
}

/// What the handler recorded: (cause, EXCVADDR, EPC1, EXCSAVE1).
fn seen(m: &mut Machine) -> (u32, u32, u32, u32) {
    let w: Vec<u32> = (0..4).map(|i| m.bus.read32(OUT + 4 * i).unwrap()).collect();
    (w[0], w[1], w[2], w[3])
}

#[test]
fn a_valid_call_returns_normally() {
    let mut m = machine();
    m.bus.load_bytes(DATA, b"abc\0").unwrap();
    call(&mut m, "strlen", &[DATA]);
    assert_eq!((m.cores[0].pc(), m.cores[0].get_ar(2), m.exceptions), (MAIN, 3, 0));
    // the mask ROM's own bytes are not modelled: they read 0, as a data load there does
    let mut m = machine();
    call(&mut m, "strlen", &[0x4000_0000]);
    assert_eq!((m.cores[0].pc(), m.cores[0].get_ar(2), m.exceptions), (MAIN, 0, 0));
    let mut m = machine();
    call(&mut m, "__udivsi3", &[100, 7]);
    assert_eq!((m.cores[0].pc(), m.cores[0].get_ar(2), m.exceptions), (MAIN, 14, 0));
}

#[test]
fn an_unmapped_load_or_store_in_a_rom_function_is_load_or_store_prohibited() {
    let fmt = DATA + 0x100;
    let (addrs, lens) = (DATA + 0x200, DATA + 0x210);
    let cases: &[(&str, Vec<u32>, u32, u32)] = &[
        ("strlen", vec![0], exc::LOAD_PROHIBITED, 0),
        ("ets_strlen", vec![UNMAPPED + 3], exc::LOAD_PROHIBITED, UNMAPPED + 3),
        ("memset", vec![0, 0, 16], exc::STORE_PROHIBITED, 0),
        ("memcpy", vec![DATA + 0x400, UNMAPPED, 4], exc::LOAD_PROHIBITED, UNMAPPED),
        ("memcpy", vec![UNMAPPED, DATA, 4], exc::STORE_PROHIBITED, UNMAPPED),
        ("memmove", vec![DATA + 0x400, UNMAPPED, 4], exc::LOAD_PROHIBITED, UNMAPPED),
        ("bzero", vec![UNMAPPED, 4], exc::STORE_PROHIBITED, UNMAPPED),
        ("memcmp", vec![DATA, UNMAPPED, 4], exc::LOAD_PROHIBITED, UNMAPPED),
        ("strcmp", vec![DATA, 0], exc::LOAD_PROHIBITED, 0),
        ("strncmp", vec![0, DATA, 4], exc::LOAD_PROHIBITED, 0),
        ("strcpy", vec![UNMAPPED, DATA], exc::STORE_PROHIBITED, UNMAPPED),
        ("strncpy", vec![DATA + 0x400, 0, 4], exc::LOAD_PROHIBITED, 0),
        ("strstr", vec![0, DATA], exc::LOAD_PROHIBITED, 0),
        // the flash window is read-only
        ("memset", vec![0x4020_0000, 0, 4], exc::STORE_PROHIBITED, 0x4020_0000),
        // `%s` with a pointer to nowhere (a NULL one prints "(null)"), and a format there
        ("ets_printf", vec![fmt, UNMAPPED], exc::LOAD_PROHIBITED, UNMAPPED),
        ("ets_printf", vec![UNMAPPED + 8], exc::LOAD_PROHIBITED, UNMAPPED + 8),
        ("SPIRead", vec![0x1000, UNMAPPED, 8], exc::STORE_PROHIBITED, UNMAPPED),
        ("SPIWrite", vec![0x1000, UNMAPPED, 8], exc::LOAD_PROHIBITED, UNMAPPED),
        ("SPI_read_status", vec![0, UNMAPPED], exc::STORE_PROHIBITED, UNMAPPED),
        ("md5_vector", vec![1, addrs, lens, DATA + 0x400], exc::LOAD_PROHIBITED, UNMAPPED),
        ("md5_vector", vec![1, addrs + 4, lens, UNMAPPED], exc::STORE_PROHIBITED, UNMAPPED),
        ("MD5Init", vec![UNMAPPED], exc::STORE_PROHIBITED, UNMAPPED),
        ("lldesc_num2link", vec![UNMAPPED, 2], exc::LOAD_PROHIBITED, UNMAPPED + 8),
    ];
    for (name, args, cause, vaddr) in cases {
        let mut m = machine();
        m.bus.load_bytes(DATA, b"abcd\0").unwrap();
        m.bus.load_bytes(fmt, b"<%s>\0").unwrap();
        m.bus.load_bytes(addrs, &[UNMAPPED.to_le_bytes(), DATA.to_le_bytes()].concat()).unwrap();
        m.bus.load_bytes(lens, &4u32.to_le_bytes()).unwrap();
        call(&mut m, name, args);
        let entry = rom::addr_of(name);
        assert_eq!(seen(&mut m), (*cause, *vaddr, entry, MAIN), "{name}{args:x?}");
        assert_eq!(m.exceptions, 1, "{name}: the machine counts it");
        assert_eq!(m.cores[0].pc(), HANDLER + 24, "{name}: in the handler's `j .`");
    }
}

#[test]
fn a_rom_post_into_a_queue_that_is_nowhere_faults() {
    let mut m = machine();
    m.bus.rom.tasks[3] = rom::sched::Task { func: MAIN, queue: UNMAPPED, len: 4, head: 0, count: 0 };
    call(&mut m, "ets_post_rom", &[3, 1, 2]);
    assert_eq!(seen(&mut m), (exc::STORE_PROHIBITED, UNMAPPED, rom::addr_of("ets_post_rom"), MAIN));
    assert_eq!(m.bus.rom.tasks[3].count, 0, "nothing queued");
}

#[test]
fn an_integer_division_by_zero_is_the_ill_at_0x4000dce5() {
    for (name, args) in [("__divsi3", vec![100, 0]), ("__udivsi3", vec![7, 0]), ("__umodsi3", vec![7, 0]),
                         ("__divdi3", vec![5, 0, 0, 0]), ("__udivdi3", vec![5, 0, 0, 0]), ("__umoddi3", vec![5, 0, 0, 0])] {
        let mut m = machine();
        call(&mut m, name, &args);
        // EXCVADDR as it was (an ILL does not write it); EXCSAVE1 = a0, the caller, which the
        // core's postmortem prints in place of EPC1
        assert_eq!(seen(&mut m), (exc::ILLEGAL, 0x0bad_0bad, DIV_ZERO_PC, MAIN), "{name}");
        assert_eq!(m.exceptions, 1, "{name}");
    }
}
