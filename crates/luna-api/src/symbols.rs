//! WLA-DX `.sym` symbol tables (issue #67, epic #63; v2 in issue #179).
//!
//! Parses the `wlalink` symbol-file format — the one every WLA-DX-built
//! SNES homebrew (including the `OpenSNES` SDK) emits next to its ROM:
//!
//! ```text
//! ; this file was created with wlalink
//! [labels]
//! 00:8000 main
//! 7e:0100 monster_x
//!
//! [definitions]
//! 00000010 SOME_CONSTANT
//! ```
//!
//! The label sections (`[labels]`, `[symbols]`, `[exports]` — the format
//! grew aliases over the years) carry `BB:AAAA name` addresses; the
//! `[definitions]` section carries `VVVVVVVV name` constants (assembler
//! `.define`s — values, not addresses). Every other section is skipped.
//!
//! v2 additions (issue #179):
//! - `[definitions]` constants resolve by name (they never participate
//!   in address annotation — a constant is not a location).
//! - Two **address spaces**: the 24-bit CPU bus and the SPC700's 16-bit
//!   ARAM ([`SymbolSpace`]). A wla-spc700 driver's `.sym` loads into the
//!   ARAM space (`SymbolTable::parse_spc`) so `disassemble_spc` can
//!   annotate without a `$00`-bank CPU label ever claiming an ARAM
//!   address. Loading one space never clobbers the other
//!   (`SymbolTable::replace_space`).
//! - Name lookups are binary searches (O(log n)); parsing dedups via a
//!   sort instead of the old per-line scan (O(n log n) total).
//!
//! The table answers both directions per space:
//! - name → address/value ([`SymbolTable::resolve`],
//!   `SymbolTable::resolve_spc`)
//! - address → nearest label at or below, same bank, as `name` or
//!   `name+0xNN` ([`SymbolTable::nearest`], `SymbolTable::nearest_spc`)

use std::path::Path;

/// Which address space a symbol lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolSpace {
    /// The 24-bit 65C816 bus (`bank << 16 | offset`).
    Cpu,
    /// The SPC700's 16-bit ARAM.
    Aram,
}

/// What a symbol names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A location — participates in nearest-label annotation.
    Label,
    /// A `[definitions]` constant — resolves by name only.
    Constant,
}

/// Why a name did not resolve ([`SymbolTable::lookup`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolError {
    /// No label or constant of that name, and no `name.<suffix>` label.
    Unknown(String),
    /// No exact entry, and more than one `name.<suffix>` label.
    Ambiguous {
        /// The name as asked.
        name: String,
        /// Every `name.<suffix>` label with its address, in name order.
        candidates: Vec<(String, u32)>,
    },
}

impl std::fmt::Display for SymbolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown symbol `{name}`"),
            Self::Ambiguous { name, candidates } => {
                write!(f, "ambiguous symbol `{name}`: ")?;
                for (i, (full, addr)) in candidates.iter().enumerate() {
                    let sep = if i == 0 { "" } else { ", " };
                    write!(
                        f,
                        "{sep}`{full}` (${:02X}:{:04X})",
                        addr >> 16,
                        addr & 0xFFFF
                    )?;
                }
                write!(f, " (write the full name)")
            }
        }
    }
}

impl std::error::Error for SymbolError {}

/// One parsed symbol.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    value: u32,
    space: SymbolSpace,
    kind: SymbolKind,
}

/// A loaded symbol table: name ↔ addresses/values across both spaces.
#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    /// All entries, load order (CPU load then ARAM load, or vice versa).
    entries: Vec<Entry>,
    /// Indices into `entries`, sorted by name (stable across spaces) —
    /// the O(log n) resolve index.
    by_name: Vec<usize>,
    /// CPU-space labels `(addr, entry index)`, sorted by address.
    cpu_by_addr: Vec<(u32, usize)>,
    /// ARAM-space labels `(addr, entry index)`, sorted by address.
    aram_by_addr: Vec<(u16, usize)>,
}

impl SymbolTable {
    /// Parse `wlalink` `.sym` text into the **CPU** space (labels) plus
    /// `[definitions]` constants. Never fails: unknown sections and
    /// malformed lines are skipped (the format has grown many optional
    /// sections; a debugger table should be liberal).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        Self::parse_into_space(text, SymbolSpace::Cpu)
    }

    /// Parse a wla-spc700 `.sym` into the **ARAM** space: label
    /// addresses keep their 16-bit offset (the `00:` bank the assembler
    /// emits is meaningless on the SPC700 bus). `[definitions]`
    /// constants parse the same as in [`Self::parse`].
    #[must_use]
    pub(crate) fn parse_spc(text: &str) -> Self {
        Self::parse_into_space(text, SymbolSpace::Aram)
    }

    fn parse_into_space(text: &str, space: SymbolSpace) -> Self {
        #[derive(PartialEq)]
        enum Section {
            Labels,
            Definitions,
            Other,
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut section = Section::Other;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some(s) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = match s.to_ascii_lowercase().as_str() {
                    "labels" | "symbols" | "exports" => Section::Labels,
                    "definitions" => Section::Definitions,
                    _ => Section::Other,
                };
                continue;
            }
            match section {
                Section::Labels => {
                    // `BB:AAAA name` — hex bank, hex offset, then the label.
                    let Some((addr_part, name)) = line.split_once(' ') else {
                        continue;
                    };
                    let Some((bank_s, off_s)) = addr_part.split_once(':') else {
                        continue;
                    };
                    let (Ok(bank), Ok(off)) = (
                        u8::from_str_radix(bank_s, 16),
                        u16::from_str_radix(off_s, 16),
                    ) else {
                        continue;
                    };
                    let name = name.trim();
                    if name.is_empty() {
                        continue;
                    }
                    let value = match space {
                        SymbolSpace::Cpu => (u32::from(bank) << 16) | u32::from(off),
                        SymbolSpace::Aram => u32::from(off),
                    };
                    entries.push(Entry {
                        name: name.to_string(),
                        value,
                        space,
                        kind: SymbolKind::Label,
                    });
                }
                Section::Definitions => {
                    // `VVVVVVVV name` — hex value (an assembler .define).
                    let Some((val_s, name)) = line.split_once(' ') else {
                        continue;
                    };
                    let Ok(value) = u32::from_str_radix(val_s, 16) else {
                        continue;
                    };
                    let name = name.trim();
                    if name.is_empty() {
                        continue;
                    }
                    entries.push(Entry {
                        name: name.to_string(),
                        value,
                        space,
                        kind: SymbolKind::Constant,
                    });
                }
                Section::Other => {}
            }
        }
        Self::build(entries)
    }

    /// Dedup (first definition wins per space, wlalink emits duplicates
    /// for section-local labels) and build the lookup indexes.
    fn build(entries: Vec<Entry>) -> Self {
        // Stable sort by (name, space) keeps load order among equals, so
        // "first definition wins" is a linear dedup pass.
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|&a, &b| {
            entries[a].name.cmp(&entries[b].name).then(
                (entries[a].space == SymbolSpace::Aram)
                    .cmp(&(entries[b].space == SymbolSpace::Aram)),
            )
        });
        let mut kept = vec![true; entries.len()];
        for pair in order.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if entries[a].name == entries[b].name && entries[a].space == entries[b].space {
                // Same name+space: the later file position loses.
                let loser = if a < b { b } else { a };
                kept[loser] = false;
            }
        }
        let entries: Vec<Entry> = entries
            .into_iter()
            .zip(kept)
            .filter_map(|(e, keep)| keep.then_some(e))
            .collect();

        let mut by_name: Vec<usize> = (0..entries.len()).collect();
        by_name.sort_by(|&a, &b| entries[a].name.cmp(&entries[b].name));

        let mut cpu_by_addr = Vec::new();
        let mut aram_by_addr = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if e.kind != SymbolKind::Label {
                continue;
            }
            match e.space {
                SymbolSpace::Cpu => cpu_by_addr.push((e.value, i)),
                SymbolSpace::Aram => aram_by_addr.push((e.value as u16, i)),
            }
        }
        cpu_by_addr.sort_by_key(|&(a, _)| a);
        aram_by_addr.sort_by_key(|&(a, _)| a);

        Self {
            entries,
            by_name,
            cpu_by_addr,
            aram_by_addr,
        }
    }

    /// Load and parse a CPU-space `.sym` file from disk.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Ok(Self::parse(&std::fs::read_to_string(path)?))
    }

    /// Load and parse an SPC700 `.sym` file from disk into the ARAM space.
    pub(crate) fn load_spc(path: &Path) -> std::io::Result<Self> {
        Ok(Self::parse_spc(&std::fs::read_to_string(path)?))
    }

    /// Replace this table's entries of `space` (labels **and** the
    /// constants that arrived with that load) with `other`'s, keeping
    /// the other space intact — so loading a driver's SPC symbols never
    /// clobbers the game's CPU symbols, and vice versa.
    pub(crate) fn replace_space(&mut self, space: SymbolSpace, other: Self) {
        let mut entries: Vec<Entry> = std::mem::take(&mut self.entries)
            .into_iter()
            .filter(|e| e.space != space)
            .collect();
        entries.extend(other.entries.into_iter().filter(|e| e.space == space));
        *self = Self::build(entries);
    }

    /// Number of symbols in the table (labels + constants, both spaces).
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no symbols were parsed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// O(log n) index of the first `by_name` slot whose name is `name`
    /// (there can be one per space).
    fn name_range_start(&self, name: &str) -> usize {
        self.by_name
            .partition_point(|&i| self.entries[i].name.as_str() < name)
    }

    fn find_in_space(&self, name: &str, space: SymbolSpace) -> Option<&Entry> {
        let start = self.name_range_start(name);
        self.by_name[start..]
            .iter()
            .map(|&i| &self.entries[i])
            .take_while(|e| e.name == name)
            .find(|e| e.space == space)
    }

    /// Resolve a CPU-space label to its 24-bit `bank:offset` address, or
    /// a `[definitions]` constant to its value. O(log n). A name with no
    /// exact entry resolves through [`Self::lookup`]'s suffix rule; an
    /// ambiguous one is `None` here.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<u32> {
        self.lookup(name).ok()
    }

    /// [`Self::resolve`] with the reason for a miss. The exact name wins.
    /// Failing that, `name` stands for the **only** CPU label spelled
    /// `name.<suffix>`: a C `static` reaches the `.sym` as
    /// `player_x.main` (its source file appended, so two files can each
    /// own a `player_x`), and the name its author reads in the C is
    /// `player_x`. Two or more such labels are an ambiguity, named in
    /// the error (`OpenSNES` ask, 2026-10-06).
    pub fn lookup(&self, name: &str) -> Result<u32, SymbolError> {
        if let Some(e) = self.find_in_space(name, SymbolSpace::Cpu) {
            return Ok(e.value);
        }
        let prefix = format!("{name}.");
        let start = self.name_range_start(&prefix);
        let found: Vec<&Entry> = self.by_name[start..]
            .iter()
            .map(|&i| &self.entries[i])
            .take_while(|e| e.name.starts_with(&prefix))
            .filter(|e| {
                e.space == SymbolSpace::Cpu
                    && e.kind == SymbolKind::Label
                    && e.name.len() > prefix.len()
            })
            .collect();
        match found.as_slice() {
            [] => Err(SymbolError::Unknown(name.to_string())),
            [only] => Ok(only.value),
            many => Err(SymbolError::Ambiguous {
                name: name.to_string(),
                candidates: many.iter().map(|e| (e.name.clone(), e.value)).collect(),
            }),
        }
    }

    /// The size in bytes the `.sym` records for a CPU label: WLA-DX writes
    /// a `_sizeof_<label>` constant beside every label it can measure.
    /// `name` follows [`Self::lookup`]'s rule (the exact label, else the
    /// only `name.<suffix>` one). `None` when the name is not a label or
    /// has no recorded size.
    #[must_use]
    pub fn size_of(&self, name: &str) -> Option<u32> {
        let label = |e: &&Entry| e.space == SymbolSpace::Cpu && e.kind == SymbolKind::Label;
        let full = match self.find_in_space(name, SymbolSpace::Cpu).filter(label) {
            Some(e) => e.name.clone(),
            None => {
                let prefix = format!("{name}.");
                let start = self.name_range_start(&prefix);
                let mut found = self.by_name[start..]
                    .iter()
                    .map(|&i| &self.entries[i])
                    .take_while(|e| e.name.starts_with(&prefix))
                    .filter(|e| label(e) && e.name.len() > prefix.len());
                let only = found.next()?;
                if found.next().is_some() {
                    return None;
                }
                only.name.clone()
            }
        };
        self.find_in_space(&format!("_sizeof_{full}"), SymbolSpace::Cpu)
            .filter(|e| e.kind == SymbolKind::Constant)
            .map(|e| e.value)
    }

    /// Resolve an ARAM-space label to its 16-bit offset. O(log n).
    #[must_use]
    pub(crate) fn resolve_spc(&self, name: &str) -> Option<u16> {
        self.find_in_space(name, SymbolSpace::Aram)
            .filter(|e| e.kind == SymbolKind::Label)
            .map(|e| e.value as u16)
    }

    /// Nearest CPU label at or below `addr` **within the same bank**,
    /// rendered as `name` (exact) or `name+0xNN`. The same-bank guard
    /// keeps a `$00`-bank label from claiming a `$7E` WRAM address.
    /// Constants never annotate.
    #[must_use]
    pub fn nearest(&self, addr: u32) -> Option<String> {
        let addr = addr & 0x00FF_FFFF;
        let idx = self.cpu_by_addr.partition_point(|&(a, _)| a <= addr);
        let &(label_addr, name_idx) = self.cpu_by_addr.get(idx.checked_sub(1)?)?;
        if label_addr >> 16 != addr >> 16 {
            return None;
        }
        Some(Self::annotate(
            &self.entries[name_idx].name,
            addr - label_addr,
        ))
    }

    /// The label at or below `addr` in the same bank, as `(name, label
    /// address)` — the un-annotated form [`Self::nearest`] renders, for
    /// folding many addresses onto one symbol (issue #227).
    #[must_use]
    pub(crate) fn nearest_label(&self, addr: u32) -> Option<(&str, u32)> {
        let addr = addr & 0x00FF_FFFF;
        let idx = self.cpu_by_addr.partition_point(|&(a, _)| a <= addr);
        let &(label_addr, name_idx) = self.cpu_by_addr.get(idx.checked_sub(1)?)?;
        if label_addr >> 16 != addr >> 16 {
            return None;
        }
        Some((self.entries[name_idx].name.as_str(), label_addr))
    }

    /// Nearest ARAM label at or below `addr`, rendered like
    /// [`Self::nearest`] (no bank guard — ARAM is one flat 64 KB).
    #[must_use]
    pub(crate) fn nearest_spc(&self, addr: u16) -> Option<String> {
        let idx = self.aram_by_addr.partition_point(|&(a, _)| a <= addr);
        let &(label_addr, name_idx) = self.aram_by_addr.get(idx.checked_sub(1)?)?;
        Some(Self::annotate(
            &self.entries[name_idx].name,
            u32::from(addr - label_addr),
        ))
    }

    fn annotate(name: &str, off: u32) -> String {
        if off == 0 {
            name.to_string()
        } else {
            format!("{name}+{off:#04X}")
        }
    }

    /// Iterate `(name, value)` pairs in load order (both spaces).
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYM: &str = "\
; this file was created with wlalink by ville helin
[information]
version 1

[labels]
00:8000 main
00:8012 main_loop
7e:0100 monster_x
7e:0102 monster_y

[definitions]
00000010 SOME_CONSTANT

[symbols]
00:9000 irq_handler
";

    #[test]
    fn parses_labels_symbols_and_definitions() {
        let t = SymbolTable::parse(SYM);
        assert_eq!(t.len(), 6);
        assert_eq!(t.resolve("main"), Some(0x00_8000));
        assert_eq!(t.resolve("monster_x"), Some(0x7E_0100));
        assert_eq!(t.resolve("irq_handler"), Some(0x00_9000));
        // [definitions] constants resolve to their value (v2, #179)...
        assert_eq!(t.resolve("SOME_CONSTANT"), Some(0x10));
        assert_eq!(t.resolve("nope"), None);
    }

    #[test]
    fn constants_never_annotate_addresses() {
        let t = SymbolTable::parse("[labels]\n00:8000 main\n[definitions]\n00008005 NEARBY\n");
        // ...but a constant whose value looks like an address never
        // claims that address.
        assert_eq!(t.nearest(0x00_8005).as_deref(), Some("main+0x05"));
    }

    #[test]
    fn nearest_annotates_with_offset_within_the_bank() {
        let t = SymbolTable::parse(SYM);
        assert_eq!(t.nearest(0x00_8000).as_deref(), Some("main"));
        assert_eq!(t.nearest(0x00_8005).as_deref(), Some("main+0x05"));
        assert_eq!(t.nearest(0x00_8012).as_deref(), Some("main_loop"));
        assert_eq!(t.nearest(0x7E_0101).as_deref(), Some("monster_x+0x01"));
        // Below the first label of a bank / different bank: no annotation.
        assert_eq!(t.nearest(0x00_7FFF), None);
        assert_eq!(t.nearest(0x7F_0000), None);
    }

    #[test]
    fn malformed_lines_and_unknown_sections_are_skipped() {
        let t = SymbolTable::parse(
            "[labels]\nbogus\nzz:0000 bad\n00:zzzz bad2\n00:1234\n[weird]\n00:9999 not_a_label_section\n[definitions]\nzzzz bad3\n",
        );
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn duplicate_names_keep_the_first_definition() {
        let t = SymbolTable::parse("[labels]\n00:8000 twice\n00:9000 twice\n");
        assert_eq!(t.len(), 1);
        assert_eq!(t.resolve("twice"), Some(0x00_8000));
    }

    #[test]
    fn spc_space_is_independent_of_cpu_space() {
        let mut t = SymbolTable::parse("[labels]\n00:8000 main\n7e:0100 monster_x\n");
        let spc = SymbolTable::parse_spc("[labels]\n00:0500 driver_loop\n00:1000 sample_dir\n");
        t.replace_space(SymbolSpace::Aram, spc);
        assert_eq!(t.len(), 4);

        // Same numeric address, different answers per space.
        assert_eq!(t.resolve("main"), Some(0x00_8000));
        assert_eq!(t.resolve_spc("driver_loop"), Some(0x0500));
        assert_eq!(t.resolve_spc("main"), None);
        assert_eq!(t.resolve("driver_loop"), None);

        assert_eq!(t.nearest_spc(0x0502).as_deref(), Some("driver_loop+0x02"));
        assert_eq!(t.nearest(0x00_0502), None); // no CPU label claims it

        // Reloading the CPU space keeps the ARAM entries.
        let cpu2 = SymbolTable::parse("[labels]\n00:8000 main2\n");
        t.replace_space(SymbolSpace::Cpu, cpu2);
        assert_eq!(t.resolve("main2"), Some(0x00_8000));
        assert_eq!(t.resolve("main"), None);
        assert_eq!(t.resolve_spc("driver_loop"), Some(0x0500));
    }

    /// `OpenSNES` ask of 2026-10-06, with their own fixture's two lines
    /// (`static_dup.sym`) as the negative control.
    #[test]
    fn a_bare_static_name_resolves_only_when_one_file_owns_it() {
        let t = SymbolTable::parse(
            "[labels]\n\
             7e:0040 player_x.main\n\
             00:00c3 k.main\n\
             00:00c9 k.other\n\
             7e:0050 score\n\
             7e:0060 score.hud\n\
             00:9000 kart\n\
             [definitions]\n\
             00000002 _sizeof_lives.main\n",
        );
        assert_eq!(t.resolve("player_x"), Some(0x7E_0040));
        assert_eq!(t.resolve("player_x.main"), Some(0x7E_0040));
        // The exact name wins over a suffixed one.
        assert_eq!(t.resolve("score"), Some(0x7E_0050));
        // Two owners: refused, both named. `kart` is not `k.<suffix>`.
        assert_eq!(t.resolve("k"), None);
        let err = t.lookup("k").unwrap_err();
        assert_eq!(
            err.to_string(),
            "ambiguous symbol `k`: `k.main` ($00:00C3), `k.other` ($00:00C9) (write the full name)"
        );
        // The size WLA-DX recorded follows the same naming rule.
        assert_eq!(t.size_of("lives"), None, "no `lives` label at all");
        let sized = SymbolTable::parse(
            "[labels]\n00:0d5b cam_x\n7e:0040 player_x.main\n00:9000 k.a\n00:9002 k.b\n\
             [definitions]\n00000002 _sizeof_cam_x\n00000024 _sizeof_player_x.main\n\
             00000002 _sizeof_k.a\n",
        );
        assert_eq!(sized.size_of("cam_x"), Some(2));
        assert_eq!(sized.size_of("player_x"), Some(0x24));
        assert_eq!(sized.size_of("player_x.main"), Some(0x24));
        assert_eq!(sized.size_of("k"), None, "ambiguous");
        assert_eq!(sized.size_of("k.b"), None, "no size recorded");
        assert_eq!(sized.size_of("_sizeof_cam_x"), None, "not a label");
        // A constant is not a location: it never stands for a bare name.
        assert_eq!(
            t.lookup("_sizeof_lives"),
            Err(SymbolError::Unknown("_sizeof_lives".into()))
        );
        assert_eq!(
            t.lookup("player_x.").unwrap_err().to_string(),
            "unknown symbol `player_x.`"
        );
    }

    #[test]
    fn resolve_scales_logarithmically_not_quadratically() {
        // 10k labels parse + resolve fast (the v1 parse dedup was O(n²)
        // and resolve O(n); this is a smoke that v2 stays comfortable).
        use std::fmt::Write as _;
        let mut text = String::from("[labels]\n");
        for i in 0..10_000u32 {
            let _ = writeln!(
                text,
                "{:02x}:{:04x} label_{i}",
                i >> 12,
                (i & 0x0FFF) | 0x8000
            );
        }
        let t = SymbolTable::parse(&text);
        assert_eq!(t.len(), 10_000);
        for i in (0..10_000u32).step_by(997) {
            assert!(t.resolve(&format!("label_{i}")).is_some());
        }
    }
}
