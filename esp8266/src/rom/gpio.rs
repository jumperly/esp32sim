//! The ROM's small device helpers: the GPIO API (tools/sdk/include/gpio.h:74-121, over the
//! GPIO registers of esp8266_peri.h:48-87), the DMA descriptor chains (`lldesc_*`, the ROM
//! prototypes ESP-IDF's components/esp_rom/include/esp32/rom/lldesc.h documents for the same
//! ROM code), `roundup2`, the CPU frequency the ROM keeps (ets_sys.h `ets_update_cpu_frequency`,
//! `ets_get_cpu_frequency`) and `ets_rtc_int_register`.
use super::{arg, ret, Lx106, SocBus};
use crate::periph::PERIPH_BASE;
use emu_core::Bus;

/// GPIO_OUT_W1TS 0x304, GPIO_OUT_W1TC 0x308, GPIO_ENABLE_W1TS 0x310, GPIO_ENABLE_W1TC 0x314,
/// GPIO_IN 0x318, GPIO_PIN0 0x328 (esp8266_peri.h:48-66).
const GPIO: u32 = PERIPH_BASE + 0x300;

fn reg_write(bus: &mut SocBus, off: u32, v: u32) { let _ = bus.write32(GPIO + off, v); }
fn reg_read(bus: &mut SocBus, off: u32) -> u32 { bus.read32(GPIO + off).unwrap_or(0) }

/// `gpio_output_set(set_mask, clear_mask, enable_mask, disable_mask)`.
pub fn gpio_output_set(core: &mut Lx106, bus: &mut SocBus) {
    let (s, c, e, d) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3));
    reg_write(bus, 0x04, s);
    reg_write(bus, 0x08, c);
    reg_write(bus, 0x10, e);
    reg_write(bus, 0x14, d);
    ret(core, bus, 0);
}

/// `gpio_input_get()`: GPIO_IN.
pub fn gpio_input_get(core: &mut Lx106, bus: &mut SocBus) { let v = reg_read(bus, 0x18); ret(core, bus, v); }

/// GPIO_PINn (esp8266_peri.h:84-85): WAKEUP_ENABLE bit 10, INT_TYPE bits 7..9 (4 low level,
/// 5 high level, the only types a wake-up takes).
const PIN_WAKEUP: u32 = 1 << 10;
const PIN_INT_TYPE: u32 = 7 << 7;

/// `gpio_pin_wakeup_enable(i, intr_state)`: pin `i` wakes the chip from light sleep at the level
/// `intr_state` (GPIO_PIN_INTR_LOLEVEL 4 or HILEVEL 5; others are ignored).
pub fn gpio_pin_wakeup_enable(core: &mut Lx106, bus: &mut SocBus) {
    let (i, state) = (arg(core, bus, 0), arg(core, bus, 1));
    if i < 16 && matches!(state, 4 | 5) {
        let v = reg_read(bus, 0x28 + 4 * i);
        reg_write(bus, 0x28 + 4 * i, (v & !PIN_INT_TYPE) | state << 7 | PIN_WAKEUP);
    }
    ret(core, bus, 0);
}

/// `gpio_pin_wakeup_disable()`: no pin wakes the chip; a pin that did loses its level interrupt.
pub fn gpio_pin_wakeup_disable(core: &mut Lx106, bus: &mut SocBus) {
    for i in 0..16 {
        let v = reg_read(bus, 0x28 + 4 * i);
        if v & PIN_WAKEUP != 0 { reg_write(bus, 0x28 + 4 * i, v & !(PIN_WAKEUP | PIN_INT_TYPE)); }
    }
    ret(core, bus, 0);
}

/// `lldesc_t` (lldesc.h): word 0 = size (12 bits) | length (12 bits) << 12 | offset << 24 |
/// sosf << 29 | eof << 30 | owner << 31; word 1 the buffer; word 2 the next descriptor.
const DESC: u32 = 12;

/// `lldesc_build_chain(descptr, desclen, mblkptr, buflen, blksz, owner, &head, &tail)` (the last
/// two on the stack): one descriptor per `blksz` bytes of the buffer, as many as fit in
/// `desclen` bytes, linked in order, each with its block size and the owner bit (digitake
/// esp8266-emulator src/rom.c:555-573 builds the same).
pub fn lldesc_build_chain(core: &mut Lx106, bus: &mut SocBus) {
    let (desc, desclen, buf, buflen, blksz, owner) = (arg(core, bus, 0), arg(core, bus, 1), arg(core, bus, 2), arg(core, bus, 3), arg(core, bus, 4), arg(core, bus, 5) & 1);
    let (headp, tailp) = (arg(core, bus, 6), arg(core, bus, 7));
    let blocks = if blksz == 0 { 0 } else { buflen.div_ceil(blksz).min(desclen / DESC) };
    let (mut first, mut prev) = (0, 0);
    for i in 0..blocks {
        let d = desc + DESC * i;
        let size = if i == blocks - 1 && buflen % blksz != 0 { buflen % blksz } else { blksz };
        let _ = bus.write32(d, (size & 0xfff) | owner << 31);
        let _ = bus.write32(d + 4, buf + i * blksz);
        let _ = bus.write32(d + 8, 0);
        if prev != 0 { let _ = bus.write32(prev + 8, d); } else { first = d; }
        prev = d;
    }
    if headp != 0 { let _ = bus.write32(headp, first); }
    if tailp != 0 { let _ = bus.write32(tailp, prev); }
    ret(core, bus, 0);
}

/// `lldesc_num2link(head, nblks)`: the `nblks`-th descriptor of the chain (the head is the first).
pub fn lldesc_num2link(core: &mut Lx106, bus: &mut SocBus) {
    let (mut d, n) = (arg(core, bus, 0), arg(core, bus, 1) & 0xffff);
    for _ in 1..n { if d == 0 { break; } d = bus.read32(d + 8).unwrap_or(0); }
    ret(core, bus, d);
}

/// `lldesc_set_owner(head, nblks, owner)`: the owner bit of the first `nblks` descriptors; the
/// last of them.
pub fn lldesc_set_owner(core: &mut Lx106, bus: &mut SocBus) {
    let (mut d, n, owner) = (arg(core, bus, 0), arg(core, bus, 1) & 0xffff, arg(core, bus, 2) & 1);
    let mut last = d;
    for _ in 0..n {
        if d == 0 { break; }
        let w = bus.read32(d).unwrap_or(0);
        let _ = bus.write32(d, (w & !(1 << 31)) | owner << 31);
        last = d;
        d = bus.read32(d + 8).unwrap_or(0);
    }
    ret(core, bus, last);
}

/// `roundup2(x, n)`: `x` rounded up to a multiple of the power of two `n`.
pub fn roundup2(core: &mut Lx106, bus: &mut SocBus) {
    let (x, n) = (arg(core, bus, 0), arg(core, bus, 1).max(1));
    ret(core, bus, x.wrapping_add(n - 1) & !(n - 1));
}

/// `ets_update_cpu_frequency(mhz)`: the ROM's idea of the CPU clock, which `ets_delay_us` counts
/// in (core_esp8266_main.cpp:96-102 sets 80 or 160 with CPU2X).
pub fn ets_update_cpu_frequency(core: &mut Lx106, bus: &mut SocBus) {
    bus.rom.cpu_mhz = arg(core, bus, 0);
    ret(core, bus, 0);
}

/// `ets_get_cpu_frequency()`.
pub fn ets_get_cpu_frequency(core: &mut Lx106, bus: &mut SocBus) { let f = bus.rom.cpu_mhz; ret(core, bus, f); }

/// `ets_rtc_int_register()`: attaches the ROM's RTC interrupt handler; no RTC interrupt is
/// raised in this model, so nothing is attached.
pub fn ets_rtc_int_register(core: &mut Lx106, bus: &mut SocBus) { ret(core, bus, 0); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_output_set_drives_the_registers_and_build_chain_links_descriptors() {
        let (mut c, mut bus) = (Lx106::new(), SocBus::new(1 << 20, [0; 6]));
        c.set_ar(0, 0x1000);
        for (i, v) in [1 << 2, 0, 1 << 2, 0].into_iter().enumerate() { c.set_ar(2 + i as u8, v); }
        gpio_output_set(&mut c, &mut bus);
        assert_eq!((bus.periph.gpio.out, bus.periph.gpio.enable), (1 << 2, 1 << 2));
        // 3 descriptors for 300 bytes in blocks of 128, the last 44
        let (desc, head, sp) = (0x3FFE_9000, 0x3FFE_9100, 0x3FFE_9200);
        bus.write32(sp, head).unwrap();
        bus.write32(sp + 4, head + 4).unwrap();
        c.set_ar(1, sp);
        for (i, v) in [desc, 5 * 12, 0x3FFE_A000, 300, 128, 1].into_iter().enumerate() { c.set_ar(2 + i as u8, v); }
        lldesc_build_chain(&mut c, &mut bus);
        assert_eq!(bus.read32(head).unwrap(), desc);
        assert_eq!(bus.read32(head + 4).unwrap(), desc + 24);
        assert_eq!(bus.read32(desc + 24).unwrap(), 44 | 1 << 31);
        assert_eq!(bus.read32(desc + 12 + 4).unwrap(), 0x3FFE_A080);
        c.set_ar(2, desc);
        c.set_ar(3, 3);
        lldesc_num2link(&mut c, &mut bus);
        assert_eq!(c.get_ar(2), desc + 24);
    }
}
