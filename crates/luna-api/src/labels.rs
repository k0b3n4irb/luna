//! Debugger labels and comments, kept by memory and offset, and their
//! Mesen2 `.mlb` file format.
//!
//! A label names a byte (or a run of bytes) of one memory — a ROM
//! offset, a work-RAM offset, an I/O register — not a bus address, so
//! one ROM label shows at every mirror of its byte. A label may carry a
//! comment; a comment may stand alone. Names are unique, and one byte
//! holds at most one label: a new label with a used name, or over a used
//! byte, replaces the old one whole.
//!
//! `.mlb` lines read `Memory:ADDR[-END]:Name[:Comment]`: the memory by
//! its Mesen2 name, the offset in hex with no prefix, both range ends
//! inclusive, the comment's line breaks written `\n`. The defaults (the
//! SNES and SPC700 register names) are ordinary labels, written by an
//! export like any other.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::memory::MemoryKind;

/// One label: a name and/or a comment on `length` bytes of a memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Label {
    /// The memory the bytes are in.
    #[schemars(with = "String")]
    pub memory: MemoryKind,
    /// Offset of the first byte in that memory.
    pub address: u32,
    /// Bytes covered, 1 to 65536.
    pub length: u32,
    /// The name: letters, digits, `_` and `@`, not starting with a digit.
    /// Empty for a comment on its own.
    pub name: String,
    /// Free text, lines separated by `\n`; empty for none.
    pub comment: String,
}

/// Why a label was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelError {
    /// The name has a character outside letters, digits, `_`, `@`, or
    /// starts with a digit.
    BadName(String),
    /// The length is 0 or above 65536.
    BadLength(u32),
    /// Neither a name nor a comment.
    Empty,
}

impl std::fmt::Display for LabelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName(n) => write!(
                f,
                "label name `{n}`: letters, digits, `_` and `@` only, not starting with a digit"
            ),
            Self::BadLength(l) => write!(f, "label length {l}: 1 to 65536 bytes"),
            Self::Empty => write!(f, "a label needs a name or a comment"),
        }
    }
}

impl std::error::Error for LabelError {}

/// What an `.mlb` import did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct MlbImport {
    /// Lines that became a label.
    pub imported: usize,
    /// Lines that did not parse (an empty line counts, as in Mesen2).
    pub errors: usize,
    /// Valid lines for a memory this cartridge does not have.
    pub skipped: usize,
}

/// The labels of one session.
#[derive(Clone, Debug, Default)]
pub struct LabelStore {
    /// Every label by (export order of its memory, first byte).
    labels: BTreeMap<(u8, u32), Label>,
    /// The first byte of the label each labelled byte belongs to.
    slots: BTreeMap<(MemoryKind, u32), u32>,
    /// Where each name is.
    names: BTreeMap<String, (MemoryKind, u32)>,
}

/// The longest name kept, in bytes (longer names are cut, as in Mesen2).
const NAME_MAX: usize = 400;

/// Mesen2's numeric id of a memory type: the export order, and a type
/// an `.mlb` may give by number.
const fn mesen_id(kind: MemoryKind) -> u8 {
    match kind {
        MemoryKind::SnesMemory => 0,
        MemoryKind::SpcMemory => 1,
        MemoryKind::Sa1Memory => 2,
        MemoryKind::GsuMemory => 4,
        MemoryKind::SnesPrgRom => 14,
        MemoryKind::SnesWorkRam => 15,
        MemoryKind::SnesSaveRam => 16,
        MemoryKind::SnesVideoRam => 17,
        MemoryKind::SnesSpriteRam => 18,
        MemoryKind::SnesCgRam => 19,
        MemoryKind::SnesRegister => 20,
        MemoryKind::SpcRam => 21,
        MemoryKind::SpcRom => 22,
        MemoryKind::SpcDspRegisters => 23,
        MemoryKind::DspProgramRom => 24,
        MemoryKind::DspDataRom => 25,
        MemoryKind::DspDataRam => 26,
        MemoryKind::Sa1InternalRam => 27,
        MemoryKind::GsuWorkRam => 28,
    }
}

/// The memory an `.mlb` names: Mesen2's name (case matters), a legacy
/// Mesen-S prefix, or a numeric id. `NecDspMemory` (the DSP's program
/// space, numbered like its program ROM) reads as `DspProgramRom`.
fn mlb_memory(field: &str) -> Option<MemoryKind> {
    if let Some(k) = MemoryKind::ALL.into_iter().find(|k| k.name() == field) {
        return Some(k);
    }
    let legacy = match field {
        "PRG" => MemoryKind::SnesPrgRom,
        "DSPPRG" | "NecDspMemory" => MemoryKind::DspProgramRom,
        "WORK" => MemoryKind::SnesWorkRam,
        "SAVE" => MemoryKind::SnesSaveRam,
        "REG" => MemoryKind::SnesRegister,
        "IRAM" => MemoryKind::Sa1InternalRam,
        "SPCRAM" => MemoryKind::SpcRam,
        "SPCROM" => MemoryKind::SpcRom,
        _ => {
            let id: u8 = field.parse().ok()?;
            if id == 3 {
                return Some(MemoryKind::DspProgramRom);
            }
            return MemoryKind::ALL.into_iter().find(|&k| mesen_id(k) == id);
        }
    };
    Some(legacy)
}

/// Whether `name` may name a label.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '@')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '@')
}

impl LabelStore {
    /// No labels at all.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            labels: BTreeMap::new(),
            slots: BTreeMap::new(),
            names: BTreeMap::new(),
        }
    }

    /// The default labels only: the SNES I/O registers (`SnesRegister`)
    /// and the SPC700's (`SpcRam` `$F0-$FF`).
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut s = Self::new();
        for (memory, address, name, comment) in default_labels() {
            s.insert(Label {
                memory,
                address,
                length: 1,
                name,
                comment: comment.to_string(),
            });
        }
        s
    }

    /// Number of labels (comment-only ones included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// `true` when there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Every label, in export order: by memory (Mesen2's order), then
    /// address.
    pub fn iter(&self) -> impl Iterator<Item = &Label> {
        self.labels.values()
    }

    /// Add `label`, replacing the label that has its name and every label
    /// on one of its bytes. A name is cut to 400 bytes.
    pub fn set(&mut self, mut label: Label) -> Result<(), LabelError> {
        if label.length == 0 || label.length > 0x1_0000 {
            return Err(LabelError::BadLength(label.length));
        }
        if !label.name.is_empty() && !valid_name(&label.name) {
            return Err(LabelError::BadName(label.name));
        }
        if label.name.is_empty() && label.comment.is_empty() {
            return Err(LabelError::Empty);
        }
        label.name.truncate(NAME_MAX);
        self.insert(label);
        Ok(())
    }

    /// [`Self::set`] without the checks.
    fn insert(&mut self, label: Label) {
        if let Some(&(m, a)) = self.names.get(&label.name) {
            self.remove(m, a);
        }
        for i in 0..label.length {
            let byte = label.address.wrapping_add(i);
            if let Some(&start) = self.slots.get(&(label.memory, byte)) {
                self.remove(label.memory, start);
            }
        }
        for i in 0..label.length {
            self.slots
                .insert((label.memory, label.address.wrapping_add(i)), label.address);
        }
        if !label.name.is_empty() {
            self.names
                .insert(label.name.clone(), (label.memory, label.address));
        }
        self.labels
            .insert((mesen_id(label.memory), label.address), label);
    }

    /// Remove the label that covers byte `address` of `memory`; the
    /// removed label, if there was one.
    pub fn remove(&mut self, memory: MemoryKind, address: u32) -> Option<Label> {
        let start = *self.slots.get(&(memory, address))?;
        let label = self.labels.remove(&(mesen_id(memory), start))?;
        for i in 0..label.length {
            self.slots.remove(&(memory, start.wrapping_add(i)));
        }
        self.names.remove(&label.name);
        Some(label)
    }

    /// The label covering byte `address` of `memory`, and that byte's
    /// index in it.
    #[must_use]
    pub fn at(&self, memory: MemoryKind, address: u32) -> Option<(&Label, u32)> {
        let start = *self.slots.get(&(memory, address))?;
        let label = self.labels.get(&(mesen_id(memory), start))?;
        Some((label, address.wrapping_sub(start)))
    }

    /// The name byte `address` of `memory` shows: `name` on a label's
    /// first byte, `name+N` on byte N; `None` for no label or a comment
    /// alone.
    #[must_use]
    pub fn name_at(&self, memory: MemoryKind, address: u32) -> Option<String> {
        let (label, i) = self.at(memory, address)?;
        if label.name.is_empty() {
            return None;
        }
        Some(if i == 0 {
            label.name.clone()
        } else {
            format!("{}+{i}", label.name)
        })
    }

    /// Where `name` is: `name`, or `name+N` (N decimal, or hex after
    /// `0x` / `$`) for byte N of it.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<(MemoryKind, u32)> {
        if let Some(&at) = self.names.get(name) {
            return Some(at);
        }
        let (base, n) = name.rsplit_once('+')?;
        let n = n.trim();
        let n = if let Some(h) = n.strip_prefix("0x").or_else(|| n.strip_prefix('$')) {
            u32::from_str_radix(h, 16).ok()?
        } else {
            n.parse().ok()?
        };
        let &(memory, start) = self.names.get(base.trim())?;
        let (label, _) = self.at(memory, start)?;
        (n < label.length).then(|| (memory, start + n))
    }

    /// Import `.mlb` text. `has(memory)` says whether this cartridge has
    /// that memory; lines for one it lacks are skipped.
    pub fn import_mlb(&mut self, text: &str, has: impl Fn(MemoryKind) -> bool) -> MlbImport {
        let mut out = MlbImport::default();
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        // LF, CRLF and a lone CR all end a line.
        for line in text.lines().flat_map(|l| l.split('\r')) {
            match parse_mlb_line(line) {
                None => out.errors += 1,
                Some(label) if !has(label.memory) => out.skipped += 1,
                Some(label) => {
                    if label.name.is_empty() && label.comment.is_empty() {
                        // An empty record clears what is at its byte.
                        self.remove(label.memory, label.address);
                    } else {
                        self.insert(label);
                    }
                    out.imported += 1;
                }
            }
        }
        out
    }

    /// Every label as `.mlb` text: UTF-8 with a byte-order mark, a line
    /// feed after each line, sorted by memory then address.
    #[must_use]
    pub fn to_mlb(&self) -> String {
        let mut s = String::from("\u{FEFF}");
        for l in self.iter() {
            s.push_str(l.memory.name());
            s.push(':');
            if l.length > 1 {
                let _ = write!(s, "{:04X}-{:04X}", l.address, l.address + (l.length - 1));
            } else {
                let _ = write!(s, "{:04X}", l.address);
            }
            s.push(':');
            s.push_str(&l.name);
            if !l.comment.trim().is_empty() {
                s.push(':');
                s.push_str(
                    &l.comment
                        .replace("\r\n", "\\n")
                        .replace(['\n', '\r'], "\\n"),
                );
            }
            s.push('\n');
        }
        s
    }
}

/// One `.mlb` line, or `None` if it does not parse.
fn parse_mlb_line(line: &str) -> Option<Label> {
    let mut fields = line.splitn(4, ':');
    let memory = mlb_memory(fields.next()?)?;
    let addr = fields.next()?;
    let name = fields.next()?;
    let comment = fields.next().unwrap_or("").replace("\\n", "\n");
    let hex = |s: &str| u32::from_str_radix(s.trim(), 16).ok();
    let (address, length) = match addr.split_once('-') {
        Some((lo, hi)) => {
            let (lo, hi) = (hex(lo)?, hex(hi)?);
            if hi <= lo || hi - lo >= 0x1_0000 {
                return None;
            }
            (lo, hi - lo + 1)
        }
        None => (hex(addr)?, 1),
    };
    if !name.is_empty() && !valid_name(name) {
        return None;
    }
    let mut name = name.to_string();
    name.truncate(NAME_MAX);
    Some(Label {
        memory,
        address,
        length,
        name,
        comment,
    })
}

/// The SNES and SPC700 register names, with a word on each.
fn default_labels() -> Vec<(MemoryKind, u32, String, &'static str)> {
    const PPU: [(u32, &str, &str); 68] = [
        (
            0x2100,
            "INIDISP",
            "Display control: forced blank, brightness",
        ),
        (0x2101, "OBSEL", "Sprite sizes and character base"),
        (0x2102, "OAMADDL", "OAM address, low"),
        (0x2103, "OAMADDH", "OAM address, high; priority rotation"),
        (0x2104, "OAMDATA", "OAM data write"),
        (0x2105, "BGMODE", "BG mode, BG3 priority, tile sizes"),
        (0x2106, "MOSAIC", "Mosaic size and enabled BGs"),
        (0x2107, "BG1SC", "BG1 tilemap address and size"),
        (0x2108, "BG2SC", "BG2 tilemap address and size"),
        (0x2109, "BG3SC", "BG3 tilemap address and size"),
        (0x210A, "BG4SC", "BG4 tilemap address and size"),
        (0x210B, "BG12NBA", "BG1 and BG2 character base"),
        (0x210C, "BG34NBA", "BG3 and BG4 character base"),
        (
            0x210D,
            "BG1HOFS",
            "BG1 horizontal scroll; Mode 7 horizontal scroll",
        ),
        (
            0x210E,
            "BG1VOFS",
            "BG1 vertical scroll; Mode 7 vertical scroll",
        ),
        (0x210F, "BG2HOFS", "BG2 horizontal scroll"),
        (0x2110, "BG2VOFS", "BG2 vertical scroll"),
        (0x2111, "BG3HOFS", "BG3 horizontal scroll"),
        (0x2112, "BG3VOFS", "BG3 vertical scroll"),
        (0x2113, "BG4HOFS", "BG4 horizontal scroll"),
        (0x2114, "BG4VOFS", "BG4 vertical scroll"),
        (0x2115, "VMAIN", "VRAM address increment and remapping"),
        (0x2116, "VMADDL", "VRAM word address, low"),
        (0x2117, "VMADDH", "VRAM word address, high"),
        (0x2118, "VMDATAL", "VRAM data write, low byte"),
        (0x2119, "VMDATAH", "VRAM data write, high byte"),
        (0x211A, "M7SEL", "Mode 7 flips and screen-over"),
        (0x211B, "M7A", "Mode 7 matrix A; multiplicand"),
        (0x211C, "M7B", "Mode 7 matrix B; multiplier"),
        (0x211D, "M7C", "Mode 7 matrix C"),
        (0x211E, "M7D", "Mode 7 matrix D"),
        (0x211F, "M7X", "Mode 7 centre X"),
        (0x2120, "M7Y", "Mode 7 centre Y"),
        (0x2121, "CGADD", "CGRAM address"),
        (0x2122, "CGDATA", "CGRAM data write"),
        (0x2123, "W12SEL", "Window mask settings, BG1 and BG2"),
        (0x2124, "W34SEL", "Window mask settings, BG3 and BG4"),
        (
            0x2125,
            "WOBJSEL",
            "Window mask settings, sprites and colour",
        ),
        (0x2126, "WH0", "Window 1 left position"),
        (0x2127, "WH1", "Window 1 right position"),
        (0x2128, "WH2", "Window 2 left position"),
        (0x2129, "WH3", "Window 2 right position"),
        (0x212A, "WBGLOG", "Window logic, BGs"),
        (0x212B, "WOBJLOG", "Window logic, sprites and colour"),
        (0x212C, "TM", "Main screen layers"),
        (0x212D, "TS", "Sub screen layers"),
        (0x212E, "TMW", "Main screen window masking"),
        (0x212F, "TSW", "Sub screen window masking"),
        (0x2130, "CGWSEL", "Colour math control A"),
        (
            0x2131,
            "CGADSUB",
            "Colour math control B: add/subtract, layers",
        ),
        (0x2132, "COLDATA", "Fixed colour"),
        (
            0x2133,
            "SETINI",
            "Screen settings: interlace, overscan, pseudo hi-res",
        ),
        (0x2134, "MPYL", "Multiplication result, low"),
        (0x2135, "MPYM", "Multiplication result, middle"),
        (0x2136, "MPYH", "Multiplication result, high"),
        (0x2137, "SLHV", "Latch the H/V counters"),
        (0x2138, "OAMDATAREAD", "OAM data read"),
        (0x2139, "VMDATALREAD", "VRAM data read, low byte"),
        (0x213A, "VMDATAHREAD", "VRAM data read, high byte"),
        (0x213B, "CGDATAREAD", "CGRAM data read"),
        (0x213C, "OPHCT", "Latched horizontal counter"),
        (0x213D, "OPVCT", "Latched vertical counter"),
        (0x213E, "STAT77", "PPU1 status and version"),
        (0x213F, "STAT78", "PPU2 status and version"),
        (0x2140, "APUIO0", "APU port 0"),
        (0x2141, "APUIO1", "APU port 1"),
        (0x2142, "APUIO2", "APU port 2"),
        (0x2143, "APUIO3", "APU port 3"),
    ];
    const CPU: [(u32, &str, &str); 36] = [
        (0x2180, "WMDATA", "Work RAM data"),
        (0x2181, "WMADDL", "Work RAM address, low"),
        (0x2182, "WMADDM", "Work RAM address, middle"),
        (0x2183, "WMADDH", "Work RAM address, high bit"),
        (0x4016, "JOYSER0", "Joypad port 1 serial; latch"),
        (0x4017, "JOYSER1", "Joypad port 2 serial"),
        (
            0x4200,
            "NMITIMEN",
            "NMI, timer IRQs and joypad auto-read enable",
        ),
        (0x4201, "WRIO", "Programmable I/O port out"),
        (0x4202, "WRMPYA", "Multiplicand"),
        (0x4203, "WRMPYB", "Multiplier; starts the multiplication"),
        (0x4204, "WRDIVL", "Dividend, low"),
        (0x4205, "WRDIVH", "Dividend, high"),
        (0x4206, "WRDIVB", "Divisor; starts the division"),
        (0x4207, "HTIMEL", "H timer, low"),
        (0x4208, "HTIMEH", "H timer, high bit"),
        (0x4209, "VTIMEL", "V timer, low"),
        (0x420A, "VTIMEH", "V timer, high bit"),
        (0x420B, "MDMAEN", "Start DMA channels"),
        (0x420C, "HDMAEN", "HDMA channels enabled"),
        (0x420D, "MEMSEL", "ROM access speed"),
        (0x4210, "RDNMI", "NMI flag and CPU version"),
        (0x4211, "TIMEUP", "Timer IRQ flag"),
        (0x4212, "HVBJOY", "Blanking and joypad auto-read status"),
        (0x4213, "RDIO", "Programmable I/O port in"),
        (0x4214, "RDDIVL", "Quotient, low"),
        (0x4215, "RDDIVH", "Quotient, high"),
        (0x4216, "RDMPYL", "Product or remainder, low"),
        (0x4217, "RDMPYH", "Product or remainder, high"),
        (0x4218, "JOY1L", "Joypad 1 auto-read, low"),
        (0x4219, "JOY1H", "Joypad 1 auto-read, high"),
        (0x421A, "JOY2L", "Joypad 2 auto-read, low"),
        (0x421B, "JOY2H", "Joypad 2 auto-read, high"),
        (0x421C, "JOY3L", "Joypad 3 auto-read, low"),
        (0x421D, "JOY3H", "Joypad 3 auto-read, high"),
        (0x421E, "JOY4L", "Joypad 4 auto-read, low"),
        (0x421F, "JOY4H", "Joypad 4 auto-read, high"),
    ];
    // `$43n0-$43nA` of channel n: the name puts n between the stem and
    // the suffix (`A1T3L`, `NTLR7`).
    const DMA: [(&str, &str, &str); 11] = [
        ("DMAP", "", "DMA transfer mode and direction"),
        ("BBAD", "", "DMA B-bus address"),
        ("A1T", "L", "DMA A-bus address, low"),
        ("A1T", "H", "DMA A-bus address, high"),
        ("A1B", "", "DMA A-bus bank"),
        (
            "DAS",
            "L",
            "DMA byte count, low; HDMA indirect address, low",
        ),
        (
            "DAS",
            "H",
            "DMA byte count, high; HDMA indirect address, high",
        ),
        ("DAS", "B", "HDMA indirect bank"),
        ("A2A", "L", "HDMA table address, low"),
        ("A2A", "H", "HDMA table address, high"),
        ("NTLR", "", "HDMA line counter"),
    ];
    const SPC: [(u32, &str, &str); 16] = [
        (0xF0, "TEST", "Test register"),
        (0xF1, "CONTROL", "Timers, port clears, IPL ROM enable"),
        (0xF2, "DSPADDR", "S-DSP register address"),
        (0xF3, "DSPDATA", "S-DSP register data"),
        (0xF4, "CPUIO0", "CPU port 0"),
        (0xF5, "CPUIO1", "CPU port 1"),
        (0xF6, "CPUIO2", "CPU port 2"),
        (0xF7, "CPUIO3", "CPU port 3"),
        (0xF8, "RAMREG1", "Spare RAM byte 1"),
        (0xF9, "RAMREG2", "Spare RAM byte 2"),
        (0xFA, "T0TARGET", "Timer 0 divider"),
        (0xFB, "T1TARGET", "Timer 1 divider"),
        (0xFC, "T2TARGET", "Timer 2 divider"),
        (0xFD, "T0OUT", "Timer 0 output"),
        (0xFE, "T1OUT", "Timer 1 output"),
        (0xFF, "T2OUT", "Timer 2 output"),
    ];
    let mut out: Vec<(MemoryKind, u32, String, &'static str)> = PPU
        .iter()
        .chain(CPU.iter())
        .map(|&(a, n, c)| (MemoryKind::SnesRegister, a, n.to_string(), c))
        .collect();
    for ch in 0..8u32 {
        for (i, &(stem, suffix, comment)) in DMA.iter().enumerate() {
            out.push((
                MemoryKind::SnesRegister,
                0x4300 + ch * 0x10 + i as u32,
                format!("{stem}{ch}{suffix}"),
                comment,
            ));
        }
    }
    out.extend(
        SPC.iter()
            .map(|&(a, n, c)| (MemoryKind::SpcRam, a, n.to_string(), c)),
    );
    out
}

/// A label found at a bus address: the label, and which of its bytes
/// the address is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct LabelHit {
    /// The label.
    pub label: Label,
    /// The byte of it at that address (0 = its first).
    pub byte: u32,
}

impl LabelHit {
    /// The name the address shows: `name`, `name+N` past the first byte;
    /// `None` for a comment alone.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        match (self.label.name.as_str(), self.byte) {
            ("", _) => None,
            (n, 0) => Some(n.to_string()),
            (n, i) => Some(format!("{n}+{i}")),
        }
    }
}

impl crate::Emulator {
    /// The label and the comment a disassembly line at `addr` on `bus`
    /// shows: the name (`name+N` inside a label), and the comment of a
    /// label that starts there.
    pub(crate) fn line_label(
        &self,
        bus: MemoryKind,
        addr: u32,
    ) -> (Option<String>, Option<String>) {
        match self.label_at(bus, addr) {
            Ok(Some(hit)) => {
                let comment = (hit.byte == 0 && !hit.label.comment.is_empty())
                    .then(|| hit.label.comment.clone());
                (hit.name(), comment)
            }
            _ => (None, None),
        }
    }

    /// Every label, sorted by memory then address.
    #[must_use]
    pub fn labels(&self) -> Vec<Label> {
        self.labels.iter().cloned().collect()
    }

    /// Add or replace a label (see [`LabelStore::set`]).
    pub fn set_label(&mut self, label: Label) -> Result<(), crate::ApiError> {
        self.labels
            .set(label)
            .map_err(|e| crate::ApiError::BadArg(e.to_string()))
    }

    /// Remove the label covering byte `address` of `memory`.
    pub fn remove_label(&mut self, memory: MemoryKind, address: u32) -> Option<Label> {
        self.labels.remove(memory, address)
    }

    /// Back to the default labels (none when they are off, see
    /// [`Self::set_default_labels`]).
    pub fn reset_labels(&mut self) {
        self.labels = if self.default_labels {
            LabelStore::with_defaults()
        } else {
            LabelStore::new()
        };
    }

    /// Whether the SNES and SPC700 register names are put back by
    /// [`Self::reset_labels`] and at each ROM load (on by default).
    pub const fn set_default_labels(&mut self, on: bool) {
        self.default_labels = on;
    }

    /// Import `.mlb` text. With `reset`, the labels go back to the
    /// defaults first (Mesen2's default); without, the file's labels are
    /// merged in and win over the ones they meet.
    pub fn import_labels_mlb(
        &mut self,
        text: &str,
        reset: bool,
    ) -> Result<MlbImport, crate::ApiError> {
        let view = self.view()?;
        let sizes: Vec<(MemoryKind, usize)> = MemoryKind::ALL
            .into_iter()
            .map(|k| (k, view.size(k)))
            .collect();
        if reset {
            self.reset_labels();
        }
        Ok(self
            .labels
            .import_mlb(text, |k| sizes.iter().any(|&(m, s)| m == k && s > 0)))
    }

    /// [`Self::import_labels_mlb`] from a file.
    pub fn load_labels(
        &mut self,
        path: &std::path::Path,
        reset: bool,
    ) -> Result<MlbImport, crate::ApiError> {
        let text = std::fs::read_to_string(path)?;
        self.import_labels_mlb(&text, reset)
    }

    /// Every label as an `.mlb` file (see [`LabelStore::to_mlb`]).
    #[must_use]
    pub fn export_labels_mlb(&self) -> String {
        self.labels.to_mlb()
    }

    /// The label at `addr` on a bus (`SnesMemory` or `SpcMemory`): one
    /// given on the bus itself first, then the one on the memory behind
    /// the address now. Any other kind is looked up as it is.
    pub fn label_at(
        &self,
        bus: MemoryKind,
        addr: u32,
    ) -> Result<Option<LabelHit>, crate::ApiError> {
        let hit = |memory, a| {
            self.labels.at(memory, a).map(|(l, byte)| LabelHit {
                label: l.clone(),
                byte,
            })
        };
        if let Some(h) = hit(bus, addr) {
            return Ok(Some(h));
        }
        let snes = self.snes.as_ref().ok_or(crate::ApiError::NoRom)?;
        let behind = match bus {
            MemoryKind::SnesMemory => self.view()?.absolute(addr).map(|l| (l.kind, l.offset)),
            MemoryKind::SpcMemory => {
                let a = addr & 0xFFFF;
                Some(if a >= 0xFFC0 && snes.apu_real.control & 0x80 != 0 {
                    (MemoryKind::SpcRom, a & 0x3F)
                } else {
                    (MemoryKind::SpcRam, a)
                })
            }
            _ => None,
        };
        Ok(behind.and_then(|(m, a)| hit(m, a)))
    }

    /// Where on `bus` (`SnesMemory` or `SpcMemory`) the label `name` (or
    /// `name+N`) is seen now: a work-RAM label at `$7E:xxxx`, a register
    /// in bank `$00`, a cartridge label at its first mirror from the
    /// program bank's quarter of the map. `None` if there is no such
    /// label, or the bus cannot see its memory.
    pub fn label_address(
        &self,
        name: &str,
        bus: MemoryKind,
    ) -> Result<Option<u32>, crate::ApiError> {
        let Some((memory, offset)) = self.labels.find(name) else {
            return Ok(None);
        };
        self.bus_address(bus, memory, offset)
    }

    /// Where on `bus` (`SnesMemory` or `SpcMemory`) byte `offset` of
    /// `memory` is seen now: work RAM at `$7E:xxxx`, a register in bank
    /// `$00`, a cartridge byte at its first mirror from the program bank's
    /// quarter of the map. `None` if the bus cannot see it.
    pub fn bus_address(
        &self,
        bus: MemoryKind,
        memory: MemoryKind,
        offset: u32,
    ) -> Result<Option<u32>, crate::ApiError> {
        let view = self.view()?;
        if memory == bus {
            return Ok(Some(offset));
        }
        Ok(match (bus, memory) {
            (MemoryKind::SpcMemory, MemoryKind::SpcRam) => Some(offset & 0xFFFF),
            (MemoryKind::SpcMemory, MemoryKind::SpcRom) => Some(0xFFC0 + (offset & 0x3F)),
            (MemoryKind::SnesMemory, MemoryKind::SnesWorkRam) => Some(0x7E_0000 + offset),
            (MemoryKind::SnesMemory, MemoryKind::SnesRegister) => Some(offset & 0xFFFF),
            (MemoryKind::SnesMemory, _) => {
                let first = u32::from(self.cpu_state()?.pb) & 0xC0;
                (0..0x100u32).find_map(|b| {
                    let bank = (first + b) & 0xFF;
                    (0..16u32).find_map(|page| {
                        let base = (bank << 16) | (page << 12);
                        let at = view.absolute(base)?;
                        let delta = offset.checked_sub(at.offset)?;
                        if at.kind != memory || delta >= 0x1000 {
                            return None;
                        }
                        let addr = base + delta;
                        (view.absolute(addr)
                            == Some(crate::MemoryLocation {
                                kind: memory,
                                offset,
                            }))
                        .then_some(addr)
                    })
                })
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(text: &str) -> (LabelStore, MlbImport) {
        let mut s = LabelStore::new();
        let r = s.import_mlb(text, |_| true);
        (s, r)
    }

    #[test]
    fn mlb_lines_parse_as_mesen2_reads_them() {
        // The lines the clean-room spec measured on Mesen2, with what it
        // made of each.
        let (s, r) = store(concat!(
            "SnesPrgRom:0000:Reset\n",
            "SnesPrgRom:0010-0013:Table4:a table\\nsecond line: with colon\n",
            "SnesWorkRam:0100::comment only\n",
            "SnesWorkRam:1FFFF:LastWram\n",
            "WORK:0200:LegacyWork\n",
            "snesworkram:0300:LowerType\n",
            "15:0400:NumericType\n",
            "SnesWorkRam:$0500:DollarAddr\n",
            "SnesWorkRam:0x0600:HexPrefix\n",
            "SnesWorkRam: 0700 :SpacedAddr\n",
            "SnesWorkRam:0800: SpacedName\n",
            "SnesWorkRam:0900:9digit\n",
            "SnesWorkRam:0A00:has-dash\n",
            "SnesWorkRam:0B00:@local_1\n",
            "SnesWorkRam:0C00:Dup\n",
            "SnesWorkRam:0C10:Dup\n",
            "SnesWorkRam:0D00:First\n",
            "SnesWorkRam:0D00:Second\n",
            "SnesWorkRam:0E05-0E01:Reversed\n",
            "SnesWorkRam:0E10-0E10:SameEnd\n",
            "SnesWorkRam:1100:CRLF\r\n",
            "SnesWorkRam:1200:abc:def:ghi\n",
            "\n",
            "NoColon\n",
        ));
        let at = |n: &str| s.find(n);
        assert_eq!(at("Reset"), Some((MemoryKind::SnesPrgRom, 0)));
        assert_eq!(at("Table4"), Some((MemoryKind::SnesPrgRom, 0x10)));
        assert_eq!(at("Table4+2"), Some((MemoryKind::SnesPrgRom, 0x12)));
        assert_eq!(at("Table4+4"), None, "past its end");
        assert_eq!(at("LastWram"), Some((MemoryKind::SnesWorkRam, 0x1FFFF)));
        assert_eq!(at("LegacyWork"), Some((MemoryKind::SnesWorkRam, 0x200)));
        assert_eq!(at("NumericType"), Some((MemoryKind::SnesWorkRam, 0x400)));
        assert_eq!(at("SpacedAddr"), Some((MemoryKind::SnesWorkRam, 0x700)));
        assert_eq!(at("@local_1"), Some((MemoryKind::SnesWorkRam, 0xB00)));
        assert_eq!(
            at("Dup"),
            Some((MemoryKind::SnesWorkRam, 0xC10)),
            "last wins"
        );
        assert_eq!(at("First"), None, "replaced at its byte");
        assert_eq!(at("Second"), Some((MemoryKind::SnesWorkRam, 0xD00)));
        assert_eq!(at("CRLF"), Some((MemoryKind::SnesWorkRam, 0x1100)));
        assert_eq!(at("abc"), Some((MemoryKind::SnesWorkRam, 0x1200)));
        assert_eq!(at("reset"), None, "names keep their case");
        for rejected in [
            "LowerType",
            "DollarAddr",
            "HexPrefix",
            "SpacedName",
            "9digit",
            "has-dash",
            "Reversed",
            "SameEnd",
        ] {
            assert_eq!(at(rejected), None, "{rejected}");
        }
        let (table, _) = s.at(MemoryKind::SnesPrgRom, 0x10).unwrap();
        assert_eq!(table.comment, "a table\nsecond line: with colon");
        assert_eq!(
            s.at(MemoryKind::SnesWorkRam, 0x1200).unwrap().0.comment,
            "def:ghi"
        );
        assert_eq!(s.at(MemoryKind::SnesWorkRam, 0x100).unwrap().0.name, "");
        assert_eq!(
            s.name_at(MemoryKind::SnesPrgRom, 0x10).as_deref(),
            Some("Table4")
        );
        assert_eq!(
            s.name_at(MemoryKind::SnesPrgRom, 0x13).as_deref(),
            Some("Table4+3")
        );
        // 8 rejected lines, the empty line and `NoColon`.
        assert_eq!(r.errors, 10);
        assert_eq!(r.imported, 14);
    }

    #[test]
    fn export_round_trips_and_follows_the_format() {
        let (s, _) = store(concat!(
            "SnesWorkRam:1FFFF:LastWram\n",
            "SnesPrgRom:0010-0013:Table4:two\\nlines\n",
            "SnesWorkRam:0100::comment only\n",
            "SpcRam:0040:SpcVar\n",
            "SnesMemory:002200:RelLabel\n",
        ));
        let text = s.to_mlb();
        assert_eq!(
            text,
            concat!(
                "\u{FEFF}",
                "SnesMemory:2200:RelLabel\n",
                "SnesPrgRom:0010-0013:Table4:two\\nlines\n",
                "SnesWorkRam:0100::comment only\n",
                "SnesWorkRam:1FFFF:LastWram\n",
                "SpcRam:0040:SpcVar\n",
            )
        );
        let (again, r) = store(&text);
        assert_eq!(r.errors, 0);
        assert_eq!(again.to_mlb(), text);
    }

    #[test]
    fn defaults_name_the_registers_and_a_user_label_replaces_one() {
        let mut s = LabelStore::with_defaults();
        assert_eq!(s.len(), 104 + 88 + 16);
        assert_eq!(s.find("INIDISP"), Some((MemoryKind::SnesRegister, 0x2100)));
        assert_eq!(s.find("A1T3L"), Some((MemoryKind::SnesRegister, 0x4332)));
        assert_eq!(s.find("NTLR7"), Some((MemoryKind::SnesRegister, 0x437A)));
        assert_eq!(s.find("T2OUT"), Some((MemoryKind::SpcRam, 0xFF)));
        s.import_mlb("SnesRegister:2100:MyInidisp\n", |_| true);
        assert_eq!(s.find("INIDISP"), None);
        assert_eq!(
            s.find("MyInidisp"),
            Some((MemoryKind::SnesRegister, 0x2100))
        );
        assert!(
            s.set(Label {
                memory: MemoryKind::SnesWorkRam,
                address: 0,
                length: 1,
                name: "bad name".into(),
                comment: String::new(),
            })
            .is_err()
        );
    }
}
