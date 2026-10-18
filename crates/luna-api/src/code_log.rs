//! The code log: which bytes the CPU ran as instructions, and how.
//!
//! The debugger's listing needs to know where instructions start and with
//! which accumulator and index widths they ran, as Mesen2's does: an
//! instruction is known once it has executed, and from there the bytes
//! straight after it are decoded ahead (marked as such) until a `PLP`, an
//! `XCE` or an `RTI` makes the widths unknowable, or until known code.
//! Every byte is kept by the memory behind it and its offset there, so a
//! ROM routine is known at every mirror. Off unless asked for: the log
//! costs a map lookup per byte of each instruction.
//!
//! Each byte carries these flags: the first six are those of a
//! code/data log (the `.cdl` file comes with it later), the last two the
//! listing's own.

use std::collections::BTreeMap;

use crate::memory::{MemoryKind, MemoryLocation, View};

/// The byte was executed as part of an instruction.
pub const CODE: u8 = 0x01;
/// The byte was read as data (not logged yet).
pub const DATA: u8 = 0x02;
/// A jump or a taken branch landed on this byte.
pub const JUMP_TARGET: u8 = 0x04;
/// A subroutine call, or an interrupt, entered here.
pub const SUB_ENTRY: u8 = 0x08;
/// The instruction starting here ran with 8-bit index registers.
pub const INDEX8: u8 = 0x10;
/// The instruction starting here ran with an 8-bit accumulator.
pub const MEMORY8: u8 = 0x20;
/// An instruction starts here (run, or decoded ahead).
pub const START: u8 = 0x40;
/// Decoded ahead, never run: the start of such an instruction (with
/// [`START`]) or one of its operand bytes.
pub const PREDECODED: u8 = 0x80;

/// Per-memory flags.
#[derive(Clone, Debug, Default)]
pub(crate) struct CodeLog {
    flags: BTreeMap<MemoryKind, Vec<u8>>,
}

impl CodeLog {
    /// Nothing logged.
    pub(crate) const fn new() -> Self {
        Self {
            flags: BTreeMap::new(),
        }
    }
}

/// What the run loop notes before a step, to judge it after.
#[derive(Clone, Copy)]
pub(crate) struct CodePre {
    pc: u32,
    m8: bool,
    x8: bool,
    idle: bool,
    interrupts: u64,
}

impl CodePre {
    /// The 65C816 as it stands before a step.
    pub(crate) fn capture(snes: &luna_core::Snes) -> Self {
        let c = &snes.cpu;
        Self {
            pc: (u32::from(c.pb) << 16) | u32::from(c.pc),
            m8: c.e || c.p.bits() & 0x20 != 0,
            x8: c.e || c.p.bits() & 0x10 != 0,
            idle: c.stopped || c.waiting,
            interrupts: c.interrupts_serviced,
        }
    }
}

/// `addr + n` in the same bank, as the program counter wraps.
const fn in_bank(addr: u32, n: u32) -> u32 {
    (addr & 0xFF_0000) | (addr.wrapping_add(n) & 0xFFFF)
}

impl CodeLog {
    /// The flags of byte `offset` of `kind`.
    pub(crate) fn get(&self, kind: MemoryKind, offset: u32) -> u8 {
        self.flags
            .get(&kind)
            .and_then(|v| v.get(offset as usize))
            .copied()
            .unwrap_or(0)
    }

    /// The offsets of `kind` whose flags have any of `flag`'s bits.
    pub(crate) fn offsets_with(&self, kind: MemoryKind, flag: u8) -> Vec<u32> {
        self.flags.get(&kind).map_or_else(Vec::new, |v| {
            v.iter()
                .enumerate()
                .filter(|&(_, &f)| f & flag != 0)
                .map(|(o, _)| o as u32)
                .collect()
        })
    }

    /// The flags of the byte behind CPU-bus address `addr`.
    pub(crate) fn at(&self, view: View<'_>, addr: u32) -> u8 {
        view.absolute(addr)
            .map_or(0, |l| self.get(l.kind, l.offset))
    }

    fn slot(&mut self, view: View<'_>, loc: MemoryLocation) -> Option<&mut u8> {
        let size = view.size(loc.kind);
        let v = self.flags.entry(loc.kind).or_insert_with(|| vec![0; size]);
        v.get_mut(loc.offset as usize)
    }

    /// Note what the step judged by `pre` did: an instruction run at its
    /// address, a handler entered, or nothing (a `WAI` / `STP` tick).
    pub(crate) fn record(&mut self, snes: &luna_core::Snes, pre: CodePre) {
        let view = View(snes);
        let now = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
        if snes.cpu.interrupts_serviced != pre.interrupts {
            // An NMI or IRQ was taken instead of an instruction (a
            // parked `WAI` woken by one included).
            self.mark(view, now, SUB_ENTRY);
            return;
        }
        if pre.idle && (snes.cpu.waiting || snes.cpu.stopped) {
            return;
        }
        let Some(loc) = view.absolute(pre.pc) else {
            return;
        };
        let read = |a: u16| view.bus_peek((pre.pc & 0xFF_0000) | u32::from(a), 0);
        let op = read(pre.pc as u16);
        let widths = if pre.m8 { MEMORY8 } else { 0 } | if pre.x8 { INDEX8 } else { 0 };
        let before = self.get(loc.kind, loc.offset);
        self.flow(view, op, pre.pc, now);
        if before & (START | CODE | PREDECODED | INDEX8 | MEMORY8) == START | CODE | widths {
            // Run before with these widths: nothing else to learn.
            return;
        }
        let len =
            u32::from(luna_cpu_65c816::disassemble(read, pre.pc as u16, pre.m8, pre.x8).length);
        for i in 0..len {
            if let Some(b) = view
                .absolute(in_bank(pre.pc, i))
                .and_then(|l| self.slot(view, l))
            {
                *b = (*b & !PREDECODED) | CODE;
            }
        }
        if let Some(b) = self.slot(view, loc) {
            *b = (*b & !(PREDECODED | INDEX8 | MEMORY8)) | START | widths;
        }
        if before & START == 0 {
            let (mut m8, mut x8) = (pre.m8, pre.x8);
            let imm = read(in_bank(pre.pc, 1) as u16);
            if Self::widths_after(op, imm, &mut m8, &mut x8, snes.cpu.e) {
                self.predecode(view, in_bank(pre.pc, len), m8, x8, snes.cpu.e);
            }
        }
    }

    /// Mark where instruction `op` at `pc` went, now at `now`: a call's or
    /// a `BRK` / `COP`'s handler, a jump's or a taken branch's target.
    fn flow(&mut self, view: View<'_>, op: u8, pc: u32, now: u32) {
        match op {
            // JSR, JSR (a,x), JSL; BRK, COP enter their handler.
            0x20 | 0xFC | 0x22 | 0x00 | 0x02 => self.mark(view, now, SUB_ENTRY),
            // BRA, BRL, JMP and JML in every form.
            0x80 | 0x82 | 0x4C | 0x5C | 0x6C | 0x7C | 0xDC => self.mark(view, now, JUMP_TARGET),
            // A conditional branch (2 bytes), when taken.
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 if now != in_bank(pc, 2) => {
                self.mark(view, now, JUMP_TARGET);
            }
            _ => {}
        }
    }

    /// The widths after instruction `op` (operand `imm`) ran: `false` when
    /// it makes them unknowable.
    const fn widths_after(op: u8, imm: u8, m8: &mut bool, x8: &mut bool, e: bool) -> bool {
        match op {
            // PLP, RTI, XCE.
            0x28 | 0x40 | 0xFB => false,
            // REP, SEP; emulation mode keeps both 8-bit.
            0xC2 => {
                *m8 &= e || imm & 0x20 == 0;
                *x8 &= e || imm & 0x10 == 0;
                true
            }
            0xE2 => {
                *m8 |= imm & 0x20 != 0;
                *x8 |= imm & 0x10 != 0;
                true
            }
            _ => true,
        }
    }

    /// Decode ahead from `addr` to the end of its bank, until known code,
    /// a byte with no memory behind it, or widths that cannot be known.
    fn predecode(&mut self, view: View<'_>, mut addr: u32, mut m8: bool, mut x8: bool, e: bool) {
        let bank = addr & 0xFF_0000;
        let read = |a: u16| view.bus_peek(bank | u32::from(a), 0);
        loop {
            let Some(loc) = view.absolute(addr) else {
                return;
            };
            if self.get(loc.kind, loc.offset) & (CODE | START) != 0 {
                return;
            }
            let pc = addr as u16;
            let op = read(pc);
            let len = u16::from(luna_cpu_65c816::disassemble(read, pc, m8, x8).length);
            if let Some(b) = self.slot(view, loc) {
                *b |=
                    START | PREDECODED | if m8 { MEMORY8 } else { 0 } | if x8 { INDEX8 } else { 0 };
            }
            for i in 1..len {
                let operand = bank | u32::from(pc.wrapping_add(i));
                if let Some(b) = view.absolute(operand).and_then(|l| self.slot(view, l))
                    && *b & (CODE | START) == 0
                {
                    *b |= PREDECODED;
                }
            }
            if !Self::widths_after(op, read(pc.wrapping_add(1)), &mut m8, &mut x8, e) {
                return;
            }
            let Some(next) = pc.checked_add(len) else {
                return;
            };
            addr = bank | u32::from(next);
        }
    }

    /// Take in the SPC700 instruction starts the APU noted since the last
    /// call: each run at its address, then decoded ahead when new.
    pub(crate) fn collect_spc(&mut self, snes: &mut luna_core::Snes) {
        let Some(fresh) = snes
            .apu_real
            .exec_log
            .as_mut()
            .map(luna_apu::SpcExecLog::take)
        else {
            return;
        };
        let view = View(snes);
        let apu = &snes.apu_real;
        let ipl_on = apu.control & 0x80 != 0;
        let loc = |pc: u16, ipl: bool| {
            if ipl && pc >= 0xFFC0 {
                MemoryLocation {
                    kind: MemoryKind::SpcRom,
                    offset: u32::from(pc - 0xFFC0),
                }
            } else {
                MemoryLocation {
                    kind: MemoryKind::SpcRam,
                    offset: u32::from(pc),
                }
            }
        };
        let len = |pc: u16| u16::from(luna_cpu_spc700::disassemble(|a| apu.peek(a), pc).length);
        for (pc, ipl) in fresh {
            let start = loc(pc, ipl);
            let before = self.get(start.kind, start.offset);
            let n = len(pc);
            for i in 0..n {
                if let Some(b) = self.slot(view, loc(pc.wrapping_add(i), ipl)) {
                    *b = (*b & !PREDECODED) | CODE;
                }
            }
            if let Some(b) = self.slot(view, start) {
                *b = (*b & !PREDECODED) | START;
            }
            if before & START != 0 {
                continue;
            }
            // Decode ahead, in the same memory, until known code.
            let in_ipl = |a: u16| ipl_on && a >= 0xFFC0;
            let mut a = pc.wrapping_add(n);
            while a > pc && in_ipl(a) == ipl {
                let l = loc(a, ipl);
                if self.get(l.kind, l.offset) & (CODE | START) != 0 {
                    break;
                }
                if let Some(b) = self.slot(view, l) {
                    *b |= START | PREDECODED;
                }
                for i in 1..len(a) {
                    let o = loc(a.wrapping_add(i), ipl);
                    if let Some(b) = self.slot(view, o)
                        && *b & (CODE | START) == 0
                    {
                        *b |= PREDECODED;
                    }
                }
                a = a.wrapping_add(len(a));
            }
        }
    }

    fn mark(&mut self, view: View<'_>, addr: u32, flag: u8) {
        if let Some(b) = view.absolute(addr).and_then(|l| self.slot(view, l)) {
            *b |= flag;
        }
    }
}

impl crate::Emulator {
    /// Start or stop logging which bytes the 65C816 runs as code (see the
    /// module doc). Stopping keeps what was logged; a ROM load clears it.
    pub fn enable_code_log(&mut self, on: bool) {
        self.code_logging = on;
        if let Some(snes) = self.snes.as_mut() {
            snes.apu_real.exec_log = on.then(luna_apu::SpcExecLog::default);
        }
    }

    /// Whether the code log is on.
    #[must_use]
    pub const fn code_logging(&self) -> bool {
        self.code_logging
    }

    /// Forget everything logged.
    pub fn clear_code_log(&mut self) {
        self.code_log = CodeLog::new();
    }

    /// The code-log flags of byte `offset` of `kind` ([`CODE`],
    /// [`JUMP_TARGET`], [`SUB_ENTRY`], [`INDEX8`], [`MEMORY8`],
    /// [`START`], [`PREDECODED`]).
    #[must_use]
    pub fn code_flags(&self, kind: MemoryKind, offset: u32) -> u8 {
        self.code_log.get(kind, offset)
    }

    /// [`Self::code_flags`] of the byte behind SPC700 address `addr`, as
    /// the IPL ROM is mapped now.
    pub fn spc_code_flags_at(&self, addr: u16) -> Result<u8, crate::ApiError> {
        let snes = self.snes.as_ref().ok_or(crate::ApiError::NoRom)?;
        Ok(if addr >= 0xFFC0 && snes.apu_real.control & 0x80 != 0 {
            self.code_log
                .get(MemoryKind::SpcRom, u32::from(addr - 0xFFC0))
        } else {
            self.code_log.get(MemoryKind::SpcRam, u32::from(addr))
        })
    }

    /// [`Self::code_flags`] of the byte behind CPU-bus address `addr`.
    pub fn code_flags_at(&self, addr: u32) -> Result<u8, crate::ApiError> {
        Ok(self.code_log.at(self.view()?, addr))
    }
}
