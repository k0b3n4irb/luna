//! The Debugger window's host side: the state it keeps between frames,
//! the [`DebuggerView`] built from `luna-api` each frame, and the
//! [`DebuggerAction`]s applied under the emulator lock. What reaches past
//! the emulator (resume, pause, a step, a file dialog) comes back to
//! `LunaApp` as [`HostEffect`]s.

use luna_api::{
    BreakCpu, BreakHit, BreakKind, BreakSources, Emulator, FindResult, ListingCpu, ListingPos,
    MemoryKind, SpcStepKind, StepKind, WatchSpace, WatchStyle,
};

use crate::ui::{DebuggerAction, DebuggerSettings, DebuggerView, WatchLine};

/// The GUI's name in a workspace file (the 65C816 window's settings; the
/// SPC700 window's go under the same name with `-spc`).
const WORKSPACE_KEY: &str = "luna-gui";

/// Something only `LunaApp` can do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HostEffect {
    /// Resume the emulation.
    Continue,
    /// Pause it (and end a long step).
    Break,
    /// Run one of Mesen2's steps while paused.
    Step(StepKind),
    /// Run one of the SPC700's steps while paused.
    SpcStep(SpcStepKind),
    /// The breakpoint registry or the break sources changed.
    SyncBreakpoints,
    /// Pick a label file to import.
    ImportLabels,
    /// Save this `.mlb` text.
    ExportLabels(String),
    /// Pick a watch file to import.
    ImportWatches,
    /// Save these watch entries, one per line.
    ExportWatches(String),
    /// Debug ▸ Reset.
    Reset,
    /// Debug ▸ Reload ROM.
    Reload,
}

/// The Debugger window's state between frames.
#[derive(Default)]
pub(crate) struct DebuggerHost {
    /// The CPU this window debugs.
    cpu: ListingCpu,
    pub settings: DebuggerSettings,
    top: ListingPos,
    visible: usize,
    cursor: Option<ListingPos>,
    watches: Vec<String>,
    watch_values: Vec<String>,
    watch_changed: Vec<bool>,
    watch_style: WatchStyle,
    /// The watches need evaluating (a break, a step, an edit).
    dirty: bool,
    back: Vec<u32>,
    forward: Vec<u32>,
    find_results: Vec<FindResult>,
    find_miss: bool,
    goto_all_query: String,
    break_on: [bool; 4],
    pub pad_override: [u16; 2],
    /// The temporary breakpoint of a Run to Location.
    temp_bp: Option<u32>,
    /// A Run to NMI / IRQ / scanline set the break sources.
    temp_sources: bool,
    /// An address to bring into view (centred) on the next frame.
    center_on: Option<u32>,
    /// Centre on the PC once the emulation is paused.
    pub center_on_pc: bool,
    break_reason: Option<String>,
    break_mclk: Option<u64>,
    status: Option<luna_api::DebuggerStatus>,
}

impl DebuggerHost {
    /// The window of the SPC700, or of the 65C816.
    pub(crate) fn for_cpu(cpu: ListingCpu) -> Self {
        Self {
            cpu,
            ..Self::default()
        }
    }

    const fn break_cpu(&self) -> BreakCpu {
        match self.cpu {
            ListingCpu::Snes => BreakCpu::Snes,
            ListingCpu::Spc => BreakCpu::Spc,
        }
    }

    const fn bus(&self) -> MemoryKind {
        match self.cpu {
            ListingCpu::Snes => MemoryKind::SnesMemory,
            ListingCpu::Spc => MemoryKind::SpcMemory,
        }
    }

    const fn expr_cpu(&self) -> luna_api::ExprCpu {
        match self.cpu {
            ListingCpu::Snes => luna_api::ExprCpu::Snes,
            ListingCpu::Spc => luna_api::ExprCpu::Spc,
        }
    }

    /// This window's names in a workspace: its watches', its settings'.
    const fn keys(&self) -> (&'static str, &'static str) {
        match self.cpu {
            ListingCpu::Snes => ("snes", WORKSPACE_KEY),
            ListingCpu::Spc => ("spc", "luna-gui-spc"),
        }
    }

    /// The window opened: log code (and, for the 65C816, calls) while it
    /// is open.
    pub(crate) fn opened(&mut self, em: &mut Emulator) {
        em.enable_code_log(true);
        if self.cpu == ListingCpu::Snes {
            em.enable_call_stack(true);
        }
        self.dirty = true;
        self.center_on_pc = true;
        if self.visible == 0 {
            self.visible = 40;
        }
    }

    /// Another ROM was loaded: what pointed into the old one goes.
    pub(crate) fn rom_changed(&mut self) {
        self.top = ListingPos::default();
        self.cursor = None;
        self.back.clear();
        self.forward.clear();
        self.find_results.clear();
        self.status = None;
        self.break_reason = None;
        self.break_mclk = None;
        self.temp_bp = None;
        self.temp_sources = false;
        self.watches.clear();
        self.watch_values.clear();
        self.center_on_pc = true;
        self.dirty = true;
    }

    /// Take the watches and the settings a ROM's workspace kept.
    pub(crate) fn load_workspace(&mut self, ws: &luna_api::Workspace) {
        let (watch_key, settings_key) = self.keys();
        self.watches = ws.watches.get(watch_key).cloned().unwrap_or_default();
        self.watch_values.clear();
        if let Some(s) = ws
            .front_end
            .get(settings_key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.settings = s;
        }
        self.dirty = true;
    }

    /// Add the watches and the settings to a workspace being saved.
    pub(crate) fn store_workspace(&self, ws: &mut luna_api::Workspace) {
        let (watch_key, settings_key) = self.keys();
        if !self.watches.is_empty() {
            ws.watches.insert(watch_key.into(), self.watches.clone());
        }
        if let Ok(v) = serde_json::to_value(self.settings) {
            ws.front_end.insert(settings_key.into(), v);
        }
    }

    /// The window closed: stop logging (what was logged stays), the code
    /// log only when no other debugger window is open.
    pub(crate) fn closed(&self, em: &mut Emulator, other_open: bool) {
        if !other_open {
            em.enable_code_log(false);
        }
        if self.cpu == ListingCpu::Snes {
            em.enable_call_stack(false);
        }
    }

    /// The CPU stopped: on a breakpoint (`hit`), after a step, or paused.
    pub(crate) fn stopped(&mut self, em: &mut Emulator, hit: Option<&BreakHit>) -> Vec<HostEffect> {
        let mut fx = Vec::new();
        if let Some(id) = self.temp_bp.take() {
            let _ = em.bp_remove(id);
            fx.push(HostEffect::SyncBreakpoints);
        }
        if self.temp_sources {
            self.temp_sources = false;
            em.set_break_sources(self.sources(BreakSources::default()));
            fx.push(HostEffect::SyncBreakpoints);
        }
        self.break_reason = Some(match hit {
            None => "CPU Step".to_string(),
            Some(h) => match h.kind {
                BreakKind::Step => "CPU Step".to_string(),
                BreakKind::Exec => format!(
                    "Breakpoint: Exec {} (${:0w$X})",
                    self.short_name(),
                    h.pc,
                    w = self.cpu.digits()
                ),
                BreakKind::Read | BreakKind::Write => format!(
                    "Breakpoint: {} {} (${:04X}:${:02X})",
                    if h.kind == BreakKind::Read {
                        "Read"
                    } else {
                        "Write"
                    },
                    self.short_name(),
                    h.addr.unwrap_or(0),
                    h.value.unwrap_or(0)
                ),
                other => format!("{other:?}").to_uppercase(),
            },
        });
        if let Ok(s) = em.debugger_status() {
            self.break_mclk = Some(s.master_clocks);
        }
        self.center_on_pc = true;
        self.dirty = true;
        fx
    }

    /// The break sources the settings ask for, plus `extra`.
    const fn sources(&self, extra: BreakSources) -> BreakSources {
        BreakSources {
            brk: self.break_on[0] || extra.brk,
            cop: self.break_on[1] || extra.cop,
            wdm: self.break_on[2] || extra.wdm,
            stp: self.break_on[3] || extra.stp,
            ..extra
        }
    }

    fn pc(&self, em: &Emulator) -> Option<u32> {
        match self.cpu {
            ListingCpu::Snes => em
                .cpu_state()
                .ok()
                .map(|c| (u32::from(c.pb) << 16) | u32::from(c.pc)),
            ListingCpu::Spc => em.spc700_state().ok().map(|c| u32::from(c.pc)),
        }
    }

    /// Mesen2's short name of the CPU's memory in a break reason.
    const fn short_name(&self) -> &'static str {
        match self.cpu {
            ListingCpu::Snes => "CPU",
            ListingCpu::Spc => "SPC",
        }
    }

    /// Bring `addr` into view: centred unless already comfortably shown.
    fn bring_into_view(&mut self, em: &Emulator, addr: u32, force_center: bool) {
        let opts = self.settings.listing;
        let Ok(pos) = em.listing_pos_of(self.cpu, addr, opts) else {
            return;
        };
        let shown = em
            .listing(self.cpu, self.top, self.visible.max(1), opts)
            .unwrap_or_default();
        let inside = shown
            .iter()
            .position(|r| r.pos == pos)
            .is_some_and(|i| i >= 1 && i + 2 < shown.len());
        if (force_center || !inside)
            && let Ok(top) = em.listing_back(self.cpu, pos, self.visible / 2, opts)
        {
            self.top = top;
        }
        self.cursor = Some(pos);
    }

    fn record_jump(&mut self) {
        if let Some(c) = self.cursor.map(|c| c.addr).or(Some(self.top.addr)) {
            self.back.push(c);
            self.forward.clear();
        }
    }

    /// What the window shows this frame.
    pub(crate) fn view(&mut self, em: &Emulator, paused: bool) -> DebuggerView {
        let opts = self.settings.listing;
        if self.center_on_pc && paused {
            self.center_on_pc = false;
            if let Some(pc) = self.pc(em) {
                self.bring_into_view(em, pc, self.settings.keep_centered);
            }
        }
        if let Some(a) = self.center_on.take() {
            self.bring_into_view(em, a, true);
        }
        let rows = em
            .listing(self.cpu, self.top, self.visible.max(1) + 1, opts)
            .unwrap_or_default();
        let space = self.cpu.space() as f32;
        let bps = em.bp_list().unwrap_or_default();
        let bp_marks = bps
            .iter()
            .filter(|b| {
                b.cpu == self.break_cpu() && b.space == WatchSpace::Bus && (b.exec || b.forbid)
            })
            .map(|b| (b.lo as f32 / space, b.enabled))
            .collect();
        let main = self.cpu == ListingCpu::Snes;
        if paused || self.settings.refresh_running || self.status.is_none() {
            // The master clocks (the elapsed time) come from the console's
            // status, whatever the window.
            self.status = em.debugger_status().ok();
        }
        let spc_status = (!main).then(|| em.spc700_state().ok()).flatten().map(|c| {
            let sp = u32::from(c.sp);
            let stack = em
                .read_memory(MemoryKind::SpcRam, 0x100 + sp + 1, (0xFF - sp) as usize)
                .unwrap_or_default();
            (c, stack)
        });
        if self.dirty || self.settings.refresh_running && !paused {
            self.dirty = false;
            let values: Vec<String> = self
                .watches
                .iter()
                .map(|w| em.watch_value(w, self.expr_cpu(), self.watch_style))
                .collect();
            self.watch_changed = values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    v.starts_with('<') || self.watch_values.get(i).is_some_and(|old| old != v)
                })
                .collect();
            self.watch_values = values;
        }
        let watches = self
            .watches
            .iter()
            .enumerate()
            .map(|(i, entry)| WatchLine {
                entry: entry.clone(),
                value: self.watch_values.get(i).cloned().unwrap_or_default(),
                changed: self.watch_changed.get(i).copied().unwrap_or(false),
            })
            .collect();
        let label_of = |a: u32| {
            em.label_at(self.bus(), a)
                .ok()
                .flatten()
                .and_then(|h| h.name())
        };
        let breakpoints = bps
            .into_iter()
            .filter(|b| b.cpu == self.break_cpu())
            .map(|b| {
                let l = (b.space == WatchSpace::Bus)
                    .then(|| label_of(b.lo))
                    .flatten();
                (b, l)
            })
            .collect();
        let vector = |name: &'static str, at: u32| {
            let lo = em
                .read_memory(MemoryKind::SnesMemory, at, 2)
                .unwrap_or_default();
            let target = lo.first().copied().map_or(0, u16::from)
                | lo.get(1).copied().map_or(0, |h| u16::from(h) << 8);
            (name, at, target)
        };
        DebuggerView {
            cpu: self.cpu,
            spc_status,
            paused,
            scroll: self.top.addr as f32 / space,
            pc_mark: self.pc(em).map(|pc| pc as f32 / space),
            bp_marks,
            rows,
            cursor: self.cursor,
            settings: self.settings,
            status: self.status.clone(),
            watches,
            watch_style: self.watch_style,
            breakpoints,
            call_stack: if main {
                em.call_stack_rows()
            } else {
                Vec::new()
            },
            labels: em.label_rows(self.cpu),
            functions: if main { em.functions() } else { Vec::new() },
            find_results: self.find_results.clone(),
            goto_all: if self.goto_all_query.trim().is_empty() {
                Vec::new()
            } else {
                em.goto_all(self.cpu, &self.goto_all_query)
            },
            break_reason: if paused {
                self.break_reason.clone()
            } else {
                None
            },
            elapsed: self
                .status
                .as_ref()
                .zip(self.break_mclk)
                .map(|(s, b)| s.master_clocks.saturating_sub(b)),
            can_back: !self.back.is_empty(),
            can_forward: !self.forward.is_empty(),
            find_miss: self.find_miss,
            pad_override: self.pad_override,
            break_on: self.break_on,
            vectors: if main {
                vec![
                    vector("NMI", 0xFFEA),
                    vector("IRQ", 0xFFEE),
                    vector("Reset", 0xFFFC),
                    vector("BRK", 0xFFE6),
                    vector("COP", 0xFFE4),
                    vector("NMI (6502)", 0xFFFA),
                    vector("IRQ (6502)", 0xFFFE),
                    vector("COP (6502)", 0xFFF4),
                ]
            } else {
                Vec::new()
            },
        }
    }

    /// Replace the watch list (File ▸ Workspace ▸ Import watch entries).
    pub(crate) fn set_watches(&mut self, text: &str) {
        self.watches = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        self.watch_values.clear();
        self.dirty = true;
    }

    /// Apply one action. What only `LunaApp` can do comes back.
    pub(crate) fn apply(&mut self, em: &mut Emulator, act: DebuggerAction) -> Vec<HostEffect> {
        let opts = self.settings.listing;
        let mut fx = Vec::new();
        match act {
            DebuggerAction::Continue => fx.push(HostEffect::Continue),
            DebuggerAction::Break => fx.push(HostEffect::Break),
            DebuggerAction::ToggleBreak => fx.push(HostEffect::Break),
            DebuggerAction::Step(k) => fx.push(match (self.cpu, k) {
                // The SPC700 steps by its own instructions; a scanline, a
                // frame or N of them run the console as a whole.
                (ListingCpu::Spc, StepKind::Into) => HostEffect::SpcStep(SpcStepKind::Into),
                (ListingCpu::Spc, StepKind::Over) => HostEffect::SpcStep(SpcStepKind::Over),
                (ListingCpu::Spc, StepKind::Out) => HostEffect::SpcStep(SpcStepKind::Out),
                (_, k) => HostEffect::Step(k),
            }),
            DebuggerAction::RunToNmi
            | DebuggerAction::RunToIrq
            | DebuggerAction::RunToScanline(_) => {
                let extra = match act {
                    DebuggerAction::RunToNmi => BreakSources {
                        nmi: true,
                        ..BreakSources::default()
                    },
                    DebuggerAction::RunToIrq => BreakSources {
                        irq: true,
                        ..BreakSources::default()
                    },
                    DebuggerAction::RunToScanline(l) => BreakSources {
                        scanline: Some(l),
                        ..BreakSources::default()
                    },
                    _ => BreakSources::default(),
                };
                em.set_break_sources(self.sources(extra));
                self.temp_sources = true;
                fx.extend([HostEffect::SyncBreakpoints, HostEffect::Continue]);
            }
            DebuggerAction::Scroll(n) if n > 0 => {
                let rows = em
                    .listing(self.cpu, self.top, n as usize + 1, opts)
                    .unwrap_or_default();
                if let Some(r) = rows.get(n as usize).or_else(|| rows.last()) {
                    self.top = r.pos;
                }
            }
            DebuggerAction::Scroll(n) => {
                if let Ok(t) = em.listing_back(self.cpu, self.top, n.unsigned_abs() as usize, opts)
                {
                    self.top = t;
                }
            }
            DebuggerAction::ScrollTo(f) => {
                let addr = ((f * self.cpu.space() as f32) as u32).min(self.cpu.space() - 1);
                if let Ok(p) = em.listing_pos_of(self.cpu, addr, opts) {
                    self.top = ListingPos {
                        addr: p.addr,
                        row: 0,
                    };
                }
            }
            DebuggerAction::Goto(a) => {
                self.record_jump();
                self.center_on = Some(a);
            }
            DebuggerAction::GotoText(t) => {
                if let Some(a) = em.goto_address(self.cpu, &t) {
                    self.record_jump();
                    self.center_on = Some(a);
                }
            }
            DebuggerAction::GotoPc => {
                if let Some(pc) = self.pc(em) {
                    self.record_jump();
                    self.center_on = Some(pc);
                }
            }
            DebuggerAction::Select(p) => self.cursor = Some(p),
            DebuggerAction::VisibleRows(n) => self.visible = n,
            DebuggerAction::ToggleBreakpoint(a) => {
                let cpu = self.break_cpu();
                let list = em.bp_list().unwrap_or_default();
                match list
                    .iter()
                    .find(|b| b.cpu == cpu && b.exec && b.space == WatchSpace::Bus && b.lo == a)
                {
                    Some(b) => {
                        let _ = em.bp_remove(b.id);
                    }
                    None => {
                        let _ = em.bp_add_exec_on(cpu, a, None);
                    }
                }
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::ToggleForbid(a) => {
                let cpu = self.break_cpu();
                let list = em.bp_list().unwrap_or_default();
                match list.iter().find(|b| {
                    b.cpu == cpu
                        && b.forbid
                        && b.space == WatchSpace::Bus
                        && (b.lo..=b.hi).contains(&a)
                }) {
                    Some(b) => {
                        let _ = em.bp_remove(b.id);
                    }
                    None => {
                        let _ = em.bp_add_forbid(cpu, self.bus(), a, a, None);
                    }
                }
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::RunTo(a) => {
                self.temp_bp = em
                    .bp_add_exec_on(self.break_cpu(), a, Some("run to location"))
                    .ok();
                fx.extend([HostEffect::SyncBreakpoints, HostEffect::Continue]);
            }
            DebuggerAction::MovePc(a) => {
                let _ = match self.cpu {
                    ListingCpu::Snes => em
                        .set_cpu_register("pb", a >> 16)
                        .and_then(|()| em.set_cpu_register("pc", a & 0xFFFF)),
                    ListingCpu::Spc => em.set_spc_register("pc", a & 0xFFFF),
                };
                self.dirty = true;
                self.center_on_pc = true;
            }
            DebuggerAction::Back => {
                if let Some(a) = self.back.pop() {
                    if let Some(c) = self.cursor {
                        self.forward.push(c.addr);
                    }
                    self.center_on = Some(a);
                }
            }
            DebuggerAction::Forward => {
                if let Some(a) = self.forward.pop() {
                    if let Some(c) = self.cursor {
                        self.back.push(c.addr);
                    }
                    self.center_on = Some(a);
                }
            }
            DebuggerAction::AddWatch(w) => {
                self.watches.push(w);
                self.dirty = true;
            }
            DebuggerAction::EditWatch(i, w) => {
                if i < self.watches.len() {
                    if w.trim().is_empty() {
                        self.watches.remove(i);
                        if i < self.watch_values.len() {
                            self.watch_values.remove(i);
                        }
                    } else {
                        self.watches[i] = w;
                    }
                }
                self.dirty = true;
            }
            DebuggerAction::MoveWatch(i, up) => {
                let j = if up { i.checked_sub(1) } else { Some(i + 1) };
                if let Some(j) = j.filter(|&j| j < self.watches.len()) {
                    self.watches.swap(i, j);
                    if i < self.watch_values.len() && j < self.watch_values.len() {
                        self.watch_values.swap(i, j);
                    }
                }
            }
            DebuggerAction::WatchStyle(s) => {
                self.watch_style = s;
                self.watch_values.clear();
                self.dirty = true;
            }
            DebuggerAction::SetLabel(l) => {
                if l.name.is_empty() && l.comment.is_empty() {
                    em.remove_label(l.memory, l.address);
                } else {
                    let _ = em.set_label(l);
                }
                self.dirty = true;
            }
            DebuggerAction::RemoveLabel(m, a) => {
                em.remove_label(m, a);
            }
            DebuggerAction::BpEnabled(id, on) => {
                let _ = em.bp_set_enabled(id, on);
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::BpMark(id, on) => {
                let _ = em.bp_set_mark(id, on);
            }
            DebuggerAction::BpRemove(id) => {
                let _ = em.bp_remove(id);
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::BpSave {
                replace,
                memory,
                lo,
                hi,
                rwx,
                condition,
                mark,
                enabled,
                forbid,
            } => {
                if let Some(id) = replace {
                    let _ = em.bp_remove(id);
                }
                let mut ids = Vec::new();
                if forbid {
                    ids.extend(
                        em.bp_add_forbid(self.break_cpu(), memory, lo, hi, None)
                            .ok(),
                    );
                } else {
                    if rwx[2] && memory == self.bus() {
                        ids.extend(em.bp_add_exec_on(self.break_cpu(), lo, None).ok());
                    }
                    if rwx[0] || rwx[1] {
                        ids.extend(em.bp_add_watch(memory, lo, hi, rwx[0], rwx[1], None).ok());
                    }
                }
                let condition = condition.trim();
                for id in ids {
                    if !condition.is_empty() {
                        let _ = em.bp_set_condition(id, Some(condition));
                    }
                    if mark && !forbid {
                        let _ = em.bp_set_mark(id, true);
                    }
                    if !enabled {
                        let _ = em.bp_set_enabled(id, false);
                    }
                }
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::SetRegister(reg, v) => {
                let _ = match self.cpu {
                    ListingCpu::Snes => em.set_cpu_register(reg, v),
                    ListingCpu::Spc => em.set_spc_register(reg, v),
                };
                self.dirty = true;
                self.status = em.debugger_status().ok();
            }
            DebuggerAction::Settings(s) => self.settings = s,
            DebuggerAction::BreakOn(on) => {
                self.break_on = on;
                let keep = em.break_sources();
                em.set_break_sources(BreakSources {
                    brk: on[0],
                    cop: on[1],
                    wdm: on[2],
                    stp: on[3],
                    ..keep
                });
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::Find(text, forward) => {
                let from = self.cursor.unwrap_or(self.top);
                match em.listing_find(self.cpu, from, &text, forward, opts) {
                    Ok(Some(p)) => {
                        self.find_miss = false;
                        self.center_on = Some(p.addr);
                    }
                    _ => self.find_miss = !text.is_empty(),
                }
            }
            DebuggerAction::FindAll(text, case, word) => {
                self.find_results = em
                    .listing_find_all(self.cpu, &text, case, word, opts)
                    .unwrap_or_default();
            }
            DebuggerAction::GotoAllQuery(q) => self.goto_all_query = q,
            DebuggerAction::PadOverride(port, mask) => {
                if let Some(m) = self.pad_override.get_mut(port) {
                    *m = mask;
                }
            }
            DebuggerAction::ImportLabels => fx.push(HostEffect::ImportLabels),
            DebuggerAction::ExportLabels => {
                fx.push(HostEffect::ExportLabels(em.export_labels_mlb()));
            }
            DebuggerAction::ImportWatches => fx.push(HostEffect::ImportWatches),
            DebuggerAction::ExportWatches => {
                fx.push(HostEffect::ExportWatches(self.watches.join("\n") + "\n"));
            }
            DebuggerAction::ResetWorkspace => {
                self.watches.clear();
                self.watch_values.clear();
                let _ = em.bp_clear();
                em.reset_labels();
                fx.push(HostEffect::SyncBreakpoints);
            }
            DebuggerAction::Reset => fx.push(HostEffect::Reset),
            DebuggerAction::Reload => fx.push(HostEffect::Reload),
        }
        fx
    }
}

/// The two Debugger windows (65C816, SPC700) and what they share.
pub(crate) struct Debuggers {
    /// The 65C816's.
    pub snes: DebuggerHost,
    /// The SPC700's.
    pub spc: DebuggerHost,
    /// Which is open, as last seen.
    open: [bool; 2],
    /// The window that asked for the step running now.
    step_owner: ListingCpu,
    /// The last view of each, redrawn while the emulator is busy.
    pub views: [Option<Box<DebuggerView>>; 2],
}

impl Default for Debuggers {
    fn default() -> Self {
        Self {
            snes: DebuggerHost::for_cpu(ListingCpu::Snes),
            spc: DebuggerHost::for_cpu(ListingCpu::Spc),
            open: [false; 2],
            step_owner: ListingCpu::Snes,
            views: [None, None],
        }
    }
}

const fn index(cpu: ListingCpu) -> usize {
    match cpu {
        ListingCpu::Snes => 0,
        ListingCpu::Spc => 1,
    }
}

impl Debuggers {
    /// The window of `cpu`.
    pub(crate) const fn host(&mut self, cpu: ListingCpu) -> &mut DebuggerHost {
        match cpu {
            ListingCpu::Snes => &mut self.snes,
            ListingCpu::Spc => &mut self.spc,
        }
    }

    /// Whether the window of `cpu` is open, as last seen.
    pub(crate) const fn is_open(&self, cpu: ListingCpu) -> bool {
        self.open[index(cpu)]
    }

    /// Whether any is open.
    pub(crate) const fn any_open(&self) -> bool {
        self.open[0] || self.open[1]
    }

    /// See the windows open or close (`open` is what is open now).
    /// Returns the windows just opened.
    pub(crate) fn sync_open(
        &mut self,
        em: Option<&mut Emulator>,
        open: [bool; 2],
    ) -> Vec<ListingCpu> {
        let mut opened = Vec::new();
        let mut em = em;
        for cpu in [ListingCpu::Snes, ListingCpu::Spc] {
            let i = index(cpu);
            if open[i] == self.open[i] {
                continue;
            }
            self.open[i] = open[i];
            let other = open[1 - i];
            if let Some(em) = em.as_deref_mut() {
                if open[i] {
                    self.host(cpu).opened(em);
                } else {
                    self.host(cpu).closed(em, other);
                }
            }
            if open[i] {
                opened.push(cpu);
            }
        }
        opened
    }

    /// A ROM was loaded into `em`: the windows start over on it.
    pub(crate) fn rom_loaded(&mut self, em: &mut Emulator, ws: Option<&luna_api::Workspace>) {
        for cpu in [ListingCpu::Snes, ListingCpu::Spc] {
            let open = self.open[index(cpu)];
            let h = self.host(cpu);
            h.rom_changed();
            if let Some(ws) = ws {
                h.load_workspace(ws);
            }
            if open {
                h.opened(em);
            }
        }
        self.views = [None, None];
    }

    /// Add both windows' watches and settings to a workspace.
    pub(crate) fn store_workspace(&self, ws: &mut luna_api::Workspace) {
        self.snes.store_workspace(ws);
        self.spc.store_workspace(ws);
    }

    /// The window `cpu` asked for a step.
    pub(crate) const fn stepping(&mut self, cpu: ListingCpu) {
        self.step_owner = cpu;
    }

    /// A CPU stopped (`hit`: on a breakpoint or a step of its; `None`: a
    /// step served or a pause). The window of the CPU that stopped takes
    /// the reason, the other shows where its CPU stands. Returns what only
    /// the app can do, and the window to bring forward.
    pub(crate) fn stopped(
        &mut self,
        em: &mut Emulator,
        hit: Option<&BreakHit>,
    ) -> (Vec<HostEffect>, ListingCpu) {
        let owner = match hit.map(|h| h.cpu) {
            Some(BreakCpu::Spc) => ListingCpu::Spc,
            Some(_) => ListingCpu::Snes,
            None => self.step_owner,
        };
        let mut fx = self.host(owner).stopped(em, hit);
        let other = match owner {
            ListingCpu::Snes => ListingCpu::Spc,
            ListingCpu::Spc => ListingCpu::Snes,
        };
        // A temporary breakpoint or break source the other window set ends
        // at any stop.
        fx.extend(self.host(other).stopped(em, None));
        self.host(other).break_reason = None;
        (fx, owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32 KB `LoROM`: `code` at `$8000`, the reset vector on it.
    fn rom(code: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 0x8000];
        r[..code.len()].copy_from_slice(code);
        r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA DEBUGGER HOST   ");
        r[0x7FD5] = 0x20;
        r[0x7FD7] = 0x08;
        r[0x7FFC] = 0x00;
        r[0x7FFD] = 0x80;
        r
    }

    fn loaded() -> (Emulator, DebuggerHost) {
        // SEI; CLC; XCE; REP #$30; JSR $8010; BRA $8008 … $8010: LDA $10; RTS
        let mut code = vec![0x78, 0x18, 0xFB, 0xC2, 0x30, 0x20, 0x10, 0x80, 0x80, 0xFE];
        code.resize(0x10, 0);
        code.extend([0xA5, 0x10, 0x60]);
        let mut em = Emulator::new();
        em.load_rom_bytes_forced(rom(&code), luna_api::MapperKind::LoRom)
            .unwrap();
        let mut host = DebuggerHost::default();
        host.opened(&mut em);
        host.apply(&mut em, DebuggerAction::VisibleRows(20));
        em.step(30).unwrap();
        (em, host)
    }

    #[test]
    fn the_view_centres_on_the_pc_and_marks_it() {
        let (mut em, mut host) = loaded();
        let fx = host.stopped(&mut em, None);
        assert!(fx.is_empty());
        let v = host.view(&em, true);
        let pc = v
            .rows
            .iter()
            .find(|r| r.is_pc)
            .expect("the PC's row is shown");
        assert_eq!(pc.pos.addr, 0x00_8008);
        assert_eq!(v.cursor, Some(pc.pos), "the PC's row is selected");
        assert_eq!(v.break_reason.as_deref(), Some("CPU Step"));
        let i = v.rows.iter().position(|r| r.is_pc).unwrap();
        assert!((5..15).contains(&i), "centred: row {i} of 20");
    }

    #[test]
    fn breakpoints_watches_and_go_to_go_through_the_api() {
        let (mut em, mut host) = loaded();
        let fx = host.apply(&mut em, DebuggerAction::ToggleBreakpoint(0x00_8010));
        assert_eq!(fx, [HostEffect::SyncBreakpoints]);
        assert_eq!(em.bp_list().unwrap().len(), 1);
        host.apply(&mut em, DebuggerAction::ToggleBreakpoint(0x00_8010));
        assert!(
            em.bp_list().unwrap().is_empty(),
            "a second toggle removes it"
        );

        em.write_memory(MemoryKind::SnesWorkRam, 0x10, &[0x42])
            .unwrap();
        host.apply(&mut em, DebuggerAction::AddWatch("[$7E0010]".into()));
        let v = host.view(&em, true);
        assert_eq!(
            (v.watches[0].value.as_str(), v.watches[0].changed),
            ("$42", false)
        );
        em.write_memory(MemoryKind::SnesWorkRam, 0x10, &[0x43])
            .unwrap();
        let _ = host.stopped(&mut em, None);
        let v = host.view(&em, true);
        assert_eq!(
            (v.watches[0].value.as_str(), v.watches[0].changed),
            ("$43", true)
        );

        em.set_label(luna_api::Label {
            memory: MemoryKind::SnesPrgRom,
            address: 0x10,
            length: 1,
            name: "Sub".into(),
            comment: String::new(),
        })
        .unwrap();
        host.apply(&mut em, DebuggerAction::GotoText("Sub".into()));
        let v = host.view(&em, true);
        let at = v.rows.iter().find(|r| Some(r.pos) == v.cursor).unwrap();
        assert_eq!(at.pos.addr, 0x00_8010);
        assert!(v.can_back, "the jump is in the history");

        // The CPU loops on the BRA at $8008: run to it.
        let fx = host.apply(&mut em, DebuggerAction::RunTo(0x00_8008));
        assert_eq!(fx, [HostEffect::SyncBreakpoints, HostEffect::Continue]);
        let hit = em.run_until_break(1000).unwrap().hit.expect("reaches it");
        let fx = host.stopped(&mut em, Some(&hit));
        assert_eq!(fx, [HostEffect::SyncBreakpoints]);
        assert!(
            em.bp_list().unwrap().is_empty(),
            "the run-to breakpoint goes"
        );
        let v = host.view(&em, true);
        assert_eq!(
            v.break_reason.as_deref(),
            Some("Breakpoint: Exec CPU ($008008)")
        );
    }

    #[test]
    fn watches_and_settings_ride_in_the_workspace() {
        let (mut em, mut host) = loaded();
        host.apply(&mut em, DebuggerAction::AddWatch("[$7E0010], H2".into()));
        let mut s = host.settings;
        s.byte_code = true;
        s.listing.lower_case = true;
        host.apply(&mut em, DebuggerAction::Settings(s));
        let mut ws = em.workspace().unwrap();
        host.store_workspace(&mut ws);
        let text = serde_json::to_string(&ws).unwrap();

        let mut fresh = DebuggerHost::default();
        fresh.rom_changed();
        fresh.load_workspace(&serde_json::from_str(&text).unwrap());
        assert_eq!(fresh.watches, ["[$7E0010], H2"]);
        assert_eq!(fresh.settings, s);
        fresh.rom_changed();
        assert!(fresh.watches.is_empty(), "another ROM starts its own");
    }

    #[test]
    fn the_spc700_window_lists_steps_and_breaks_on_its_cpu() {
        let (mut em, _) = loaded();
        let mut d = Debuggers::default();
        let opened = d.sync_open(Some(&mut em), [false, true]);
        assert_eq!(opened, [ListingCpu::Spc]);
        d.spc.apply(&mut em, DebuggerAction::VisibleRows(20));
        let hit = em
            .step_spc(SpcStepKind::Into, 100_000)
            .unwrap()
            .hit
            .expect("the SPC700 steps");
        let (fx, owner) = d.stopped(&mut em, Some(&hit));
        assert!(fx.is_empty());
        assert_eq!(owner, ListingCpu::Spc);
        let v = d.spc.view(&em, true);
        assert_eq!(v.cpu, ListingCpu::Spc);
        let pc = v
            .rows
            .iter()
            .find(|r| r.is_pc)
            .expect("the SPC700's PC row");
        assert_eq!(pc.pos.addr, hit.pc);
        assert!(pc.pos.addr <= 0xFFFF);
        assert_eq!(v.break_reason.as_deref(), Some("CPU Step"));
        assert!(v.spc_status.is_some() && v.status.is_some());
        assert!(v.vectors.is_empty() && v.functions.is_empty());

        // Its breakpoints are the SPC700's.
        let fx = d
            .spc
            .apply(&mut em, DebuggerAction::ToggleBreakpoint(0xFFC5));
        assert_eq!(fx, [HostEffect::SyncBreakpoints]);
        let bp = &em.bp_list().unwrap()[0];
        assert_eq!((bp.cpu, bp.lo, bp.exec), (BreakCpu::Spc, 0xFFC5, true));
        // Into, over and out are the SPC700's steps.
        assert_eq!(
            d.spc.apply(&mut em, DebuggerAction::Step(StepKind::Over)),
            [HostEffect::SpcStep(SpcStepKind::Over)]
        );
        assert_eq!(
            d.spc.apply(&mut em, DebuggerAction::Step(StepKind::Frame)),
            [HostEffect::Step(StepKind::Frame)],
            "a frame runs the console"
        );
    }
}
