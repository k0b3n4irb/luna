//! The Memory Tools and Memory Search windows' host side (Mesen2's
//! Memory Viewer and Memory Search). For the Memory Tools window: the
//! state it keeps between frames, the [`MemoryToolsView`] built from
//! `luna-api` each frame, and the [`MemoryToolsAction`]s applied under the
//! emulator lock. What reaches past the emulator (a file dialog, the
//! clipboard, the Debugger window) comes back as [`MemoryEffect`]s.

use std::fmt::Write as _;

use luna_api::{
    ByteView, Emulator, FindData, FindFilters, HighlightOptions, ListingCpu, MemoryKind, NavTarget,
    TextTable,
};

/// The window's name in a workspace file.
const WORKSPACE_KEY: &str = "luna-gui-memory";

/// Rows a frame shows before the panel says how many fit.
const DEFAULT_ROWS: usize = 32;

/// Mesen2's column counts.
pub(crate) const COLUMNS: [u16; 10] = [4, 8, 16, 32, 48, 64, 80, 96, 112, 128];

/// The window's settings (Mesen2's options panel), kept in the ROM's
/// workspace.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct MemoryToolsSettings {
    /// The memory shown, by its name.
    pub kind: String,
    /// Bytes per row.
    pub columns: u16,
    /// Show the text column.
    pub show_chars: bool,
    /// Tighter rows.
    pub high_density: bool,
    /// Show the tooltip over a byte.
    pub tooltips: bool,
    /// Show the options panel.
    pub show_settings: bool,
    /// The highlights.
    pub highlight: HighlightOptions,
    /// The text table loaded, if any.
    pub tbl_path: Option<String>,
}

impl Default for MemoryToolsSettings {
    fn default() -> Self {
        Self {
            kind: MemoryKind::SnesMemory.name().to_string(),
            columns: 16,
            show_chars: true,
            high_density: false,
            tooltips: true,
            show_settings: true,
            highlight: HighlightOptions::default(),
            tbl_path: None,
        }
    }
}

/// A Find the window remembers, for Find next / previous.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FindRequest {
    /// What to look for.
    pub data: FindData,
    /// Through the text table.
    pub use_tbl: bool,
    /// The filters.
    pub filters: FindFilters,
}

/// What the Memory Tools window did this frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MemoryToolsAction {
    /// Show another memory.
    SetKind(MemoryKind),
    /// How many rows the panel shows.
    VisibleRows(usize),
    /// Scroll by rows.
    Scroll(i64),
    /// Put this row at the top.
    ScrollTo(u64),
    /// Click on a byte (`shift`: extend the selection; `text`: in the text
    /// column).
    Click {
        /// The byte's offset.
        addr: u32,
        /// Shift held.
        shift: bool,
        /// In the text column.
        text: bool,
    },
    /// Move the cursor by bytes (arrows, pages).
    Move(i64, bool),
    /// Move the cursor to an offset.
    MoveTo(u32, bool),
    /// A hex digit typed in the hex column.
    HexDigit(u8),
    /// A character typed in the text column.
    Char(char),
    /// Select every byte.
    SelectAll,
    /// Copy the selection.
    Copy,
    /// Paste hex text from the cursor.
    Paste(String),
    /// Undo the last edit.
    Undo,
    /// Freeze or thaw the selection.
    Freeze(bool),
    /// The pointer is over this byte (for the tooltip).
    Hover(Option<u32>),
    /// Go to a hex address.
    Goto(String),
    /// Find from the cursor.
    Find(FindRequest, bool),
    /// Find the last query again (`true`: backward).
    FindAgain(bool),
    /// Navigate to the next / previous block.
    Navigate(NavTarget, bool),
    /// Import a dump.
    Import,
    /// Export the memory.
    Export,
    /// Reset the access counters.
    ResetCounters,
    /// Load a text table.
    LoadTbl,
    /// Back to ASCII.
    ResetTbl,
    /// New settings.
    Settings(MemoryToolsSettings),
    /// Add the selection to the watch list.
    AddWatch,
    /// Toggle a read / write breakpoint on the selection.
    ToggleBreakpoint,
    /// Name the selection's first byte.
    SetLabel(String, String),
    /// Show the cursor's address in the Debugger.
    ViewInDebugger,
    /// Switch between the bus and the memory behind it.
    ViewIn,
}

/// Something only `LunaApp` can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MemoryEffect {
    /// Pick a dump to load into this memory.
    Import(MemoryKind),
    /// Save these bytes, under this file name.
    Export(String, Vec<u8>),
    /// Pick a `.tbl` file.
    LoadTbl,
    /// Add these entries to a CPU's watch list.
    AddWatch(ListingCpu, Vec<String>),
    /// Show this address in a CPU's Debugger window.
    ViewInDebugger(ListingCpu, u32),
    /// The breakpoint registry changed.
    SyncBreakpoints,
}

/// What the Memory Tools window shows, built each frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryToolsView {
    /// The memories this cartridge has, with their sizes.
    pub kinds: Vec<(MemoryKind, usize)>,
    /// The memory shown.
    pub kind: Option<MemoryKind>,
    /// Its size.
    pub size: usize,
    /// The first row shown.
    pub top_row: u64,
    /// Rows in the memory.
    pub total_rows: u64,
    /// The rows shown: their first offset and bytes.
    pub rows: Vec<(u32, Vec<ByteView>)>,
    /// The text column, per row: each glyph and the bytes it covers.
    pub text: Vec<Vec<(String, usize)>>,
    /// The cursor.
    pub cursor: u32,
    /// The selection, first and last byte.
    pub selection: (u32, u32),
    /// The high nibble typed, waiting for the low one.
    pub pending: Option<u8>,
    /// Typing goes to the text column.
    pub text_focus: bool,
    /// Settings.
    pub settings: MemoryToolsSettings,
    /// The status bar.
    pub status: String,
    /// The tooltip over the hovered byte.
    pub tooltip: Option<(u32, String)>,
    /// The last Find missed.
    pub find_miss: bool,
    /// A Find is remembered.
    pub can_find_again: bool,
    /// Edits Undo can take back.
    pub undo_depth: usize,
    /// Addresses of this memory can be frozen.
    pub freezable: bool,
    /// A text table is loaded.
    pub tbl: bool,
    /// Text a Copy put out, for the panel to hand to the clipboard.
    pub clipboard: Option<String>,
}

/// The Memory Tools window's state between frames.
#[derive(Default)]
pub(crate) struct MemoryToolsHost {
    pub settings: MemoryToolsSettings,
    top_row: u64,
    visible: usize,
    cursor: u32,
    anchor: u32,
    pending: Option<u8>,
    text_focus: bool,
    hover: Option<u32>,
    last_find: Option<FindRequest>,
    find_miss: bool,
    tbl: Option<TextTable>,
    clipboard: Option<String>,
    /// The window is open.
    pub open: bool,
}

/// The CPU whose bus a memory is.
const fn bus_cpu(kind: MemoryKind) -> Option<ListingCpu> {
    match kind {
        MemoryKind::SnesMemory => Some(ListingCpu::Snes),
        MemoryKind::SpcMemory => Some(ListingCpu::Spc),
        _ => None,
    }
}

/// Memories addressed in words (Mesen2 adds the word address).
const fn word_addressed(kind: MemoryKind) -> bool {
    matches!(
        kind,
        MemoryKind::SnesVideoRam | MemoryKind::SnesSpriteRam | MemoryKind::SnesCgRam
    )
}

impl MemoryToolsHost {
    /// The memory shown, if this cartridge has it.
    fn kind(&self, em: &Emulator) -> Option<MemoryKind> {
        MemoryKind::from_name(&self.settings.kind)
            .filter(|k| em.memory_size(*k).is_ok_and(|s| s > 0))
            .or_else(|| {
                Some(MemoryKind::SnesMemory)
                    .filter(|_| em.memory_size(MemoryKind::SnesMemory).is_ok())
            })
    }

    const fn cols(&self) -> u64 {
        self.settings.columns as u64
    }

    fn selection(&self) -> (u32, u32) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    /// The window opened: count accesses while it is open.
    pub(crate) fn opened(&mut self, em: &mut Emulator) {
        self.open = true;
        if !em.access_counters_enabled() {
            let _ = em.enable_access_counters(true);
        }
        if self.visible == 0 {
            self.visible = DEFAULT_ROWS;
        }
    }

    /// The window closed: stop counting unless `keep` (Memory Search is
    /// open).
    pub(crate) fn closed(&mut self, em: &mut Emulator, keep: bool) {
        self.open = false;
        if !keep {
            let _ = em.enable_access_counters(false);
        }
    }

    /// Another ROM was loaded.
    pub(crate) fn rom_loaded(&mut self, em: &mut Emulator, ws: Option<&luna_api::Workspace>) {
        self.top_row = 0;
        self.cursor = 0;
        self.anchor = 0;
        self.pending = None;
        self.last_find = None;
        self.tbl = None;
        if let Some(s) = ws
            .and_then(|w| w.front_end.get(WORKSPACE_KEY))
            .and_then(|v| serde_json::from_value::<MemoryToolsSettings>(v.clone()).ok())
        {
            self.settings = s;
        }
        if let Some(text) = self
            .settings
            .tbl_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
        {
            self.tbl = Some(TextTable::parse(&text));
        }
        if self.open {
            self.opened(em);
        }
    }

    /// Add the settings to a workspace.
    pub(crate) fn store_workspace(&self, ws: &mut luna_api::Workspace) {
        if let Ok(v) = serde_json::to_value(&self.settings) {
            ws.front_end.insert(WORKSPACE_KEY.to_string(), v);
        }
    }

    /// A text table was picked.
    pub(crate) fn load_tbl(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.tbl = Some(TextTable::parse(&text));
        self.settings.tbl_path = Some(path.to_string_lossy().into_owned());
        Ok(())
    }

    /// Bring the cursor's row into view.
    fn follow_cursor(&mut self) {
        let row = u64::from(self.cursor) / self.cols();
        let visible = self.visible.max(1) as u64;
        if row < self.top_row {
            self.top_row = row;
        } else if row >= self.top_row + visible {
            self.top_row = row + 1 - visible;
        }
    }

    /// Put the cursor's row at the top (Go To, Find).
    fn cursor_to_top(&mut self) {
        self.top_row = u64::from(self.cursor) / self.cols();
    }

    /// What the window shows now.
    pub(crate) fn view(&mut self, em: &Emulator) -> MemoryToolsView {
        let kinds: Vec<(MemoryKind, usize)> = MemoryKind::ALL
            .iter()
            .filter(|k| **k != MemoryKind::SnesRegister)
            .filter_map(|&k| em.memory_size(k).ok().filter(|&s| s > 0).map(|s| (k, s)))
            .collect();
        let Some(kind) = self.kind(em) else {
            return MemoryToolsView {
                kinds,
                settings: self.settings.clone(),
                ..MemoryToolsView::default()
            };
        };
        let size = em.memory_size(kind).unwrap_or(0);
        let cols = self.cols();
        let total_rows = (size as u64).div_ceil(cols);
        let visible = if self.visible == 0 {
            DEFAULT_ROWS
        } else {
            self.visible
        } as u64;
        self.top_row = self.top_row.min(total_rows.saturating_sub(visible));
        let first = self.top_row * cols;
        let len = ((visible * cols).min(size as u64 - first.min(size as u64))) as usize;
        let bytes = em
            .memory_view(kind, first as u32, len, &self.settings.highlight)
            .unwrap_or_default();
        let rows: Vec<(u32, Vec<ByteView>)> = bytes
            .chunks(cols as usize)
            .enumerate()
            .map(|(i, c)| ((first + i as u64 * cols) as u32, c.to_vec()))
            .collect();
        let text = rows
            .iter()
            .map(|(_, r)| {
                let values: Vec<u8> = r.iter().map(|b| b.value).collect();
                match &self.tbl {
                    Some(t) if !t.is_empty() => {
                        let mut out = Vec::new();
                        let mut at = 0;
                        while at < values.len() {
                            let (g, n) = t.decode(&values[at..]);
                            out.push((g, n));
                            at += n;
                        }
                        out
                    }
                    _ => values
                        .iter()
                        .map(|&b| (luna_api::memory_view::ascii_glyph(b).to_string(), 1))
                        .collect(),
                }
            })
            .collect();
        let selection = self.selection();
        let tooltip = self
            .hover
            .filter(|_| self.settings.tooltips)
            .and_then(|a| em.memory_tooltip(kind, a).ok().map(|t| (a, t)));
        MemoryToolsView {
            kinds,
            kind: Some(kind),
            size,
            top_row: self.top_row,
            total_rows,
            rows,
            text,
            cursor: self.cursor,
            selection,
            pending: self.pending,
            text_focus: self.text_focus,
            settings: self.settings.clone(),
            status: self.status(em, kind, size),
            tooltip,
            find_miss: self.find_miss,
            can_find_again: self.last_find.is_some(),
            undo_depth: em.memory_undo_depth(),
            freezable: bus_cpu(kind).is_some(),
            tbl: self.tbl.is_some(),
            clipboard: self.clipboard.take(),
        }
    }

    /// Mesen2's status bar: the location or the selection, its length,
    /// the word address of a word-addressed memory, the label there.
    fn status(&self, em: &Emulator, kind: MemoryKind, size: usize) -> String {
        let digits = format!("{:X}", size.saturating_sub(1)).len().max(4);
        let (a, b) = self.selection();
        let mut s = if a == b {
            let mut s = format!("Location: ${a:0digits$X}");
            if word_addressed(kind) {
                let _ = write!(s, " (${:X}.w)", a / 2);
            }
            s
        } else {
            let mut s = format!("Selection: ${a:0digits$X} - ${b:0digits$X}");
            if word_addressed(kind) {
                let _ = write!(s, " (${:X}.w - ${:X}.w)", a / 2, b / 2);
            }
            let n = b - a + 1;
            let _ = write!(s, "   Length: {n} bytes (${n:X})");
            s
        };
        if let Ok(Some(h)) = em.label_at(kind, a)
            && h.byte == 0
            && !h.label.name.is_empty()
            && (a == b || h.label.length == b - a + 1)
        {
            let _ = write!(s, " ({})", h.label.name);
        }
        s
    }

    /// Write `data` at the cursor (or fill the selection with one byte).
    fn write_at_cursor(&mut self, em: &mut Emulator, kind: MemoryKind, data: &[u8], size: usize) {
        let (a, b) = self.selection();
        if data.len() == 1 && a != b {
            let n = (b - a + 1) as usize;
            let _ = em.memory_edit(kind, a, &vec![data[0]; n]);
            self.cursor = ((b as usize + 1) % size) as u32;
        } else {
            let _ = em.memory_edit(kind, self.cursor, data);
            self.cursor = ((self.cursor as usize + data.len()) % size) as u32;
        }
        self.anchor = self.cursor;
        self.follow_cursor();
    }

    /// Run a Find from the cursor.
    fn find(&mut self, em: &Emulator, kind: MemoryKind, req: &FindRequest, backward: bool) {
        let (a, b) = self.selection();
        let from = if backward {
            a
        } else if a == b {
            self.cursor
        } else {
            a.wrapping_add(1)
        };
        let table = if req.use_tbl { self.tbl.as_ref() } else { None };
        match em.memory_find(kind, from, &req.data, &req.filters, table, backward) {
            Ok(Some((at, len))) => {
                self.find_miss = false;
                self.anchor = at;
                self.cursor = at + len as u32 - 1;
                self.top_row = u64::from(at) / self.cols();
            }
            _ => self.find_miss = true,
        }
    }

    /// Apply what the window did.
    #[allow(clippy::too_many_lines)] // One arm per action, as the debugger's.
    pub(crate) fn apply(&mut self, em: &mut Emulator, act: MemoryToolsAction) -> Vec<MemoryEffect> {
        let mut fx = Vec::new();
        let Some(kind) = self.kind(em) else {
            return fx;
        };
        let size = em.memory_size(kind).unwrap_or(1).max(1);
        let last = (size - 1) as u32;
        let total_rows = (size as u64).div_ceil(self.cols());
        match act {
            MemoryToolsAction::SetKind(k) => {
                self.settings.kind = k.name().to_string();
                self.cursor = 0;
                self.anchor = 0;
                self.top_row = 0;
                self.pending = None;
            }
            MemoryToolsAction::VisibleRows(n) => self.visible = n.max(1),
            MemoryToolsAction::Scroll(d) => {
                self.top_row = self
                    .top_row
                    .saturating_add_signed(d)
                    .min(total_rows.saturating_sub(1));
            }
            MemoryToolsAction::ScrollTo(r) => self.top_row = r.min(total_rows.saturating_sub(1)),
            MemoryToolsAction::Click { addr, shift, text } => {
                self.cursor = addr.min(last);
                if !shift {
                    self.anchor = self.cursor;
                }
                self.text_focus = text;
                self.pending = None;
            }
            MemoryToolsAction::Move(d, shift) => {
                self.cursor = (i64::from(self.cursor) + d).clamp(0, i64::from(last)) as u32;
                if !shift {
                    self.anchor = self.cursor;
                }
                self.pending = None;
                self.follow_cursor();
            }
            MemoryToolsAction::MoveTo(a, shift) => {
                self.cursor = a.min(last);
                if !shift {
                    self.anchor = self.cursor;
                }
                self.pending = None;
                self.follow_cursor();
            }
            MemoryToolsAction::HexDigit(d) => match self.pending.take() {
                None => self.pending = Some(d & 0xF),
                Some(hi) => self.write_at_cursor(em, kind, &[(hi << 4) | (d & 0xF)], size),
            },
            MemoryToolsAction::Char(c) => {
                let data = match &self.tbl {
                    Some(t) if !t.is_empty() => t.encode(&c.to_string()),
                    _ => u8::try_from(c).map(|b| vec![b]).unwrap_or_default(),
                };
                if !data.is_empty() {
                    let (a, _) = self.selection();
                    self.cursor = a;
                    self.anchor = a;
                    self.write_at_cursor(em, kind, &data, size);
                }
            }
            MemoryToolsAction::SelectAll => {
                self.anchor = 0;
                self.cursor = last;
            }
            MemoryToolsAction::Copy => {
                let (a, b) = self.selection();
                let bytes = em
                    .read_memory(kind, a, (b - a + 1) as usize)
                    .unwrap_or_default();
                let text = if self.text_focus {
                    match &self.tbl {
                        Some(t) if !t.is_empty() => {
                            let mut s = String::new();
                            let mut at = 0;
                            while at < bytes.len() {
                                let (g, n) = t.decode(&bytes[at..]);
                                s += &g;
                                at += n;
                            }
                            s
                        }
                        _ => bytes
                            .iter()
                            .map(|&b| luna_api::memory_view::ascii_glyph(b))
                            .collect(),
                    }
                } else {
                    bytes
                        .chunks(self.cols() as usize)
                        .map(|r| {
                            r.iter()
                                .map(|b| format!("{b:02X}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.clipboard = Some(text);
            }
            MemoryToolsAction::Paste(text) => {
                let hex: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                if hex.len().is_multiple_of(2)
                    && let Some(data) = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
                        .collect::<Option<Vec<u8>>>()
                    && !data.is_empty()
                {
                    let (a, _) = self.selection();
                    self.cursor = a;
                    self.anchor = a;
                    let n = data.len().min(size - a as usize);
                    self.write_at_cursor(em, kind, &data[..n], size);
                }
            }
            MemoryToolsAction::Undo => {
                let _ = em.memory_undo();
            }
            MemoryToolsAction::Freeze(on) => {
                let (a, b) = self.selection();
                let _ = em.set_write_freeze(kind, a, (b - a + 1) as usize, on);
            }
            MemoryToolsAction::Hover(a) => self.hover = a,
            MemoryToolsAction::Goto(text) => {
                let t = text.trim().trim_start_matches('$');
                if let Ok(a) = u32::from_str_radix(t, 16) {
                    self.cursor = a.min(last);
                    self.anchor = self.cursor;
                    self.cursor_to_top();
                }
            }
            MemoryToolsAction::Find(req, backward) => {
                self.find(em, kind, &req, backward);
                self.last_find = Some(req);
            }
            MemoryToolsAction::FindAgain(backward) => {
                if let Some(req) = self.last_find.clone() {
                    self.find(em, kind, &req, backward);
                }
            }
            MemoryToolsAction::Navigate(target, forward) => {
                if let Ok(Some(a)) = em.memory_navigate(
                    kind,
                    self.cursor,
                    target,
                    forward,
                    self.settings.highlight.fade,
                ) {
                    self.cursor = a;
                    self.anchor = a;
                    self.follow_cursor();
                }
            }
            MemoryToolsAction::Import => fx.push(MemoryEffect::Import(kind)),
            MemoryToolsAction::Export => {
                if let Ok(bytes) = em.read_memory(kind, 0, size) {
                    fx.push(MemoryEffect::Export(em.memory_dump_name(kind), bytes));
                }
            }
            MemoryToolsAction::ResetCounters => {
                let _ = em.reset_access_counters();
            }
            MemoryToolsAction::LoadTbl => fx.push(MemoryEffect::LoadTbl),
            MemoryToolsAction::ResetTbl => {
                self.tbl = None;
                self.settings.tbl_path = None;
            }
            MemoryToolsAction::Settings(s) => {
                if s.columns != self.settings.columns {
                    self.top_row = u64::from(self.cursor) / u64::from(s.columns.max(1));
                }
                self.settings = s;
            }
            MemoryToolsAction::AddWatch => {
                if let Some(cpu) = bus_cpu(kind) {
                    let (a, b) = self.selection();
                    let width = if cpu == ListingCpu::Snes { 6 } else { 4 };
                    let entries = (a..=b).map(|x| format!("[${x:0width$X}]")).collect();
                    fx.push(MemoryEffect::AddWatch(cpu, entries));
                }
            }
            MemoryToolsAction::ToggleBreakpoint => {
                let (a, b) = self.selection();
                let existing = em.bp_list().unwrap_or_default().into_iter().find(|bp| {
                    !bp.exec && bp.lo == a && bp.hi == b && luna_api::breakpoint_memory(bp) == kind
                });
                if let Some(bp) = existing {
                    let _ = em.bp_remove(bp.id);
                } else if kind == MemoryKind::SnesMemory {
                    let _ = em.bp_add_mem(a, b, true, true, false, None);
                } else {
                    let _ = em.bp_add_watch(kind, a, b, true, true, None);
                }
                fx.push(MemoryEffect::SyncBreakpoints);
            }
            MemoryToolsAction::SetLabel(name, comment) => {
                let (a, b) = self.selection();
                let Some((memory, address)) = em.memory_behind(kind, a) else {
                    return fx;
                };
                if name.is_empty() && comment.is_empty() {
                    em.remove_label(memory, address);
                } else {
                    let _ = em.set_label(luna_api::Label {
                        memory,
                        address,
                        length: b - a + 1,
                        name,
                        comment,
                    });
                }
            }
            MemoryToolsAction::ViewInDebugger => {
                if let Some(cpu) = bus_cpu(kind) {
                    fx.push(MemoryEffect::ViewInDebugger(cpu, self.cursor));
                }
            }
            MemoryToolsAction::ViewIn => {
                let target = if matches!(kind, MemoryKind::SnesMemory | MemoryKind::SpcMemory) {
                    em.memory_behind(kind, self.cursor)
                } else {
                    let bus = if matches!(kind, MemoryKind::SpcRam | MemoryKind::SpcRom) {
                        MemoryKind::SpcMemory
                    } else {
                        MemoryKind::SnesMemory
                    };
                    em.bus_address(bus, kind, self.cursor)
                        .ok()
                        .flatten()
                        .map(|a| (bus, a))
                };
                if let Some((k, a)) = target {
                    self.settings.kind = k.name().to_string();
                    self.cursor = a;
                    self.anchor = a;
                    self.cursor_to_top();
                }
            }
        }
        fx
    }
}

// ---------------------------------------------------------------------------
// Memory Search
// ---------------------------------------------------------------------------

/// How Memory Search shows values (Mesen2's *Format*).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SearchDisplay {
    /// Hexadecimal, read unsigned.
    #[default]
    Hex,
    /// Signed decimal.
    Signed,
    /// Unsigned decimal.
    Unsigned,
}

/// What a filter compares with, as the window's radio buttons hold it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CompareTo {
    /// The value at the last filter.
    PreviousSearch,
    /// The value at the last refresh (Mesen2's default).
    #[default]
    PreviousRefresh,
    /// The value typed.
    Value,
    /// The value at the address typed.
    Address,
}

/// A column Memory Search sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchColumn {
    Address,
    Value,
    LastValue,
    Reads,
    LastRead,
    Writes,
    LastWrite,
    Execs,
    LastExec,
    Match,
}

/// The window's form: what the next filter is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchForm {
    /// How values show.
    pub display: SearchDisplay,
    /// 1, 2 or 4 bytes.
    pub size: u8,
    /// The target.
    pub compare: CompareTo,
    /// The value typed (hex when the format is Hex).
    pub value: String,
    /// The address typed (hex).
    pub address: String,
    /// The operator.
    pub operator: luna_api::SearchOperator,
}

impl Default for SearchForm {
    fn default() -> Self {
        Self {
            display: SearchDisplay::Hex,
            size: 1,
            compare: CompareTo::PreviousRefresh,
            value: String::new(),
            address: String::new(),
            operator: luna_api::SearchOperator::Equal,
        }
    }
}

impl SearchForm {
    /// How values are read.
    pub(crate) fn format(&self) -> luna_api::SearchFormat {
        luna_api::SearchFormat {
            size: self.size,
            signed: self.display == SearchDisplay::Signed,
        }
    }

    /// The filter the form describes, `None` while a typed field does not
    /// parse or is out of the size's range.
    pub(crate) fn filter(&self) -> Option<luna_api::SearchFilter> {
        use luna_api::SearchTarget;
        let target = match self.compare {
            CompareTo::PreviousSearch => SearchTarget::PreviousSearch,
            CompareTo::PreviousRefresh => SearchTarget::PreviousRefresh,
            CompareTo::Value => {
                let t = self.value.trim().trim_start_matches('$');
                let v = if self.display == SearchDisplay::Hex {
                    i64::from_str_radix(t, 16).ok()?
                } else {
                    t.parse().ok()?
                };
                let bits = u32::from(self.size) * 8;
                let (lo, hi) = if self.display == SearchDisplay::Signed {
                    (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
                } else {
                    (0, (1i64 << bits) - 1)
                };
                if v < lo || v > hi {
                    return None;
                }
                SearchTarget::Value(v)
            }
            CompareTo::Address => {
                let t = self.address.trim().trim_start_matches('$');
                SearchTarget::Address(u32::from_str_radix(t, 16).ok()?)
            }
        };
        Some(luna_api::SearchFilter {
            target,
            operator: self.operator,
        })
    }
}

/// What the Memory Search window did this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MemorySearchAction {
    /// Search another memory.
    SetKind(MemoryKind),
    /// The form changed.
    Form(SearchForm),
    /// Keep the candidates that pass.
    Apply,
    /// Undo the last filter.
    Undo,
    /// Start over.
    Reset,
    /// Reset the access counters.
    ResetCounters,
    /// Sort by a column (again: the other way).
    Sort(SearchColumn),
    /// How many rows the table shows.
    VisibleRows(usize),
    /// Scroll by rows.
    Scroll(i64),
    /// Put this row at the top.
    ScrollTo(usize),
}

/// One row of the table, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchLine {
    pub address: String,
    pub value: String,
    pub last_value: String,
    pub reads: String,
    pub last_read: String,
    pub writes: String,
    pub last_write: String,
    pub execs: String,
    pub last_exec: String,
    /// `None` while the form's filter does not parse.
    pub matches: Option<bool>,
}

/// What the Memory Search window shows.
#[derive(Clone, Debug, Default)]
pub(crate) struct MemorySearchView {
    /// The memories it can search.
    pub kinds: Vec<MemoryKind>,
    /// The memory searched.
    pub kind: Option<MemoryKind>,
    /// The form.
    pub form: SearchForm,
    /// The filter the form describes parses.
    pub filter_ok: bool,
    /// Candidates left.
    pub candidates: usize,
    /// Filters Undo can take back.
    pub undo: usize,
    /// The rows shown.
    pub rows: Vec<SearchLine>,
    /// The first row shown.
    pub top: usize,
    /// The sort.
    pub sort: Option<(SearchColumn, bool)>,
}

/// The Memory Search window's state between frames.
#[derive(Default)]
pub(crate) struct MemorySearchHost {
    form: SearchForm,
    top: usize,
    visible: usize,
    sort: Option<(SearchColumn, bool)>,
    last_refresh: Option<std::time::Instant>,
    /// The window is open.
    pub open: bool,
}

/// Mesen2's text for master cycles since a stamp: `n/a` for never.
fn since(now: u64, stamp: u64) -> String {
    if stamp == 0 {
        "n/a".into()
    } else {
        luna_api::memory_view::short_count(now.saturating_sub(stamp))
    }
}

impl MemorySearchHost {
    /// The window opened: count accesses, and start on work RAM when no
    /// search runs.
    pub(crate) fn opened(&mut self, em: &mut Emulator) {
        self.open = true;
        if !em.access_counters_enabled() {
            let _ = em.enable_access_counters(true);
        }
        if em.mem_search_kind().is_none() {
            let _ = em.mem_search_start(MemoryKind::SnesWorkRam);
        }
        if self.visible == 0 {
            self.visible = DEFAULT_ROWS;
        }
    }

    /// The window closed: stop counting unless `keep`.
    pub(crate) fn closed(&mut self, em: &mut Emulator, keep: bool) {
        self.open = false;
        if !keep {
            let _ = em.enable_access_counters(false);
        }
    }

    /// Another ROM was loaded.
    pub(crate) fn rom_loaded(&mut self, em: &mut Emulator) {
        self.top = 0;
        if self.open {
            self.opened(em);
        }
    }

    fn value_text(&self, v: i64) -> String {
        match self.form.display {
            SearchDisplay::Hex => {
                let digits = usize::from(self.form.size) * 2;
                let mask = (1u64 << (u64::from(self.form.size) * 8)) - 1;
                format!("{:0digits$X}", (v as u64) & mask)
            }
            _ => v.to_string(),
        }
    }

    /// What the window shows now; takes the current values as the
    /// previous-refresh ones about 30 times a second.
    pub(crate) fn view(&mut self, em: &mut Emulator) -> MemorySearchView {
        let kinds = MemoryKind::ALL
            .iter()
            .copied()
            .filter(|k| {
                !matches!(
                    k,
                    MemoryKind::SnesMemory
                        | MemoryKind::SpcMemory
                        | MemoryKind::Sa1Memory
                        | MemoryKind::GsuMemory
                        | MemoryKind::SnesRegister
                ) && em.memory_size(*k).is_ok_and(|s| s > 0)
            })
            .collect();
        let kind = em.mem_search_kind();
        let (candidates, undo) = em.mem_search_count().unwrap_or((0, 0));
        let filter = self.form.filter();
        let format = self.form.format();
        let visible = self.visible.max(1);
        self.top = self.top.min(candidates.saturating_sub(visible));
        let (first, count) = if self.sort.is_some() {
            (0, candidates)
        } else {
            (self.top, visible)
        };
        let mut rows = em
            .mem_search_rows(first, count, format, filter)
            .unwrap_or_default();
        if let Some((col, asc)) = self.sort {
            let key = |r: &luna_api::SearchRow| -> i128 {
                let c = r.counters.unwrap_or_default();
                // "Last …" sorts the most recent first: by stamp, reversed.
                match col {
                    SearchColumn::Address => i128::from(r.offset),
                    SearchColumn::Value => i128::from(r.value),
                    SearchColumn::LastValue => i128::from(r.last_value),
                    SearchColumn::Reads => i128::from(c.read),
                    SearchColumn::LastRead => -i128::from(c.last_read),
                    SearchColumn::Writes => i128::from(c.write),
                    SearchColumn::LastWrite => -i128::from(c.last_write),
                    SearchColumn::Execs => i128::from(c.exec),
                    SearchColumn::LastExec => -i128::from(c.last_exec),
                    SearchColumn::Match => i128::from(r.matches == Some(true)),
                }
            };
            rows.sort_by_key(|r| (key(r), r.offset));
            if !asc {
                rows.reverse();
            }
            rows = rows.into_iter().skip(self.top).take(visible).collect();
        }
        let now = em.master_clock().unwrap_or(0);
        let digits = kind
            .and_then(|k| em.memory_size(k).ok())
            .map_or(4, |s| format!("{:X}", s.saturating_sub(1)).len().max(4));
        let lines = rows
            .iter()
            .map(|r| {
                let c = r.counters.unwrap_or_default();
                let n = |v: u32| luna_api::memory_view::short_count(u64::from(v));
                SearchLine {
                    address: format!("{:0digits$X}", r.offset),
                    value: self.value_text(r.value),
                    last_value: self.value_text(r.last_value),
                    reads: n(c.read),
                    last_read: since(now, c.last_read),
                    writes: n(c.write),
                    last_write: since(now, c.last_write),
                    execs: n(c.exec),
                    last_exec: since(now, c.last_exec),
                    matches: r.matches,
                }
            })
            .collect();
        let due = self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(33));
        if due {
            let _ = em.mem_search_refresh();
            self.last_refresh = Some(std::time::Instant::now());
        }
        MemorySearchView {
            kinds,
            kind,
            form: self.form.clone(),
            filter_ok: filter.is_some(),
            candidates,
            undo,
            rows: lines,
            top: self.top,
            sort: self.sort,
        }
    }

    /// Apply what the window did.
    pub(crate) fn apply(&mut self, em: &mut Emulator, act: MemorySearchAction) {
        match act {
            MemorySearchAction::SetKind(k) => {
                let _ = em.mem_search_start(k);
                self.top = 0;
            }
            MemorySearchAction::Form(f) => self.form = f,
            MemorySearchAction::Apply => {
                if let Some(f) = self.form.filter() {
                    let _ = em.mem_search_filter(self.form.format(), f);
                    self.top = 0;
                }
            }
            MemorySearchAction::Undo => {
                let _ = em.mem_search_undo();
            }
            MemorySearchAction::Reset => {
                if let Some(k) = em.mem_search_kind() {
                    let _ = em.mem_search_start(k);
                }
                self.top = 0;
            }
            MemorySearchAction::ResetCounters => {
                let _ = em.reset_access_counters();
            }
            MemorySearchAction::Sort(col) => {
                self.sort = match self.sort {
                    Some((c, asc)) if c == col => Some((c, !asc)),
                    _ => Some((col, true)),
                };
            }
            MemorySearchAction::VisibleRows(n) => self.visible = n.max(1),
            MemorySearchAction::Scroll(d) => self.top = self.top.saturating_add_signed(d as isize),
            MemorySearchAction::ScrollTo(r) => self.top = r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32 KB `LoROM`: `LDA $0010 ; STA $0011 ; BRA` at `$8000`.
    fn loaded() -> (Emulator, MemoryToolsHost) {
        let mut r = vec![0u8; 0x8000];
        r[..8].copy_from_slice(&[0xAD, 0x10, 0x00, 0x8D, 0x11, 0x00, 0x80, 0xF8]);
        r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA MEMORY TOOLS    ");
        r[0x7FD5] = 0x20;
        r[0x7FD7] = 0x08;
        r[0x7FFC] = 0x00;
        r[0x7FFD] = 0x80;
        let mut em = Emulator::new();
        em.load_rom_bytes_forced(r, luna_api::MapperKind::LoRom)
            .unwrap();
        let mut h = MemoryToolsHost::default();
        h.opened(&mut em);
        (em, h)
    }

    #[test]
    fn opening_counts_accesses_and_the_view_shows_them() {
        let (mut em, mut h) = loaded();
        assert!(em.access_counters_enabled());
        em.step(50).unwrap();
        h.apply(&mut em, MemoryToolsAction::SetKind(MemoryKind::SnesWorkRam));
        let v = h.view(&em);
        assert_eq!(v.kind, Some(MemoryKind::SnesWorkRam));
        assert_eq!(v.total_rows, 0x2_0000 / 16);
        assert_eq!(v.rows[1].0, 0x10);
        assert_eq!(
            v.rows[1].1[0].access.map(|a| a.0),
            Some(luna_api::AccessKind::Read)
        );
        assert_eq!(v.status, "Location: $00000");
        assert!(!v.kinds.iter().any(|(k, _)| *k == MemoryKind::SnesRegister));
        h.closed(&mut em, false);
        assert!(!em.access_counters_enabled());
    }

    #[test]
    fn typing_fills_and_undo_takes_back() {
        let (mut em, mut h) = loaded();
        h.apply(&mut em, MemoryToolsAction::SetKind(MemoryKind::SnesWorkRam));
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x100, false));
        h.apply(&mut em, MemoryToolsAction::HexDigit(0xA));
        assert_eq!(h.view(&em).pending, Some(0xA));
        h.apply(&mut em, MemoryToolsAction::HexDigit(0x5));
        assert_eq!(
            em.read_memory(MemoryKind::SnesWorkRam, 0x100, 1).unwrap(),
            [0xA5]
        );
        assert_eq!(h.view(&em).cursor, 0x101);
        // A selection is filled by one byte typed.
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x110, false));
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x113, true));
        let v = h.view(&em);
        assert_eq!(v.selection, (0x110, 0x113));
        assert!(
            v.status
                .starts_with("Selection: $00110 - $00113   Length: 4 bytes ($4)")
        );
        h.apply(&mut em, MemoryToolsAction::HexDigit(7));
        h.apply(&mut em, MemoryToolsAction::HexDigit(7));
        assert_eq!(
            em.read_memory(MemoryKind::SnesWorkRam, 0x110, 5).unwrap()[..4],
            [0x77; 4]
        );
        h.apply(&mut em, MemoryToolsAction::Undo);
        assert_eq!(
            em.read_memory(MemoryKind::SnesWorkRam, 0x110, 1).unwrap(),
            [0]
        );
        // Paste hex from the cursor, then copy it back.
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x120, false));
        h.apply(&mut em, MemoryToolsAction::Paste("de ad\nbe ef".into()));
        assert_eq!(
            em.read_memory(MemoryKind::SnesWorkRam, 0x120, 4).unwrap(),
            [0xDE, 0xAD, 0xBE, 0xEF]
        );
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x120, false));
        h.apply(&mut em, MemoryToolsAction::MoveTo(0x123, true));
        h.apply(&mut em, MemoryToolsAction::Copy);
        assert_eq!(h.view(&em).clipboard.as_deref(), Some("DE AD BE EF"));
    }

    #[test]
    fn find_freeze_watch_and_switch_views() {
        let (mut em, mut h) = loaded();
        h.apply(&mut em, MemoryToolsAction::SetKind(MemoryKind::SnesPrgRom));
        let req = FindRequest {
            data: FindData::Hex(vec![Some(0x8D), None, Some(0x00)]),
            use_tbl: false,
            filters: FindFilters::default(),
        };
        h.apply(&mut em, MemoryToolsAction::Find(req, false));
        assert_eq!(h.view(&em).selection, (3, 5));
        h.apply(&mut em, MemoryToolsAction::FindAgain(false));
        let v = h.view(&em);
        assert!(
            !v.find_miss && v.selection == (3, 5),
            "the only match, found again"
        );
        // ROM → the CPU bus, and back.
        h.apply(&mut em, MemoryToolsAction::ViewIn);
        let v = h.view(&em);
        assert_eq!(
            (v.kind, v.cursor),
            (Some(MemoryKind::SnesMemory), 0x00_8005)
        );
        let fx = h.apply(&mut em, MemoryToolsAction::AddWatch);
        assert_eq!(
            fx,
            [MemoryEffect::AddWatch(
                ListingCpu::Snes,
                vec!["[$008005]".into()]
            )]
        );
        h.apply(&mut em, MemoryToolsAction::Freeze(true));
        assert!(em.write_frozen(MemoryKind::SnesMemory, 0x00_8005).unwrap());
        h.apply(&mut em, MemoryToolsAction::ViewIn);
        assert_eq!(h.view(&em).kind, Some(MemoryKind::SnesPrgRom));
        assert_eq!(
            h.apply(&mut em, MemoryToolsAction::Export),
            [MemoryEffect::Export(
                "LUNA MEMORY TOOLS - SnesPrgRom.dmp".into(),
                em.read_memory(MemoryKind::SnesPrgRom, 0, 0x8000).unwrap()
            )]
        );
    }

    #[test]
    fn memory_search_filters_sorts_and_undoes() {
        let (mut em, _) = loaded();
        let mut h = MemorySearchHost::default();
        h.opened(&mut em);
        h.apply(&mut em, MemorySearchAction::VisibleRows(4));
        let v = h.view(&mut em);
        assert_eq!(
            (v.kind, v.candidates),
            (Some(MemoryKind::SnesWorkRam), 0x2_0000)
        );
        assert_eq!(v.rows[0].address, "00000");
        // A value out of the 8-bit range does not parse.
        let mut f = SearchForm {
            compare: CompareTo::Value,
            value: "1FF".into(),
            ..SearchForm::default()
        };
        h.apply(&mut em, MemorySearchAction::Form(f.clone()));
        assert!(!h.view(&mut em).filter_ok);
        f.value = "0".into();
        f.operator = luna_api::SearchOperator::NotEqual;
        h.apply(&mut em, MemorySearchAction::Form(f));
        em.write_memory(MemoryKind::SnesWorkRam, 0x200, &[5, 9])
            .unwrap();
        let v = h.view(&mut em);
        assert!(v.filter_ok);
        h.apply(&mut em, MemorySearchAction::Apply);
        let v = h.view(&mut em);
        assert!(v.candidates >= 2 && v.undo == 1, "{}", v.candidates);
        // Sorted by value, the largest last; again, the largest first.
        h.apply(&mut em, MemorySearchAction::Sort(SearchColumn::Value));
        h.apply(&mut em, MemorySearchAction::Sort(SearchColumn::Value));
        let v = h.view(&mut em);
        assert!(v.rows.iter().all(|r| r.matches == Some(true)));
        assert!(v.rows[0].value.as_str() >= v.rows[1].value.as_str());
        h.apply(&mut em, MemorySearchAction::Undo);
        assert_eq!(h.view(&mut em).candidates, 0x2_0000);
    }
}
