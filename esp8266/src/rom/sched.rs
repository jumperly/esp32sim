//! The ROM's task scheduler (tools/sdk/include/ets_sys.h:46-62; the SDK's `system_os_task` and
//! `system_os_post` and the Arduino core's `ets_post` wrapper, core_esp8266_main.cpp:230-240,
//! sit on it). A task has a priority, a function and a queue of `ETSEvent`s (two words: signal,
//! parameter) in the guest's memory; `ets_post_rom` appends an event, `ets_run` runs the
//! scheduler forever: the highest-priority task with an event gets it (`task(&event)`), and when
//! no task has one the idle callback runs once, then the CPU waits for an interrupt (`waiti 0`).
//!
//! `ets_run`'s loop lives at the private address `RUN_LOOP`: a task is called with a0 =
//! `RUN_LOOP`, so its return comes back to the loop, and the wait is the core's own WAITI at
//! `RUN_LOOP`, so an interrupt returns there too and the machine skips idle time.
use super::{arg, done, ret, store32, Lx106, SocBus};
use emu_core::Core;
use xtensa_lx7::state::{ps, sr};

/// The scheduler's loop.
pub const RUN_LOOP: u32 = 0x4000_FE0C;
/// Priorities 0..=32. The SDK's own task (libpp) posts at 32, above the 0..31 the SDK documents
/// for users (digitake esp8266-emulator src/rom.c:48 keeps 33 for the same reason).
pub const PRIORITIES: usize = 33;
/// `ETSEvent` (ets_sys.h:51-54): signal, parameter.
const EVENT_SIZE: u32 = 8;

/// One task: its function, its event queue (`len` events at `queue`), the ring's head and fill.
#[derive(Clone, Copy, Default, Debug)]
pub struct Task { pub func: u32, pub queue: u32, pub len: u32, pub head: u32, pub count: u32 }

/// `ets_task(task, prio, queue, qlen)`: install a task.
pub fn ets_task(core: &mut Lx106, bus: &mut SocBus) {
    let (func, prio, queue, len) = (arg(core, bus, 0), arg(core, bus, 1) as usize, arg(core, bus, 2), arg(core, bus, 3));
    let Some(t) = bus.rom.tasks.get_mut(prio) else {
        bus.rom.log_once("task-prio", || format!("ets_task at priority {prio}, above {}: ignored", PRIORITIES - 1));
        ret(core, bus, 1);
        return;
    };
    *t = Task { func, queue, len: len.max(1), head: 0, count: 0 };
    ret(core, bus, 0);
}

/// `ets_post_rom(prio, sig, par)` (`ets_post` in the SDK headers): queue an event for the task
/// at `prio`; 0 when queued, 1 when the task is missing or its queue is full. A queue the ROM's
/// store faults on is that exception (`super::store32`), and nothing is queued.
pub fn ets_post(core: &mut Lx106, bus: &mut SocBus) {
    let (prio, sig, par) = (arg(core, bus, 0) as usize, arg(core, bus, 1), arg(core, bus, 2));
    let r = match bus.rom.tasks.get(prio).copied() {
        Some(t) if t.func != 0 && t.count < t.len => {
            let at = t.queue.wrapping_add((t.head + t.count) % t.len * EVENT_SIZE);
            let stored = store32(bus, at, sig).and_then(|()| store32(bus, at.wrapping_add(4), par));
            if stored.is_ok() { bus.rom.tasks[prio].count += 1; }
            stored.map(|()| 0)
        }
        _ => Ok(1),
    };
    done(core, bus, r);
}

/// `ets_set_idle_cb(fn, arg)`.
pub fn ets_set_idle_cb(core: &mut Lx106, bus: &mut SocBus) {
    let (f, a) = (arg(core, bus, 0), arg(core, bus, 1));
    bus.rom.idle = (f, a);
    ret(core, bus, 0);
}

/// `ets_run()`: never returns; the scheduler runs on the caller's stack.
pub fn ets_run(core: &mut Lx106, bus: &mut SocBus) {
    bus.rom.run_sp = core.get_ar(1);
    core.set_pc(RUN_LOOP);
    core.spin(1);
}

/// One pass of the scheduler at `RUN_LOOP`. Returns the steps it took: 0 when an interrupt is
/// pending (the core takes it here and returns here).
pub fn run_loop(core: &mut Lx106, bus: &mut SocBus) -> u32 {
    if core.irq_pending() { return 0; }
    let rom = &mut bus.rom;
    // tasks and the idle callback run with interrupts enabled, in user mode
    let ps_run = (core.read_sr(sr::PS).unwrap_or(0) & !(ps::EXCM | ps::INTLEVEL_MASK)) | ps::UM;
    if let Some(p) = (0..PRIORITIES).rev().find(|&p| rom.tasks[p].count > 0) {
        let t = &mut rom.tasks[p];
        let event = t.queue.wrapping_add(t.head * EVENT_SIZE);
        t.head = (t.head + 1) % t.len;
        t.count -= 1;
        let f = t.func;
        rom.idle_called = false;
        enter(core, rom.run_sp, ps_run, f, event);
        return 1;
    }
    let (f, a) = rom.idle;
    if f != 0 && !rom.idle_called {
        // Model choice: the ROM calls the idle callback on every empty pass; here it runs once
        // per wake-up and the loop then waits, so an idle callback that returns at once does not
        // keep the CPU busy (digitake esp8266-emulator src/rom.c:390-402 does the same).
        rom.idle_called = true;
        enter(core, rom.run_sp, ps_run, f, a);
        return 1;
    }
    rom.idle_called = false;
    core.write_sr(sr::PS, ps_run);
    core.waiti(0);
    core.spin(1);
    1
}

/// Call `f(a)` from the loop: it returns to `RUN_LOOP` on the scheduler's stack.
fn enter(core: &mut Lx106, sp: u32, ps_run: u32, f: u32, a: u32) {
    core.write_sr(sr::PS, ps_run);
    core.set_ar(0, RUN_LOOP);
    core.set_ar(1, sp);
    core.set_ar(2, a);
    core.set_pc(f);
    core.spin(1);
}
