//! Predecoded straight-line blocks for the LX106 interpreter (`Lx106::run`).
//!
//! The uncached loop fetches, decodes and classifies every instruction it executes, and pays the
//! interrupt, boundary and budget checks in front of each one. Here a block is a run of decoded
//! instructions, classified once, that ends at a control transfer, at an instruction that changes
//! interrupt or timer state, or in front of a boundary pc; the checks run once per block and the
//! instructions run back to back. The cache belongs to the LX106 profile alone: it never enters the
//! LX6/LX7 block cache (`crate::block`) or a JIT, and its instructions go through the profile's own
//! rules (the positive instruction list, the alignment rule, LITBASE).
//!
//! What stays exact, compared with the uncached loop:
//! - Interrupts are taken in front of the same instruction. A block only starts where the uncached
//!   loop would check, and nothing inside a block can make an interrupt pending: the core's inputs
//!   change between dispatches (`Core::set_irq`); the instructions that write INTERRUPT, INTENABLE,
//!   PS or CCOMPARE0 (WSR, XSR, RSIL, WAITI, RFE, RFI, RFDE) are the last of their block; and a run
//!   is cut at the instruction whose cycles reach the CCOMPARE0 match (Xtensa ISA, Timer Interrupt
//!   Option: the interrupt is raised when CCOUNT equals CCOMPARE).
//! - The instruction count and CCOUNT move by a run's instructions when it ends. A special register
//!   access (RSR, WSR, XSR, which can read CCOUNT or write CCOMPARE0) first brings them up to date.
//! - Faults: every instruction still traps where it would, with the same cause, EPC and count.
//! - Code changes: a block remembers the write versions of the (at most two) pages it was decoded
//!   from (`Bus::page_versions`) and is rebuilt when either moves. A store that moved one ends the
//!   run, so even a store into the block's next instruction is seen as the uncached loop sees it
//!   (the ISA asks for an ISYNC there). A bus without page versions is never cached.
//! - Boundaries: a block stops in front of a boundary pc (bloom or range); new boundaries throw the
//!   decoded blocks away.
use super::supports;
use crate::decode::{decode, Insn, Op};
use emu_core::core::pc_bit;
use emu_core::Bus;

/// Instructions per block. No LX106 instruction is longer than 3 bytes (core-isa.h:53
/// XCHAL_MAX_INSTRUCTION_SIZE 3), so a block covers at most 96 bytes: two pages of the at least
/// 128 bytes `Bus::page_versions` promises. There are no zero-overhead loops (core-isa.h:56), so
/// inside a block the pc only leaves the straight line at its last instruction or by a trap.
const MAX_LEN: usize = 32;
/// Direct-mapped block table, indexed by the start pc.
const ENTRIES: usize = 1 << 15;
/// Decoded instructions kept before everything is thrown away and decoded again.
const ARENA_MAX: usize = 1 << 17;

/// `Decoded::kind`: the address mask of the alignment rule (core-isa.h:110-113, unaligned loads
/// and stores raise an exception: L16 and S16 need 2-byte, L32 and S32 4-byte alignment).
pub(super) const ALIGN: u8 = 3;
/// An L32R: with LITBASE enabled its literal is not where the decoder put it.
pub(super) const LITERAL: u8 = 1 << 2;
/// A store: it may have changed code, so the block is checked again after it.
pub(super) const STORE: u8 = 1 << 3;
/// Runs through `Lx106::execute` with the instruction count and CCOUNT brought up to date: the
/// special register accesses, BREAK, RER/WER and every instruction outside the LX106 ISA.
pub(super) const SLOW: u8 = 1 << 4;
/// The last instruction of its block.
const ENDS: u8 = 1 << 5;

/// One instruction as `Lx106::execute` would work it out, minus the work.
#[derive(Clone, Copy)]
pub(super) struct Decoded { pub insn: Insn, pub kind: u8 }

fn kind(i: &Insn) -> u8 {
    use Op::*;
    if !supports(i) { return SLOW | ENDS; }
    match i.op {
        L16ui | L16si => 1,
        S16i => 1 | STORE,
        L32i | L32iN => 3,
        S32i | S32iN => 3 | STORE,
        S8i => STORE,
        L32r => LITERAL,
        Rsr => SLOW,
        Wsr | Xsr | Break | BreakN | Rer | Wer => SLOW | ENDS,
        J | Jx | Call0 | Callx0 | Ret | RetN | Rfe | Rfde | Rfi | Syscall | Waiti | Rsil | Isync
        | Beqz | Bnez | Bltz | Bgez | BeqzN | BnezN | Beqi | Bnei | Blti | Bgei | Bltui | Bgeui
        | Bnone | Beq | Blt | Bltu | Ball | Bbc | Bbci | Bany | Bne | Bge | Bgeu | Bnall | Bbs | Bbsi => ENDS,
        _ => 0,
    }
}

#[derive(Clone, Copy)]
struct Entry { pc: u32, start: u32, n: u32, vidx: [u32; 2], ver: [u32; 2] }
/// An empty slot never validates: no bus has a page `u32::MAX`, and a missing page reads 0.
impl Entry { const EMPTY: Entry = Entry { pc: 1, start: 0, n: 0, vidx: [u32::MAX; 2], ver: [1; 2] }; }

/// Where a pc may stop a block: the machine's bloom over `pc_bit` and the exact range.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Bounds { pub bloom: u64, pub lo: u32, pub len: u32 }
impl Bounds {
    #[inline(always)]
    pub fn hit(&self, pc: u32) -> bool { self.bloom & pc_bit(pc) != 0 || pc.wrapping_sub(self.lo) < self.len }
}

pub(super) struct Blocks {
    entries: Vec<Entry>,
    pub arena: Vec<Decoded>,
    /// A run cut short inside a block (budget, timer) goes on here: (pc, entry index, arena index).
    resume: (u32, u32, u32),
    pub enabled: bool,
    pub builds: u64,
    pub flushes: u64,
}

impl Blocks {
    pub fn new(enabled: bool) -> Self {
        Blocks { entries: Vec::new(), arena: Vec::new(), resume: (1, 0, 0), enabled, builds: 0, flushes: 0 }
    }

    /// Forget every decoded block (the table is allocated on first use).
    pub fn flush(&mut self) {
        if self.arena.is_empty() { return; }
        self.entries.fill(Entry::EMPTY);
        self.arena.clear();
        self.resume = (1, 0, 0);
        self.flushes += 1;
    }

    #[inline(always)]
    fn index(pc: u32) -> usize { ((pc >> 1) ^ (pc >> 16)) as usize & (ENTRIES - 1) }

    #[inline(always)]
    fn valid(e: &Entry, pv: &[u32]) -> bool {
        pv.get(e.vidx[0] as usize).copied().unwrap_or(0) == e.ver[0] && pv.get(e.vidx[1] as usize).copied().unwrap_or(0) == e.ver[1]
    }

    /// Whether block `ei` still holds the bytes it was decoded from.
    #[inline(always)]
    pub fn still_valid(&self, ei: u32, pv: &[u32]) -> bool { Self::valid(&self.entries[ei as usize], pv) }

    /// Remember that a run stopped at `pc`, arena index `k` of block `ei`.
    #[inline(always)]
    pub fn cut(&mut self, pc: u32, ei: u32, k: u32) { self.resume = (pc, ei, k); }

    /// The block to run at `pc`: (entry index, arena index of the instruction at `pc`, arena end).
    /// `None` when the instruction at `pc` cannot be fetched.
    #[inline(always)]
    pub fn find<B: Bus>(&mut self, bus: &mut B, pc: u32, bounds: Bounds) -> Option<(u32, u32, u32)> {
        let (rpc, rei, rk) = self.resume;
        if rpc == pc {
            self.resume.0 = 1;
            // the arena only grows between flushes, so an entry whose range still holds `rk` is
            // the build the run was cut in, and arena[rk] is the instruction at `pc`
            if let Some(e) = self.entries.get(rei as usize).filter(|e| rk >= e.start && rk < e.start + e.n) {
                if Self::valid(e, bus.page_versions()) { return Some((rei, rk, e.start + e.n)); }
            }
        }
        let ei = Self::index(pc);
        if let Some(e) = self.entries.get(ei) {
            if e.pc == pc && Self::valid(e, bus.page_versions()) { return Some((ei as u32, e.start, e.start + e.n)); }
        }
        self.build(bus, pc, bounds)
    }

    /// Decode the block at `pc0`. It ends at an instruction that must be last, at `MAX_LEN`, in
    /// front of a boundary pc, or in front of an instruction that cannot be fetched (which faults
    /// when it is reached as the start of a block).
    #[cold]
    fn build<B: Bus>(&mut self, bus: &mut B, pc0: u32, bounds: Bounds) -> Option<(u32, u32, u32)> {
        let mut bytes = bus.fetch(pc0).ok()?;
        if self.entries.is_empty() { self.entries = vec![Entry::EMPTY; ENTRIES]; }
        if self.arena.len() + MAX_LEN > ARENA_MAX { self.flush(); }
        let start = self.arena.len() as u32;
        let (mut pc, mut last) = (pc0, pc0);
        loop {
            let insn = decode(pc, bytes);
            if pc != pc0 && bounds.hit(pc) { break; }
            let kind = kind(&insn);
            self.arena.push(Decoded { insn, kind });
            last = pc;
            pc = pc.wrapping_add(u32::from(insn.len));
            if kind & ENDS != 0 || self.arena.len() - start as usize == MAX_LEN { break; }
            match bus.fetch(pc) { Ok(b) => bytes = b, Err(_) => break }
        }
        let n = self.arena.len() as u32 - start;
        let last_byte = last.wrapping_add(u32::from(self.arena[self.arena.len() - 1].insn.len.max(1)) - 1);
        let vidx = [bus.code_page(pc0), bus.code_page(last_byte)];
        // EX110: watch the pages before reading the versions this block will compare against.
        bus.note_code_page(vidx[0]);
        if vidx[1] != vidx[0] { bus.note_code_page(vidx[1]); }
        let pv = bus.page_versions();
        let ver = vidx.map(|v| pv.get(v as usize).copied().unwrap_or(0));
        let ei = Self::index(pc0);
        self.entries[ei] = Entry { pc: pc0, start, n, vidx, ver };
        self.builds += 1;
        Some((ei as u32, start, start + n))
    }
}
