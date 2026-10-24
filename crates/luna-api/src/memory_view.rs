//! What Mesen2's Memory Viewer shows and does, as data a front-end draws:
//! each byte's colours (access highlighting with its fade, frozen bytes,
//! code / data / label backgrounds, breakpoint borders), the hover
//! tooltip, Find (hex with wildcards, text, integers, with code / data /
//! access filters), the *Navigate to* jumps, text tables (`.tbl`), edits
//! with undo, and the dump file name.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

use crate::access::AccessCounter;
use crate::code_log::{CODE, DATA};
use crate::memory::MemoryKind;
use crate::{ApiError, Emulator};

/// How long an access stays coloured (Mesen2's *Fade speed*).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FadeSpeed {
    /// Coloured for ever once accessed.
    DoNotFade,
    /// 600 frames.
    Slow,
    /// 300 frames.
    #[default]
    Normal,
    /// 120 frames.
    Fast,
}

impl FadeSpeed {
    /// Every speed, in Mesen2's list order.
    pub const ALL: [Self; 4] = [Self::DoNotFade, Self::Slow, Self::Normal, Self::Fast];

    /// The fade window in frames, `None` for never.
    #[must_use]
    pub const fn frames(self) -> Option<u32> {
        match self {
            Self::DoNotFade => None,
            Self::Slow => Some(600),
            Self::Normal => Some(300),
            Self::Fast => Some(120),
        }
    }

    /// Mesen2's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DoNotFade => "Do not fade",
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
        }
    }
}

/// Which highlights a view asks for (Mesen2's defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Mesen2's option panel: one box each.
pub struct HighlightOptions {
    /// Colour bytes recently read.
    pub read: bool,
    /// Colour bytes recently written.
    pub write: bool,
    /// Colour bytes recently executed.
    pub exec: bool,
    /// How long the access colours last.
    pub fade: FadeSpeed,
    /// Shade bytes the code log marks as code.
    pub code: bool,
    /// Shade bytes the code log marks as data.
    pub data: bool,
    /// Shade bytes that carry a label.
    pub labels: bool,
    /// Colour frozen bytes.
    pub frozen: bool,
    /// Frame bytes an enabled breakpoint covers.
    pub breakpoints: bool,
}

impl Default for HighlightOptions {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            exec: true,
            fade: FadeSpeed::Normal,
            code: false,
            data: false,
            labels: false,
            frozen: true,
            breakpoints: false,
        }
    }
}

/// A kind of access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// A data read.
    Read,
    /// A write.
    Write,
    /// A code read.
    Exec,
}

/// A byte's background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteShade {
    /// The code log saw it run.
    Code,
    /// The code log saw it read as data.
    Data,
    /// It carries a label.
    Label,
}

/// One byte as the viewer draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ByteView {
    /// Its value.
    pub value: u8,
    /// The access colour and its strength (1 = full, falling to 0 over
    /// the last fifth of the fade window). Exec wins over write over read.
    pub access: Option<(AccessKind, f32)>,
    /// Frozen: drawn in the frozen colour, over the access colour.
    pub frozen: bool,
    /// Its background.
    pub shade: Option<ByteShade>,
    /// The border of the breakpoint covering it (exec, then write, then
    /// read).
    pub breakpoint: Option<AccessKind>,
}

/// What *Navigate to* looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTarget {
    /// A byte read within the fade window.
    Read,
    /// A byte written within the fade window.
    Write,
    /// A byte executed within the fade window.
    Exec,
    /// A byte the code log marks as code.
    Code,
    /// A byte the code log marks as data.
    Data,
    /// A byte the code log marks as neither.
    Unidentified,
}

/// What Find looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindData {
    /// Bytes, `None` matching any byte (`FF ? 55`).
    Hex(Vec<Option<u8>>),
    /// Text, encoded through the text table when given, else UTF-8.
    Text {
        /// The text.
        text: String,
        /// Match the case exactly.
        case_sensitive: bool,
    },
    /// A little-endian integer.
    Integer {
        /// Its value.
        value: i64,
        /// Its width in bytes (1, 2, 4), `None` for the narrowest that
        /// holds the value.
        size: Option<u8>,
    },
}

/// Find's filters: a match passes when every byte it covers does. A
/// group with every box ticked or none is no filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Mesen2's filter panel: one box each.
pub struct FindFilters {
    /// Bytes the code log marks as code.
    pub code: bool,
    /// Bytes the code log marks as data.
    pub data: bool,
    /// Bytes the code log marks as neither.
    pub unidentified: bool,
    /// Bytes never accessed.
    pub not_accessed: bool,
    /// Bytes read.
    pub read: bool,
    /// Bytes written.
    pub written: bool,
    /// Bytes executed.
    pub executed: bool,
    /// Count only accesses this many frames old at most.
    pub last_frames: Option<u32>,
}

/// A Mesen2 text table (`.tbl`): `HEX=text` lines, the longest key
/// matching first when decoding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextTable {
    map: BTreeMap<Vec<u8>, String>,
    longest: usize,
}

impl TextTable {
    /// Read a table: one `HEX=text` per line (1 to 8 bytes, spaces in the
    /// hex ignored, an empty text a space). Other lines are skipped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let mut parts = line.splitn(3, '=');
            let (Some(hex), Some(value), None) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
            if hex.is_empty() || !hex.len().is_multiple_of(2) || hex.len() > 16 {
                continue;
            }
            let Some(key) = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
                .collect::<Option<Vec<u8>>>()
            else {
                continue;
            };
            let value = if value.is_empty() { " " } else { value };
            map.insert(key, value.to_string());
        }
        let longest = map.keys().map(Vec::len).max().unwrap_or(0);
        Self { map, longest }
    }

    /// No mapping.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The glyph at the start of `bytes` and how many bytes it covers:
    /// the longest key that matches, else `.` for one byte.
    #[must_use]
    pub fn decode(&self, bytes: &[u8]) -> (String, usize) {
        for n in (1..=self.longest.min(bytes.len())).rev() {
            if let Some(v) = self.map.get(&bytes[..n]) {
                return (v.clone(), n);
            }
        }
        (".".to_string(), 1)
    }

    /// Text as bytes: at each position the shortest value that matches,
    /// a character with no mapping as `00`.
    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let mut entries: Vec<(&Vec<u8>, &String)> = self.map.iter().collect();
        entries.sort_by_key(|(_, v)| v.len());
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            if let Some((k, v)) = entries.iter().find(|(_, v)| rest.starts_with(v.as_str())) {
                out.extend_from_slice(k);
                rest = &rest[v.len()..];
            } else {
                out.push(0);
                rest = &rest[c.len_utf8()..];
            }
        }
        out
    }
}

/// The glyph a byte shows without a table: printable ASCII as is, `.`
/// otherwise.
#[must_use]
pub const fn ascii_glyph(b: u8) -> char {
    if b >= 0x20 && b <= 0x7E {
        b as char
    } else {
        '.'
    }
}

/// Parse a hex search: bytes separated by spaces, `?` for any byte
/// (`FF ? 55`, or `FF?55`). `None` when it does not parse.
#[must_use]
pub fn parse_hex_query(text: &str) -> Option<Vec<Option<u8>>> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let mut rest = token;
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix('?') {
                out.push(None);
                rest = r;
            } else {
                let digits = rest.get(..2)?;
                out.push(Some(u8::from_str_radix(digits, 16).ok()?));
                rest = &rest[2..];
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The edits Undo takes back (Mesen2 keeps 200, without redo).
const UNDO_DEPTH: usize = 200;

/// One edit's previous bytes.
#[derive(Debug, Clone)]
pub(crate) struct EditUndo {
    kind: MemoryKind,
    offset: u32,
    before: Vec<u8>,
}

/// A text a counter shows: `1234`, then `1.23 K`, `4.56 M`… past 1000.
#[must_use]
pub fn short_count(n: u64) -> String {
    const UNITS: [&str; 4] = ["K", "M", "G", "T"];
    if n < 1000 {
        return n.to_string();
    }
    let mut v = n as f64 / 1000.0;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    format!("{v:.2} {}", UNITS[unit])
}

impl Emulator {
    /// Master clocks per frame, to turn stamps into frames.
    fn mclk_per_frame(&self) -> f64 {
        let hz = if self.region() == luna_cartridge::Region::Pal {
            luna_apu::PAL_MASTER_CLOCK_HZ
        } else {
            luna_apu::MASTER_CLOCK_HZ
        };
        hz as f64 / self.frame_rate_hz()
    }

    /// Frames since master clock `stamp`, `None` for never (0).
    fn frames_since(&self, now: u64, stamp: u64) -> Option<f64> {
        (stamp != 0).then(|| now.saturating_sub(stamp) as f64 / self.mclk_per_frame())
    }

    /// The code-log flags of byte `offset` of `kind` (a bus maps it to the
    /// memory behind).
    fn view_code_flags(&self, kind: MemoryKind, offset: u32) -> u8 {
        match kind {
            MemoryKind::SnesMemory => self.code_flags_at(offset).unwrap_or(0),
            MemoryKind::SpcMemory => self.spc_code_flags_at(offset as u16).unwrap_or(0),
            _ => self.code_flags(kind, offset),
        }
    }

    /// `len` bytes of `kind` from `offset` as the Memory Viewer draws
    /// them.
    pub fn memory_view(
        &self,
        kind: MemoryKind,
        offset: u32,
        len: usize,
        opts: &HighlightOptions,
    ) -> Result<Vec<ByteView>, ApiError> {
        let values = self.read_memory(kind, offset, len)?;
        let counters = if self.access_counters_enabled() && (opts.read || opts.write || opts.exec) {
            self.access_counters(kind, offset, len).ok()
        } else {
            None
        };
        let now = self.master_clock()?;
        let window = opts.fade.frames().map(f64::from);
        let strength = |stamp: u64| -> Option<f32> {
            let age = self.frames_since(now, stamp)?;
            let Some(w) = window else {
                return Some(1.0);
            };
            if age > w {
                None
            } else if age <= w * 0.8 {
                Some(1.0)
            } else {
                Some(((w - age) / (w * 0.2)) as f32)
            }
        };
        let freezable = matches!(kind, MemoryKind::SnesMemory | MemoryKind::SpcMemory);
        let bps = if opts.breakpoints {
            self.bp_list().unwrap_or_default()
        } else {
            Vec::new()
        };
        let size = self.memory_size(kind)?;
        Ok(values
            .iter()
            .enumerate()
            .map(|(i, &value)| {
                let at = ((offset as usize + i) % size) as u32;
                let access = counters.as_ref().and_then(|c| {
                    let c = c[i];
                    [
                        (opts.exec, AccessKind::Exec, c.last_exec),
                        (opts.write, AccessKind::Write, c.last_write),
                        (opts.read, AccessKind::Read, c.last_read),
                    ]
                    .into_iter()
                    .filter(|(on, ..)| *on)
                    .find_map(|(_, k, stamp)| strength(stamp).map(|s| (k, s)))
                });
                let frozen =
                    opts.frozen && freezable && self.write_frozen(kind, at).unwrap_or(false);
                let labelled = opts.labels
                    && self
                        .label_at(kind, at)
                        .ok()
                        .flatten()
                        .is_some_and(|h| !h.label.name.is_empty());
                let flags = if opts.code || opts.data {
                    self.view_code_flags(kind, at)
                } else {
                    0
                };
                let shade = if labelled {
                    Some(ByteShade::Label)
                } else if opts.code && flags & CODE != 0 {
                    Some(ByteShade::Code)
                } else if opts.data && flags & DATA != 0 {
                    Some(ByteShade::Data)
                } else {
                    None
                };
                let breakpoint = bps
                    .iter()
                    .filter(|b| {
                        b.enabled
                            && !b.forbid
                            && crate::workspace::memory_of(b.space, b.cpu) == kind
                            && (b.lo..=b.hi).contains(&at)
                    })
                    .map(|b| {
                        if b.exec {
                            AccessKind::Exec
                        } else if b.on_write {
                            AccessKind::Write
                        } else {
                            AccessKind::Read
                        }
                    })
                    .min_by_key(|k| match k {
                        AccessKind::Exec => 0,
                        AccessKind::Write => 1,
                        AccessKind::Read => 2,
                    });
                ByteView {
                    value,
                    access,
                    frozen,
                    shade,
                    breakpoint,
                }
            })
            .collect())
    }

    /// The tooltip over byte `offset` of `kind`, Mesen2's lines: the label
    /// (`+N` inside a multi-byte one), the address (and the byte behind a
    /// bus address), the value as byte, word and dword in hex and
    /// decimal, then a line per kind of access with its count and age.
    pub fn memory_tooltip(&self, kind: MemoryKind, offset: u32) -> Result<String, ApiError> {
        let mut lines = Vec::new();
        if let Some(h) = self.label_at(kind, offset)?
            && !h.label.name.is_empty()
        {
            if h.byte == 0 {
                lines.push(h.label.name);
            } else {
                lines.push(format!("{}+{}", h.label.name, h.byte));
            }
        }
        let size = self.memory_size(kind)?;
        let digits = format!("{:X}", size.saturating_sub(1)).len().max(4);
        let mut at = format!("{}: ${offset:0digits$X}", kind.name());
        if kind == MemoryKind::SnesMemory
            && let Some(l) = self.absolute_address(offset)?
        {
            let _ = write!(at, " ({} ${:X})", l.kind.name(), l.offset);
        }
        lines.push(at);
        let n = 4.min(size);
        let bytes = self.read_memory(kind, offset, n)?;
        let b = u32::from(bytes[0]);
        let w = u32::from(bytes[0]) | (u32::from(*bytes.get(1).unwrap_or(&0)) << 8);
        let d = u32::from_le_bytes([
            bytes[0],
            *bytes.get(1).unwrap_or(&0),
            *bytes.get(2).unwrap_or(&0),
            *bytes.get(3).unwrap_or(&0),
        ]);
        lines.push(format!("Value: ${b:02X} ({b})"));
        lines.push(format!("Word: ${w:04X} ({w})"));
        lines.push(format!("Dword: ${d:08X} ({d})"));
        if self.access_counters_enabled()
            && let Ok(c) = self.access_counters(kind, offset, 1)
        {
            let c: AccessCounter = c[0];
            let now = self.master_clock()?;
            for (tag, count, stamp) in [
                ("R", c.read, c.last_read),
                ("W", c.write, c.last_write),
                ("X", c.exec, c.last_exec),
            ] {
                if let Some(age) = self.frames_since(now, stamp) {
                    lines.push(format!(
                        "{tag}: {} - {} frames",
                        short_count(u64::from(count)),
                        age as u64
                    ));
                }
            }
        }
        Ok(lines.join("\n"))
    }

    /// Whether byte `at` of `kind` passes Find's filters, `counters` its
    /// access counters when they are on.
    fn passes_find_filters(
        &self,
        kind: MemoryKind,
        at: u32,
        f: &FindFilters,
        counter: Option<AccessCounter>,
        now: u64,
    ) -> bool {
        let groups = [f.code, f.data, f.unidentified];
        if groups.iter().any(|&b| b) && !groups.iter().all(|&b| b) {
            let flags = self.view_code_flags(kind, at);
            let code = flags & CODE != 0;
            let data = flags & DATA != 0;
            let ok = (f.code && code) || (f.data && data) || (f.unidentified && !code && !data);
            if !ok {
                return false;
            }
        }
        let access = [f.not_accessed, f.read, f.written, f.executed];
        if access.iter().any(|&b| b) && !access.iter().all(|&b| b) {
            let c = counter.unwrap_or_default();
            let recent = |stamp: u64| {
                self.frames_since(now, stamp)
                    .is_some_and(|age| f.last_frames.is_none_or(|n| age <= f64::from(n)))
            };
            let (r, w, x) = (
                recent(c.last_read),
                recent(c.last_write),
                recent(c.last_exec),
            );
            let ok = (f.not_accessed && !r && !w && !x)
                || (f.read && r)
                || (f.written && w)
                || (f.executed && x);
            if !ok {
                return false;
            }
        }
        true
    }

    /// The patterns a Find query looks for: each a list of positions, each
    /// position the bytes it accepts (empty for any byte).
    fn find_patterns(
        query: &FindData,
        table: Option<&TextTable>,
    ) -> Result<Vec<Vec<u8>>, ApiError> {
        let mut positions: Vec<Vec<u8>> = Vec::new();
        match query {
            FindData::Hex(bytes) => {
                for b in bytes {
                    positions.push(b.map(|v| vec![v]).unwrap_or_default());
                }
            }
            FindData::Text {
                text,
                case_sensitive,
            } => {
                let enc = |t: &str| match table {
                    Some(tbl) if !tbl.is_empty() => tbl.encode(t),
                    _ => t.as_bytes().to_vec(),
                };
                let exact = enc(text);
                if *case_sensitive {
                    positions = exact.into_iter().map(|b| vec![b]).collect();
                } else {
                    let (lo, hi) = (enc(&text.to_lowercase()), enc(&text.to_uppercase()));
                    if lo.len() != hi.len() {
                        return Err(ApiError::BadArg(
                            "this text has no case-insensitive form of the same length".into(),
                        ));
                    }
                    positions = lo.into_iter().zip(hi).map(|(a, b)| vec![a, b]).collect();
                }
            }
            FindData::Integer { value, size } => {
                let v = *value;
                let size = size.unwrap_or(if (-0x80..=0xFF).contains(&v) {
                    1
                } else if (-0x8000..=0xFFFF).contains(&v) {
                    2
                } else {
                    4
                });
                if !matches!(size, 1 | 2 | 4) {
                    return Err(ApiError::BadArg(format!("integer size {size}: 1, 2 or 4")));
                }
                positions = (v as u32).to_le_bytes()[..usize::from(size)]
                    .iter()
                    .map(|&b| vec![b])
                    .collect();
            }
        }
        if positions.is_empty() {
            return Err(ApiError::BadArg("nothing to find".into()));
        }
        Ok(positions)
    }

    /// Find (Mesen2's *Find next* / *Find previous*): the first match of
    /// `query` from `from` (forward from it, backward from the byte before
    /// it), wrapping around the memory once. Returns its offset and length.
    pub fn memory_find(
        &self,
        kind: MemoryKind,
        from: u32,
        query: &FindData,
        filters: &FindFilters,
        table: Option<&TextTable>,
        backward: bool,
    ) -> Result<Option<(u32, usize)>, ApiError> {
        let pattern = Self::find_patterns(query, table)?;
        let size = self.memory_size(kind)?;
        let mem = self.read_memory(kind, 0, size)?;
        let counters = self
            .access_counters_enabled()
            .then(|| self.access_counters(kind, 0, size).ok())
            .flatten();
        let now = self.master_clock()?;
        let len = pattern.len();
        let matches_at = |s: usize| {
            pattern.iter().enumerate().all(|(i, accept)| {
                let b = mem[(s + i) % size];
                accept.is_empty() || accept.contains(&b)
            }) && (0..len).all(|i| {
                let at = (s + i) % size;
                self.passes_find_filters(
                    kind,
                    at as u32,
                    filters,
                    counters.as_ref().map(|c| c[at]),
                    now,
                )
            })
        };
        let from = from as usize % size;
        for step in 0..size {
            let s = if backward {
                (from + size - 1 - step) % size
            } else {
                (from + step) % size
            };
            if matches_at(s) {
                return Ok(Some((s as u32, len)));
            }
        }
        Ok(None)
    }

    /// Whether byte `at` is one *Navigate to* `target` looks for.
    fn nav_hit(
        &self,
        kind: MemoryKind,
        at: u32,
        target: NavTarget,
        c: Option<AccessCounter>,
        now: u64,
        fade: FadeSpeed,
    ) -> bool {
        let recent = |stamp: u64| {
            self.frames_since(now, stamp)
                .is_some_and(|age| fade.frames().is_none_or(|w| age <= f64::from(w)))
        };
        let flags = || self.view_code_flags(kind, at);
        match target {
            NavTarget::Read => c.is_some_and(|c| recent(c.last_read)),
            NavTarget::Write => c.is_some_and(|c| recent(c.last_write)),
            NavTarget::Exec => c.is_some_and(|c| recent(c.last_exec)),
            NavTarget::Code => flags() & CODE != 0,
            NavTarget::Data => flags() & DATA != 0,
            NavTarget::Unidentified => flags() & (CODE | DATA) == 0,
        }
    }

    /// *Navigate to* the next (or previous) block of `target` from `from`:
    /// a run the cursor sits in is skipped first, so the jump lands on the
    /// next block, not the next byte. Wraps around once.
    pub fn memory_navigate(
        &self,
        kind: MemoryKind,
        from: u32,
        target: NavTarget,
        forward: bool,
        fade: FadeSpeed,
    ) -> Result<Option<u32>, ApiError> {
        let size = self.memory_size(kind)?;
        let counters = if matches!(target, NavTarget::Read | NavTarget::Write | NavTarget::Exec) {
            Some(self.access_counters(kind, 0, size)?)
        } else {
            None
        };
        let now = self.master_clock()?;
        let hit = |a: usize| {
            self.nav_hit(
                kind,
                a as u32,
                target,
                counters.as_ref().map(|c| c[a]),
                now,
                fade,
            )
        };
        let next = |a: usize| {
            if forward {
                (a + 1) % size
            } else {
                (a + size - 1) % size
            }
        };
        let mut a = from as usize % size;
        let mut steps = 0;
        while hit(a) && steps < size {
            a = next(a);
            steps += 1;
        }
        while steps < size {
            if hit(a) {
                if !forward {
                    // Land on the block's first byte.
                    while hit((a + size - 1) % size) && steps < size {
                        a = (a + size - 1) % size;
                        steps += 1;
                    }
                }
                return Ok(Some(a as u32));
            }
            a = next(a);
            steps += 1;
        }
        Ok(None)
    }

    /// Write `data` into `kind` from `offset` as a debugger's edit, kept
    /// for [`Self::memory_undo`] (the last 200 edits). A frozen byte
    /// changes too.
    pub fn memory_edit(
        &mut self,
        kind: MemoryKind,
        offset: u32,
        data: &[u8],
    ) -> Result<usize, ApiError> {
        let before = self.read_memory(kind, offset, data.len())?;
        let written = self.write_memory(kind, offset, data)?;
        if self.edit_history.len() == UNDO_DEPTH {
            self.edit_history.pop_front();
        }
        self.edit_history.push_back(EditUndo {
            kind,
            offset,
            before,
        });
        Ok(written)
    }

    /// Take the last edit back. `false` when there is none.
    pub fn memory_undo(&mut self) -> Result<bool, ApiError> {
        let Some(e) = self.edit_history.pop_back() else {
            return Ok(false);
        };
        self.write_memory(e.kind, e.offset, &e.before)?;
        Ok(true)
    }

    /// How many edits [`Self::memory_undo`] can take back.
    #[must_use]
    pub fn memory_undo_depth(&self) -> usize {
        self.edit_history.len()
    }

    /// The memory and offset behind address `addr` of `kind`: on a bus
    /// (`SnesMemory`, `SpcMemory`) the byte it reaches now, elsewhere the
    /// byte itself. `None` for a bus address nothing answers.
    #[must_use]
    pub fn memory_behind(&self, kind: MemoryKind, addr: u32) -> Option<(MemoryKind, u32)> {
        match kind {
            MemoryKind::SnesMemory => self
                .absolute_address(addr)
                .ok()
                .flatten()
                .map(|l| (l.kind, l.offset)),
            MemoryKind::SpcMemory => {
                let ipl = self
                    .snes
                    .as_ref()
                    .is_some_and(|s| s.apu_real.control & 0x80 != 0);
                let a = addr & 0xFFFF;
                Some(if ipl && a >= 0xFFC0 {
                    (MemoryKind::SpcRom, a - 0xFFC0)
                } else {
                    (MemoryKind::SpcRam, a)
                })
            }
            _ => Some((kind, addr)),
        }
    }

    /// Mesen2's dump file name for `kind`: `<ROM name> - <Kind>.dmp`.
    #[must_use]
    pub fn memory_dump_name(&self, kind: MemoryKind) -> String {
        let rom = self
            .rom_file()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| self.rom_info.as_ref().map(|i| i.title.trim().to_string()))
            .unwrap_or_else(|| "luna".into());
        format!("{rom} - {}.dmp", kind.name())
    }

    /// Load a dump into `kind` from offset 0 (Mesen2's *Import*): a shorter
    /// file fills the start, a longer one is refused, a bus is refused.
    pub fn memory_import(&mut self, kind: MemoryKind, data: &[u8]) -> Result<usize, ApiError> {
        if matches!(
            kind,
            MemoryKind::SnesMemory
                | MemoryKind::SpcMemory
                | MemoryKind::Sa1Memory
                | MemoryKind::GsuMemory
        ) {
            return Err(ApiError::BadArg(format!(
                "{}: a dump loads into a memory, not a bus",
                kind.name()
            )));
        }
        let size = self.memory_size(kind)?;
        if data.len() > size {
            return Err(ApiError::BadArg(format!(
                "the dump is {} bytes, {} has {size}",
                data.len(),
                kind.name()
            )));
        }
        self.memory_edit(kind, 0, data)
    }
}

/// The edit history's type, for the [`Emulator`] field.
pub(crate) type EditHistory = VecDeque<EditUndo>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_queries_take_wildcards() {
        assert_eq!(
            parse_hex_query("FF ? 55"),
            Some(vec![Some(0xFF), None, Some(0x55)])
        );
        assert_eq!(
            parse_hex_query("a9?00"),
            Some(vec![Some(0xA9), None, Some(0)])
        );
        assert_eq!(parse_hex_query("F"), None);
        assert_eq!(parse_hex_query("GG"), None);
        assert_eq!(parse_hex_query("  "), None);
    }

    #[test]
    fn a_text_table_decodes_the_longest_key_and_encodes_the_shortest_value() {
        let t = TextTable::parse("00=A\n0102=the\n01=t\n7F=\nbad line\n123=x\n");
        assert_eq!(t.decode(&[0x01, 0x02, 0x00]), ("the".into(), 2));
        assert_eq!(t.decode(&[0x01, 0x00]), ("t".into(), 1));
        assert_eq!(t.decode(&[0x7F]), (" ".into(), 1));
        assert_eq!(t.decode(&[0x55]), (".".into(), 1));
        assert_eq!(t.encode("tA?"), vec![0x01, 0x00, 0x00]);
    }

    #[test]
    fn counts_shorten_past_a_thousand() {
        assert_eq!(short_count(999), "999");
        assert_eq!(short_count(1234), "1.23 K");
        assert_eq!(short_count(4_560_000), "4.56 M");
    }
}
