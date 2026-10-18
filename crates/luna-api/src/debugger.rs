//! What the debugger window's panes show, beside the listing: watch
//! values in Mesen2's formats, the Go To and Go To All searches, Find in
//! the listing, the function list, the label list and the call stack as
//! Mesen2 lays them out.

use serde::Serialize;

use crate::code_log::SUB_ENTRY;
use crate::expr::{ExprCpu, ExprKind};
use crate::listing::{ListingCpu, ListingOptions, ListingPos, ListingRow, RowKind};
use crate::memory::{MemoryKind, MemoryLocation};
use crate::{ApiError, CallKind, Emulator, Label};

/// How a watch shows a number when its entry names no format.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum WatchStyle {
    /// `$12` (one byte by default).
    #[default]
    Hex,
    /// Signed decimal (four bytes by default).
    Decimal,
    /// `%0001.0010` (one byte by default).
    Binary,
}

/// A watch entry's format: a style and a byte count.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Format {
    Hex(u8),
    Signed(u8),
    Unsigned(Option<u8>),
    Binary(u8),
}

/// Split a `, H2`-style format suffix off a watch entry.
fn split_format(entry: &str) -> (&str, Option<(char, Option<u8>)>) {
    if let Some((expr, suffix)) = entry.rsplit_once(',') {
        let suffix = suffix.trim();
        let mut chars = suffix.chars();
        if let Some(c @ ('H' | 'S' | 'U' | 'B')) = chars.next() {
            let rest: String = chars.collect();
            let digits = match rest.as_str() {
                "" => Some(None),
                d => d
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=4).contains(n))
                    .map(Some),
            };
            if let Some(n) = digits {
                return (expr.trim(), Some((c, n)));
            }
        }
    }
    (entry.trim(), None)
}

fn format_number(v: i64, f: Format) -> String {
    let mask = |n: u8| -> u64 {
        if n >= 8 {
            u64::MAX
        } else {
            (1u64 << (u32::from(n) * 8)) - 1
        }
    };
    let u = v as u64;
    match f {
        Format::Hex(n) => format!("${:0w$X}", u & mask(n), w = usize::from(n) * 2),
        Format::Unsigned(Some(n)) => format!("{}", u & mask(n)),
        Format::Unsigned(None) => format!("{v}"),
        Format::Signed(n) => {
            let bits = u32::from(n) * 8;
            let x = (u & mask(n)) as i64;
            let signed = if bits < 64 && x & (1 << (bits - 1)) != 0 {
                x - (1 << bits)
            } else {
                x
            };
            format!("{signed}")
        }
        Format::Binary(n) => {
            let bits = usize::from(n) * 8;
            let s = format!("{:0bits$b}", u & mask(n));
            let groups: Vec<String> = s
                .as_bytes()
                .chunks(4)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            format!("%{}", groups.join("."))
        }
    }
}

/// One result of Go To All.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct GotoResult {
    /// The name.
    pub name: String,
    /// The memory and offset it names, when it is a label.
    #[schemars(with = "Option<serde_json::Value>")]
    pub absolute: Option<MemoryLocation>,
    /// Where this CPU sees it now; `None` when it is out of view.
    pub address: Option<u32>,
}

/// One row of the label list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct LabelRow {
    /// The label.
    pub label: Label,
    /// Where this CPU sees it now; `None` (`<unavailable>`) when it is
    /// out of view.
    pub address: Option<u32>,
}

/// One row of the function list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct FunctionRow {
    /// Its label, if it has one.
    pub name: Option<String>,
    /// Its offset in the program ROM.
    pub rom: u32,
    /// Where the CPU sees it now; `None` when it is out of view.
    pub address: Option<u32>,
}

/// One row of the call stack, as Mesen2's pane lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct CallStackRow {
    /// The routine this row is in: `label ($XXXXXX)`, `[nmi] $XXXXXX`,
    /// `[irq] $XXXXXX`, `$XXXXXX`, or `[bottom of stack]`.
    pub function: String,
    /// The current instruction (the first row), or the call made from
    /// that routine.
    pub pc: u32,
    /// The memory behind `pc`.
    #[schemars(with = "Option<serde_json::Value>")]
    pub absolute: Option<MemoryLocation>,
}

/// What the Status pane of the 65C816 debugger shows beside the
/// registers.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct DebuggerStatus {
    /// The registers and flags.
    pub cpu: crate::CpuState,
    /// Master clocks since power-on.
    pub master_clocks: u64,
    /// An NMI is pending.
    pub nmi: bool,
    /// The H/V timer's IRQ is raised (`TIMEUP`).
    pub hv_irq: bool,
    /// A cartridge coprocessor holds the IRQ line.
    pub coprocessor_irq: bool,
    /// Master clocks into the current scanline.
    pub h_clock: u32,
    /// The PPU dot (`h_clock / 4`).
    pub dot: u32,
    /// The current scanline.
    pub scanline: u16,
    /// The VRAM word address.
    pub vram_addr: u16,
    /// OAM's internal byte address.
    pub oam_addr: u16,
    /// The CGRAM word address.
    pub cgram_addr: u8,
    /// The stack from `SP + 1` to the end of its page.
    pub stack: Vec<u8>,
}

/// One result of Find All Occurrences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct FindResult {
    /// Its place in the listing.
    pub pos: ListingPos,
    /// The instruction and its effective address.
    pub text: String,
}

/// The memories whose labels a CPU's lists show (Mesen2's table).
const fn sees(cpu: ListingCpu, kind: MemoryKind) -> bool {
    match cpu {
        ListingCpu::Snes => matches!(
            kind,
            MemoryKind::SnesPrgRom
                | MemoryKind::SnesSaveRam
                | MemoryKind::SnesWorkRam
                | MemoryKind::SnesRegister
                | MemoryKind::SnesVideoRam
                | MemoryKind::SnesSpriteRam
                | MemoryKind::SnesCgRam
                | MemoryKind::SnesMemory
        ),
        ListingCpu::Spc => matches!(
            kind,
            MemoryKind::SpcRam
                | MemoryKind::SpcRom
                | MemoryKind::SpcDspRegisters
                | MemoryKind::SpcMemory
        ),
    }
}

const fn expr_cpu(cpu: ListingCpu) -> ExprCpu {
    match cpu {
        ListingCpu::Snes => ExprCpu::Snes,
        ListingCpu::Spc => ExprCpu::Spc,
    }
}

const fn bus(cpu: ListingCpu) -> MemoryKind {
    match cpu {
        ListingCpu::Snes => MemoryKind::SnesMemory,
        ListingCpu::Spc => MemoryKind::SpcMemory,
    }
}

/// The search terms of a Go To All query: split at spaces and before
/// capitals, lower-cased; one-letter terms dropped when longer ones exist.
fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in query.split_whitespace() {
        let mut cur = String::new();
        for c in word.chars() {
            if c.is_uppercase() && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.extend(c.to_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.iter().any(|t| t.chars().count() > 1) {
        out.retain(|t| t.chars().count() > 1);
    }
    out
}

impl Emulator {
    /// A watch entry's value as Mesen2's Watch pane shows it: the
    /// expression, with an optional `, H` / `, S` / `, U` / `, B` suffix
    /// and a byte count 1-4; `[addr, count]` for `count` bytes; `style`
    /// when the entry names no format. Errors read `<invalid
    /// expression>`, `<division by zero>`, `<label out of scope>`.
    #[must_use]
    pub fn watch_value(&self, entry: &str, cpu: ExprCpu, style: WatchStyle) -> String {
        let (expr, fmt) = split_format(entry);
        let words = expr.starts_with('{');
        let format = match fmt {
            Some(('H', n)) => Format::Hex(n.unwrap_or(1)),
            Some(('S', n)) => Format::Signed(n.unwrap_or(4)),
            Some(('U', n)) => Format::Unsigned(n),
            Some((_, n)) => Format::Binary(n.unwrap_or(1)),
            None => match style {
                WatchStyle::Hex => Format::Hex(if words { 2 } else { 1 }),
                WatchStyle::Decimal => Format::Signed(if words { 2 } else { 4 }),
                WatchStyle::Binary => Format::Binary(if words { 2 } else { 1 }),
            },
        };
        // `[addr, count]`: count bytes from addr.
        if let Some(inner) = expr.strip_prefix('[').and_then(|e| e.strip_suffix(']'))
            && let Some((addr, count)) = inner.rsplit_once(',')
            && let Ok(count) = count.trim().parse::<u32>()
        {
            let Ok(r) = self.evaluate(addr.trim(), cpu) else {
                return "<invalid expression>".into();
            };
            if r.kind != ExprKind::Numeric {
                return "<invalid expression>".into();
            }
            let one = match format {
                Format::Hex(_) => Format::Hex(1),
                Format::Signed(_) => Format::Signed(1),
                Format::Unsigned(_) => Format::Unsigned(Some(1)),
                Format::Binary(_) => Format::Binary(1),
            };
            let kind = match cpu {
                ExprCpu::Spc => MemoryKind::SpcMemory,
                _ => MemoryKind::SnesMemory,
            };
            return (0..count.min(256))
                .map(|i| {
                    let a = (r.value as u32).wrapping_add(i);
                    let b = self.read_memory(kind, a, 1).map_or(0, |v| v[0]);
                    format_number(i64::from(b), one)
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
        match self.evaluate(expr, cpu) {
            Ok(r) => match r.kind {
                ExprKind::Boolean => if r.value != 0 { "true" } else { "false" }.into(),
                ExprKind::Numeric => format_number(r.value, format),
                ExprKind::DivideBy0 => "<division by zero>".into(),
                ExprKind::OutOfScope => "<label out of scope>".into(),
                ExprKind::Invalid => "<invalid expression>".into(),
            },
            Err(_) => "<invalid expression>".into(),
        }
    }

    /// The Status pane's content (see [`DebuggerStatus`]).
    pub fn debugger_status(&self) -> Result<DebuggerStatus, ApiError> {
        let cpu = self.cpu_state()?;
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let view = self.view()?;
        let sp = u32::from(cpu.sp);
        let stack = (sp + 1..=(sp | 0xFF))
            .map(|a| view.bus_peek(a & 0xFFFF, 0))
            .collect();
        Ok(DebuggerStatus {
            master_clocks: snes.total_mclk,
            nmi: snes.nmi_pending,
            hv_irq: snes.cpu_regs.irq_flag,
            coprocessor_irq: snes.mapper.coproc_main_irq_pending(),
            h_clock: snes.mcycles_in_line,
            dot: snes.mcycles_in_line / 4,
            scanline: snes.ppu_line,
            vram_addr: snes.ppu.vram.address,
            oam_addr: snes.ppu.oam.address,
            cgram_addr: snes.ppu.cgram.address,
            stack,
            cpu,
        })
    }

    /// The address Go To lands on for `text`: a label's, plain digits
    /// read as hex (`8000` is `$8000`), else the value of an expression
    /// (`{$FFEA}`, `pc+4`). `None` when it is none of these or falls
    /// outside the CPU's space.
    #[must_use]
    pub fn goto_address(&self, cpu: ListingCpu, text: &str) -> Option<u32> {
        let text = text.trim();
        let found = self
            .label_address(text, bus(cpu))
            .ok()
            .flatten()
            .or_else(|| match cpu {
                ListingCpu::Snes => self.symbols.as_ref().and_then(|t| t.resolve(text)),
                ListingCpu::Spc => self.resolve_symbol_spc(text).map(u32::from),
            })
            .or_else(|| {
                (!text.is_empty() && text.chars().all(|c| c.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(text, 16).ok())
                    .flatten()
            })
            .or_else(|| {
                self.evaluate(text, expr_cpu(cpu))
                    .ok()
                    .filter(|r| r.kind == ExprKind::Numeric)
                    .and_then(|r| u32::try_from(r.value).ok())
            });
        found.filter(|&a| a < cpu.space())
    }

    /// Go To All: the labels (and `.sym` names) of this CPU matching
    /// `query` — every term of it, case-insensitively (terms split at
    /// spaces and capitals) — those in view first, then those starting
    /// with the query, then by name.
    #[must_use]
    pub fn goto_all(&self, cpu: ListingCpu, query: &str) -> Vec<GotoResult> {
        let terms = terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let matches = |name: &str| {
            let n = name.to_lowercase();
            terms.iter().all(|t| n.contains(t.as_str()))
        };
        let mut out: Vec<GotoResult> = self
            .labels
            .iter()
            .filter(|l| !l.name.is_empty() && sees(cpu, l.memory) && matches(&l.name))
            .map(|l| GotoResult {
                name: l.name.clone(),
                absolute: Some(MemoryLocation {
                    kind: l.memory,
                    offset: l.address,
                }),
                address: self.label_address(&l.name, bus(cpu)).ok().flatten(),
            })
            .collect();
        if let Some(t) = &self.symbols {
            let space = match cpu {
                ListingCpu::Snes => crate::SymbolSpace::Cpu,
                ListingCpu::Spc => crate::SymbolSpace::Aram,
            };
            let sym: Vec<(String, u32)> = t
                .labels_in(space)
                .map(|(n, a)| (n.to_string(), a))
                .collect();
            for (name, addr) in sym {
                if matches(&name) && !out.iter().any(|r| r.name == name) {
                    out.push(GotoResult {
                        name,
                        absolute: None,
                        address: Some(addr),
                    });
                }
            }
        }
        let q = query.trim().to_lowercase();
        out.sort_by(|a, b| {
            (
                a.address.is_none(),
                !a.name.to_lowercase().starts_with(&q),
                &a.name,
            )
                .cmp(&(
                    b.address.is_none(),
                    !b.name.to_lowercase().starts_with(&q),
                    &b.name,
                ))
        });
        out
    }

    /// The text Find looks through on a row.
    fn row_haystack(r: &ListingRow, with_value: bool) -> String {
        let mut s = r.text.clone();
        if let Some(e) = &r.effective {
            s.push(' ');
            s.push_str(e);
        }
        if with_value && let Some(v) = &r.value {
            s.push_str(" = ");
            s.push_str(v);
        }
        if let Some(c) = &r.comment {
            s.push(' ');
            s.push_str(c);
        }
        s
    }

    /// Find: the next row after `from` (or before, `forward` false) whose
    /// instruction, effective address, value or comment contains `text`,
    /// ignoring case, wrapping round the space. Gives up after a lap or
    /// 500 000 rows.
    pub fn listing_find(
        &self,
        cpu: ListingCpu,
        from: ListingPos,
        text: &str,
        forward: bool,
        opts: ListingOptions,
    ) -> Result<Option<ListingPos>, ApiError> {
        const LIMIT: usize = 500_000;
        const CHUNK: usize = 512;
        let needle = text.to_lowercase();
        if needle.is_empty() {
            return Ok(None);
        }
        let hit = |r: &ListingRow| {
            r.pos != from && Self::row_haystack(r, true).to_lowercase().contains(&needle)
        };
        let mut seen = 0;
        let mut pos = from;
        if forward {
            while seen < LIMIT {
                let rows = self.listing(cpu, pos, CHUNK, opts)?;
                if let Some(r) = rows.iter().skip(usize::from(seen > 0)).find(|r| hit(r)) {
                    return Ok(Some(r.pos));
                }
                let Some(last) = rows.last() else { break };
                if rows.len() < CHUNK {
                    break;
                }
                pos = last.pos;
                seen += CHUNK;
            }
        } else {
            while seen < LIMIT {
                let start = self.listing_back(cpu, pos, CHUNK, opts)?;
                let rows = self.listing(cpu, start, CHUNK, opts)?;
                if let Some(r) = rows.iter().rev().find(|r| hit(r)) {
                    return Ok(Some(r.pos));
                }
                pos = start;
                seen += CHUNK;
            }
        }
        Ok(None)
    }

    /// Find All Occurrences: up to 500 instruction rows of the whole space
    /// containing `text`, optionally with its case and as a whole word.
    pub fn listing_find_all(
        &self,
        cpu: ListingCpu,
        text: &str,
        match_case: bool,
        whole_word: bool,
        opts: ListingOptions,
    ) -> Result<Vec<FindResult>, ApiError> {
        const MAX: usize = 500;
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let needle = if match_case {
            text.to_string()
        } else {
            text.to_lowercase()
        };
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '@';
        let found = |hay: &str| {
            let hay = if match_case {
                hay.to_string()
            } else {
                hay.to_lowercase()
            };
            hay.match_indices(&needle).any(|(i, m)| {
                !whole_word
                    || (!hay[..i].chars().next_back().is_some_and(word)
                        && !hay[i + m.len()..].chars().next().is_some_and(word))
            })
        };
        let view = self.view()?;
        let mut out = Vec::new();
        let mut addr = 0;
        while addr < cpu.space() && out.len() < MAX {
            let (len, rows) = self.item_rows(cpu, view, addr, opts, &[])?;
            for r in rows.iter().filter(|r| r.kind == RowKind::Instruction) {
                if out.len() < MAX && found(&Self::row_haystack(r, false)) {
                    let mut text = r.text.clone();
                    if let Some(e) = &r.effective {
                        text.push(' ');
                        text.push_str(e);
                    }
                    out.push(FindResult { pos: r.pos, text });
                }
            }
            addr += len;
        }
        Ok(out)
    }

    /// The function list (65C816): every subroutine entry the code log
    /// saw in the program ROM, by ROM offset.
    #[must_use]
    pub fn functions(&self) -> Vec<FunctionRow> {
        self.code_log
            .offsets_with(MemoryKind::SnesPrgRom, SUB_ENTRY)
            .into_iter()
            .map(|rom| {
                let name = self.labels.name_at(MemoryKind::SnesPrgRom, rom);
                let address = self.rom_address(rom);
                FunctionRow { name, rom, address }
            })
            .collect()
    }

    /// Where the 65C816 sees program-ROM offset `rom` now.
    fn rom_address(&self, rom: u32) -> Option<u32> {
        let view = self.view().ok()?;
        let first = self.cpu_state().ok().map_or(0, |c| u32::from(c.pb) & 0xC0);
        (0..0x100u32).find_map(|b| {
            let bank = (first + b) & 0xFF;
            (0..16u32).find_map(|page| {
                let base = (bank << 16) | (page << 12);
                let at = view.absolute(base)?;
                let delta = rom.checked_sub(at.offset)?;
                if at.kind != MemoryKind::SnesPrgRom || delta >= 0x1000 {
                    return None;
                }
                (view.absolute(base + delta)
                    == Some(MemoryLocation {
                        kind: MemoryKind::SnesPrgRom,
                        offset: rom,
                    }))
                .then_some(base + delta)
            })
        })
    }

    /// The label list of a CPU: every label of a memory it sees, the
    /// register defaults included, with where it sees each now.
    #[must_use]
    pub fn label_rows(&self, cpu: ListingCpu) -> Vec<LabelRow> {
        self.labels
            .iter()
            .filter(|l| sees(cpu, l.memory))
            .map(|l| LabelRow {
                label: l.clone(),
                address: if l.name.is_empty() {
                    None
                } else {
                    self.label_address(&l.name, bus(cpu)).ok().flatten()
                },
            })
            .collect()
    }

    /// The call stack as Mesen2's pane lists it, innermost first: the
    /// current instruction in the routine last entered, then each call
    /// with the routine it was made from, down to `[bottom of stack]`.
    /// Empty while call-stack tracking is off.
    #[must_use]
    pub fn call_stack_rows(&self) -> Vec<CallStackRow> {
        let Some(stack) = &self.call_stack else {
            return Vec::new();
        };
        let Ok(view) = self.view() else {
            return Vec::new();
        };
        let name = |pc: u32, kind: Option<CallKind>| {
            let label = self
                .label_at(MemoryKind::SnesMemory, pc)
                .ok()
                .flatten()
                .and_then(|h| h.name())
                .or_else(|| {
                    self.symbols
                        .as_ref()
                        .and_then(|t| t.nearest(pc))
                        .filter(|n| !n.contains('+'))
                });
            match (kind, label) {
                (Some(CallKind::Nmi), _) => format!("[nmi] ${pc:06X}"),
                (Some(CallKind::Irq), _) => format!("[irq] ${pc:06X}"),
                (_, Some(l)) => format!("{l} (${pc:06X})"),
                _ => format!("${pc:06X}"),
            }
        };
        let row = |function: String, pc: u32| CallStackRow {
            function,
            pc,
            absolute: view.absolute(pc),
        };
        let mut out = Vec::new();
        let Ok(c) = self.cpu_state() else {
            return out;
        };
        let here = (u32::from(c.pb) << 16) | u32::from(c.pc);
        // Each frame: the routine entered (`pc`) and the call that entered
        // it (`from`), which sits in the routine of the frame below.
        let mut function = stack.last().map_or_else(
            || "[bottom of stack]".to_string(),
            |f| name(f.pc, Some(f.kind)),
        );
        out.push(row(function.clone(), here));
        for (i, f) in stack.iter().enumerate().rev() {
            function = if i == 0 {
                "[bottom of stack]".to_string()
            } else {
                let below = &stack[i - 1];
                name(below.pc, Some(below.kind))
            };
            out.push(row(function.clone(), f.from));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_take_mesen2s_formats() {
        assert_eq!(format_number(0x12, Format::Hex(1)), "$12");
        assert_eq!(format_number(0x1234, Format::Hex(1)), "$34");
        assert_eq!(format_number(0x1234, Format::Hex(2)), "$1234");
        assert_eq!(format_number(0xFF, Format::Signed(1)), "-1");
        assert_eq!(format_number(0xFF, Format::Signed(4)), "255");
        assert_eq!(format_number(0x12, Format::Binary(1)), "%0001.0010");
        assert_eq!(format_number(300, Format::Unsigned(None)), "300");
        assert_eq!(format_number(300, Format::Unsigned(Some(1))), "44");
        assert_eq!(split_format("[$10], H2"), ("[$10]", Some(('H', Some(2)))));
        assert_eq!(split_format("x, S"), ("x", Some(('S', None))));
        assert_eq!(
            split_format("[$10, 4]"),
            ("[$10, 4]", None),
            "an array, not a suffix"
        );
        assert_eq!(split_format("x, h2"), ("x, h2", None), "upper case only");
    }

    #[test]
    fn go_to_all_terms_split_at_capitals() {
        assert_eq!(terms("PlayerX"), ["player"], "a one-letter term goes");
        assert_eq!(terms("nmi handler"), ["nmi", "handler"]);
        assert_eq!(terms("x"), ["x"]);
    }
}
