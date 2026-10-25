//! The debugger's code listing, as Mesen2's Disassembly pane draws it.
//!
//! One continuous listing of a CPU's whole address space (24-bit for the
//! 65C816, 16-bit for the SPC700), generated on demand from any place in
//! it. What is known to be code (see [`crate::code_log`]) is
//! disassembled with the widths it ran with; every other run of bytes,
//! up to the end of its bank, collapses into a block titled
//! `unidentified`, `data` or `unmapped`. A label puts a `name:` row before
//! its instruction, a multi-line comment its lines, a subroutine entry a
//! separator, a return an end-of-function separator.
//!
//! Each instruction row carries what Mesen2 prints after the operand:
//! the address the instruction would reach if it ran now (from the live
//! registers), `[$7E0010]` or `[label]`, and the value there.
//!
//! A place in the listing is an address and a row inside what starts
//! there ([`ListingPos`]); [`crate::Emulator::listing`] reads rows from a
//! place, [`crate::Emulator::listing_back`] finds the place some rows
//! before another.

use serde::Serialize;

use crate::code_log::{CODE, DATA, JUMP_TARGET, PREDECODED, START, SUB_ENTRY};
use crate::memory::{MemoryKind, MemoryLocation, View};
use crate::{ApiError, BreakpointInfo, Emulator};

/// The CPUs that have a listing.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ListingCpu {
    /// The 65C816.
    #[default]
    Snes,
    /// The SPC700.
    Spc,
}

impl ListingCpu {
    /// Bytes in its address space.
    #[must_use]
    pub const fn space(self) -> u32 {
        match self {
            Self::Snes => 0x100_0000,
            Self::Spc => 0x1_0000,
        }
    }

    /// Hex digits of an address.
    #[must_use]
    pub const fn digits(self) -> usize {
        match self {
            Self::Snes => 6,
            Self::Spc => 4,
        }
    }

    const fn bus(self) -> MemoryKind {
        match self {
            Self::Snes => MemoryKind::SnesMemory,
            Self::Spc => MemoryKind::SpcMemory,
        }
    }

    const fn break_cpu(self) -> luna_core::BreakCpu {
        match self {
            Self::Snes => luna_core::BreakCpu::Snes,
            Self::Spc => luna_core::BreakCpu::Spc,
        }
    }
}

/// How a block of bytes that is not code shows.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum BlockView {
    /// One empty row (Mesen2's default).
    #[default]
    Hide,
    /// `.db` rows of 8 bytes.
    Show,
}

/// What the listing shows (Mesen2's "Disassembly settings").
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ListingOptions {
    /// Bytes the code log saw read as data.
    pub data: BlockView,
    /// Bytes neither run nor read.
    pub unidentified: BlockView,
    /// ` = $XX` after the effective address.
    pub memory_values: bool,
    /// A `$ADDR:` row before a jump target or entry point with no label.
    pub jump_labels: bool,
    /// Mnemonics, hex and generated labels in lower case (names keep
    /// theirs).
    pub lower_case: bool,
}

impl Default for ListingOptions {
    fn default() -> Self {
        Self {
            data: BlockView::Hide,
            unidentified: BlockView::Hide,
            memory_values: true,
            jump_labels: false,
            lower_case: false,
        }
    }
}

/// A place in the listing: an address, and a row of what starts there.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ListingPos {
    /// The address an instruction or a block starts at.
    pub addr: u32,
    /// The row inside it (0 = its first).
    pub row: u16,
}

/// What a row is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// An instruction.
    Instruction,
    /// `name:` before an instruction.
    Label,
    /// One line of a multi-line comment, before its instruction.
    Comment,
    /// The separator before a subroutine entry, titled with its label or
    /// `sub start`.
    FunctionStart,
    /// The untitled separator after a return.
    FunctionEnd,
    /// The titled separator that opens a block.
    BlockStart,
    /// A block's content: one empty row, or a `.db` row.
    BlockBody,
    /// The separator that closes a block.
    BlockEnd,
}

/// What the bytes of a row are, for its background.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowCode {
    /// Code that ran.
    Executed,
    /// Code decoded ahead of what ran, never run itself.
    Predecoded,
    /// Bytes read as data.
    Data,
    /// Neither.
    Unidentified,
    /// No memory behind (an I/O register, open bus).
    Unmapped,
}

/// A breakpoint on a row: what Mesen2 draws in the gutter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RowBreakpoint {
    /// Registry id of the one drawn.
    pub id: u32,
    /// What it is: `exec`, `write`, `read` or `forbid` (the first that
    /// applies, enabled ones first).
    pub kind: &'static str,
    /// It fires.
    pub enabled: bool,
    /// It has a condition.
    pub conditional: bool,
    /// It records a mark instead of halting.
    pub mark: bool,
}

/// One row of the listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ListingRow {
    /// Its place.
    pub pos: ListingPos,
    /// What it is.
    pub kind: RowKind,
    /// The memory behind its address, if any:
    /// `{kind, offset}`, as `absolute_address` gives it.
    #[schemars(with = "Option<serde_json::Value>")]
    pub absolute: Option<MemoryLocation>,
    /// The instruction's bytes, or a `.db` row's.
    pub bytes: Vec<u8>,
    /// The instruction (`LDA $10`), `name:`, `;comment line`, a block's
    /// title, `.db $FF $00 …`; empty for a separator or an empty block row.
    pub text: String,
    /// After the operand: `[$7E0010]`, `[label]`, `[label+2]`.
    pub effective: Option<String>,
    /// After that: `$12`, `$1234`.
    pub value: Option<String>,
    /// A one-line comment at the end of an instruction row.
    pub comment: Option<String>,
    /// What its bytes are.
    pub code: RowCode,
    /// The live program counter is on this instruction.
    pub is_pc: bool,
    /// A breakpoint on it.
    pub breakpoint: Option<RowBreakpoint>,
}

/// What starts at an address.
enum Item {
    /// An instruction of `len` bytes.
    Instruction { len: u32 },
    /// A block of `len` bytes.
    Block { len: u32, code: RowCode },
}

/// The label name a bus address shows on `cpu`, for operands and
/// annotations: a debugger label (`name+N` inside one), else a loaded
/// symbol on that very address.
pub(crate) fn bus_name(
    labels: &crate::labels::LabelStore,
    symbols: Option<&crate::SymbolTable>,
    view: View<'_>,
    cpu: ListingCpu,
    addr: u32,
) -> Option<String> {
    crate::labels::label_hit(labels, view, cpu.bus(), addr)
        .and_then(|h| h.name())
        .or_else(|| {
            let t = symbols?;
            let near = match cpu {
                ListingCpu::Snes => t.nearest(addr),
                ListingCpu::Spc => t.nearest_spc(addr as u16),
            };
            // Only a symbol on that very address names it.
            near.filter(|n| !n.contains('+'))
        })
}

impl Emulator {
    /// `count` rows of `cpu`'s listing from `from` (see the module doc).
    pub fn listing(
        &self,
        cpu: ListingCpu,
        from: ListingPos,
        count: usize,
        opts: ListingOptions,
    ) -> Result<Vec<ListingRow>, ApiError> {
        let view = self.view()?;
        let bps = self.bp_list()?;
        let mut out = Vec::with_capacity(count);
        let mut addr = self.item_start(cpu, view, from.addr);
        let mut skip = if addr == from.addr {
            usize::from(from.row)
        } else {
            0
        };
        // A lap around the whole space at most.
        let mut walked = 0u64;
        while out.len() < count && walked < u64::from(cpu.space()) {
            let (len, rows) = self.item_rows(cpu, view, addr, opts, &bps)?;
            out.extend(rows.into_iter().skip(skip).take(count - out.len()));
            skip = 0;
            walked += u64::from(len);
            addr = (addr + len) % cpu.space();
        }
        Ok(out)
    }

    /// The place `rows` rows before `from` (wrapping round the space).
    pub fn listing_back(
        &self,
        cpu: ListingCpu,
        from: ListingPos,
        rows: usize,
        opts: ListingOptions,
    ) -> Result<ListingPos, ApiError> {
        let view = self.view()?;
        let bps = Vec::new();
        let mut addr = self.item_start(cpu, view, from.addr);
        let mut row = if addr == from.addr {
            usize::from(from.row)
        } else {
            0
        };
        let mut left = rows;
        loop {
            if left <= row {
                return Ok(ListingPos {
                    addr,
                    row: u16::try_from(row - left).unwrap_or(u16::MAX),
                });
            }
            left -= row;
            let prev = (addr + cpu.space() - 1) % cpu.space();
            addr = self.item_start(cpu, view, prev);
            row = self.item_rows(cpu, view, addr, opts, &bps)?.1.len();
        }
    }

    /// The place of the row an address shows on: the instruction row of
    /// the instruction covering it, or the start of its block.
    pub fn listing_pos_of(
        &self,
        cpu: ListingCpu,
        addr: u32,
        opts: ListingOptions,
    ) -> Result<ListingPos, ApiError> {
        let view = self.view()?;
        let start = self.item_start(cpu, view, addr % cpu.space());
        let (_, rows) = self.item_rows(cpu, view, start, opts, &[])?;
        let row = rows
            .iter()
            .position(|r| r.kind == RowKind::Instruction)
            .unwrap_or(0);
        Ok(ListingPos {
            addr: start,
            row: u16::try_from(row).unwrap_or(0),
        })
    }

    /// The memory behind `addr` on `cpu`'s bus, and its code-log flags.
    fn byte_info(
        &self,
        cpu: ListingCpu,
        view: View<'_>,
        addr: u32,
    ) -> (Option<MemoryLocation>, u8) {
        let loc = match cpu {
            ListingCpu::Snes => view
                .absolute(addr)
                .filter(|l| l.kind != MemoryKind::SnesRegister),
            ListingCpu::Spc => {
                let a = addr as u16;
                Some(if a >= 0xFFC0 && view.0.apu_real.control & 0x80 != 0 {
                    MemoryLocation {
                        kind: MemoryKind::SpcRom,
                        offset: u32::from(a - 0xFFC0),
                    }
                } else {
                    MemoryLocation {
                        kind: MemoryKind::SpcRam,
                        offset: u32::from(a),
                    }
                })
            }
        };
        let flags = loc.map_or(0, |l| self.code_flags(l.kind, l.offset));
        (loc, flags)
    }

    /// What a non-code byte belongs to.
    fn block_code(&self, cpu: ListingCpu, view: View<'_>, addr: u32) -> Option<RowCode> {
        let (loc, f) = self.byte_info(cpu, view, addr);
        if loc.is_none() {
            return Some(RowCode::Unmapped);
        }
        if f & (START | CODE | PREDECODED) != 0 {
            return None;
        }
        Some(if f & DATA != 0 {
            RowCode::Data
        } else {
            RowCode::Unidentified
        })
    }

    /// The length of the instruction at `addr`, with the widths it is
    /// logged with.
    fn insn_len(cpu: ListingCpu, view: View<'_>, addr: u32, flags: u8) -> u32 {
        match cpu {
            ListingCpu::Snes => {
                let bank = addr & 0xFF_0000;
                let read = |a: u16| view.bus_peek(bank | u32::from(a), 0);
                u32::from(
                    luna_cpu_65c816::disassemble(
                        read,
                        addr as u16,
                        flags & crate::code_log::MEMORY8 != 0,
                        flags & crate::code_log::INDEX8 != 0,
                    )
                    .length,
                )
            }
            ListingCpu::Spc => u32::from(
                luna_cpu_spc700::disassemble(|a| view.0.apu_real.peek(a), addr as u16).length,
            ),
        }
    }

    /// The address the item covering `addr` starts at.
    fn item_start(&self, cpu: ListingCpu, view: View<'_>, addr: u32) -> u32 {
        let space = cpu.space();
        let addr = addr % space;
        let (_, f) = self.byte_info(cpu, view, addr);
        if f & START != 0 {
            return addr;
        }
        if f & (CODE | PREDECODED) != 0 {
            // Inside an instruction: the start at most 3 bytes back whose
            // length covers it.
            for back in 1..=3u32 {
                let a = (addr + space - back) % space;
                let (_, fa) = self.byte_info(cpu, view, a);
                if fa & START != 0 && Self::insn_len(cpu, view, a, fa) > back {
                    return a;
                }
            }
            return addr;
        }
        let code = self.block_code(cpu, view, addr);
        let bank = addr & !0xFFFF;
        let mut a = addr;
        while a > bank && self.block_code(cpu, view, a - 1) == code {
            a -= 1;
        }
        a
    }

    /// The item starting at `addr`.
    fn item_at(&self, cpu: ListingCpu, view: View<'_>, addr: u32) -> Item {
        let (_, f) = self.byte_info(cpu, view, addr);
        if f & START != 0 {
            // The listing goes bank by bank: an instruction at the end of
            // one (its operand wraps in the bank) stops there.
            let to_bank_end = 0x1_0000 - (addr & 0xFFFF);
            return Item::Instruction {
                len: Self::insn_len(cpu, view, addr, f).clamp(1, to_bank_end),
            };
        }
        let code = self
            .block_code(cpu, view, addr)
            .unwrap_or(RowCode::Unidentified);
        let end = (addr | 0xFFFF) + 1;
        let mut b = addr + 1;
        while b < end.min(cpu.space()) && self.block_code(cpu, view, b) == Some(code) {
            b += 1;
        }
        Item::Block {
            len: b - addr,
            code,
        }
    }

    /// The label name a bus address shows, for operands and annotations.
    fn bus_name(&self, cpu: ListingCpu, addr: u32) -> Option<String> {
        bus_name(
            &self.labels,
            self.symbols.as_ref(),
            self.view().ok()?,
            cpu,
            addr,
        )
    }

    /// The breakpoint Mesen2 draws on the row of `addr`.
    fn row_breakpoint(
        cpu: ListingCpu,
        bps: &[BreakpointInfo],
        addr: u32,
        loc: Option<MemoryLocation>,
    ) -> Option<RowBreakpoint> {
        let covers = |b: &BreakpointInfo| {
            if b.cpu != cpu.break_cpu() || !(b.exec || b.forbid || b.lo == b.hi) {
                return false;
            }
            let space = loc.and_then(|l| match l.kind {
                MemoryKind::SpcRam => Some((luna_core::WatchSpace::Aram, l.offset)),
                MemoryKind::SpcRom => Some((luna_core::WatchSpace::Ipl, l.offset)),
                k => crate::memory_space(k).map(|s| (s, l.offset)),
            });
            match b.space {
                luna_core::WatchSpace::Bus => (b.lo..=b.hi).contains(&addr),
                s => space.is_some_and(|(ls, o)| ls == s && (b.lo..=b.hi).contains(&o)),
            }
        };
        let rank = |b: &BreakpointInfo| {
            let kind = if b.forbid {
                3
            } else if b.exec {
                0
            } else if b.on_write {
                1
            } else {
                2
            };
            (u8::from(!b.enabled), kind)
        };
        bps.iter()
            .filter(|b| covers(b))
            .min_by_key(|b| rank(b))
            .map(|b| RowBreakpoint {
                id: b.id,
                kind: match rank(b).1 {
                    0 => "exec",
                    1 => "write",
                    2 => "read",
                    _ => "forbid",
                },
                enabled: b.enabled,
                conditional: b.condition.is_some(),
                mark: b.mark,
            })
    }

    /// The length of the item at `addr` and its rows.
    pub(crate) fn item_rows(
        &self,
        cpu: ListingCpu,
        view: View<'_>,
        addr: u32,
        opts: ListingOptions,
        bps: &[BreakpointInfo],
    ) -> Result<(u32, Vec<ListingRow>), ApiError> {
        let (loc, flags) = self.byte_info(cpu, view, addr);
        let lower = |s: String| if opts.lower_case { s.to_lowercase() } else { s };
        let row = |n: usize, kind, text: String, code| ListingRow {
            pos: ListingPos {
                addr,
                row: u16::try_from(n).unwrap_or(u16::MAX),
            },
            kind,
            absolute: loc,
            bytes: Vec::new(),
            text,
            effective: None,
            value: None,
            comment: None,
            code,
            is_pc: false,
            breakpoint: None,
        };
        match self.item_at(cpu, view, addr) {
            Item::Block { len, code } => {
                let title = match code {
                    RowCode::Data => "data",
                    RowCode::Unmapped => "unmapped",
                    _ => "unidentified",
                };
                let mut rows = vec![row(0, RowKind::BlockStart, title.to_string(), code)];
                let shown = match code {
                    RowCode::Data => opts.data,
                    RowCode::Unidentified => opts.unidentified,
                    _ => BlockView::Hide,
                };
                if shown == BlockView::Show {
                    for chunk in 0..len.div_ceil(8) {
                        let a0 = addr + chunk * 8;
                        let bytes: Vec<u8> = (a0..(a0 + 8).min(addr + len))
                            .map(|a| Self::peek_on(cpu, view, a))
                            .collect();
                        let text = lower(format!(
                            ".db {}",
                            bytes
                                .iter()
                                .map(|b| format!("${b:02X}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        ));
                        let mut r = row(rows.len(), RowKind::BlockBody, text, code);
                        r.bytes = bytes;
                        rows.push(r);
                    }
                } else {
                    rows.push(row(rows.len(), RowKind::BlockBody, String::new(), code));
                }
                rows.push(row(rows.len(), RowKind::BlockEnd, String::new(), code));
                Ok((len, rows))
            }
            Item::Instruction { len } => {
                let code = if flags & PREDECODED != 0 {
                    RowCode::Predecoded
                } else {
                    RowCode::Executed
                };
                let hit = self.label_at(cpu.bus(), addr)?;
                let mut rows = Vec::new();
                if flags & SUB_ENTRY != 0 {
                    let title = hit
                        .as_ref()
                        .and_then(crate::labels::LabelHit::name)
                        .unwrap_or_else(|| "sub start".to_string());
                    rows.push(row(0, RowKind::FunctionStart, title, code));
                }
                let comment = hit
                    .as_ref()
                    .filter(|h| h.byte == 0 && !h.label.comment.is_empty())
                    .map(|h| h.label.comment.clone());
                if let Some(c) = comment.as_ref().filter(|c| c.contains('\n')) {
                    for line in c.split('\n') {
                        rows.push(row(rows.len(), RowKind::Comment, format!(";{line}"), code));
                    }
                }
                if let Some(name) = hit.as_ref().and_then(crate::labels::LabelHit::name) {
                    rows.push(row(rows.len(), RowKind::Label, format!("{name}:"), code));
                } else if opts.jump_labels && flags & (JUMP_TARGET | SUB_ENTRY) != 0 {
                    let text = lower(format!("${addr:0w$X}:", w = cpu.digits()));
                    rows.push(row(rows.len(), RowKind::Label, text, code));
                }
                let mut r = row(rows.len(), RowKind::Instruction, String::new(), code);
                r.bytes = (0..len)
                    .map(|i| Self::peek_on(cpu, view, Self::wrap(cpu, addr, i)))
                    .collect();
                self.instruction_text(cpu, view, addr, flags, opts, &mut r);
                r.comment = comment.filter(|c| !c.contains('\n'));
                r.breakpoint = Self::row_breakpoint(cpu, bps, addr, loc);
                let returns = matches!(
                    (cpu, r.bytes.first()),
                    (ListingCpu::Snes, Some(0x60 | 0x6B | 0x40))
                        | (ListingCpu::Spc, Some(0x6F | 0x7F))
                );
                rows.push(r);
                if returns {
                    rows.push(row(rows.len(), RowKind::FunctionEnd, String::new(), code));
                }
                Ok((len, rows))
            }
        }
    }

    /// `addr + i` as the program counter wraps on `cpu`.
    const fn wrap(cpu: ListingCpu, addr: u32, i: u32) -> u32 {
        match cpu {
            ListingCpu::Snes => (addr & 0xFF_0000) | (addr.wrapping_add(i) & 0xFFFF),
            ListingCpu::Spc => addr.wrapping_add(i) & 0xFFFF,
        }
    }

    /// A byte on `cpu`'s bus, without side effects.
    fn peek_on(cpu: ListingCpu, view: View<'_>, addr: u32) -> u8 {
        match cpu {
            ListingCpu::Snes => view.bus_peek(addr, 0),
            ListingCpu::Spc => view.0.apu_real.peek(addr as u16),
        }
    }

    /// Fill an instruction row's text, effective address and value.
    fn instruction_text(
        &self,
        cpu: ListingCpu,
        view: View<'_>,
        addr: u32,
        flags: u8,
        opts: ListingOptions,
        r: &mut ListingRow,
    ) {
        // Names are kept out of the lower-casing: they are wrapped in
        // markers while the text is built.
        let named = |n: Option<String>| n.map(|n| format!("\u{1}{n}\u{2}"));
        let finish = |s: &str| -> String {
            let mut out = String::new();
            let mut inside = false;
            for c in s.chars() {
                match c {
                    '\u{1}' => inside = true,
                    '\u{2}' => inside = false,
                    c if opts.lower_case && !inside => out.extend(c.to_lowercase()),
                    c => out.push(c),
                }
            }
            out
        };
        let (target, width) = match cpu {
            ListingCpu::Snes => {
                let snes = view.0;
                let bank = addr & 0xFF_0000;
                let read = |a: u16| view.bus_peek(bank | u32::from(a), 0);
                let m8 = flags & crate::code_log::MEMORY8 != 0;
                let x8 = flags & crate::code_log::INDEX8 != 0;
                let text = luna_cpu_65c816::disassemble_named(
                    read,
                    (addr >> 16) as u8,
                    addr as u16,
                    m8,
                    x8,
                    &|a| named(self.bus_name(cpu, a)),
                )
                .text;
                r.text = finish(&text);
                let c = &snes.cpu;
                r.is_pc = addr == (u32::from(c.pb) << 16) | u32::from(c.pc);
                let regs = luna_cpu_65c816::EffectiveRegs {
                    pb: (addr >> 16) as u8,
                    db: c.db,
                    d: c.dp,
                    s: c.sp,
                    x: c.x,
                    y: c.y,
                    m8: c.e || c.p.bits() & 0x20 != 0,
                    x8: c.e || c.p.bits() & 0x10 != 0,
                    e: c.e,
                };
                match luna_cpu_65c816::effective_address(
                    |a| view.bus_peek(a, 0),
                    addr as u16,
                    &regs,
                ) {
                    Some(e) => (Some(e.addr), e.width),
                    None => (None, 0),
                }
            }
            ListingCpu::Spc => {
                let apu = &view.0.apu_real;
                let text = luna_cpu_spc700::disassemble_named(|a| apu.peek(a), addr as u16, &|a| {
                    named(self.bus_name(cpu, u32::from(a)))
                })
                .text;
                r.text = finish(&text);
                r.is_pc = addr == u32::from(apu.cpu.pc);
                let regs = luna_cpu_spc700::EffectiveRegs {
                    x: apu.cpu.x,
                    y: apu.cpu.y,
                    p: apu.cpu.psw.0 & 0x20 != 0,
                };
                match luna_cpu_spc700::effective_address(|a| apu.peek(a), addr as u16, regs) {
                    Some(e) => (Some(u32::from(e.addr)), e.width),
                    None => (None, 0),
                }
            }
        };
        if let Some(t) = target {
            let shown = named(self.bus_name(cpu, t))
                .unwrap_or_else(|| format!("${t:0w$X}", w = cpu.digits()));
            r.effective = Some(finish(&format!("[{shown}]")));
            if opts.memory_values && width > 0 {
                let lo = Self::peek_on(cpu, view, t);
                let v = if width == 2 {
                    let hi = Self::peek_on(cpu, view, (t + 1) % cpu.space());
                    format!("${:04X}", u16::from(lo) | (u16::from(hi) << 8))
                } else {
                    format!("${lo:02X}")
                };
                r.value = Some(finish(&v));
            }
        }
    }
}
