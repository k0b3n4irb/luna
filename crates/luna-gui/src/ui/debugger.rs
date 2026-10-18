//! The Debugger window (65C816), laid out as Mesen2's: menus and a
//! toolbar, the code listing on the left, the Status pane above a tab
//! strip (Labels, Functions, Find Results, Controllers) on the right,
//! Watch, Breakpoints and Call Stack along the bottom, a status bar, and
//! the settings panel at the right edge.
//!
//! Everything shown comes from `luna-api` through [`DebuggerView`],
//! built by `LunaApp` each frame; everything done goes back as
//! [`DebuggerAction`]s, applied under the emulator lock. The panel only
//! keeps what a widget needs between two frames (text being typed, the
//! open dialog) in egui's memory.

// Screen coordinates: `a + b * c` reads as the layout it places, and
// `mul_add` would gain nothing a pixel can show.
#![allow(clippy::suboptimal_flops)]

use luna_api::{
    BlockView, BreakpointInfo, CallStackRow, DebuggerStatus, FindResult, FunctionRow, GotoResult,
    Label, LabelRow, ListingOptions, ListingPos, ListingRow, MemoryKind, RowCode, RowKind,
    StepKind, WatchStyle,
};

/// How the address margin reads (Mesen2's "Margin:").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum MarginMode {
    /// `008000`.
    #[default]
    Cpu,
    /// The offset in the memory behind it.
    Rom,
    /// `008000 [000000]`.
    Both,
    /// `008000 [0]`: the offset in 4 KB units.
    Compact,
}

/// The window's settings (Mesen2's settings panel), kept in the ROM's
/// workspace.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct DebuggerSettings {
    /// The listing options.
    pub listing: ListingOptions,
    /// The address margin.
    pub margin: MarginMode,
    /// Show the byte-code column.
    pub byte_code: bool,
    /// Re-centre on the PC at every break.
    pub keep_centered: bool,
    /// Bring this window forward when the 65C816 breaks.
    pub bring_to_front: bool,
    /// Break when the window opens.
    pub break_on_open: bool,
    /// Refresh the Status pane and the watches while running.
    pub refresh_running: bool,
    /// Show the settings panel.
    pub show_settings: bool,
}

impl Default for DebuggerSettings {
    fn default() -> Self {
        Self {
            listing: ListingOptions::default(),
            margin: MarginMode::Cpu,
            byte_code: false,
            keep_centered: false,
            bring_to_front: true,
            break_on_open: true,
            refresh_running: false,
            show_settings: true,
        }
    }
}

/// One watch row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WatchLine {
    /// What the user typed.
    pub entry: String,
    /// Its value now.
    pub value: String,
    /// It differs from the previous evaluation.
    pub changed: bool,
}

/// What the Debugger window shows, built by `LunaApp` each frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct DebuggerView {
    /// The emulation is paused.
    pub paused: bool,
    /// The listing rows from the top of the view.
    pub rows: Vec<ListingRow>,
    /// The selected row.
    pub cursor: Option<ListingPos>,
    /// The top row's position in the whole space, 0..1, for the scrollbar.
    pub scroll: f32,
    /// The PC's position in the space, 0..1.
    pub pc_mark: Option<f32>,
    /// Breakpoints' positions in the space, 0..1, enabled or not.
    pub bp_marks: Vec<(f32, bool)>,
    /// Settings.
    pub settings: DebuggerSettings,
    /// The Status pane.
    pub status: Option<DebuggerStatus>,
    /// The watch list.
    pub watches: Vec<WatchLine>,
    /// Its default style.
    pub watch_style: WatchStyle,
    /// The breakpoint list, with the label at each start.
    pub breakpoints: Vec<(BreakpointInfo, Option<String>)>,
    /// The call stack.
    pub call_stack: Vec<CallStackRow>,
    /// The label list.
    pub labels: Vec<LabelRow>,
    /// The function list.
    pub functions: Vec<FunctionRow>,
    /// Find All Occurrences' results.
    pub find_results: Vec<FindResult>,
    /// Go To All's results for the query in its dialog.
    pub goto_all: Vec<GotoResult>,
    /// Why the CPU stopped.
    pub break_reason: Option<String>,
    /// Master clocks since that stop.
    pub elapsed: Option<u64>,
    /// Navigation history.
    pub can_back: bool,
    /// Navigation history, forward.
    pub can_forward: bool,
    /// The last Find missed.
    pub find_miss: bool,
    /// Buttons forced pressed on ports 1 and 2.
    pub pad_override: [u16; 2],
    /// The break sources (`brk`, `cop`, `wdm`, `stp`) the settings set.
    pub break_on: [bool; 4],
    /// The CPU's vectors: (name, vector address, target).
    pub vectors: Vec<(&'static str, u32, u16)>,
}

/// What the Debugger window asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DebuggerAction {
    /// Resume.
    Continue,
    /// Pause (ends a long step too).
    Break,
    /// Pause if running, resume if paused (Escape).
    ToggleBreak,
    /// One of Mesen2's steps.
    Step(StepKind),
    /// Run until the next NMI / IRQ handler.
    RunToNmi,
    /// Run until the next IRQ handler.
    RunToIrq,
    /// Run until this scanline.
    RunToScanline(u16),
    /// Scroll by rows (negative: up).
    Scroll(i64),
    /// Jump the view to this point of the space (0..1).
    ScrollTo(f32),
    /// Show this address (centred) and select its row, recording the jump.
    Goto(u32),
    /// Go to the PC.
    GotoPc,
    /// Select a row.
    Select(ListingPos),
    /// Rows the view shows.
    VisibleRows(usize),
    /// Toggle an exec breakpoint on this CPU address.
    ToggleBreakpoint(u32),
    /// Toggle a forbid breakpoint on this CPU address.
    ToggleForbid(u32),
    /// Run to this address (a temporary breakpoint).
    RunTo(u32),
    /// Set the PC.
    MovePc(u32),
    /// Navigation history.
    Back,
    /// Navigation history.
    Forward,
    /// Add a watch entry.
    AddWatch(String),
    /// Replace watch `i` (empty removes it).
    EditWatch(usize, String),
    /// Move watch `i` up (`true`) or down.
    MoveWatch(usize, bool),
    /// The watches' default style.
    WatchStyle(WatchStyle),
    /// Add or replace a label (an empty name and comment removes the one
    /// on that byte).
    SetLabel(Label),
    /// Remove the label on a byte.
    RemoveLabel(MemoryKind, u32),
    /// Enable / disable a breakpoint.
    BpEnabled(u32, bool),
    /// Mark / unmark a breakpoint.
    BpMark(u32, bool),
    /// Remove a breakpoint.
    BpRemove(u32),
    /// Add (replacing `replace`) a breakpoint.
    BpSave {
        /// The breakpoint edited, if any.
        replace: Option<u32>,
        /// Its memory.
        memory: MemoryKind,
        /// Range start.
        lo: u32,
        /// Range end.
        hi: u32,
        /// Read, write, execute.
        rwx: [bool; 3],
        /// Condition.
        condition: String,
        /// Mark instead of break.
        mark: bool,
        /// Enabled.
        enabled: bool,
        /// A forbid range.
        forbid: bool,
    },
    /// Write a register (`a`, `x`, `y`, `pc`, `pb`, `d`, `db`, `sp`, `p`, `e`).
    SetRegister(&'static str, u32),
    /// New settings.
    Settings(DebuggerSettings),
    /// The break sources BRK, COP, WDM, STP.
    BreakOn([bool; 4]),
    /// Find from the cursor.
    Find(String, bool),
    /// Find All Occurrences.
    FindAll(String, bool, bool),
    /// Go To All's query.
    GotoAllQuery(String),
    /// Go To: an address, a label or an expression to resolve.
    GotoText(String),
    /// A controller button forced pressed or released.
    PadOverride(usize, u16),
    /// File ▸ Workspace.
    ImportLabels,
    /// File ▸ Workspace.
    ExportLabels,
    /// File ▸ Workspace.
    ImportWatches,
    /// File ▸ Workspace.
    ExportWatches,
    /// Empty the watches, the breakpoints, the labels back to defaults.
    ResetWorkspace,
    /// Debug ▸ Reset.
    Reset,
    /// Debug ▸ Reload ROM.
    Reload,
}

// ---------------------------------------------------------------------------
// Panel-local state (egui memory)
// ---------------------------------------------------------------------------

/// A dialog of the window.
#[derive(Clone, Debug, Default, PartialEq)]
enum Dialog {
    #[default]
    None,
    GotoAddress,
    GotoAll,
    BreakIn,
    RunToScanline,
    FindAll,
    EditLabel {
        memory: MemoryKind,
        address: String,
        name: String,
        comment: String,
        length: String,
        existing: bool,
    },
    EditComment {
        memory: MemoryKind,
        address: u32,
        text: String,
    },
    EditBreakpoint {
        replace: Option<u32>,
        memory: MemoryKind,
        lo: String,
        hi: String,
        rwx: [bool; 3],
        condition: String,
        mark: bool,
        enabled: bool,
        forbid: bool,
    },
}

/// Which tab of the right-hand strip is shown.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Labels,
    Functions,
    FindResults,
    Controllers,
}

/// What a widget keeps between frames.
#[derive(Clone, Debug, Default)]
struct UiState {
    dialog: Dialog,
    tab: Tab,
    goto_text: String,
    goto_all_text: String,
    break_in: String,
    break_in_unit: usize,
    scanline: String,
    find_open: bool,
    find_text: String,
    find_all_text: String,
    find_case: bool,
    find_word: bool,
    new_watch: String,
    watch_edit: Option<(usize, String)>,
    regs: Vec<(&'static str, String)>,
    /// Rows the wheel moved but not yet a whole row.
    wheel: f32,
}

fn state_id() -> egui::Id {
    egui::Id::new("luna_debugger_state")
}

// ---------------------------------------------------------------------------
// Colours (Mesen2's roles, toned for the dark theme)
// ---------------------------------------------------------------------------

const C_OPCODE: egui::Color32 = egui::Color32::from_rgb(220, 226, 236);
const C_LABEL: egui::Color32 = egui::Color32::from_rgb(110, 160, 255);
const C_IMMEDIATE: egui::Color32 = egui::Color32::from_rgb(222, 150, 90);
const C_ADDRESS: egui::Color32 = egui::Color32::from_rgb(232, 110, 110);
const C_COMMENT: egui::Color32 = egui::Color32::from_rgb(110, 200, 120);
const C_EFFECTIVE: egui::Color32 = egui::Color32::from_rgb(120, 170, 210);
const C_VALUE: egui::Color32 = egui::Color32::from_rgb(150, 150, 160);
const C_MARGIN: egui::Color32 = egui::Color32::from_rgb(130, 130, 145);
const C_PC: egui::Color32 = egui::Color32::from_rgb(255, 230, 70);
const C_BP_EXEC: egui::Color32 = egui::Color32::from_rgb(190, 60, 60);
const C_BP_WRITE: egui::Color32 = egui::Color32::from_rgb(60, 160, 100);
const C_BP_READ: egui::Color32 = egui::Color32::from_rgb(80, 90, 230);
const C_BP_FORBID: egui::Color32 = egui::Color32::from_rgb(130, 130, 130);

const fn bp_colour(kind: &str) -> egui::Color32 {
    match kind.as_bytes() {
        [b'e', ..] => C_BP_EXEC,
        [b'w', ..] => C_BP_WRITE,
        [b'r', ..] => C_BP_READ,
        _ => C_BP_FORBID,
    }
}

const fn row_background(code: RowCode) -> Option<egui::Color32> {
    match code {
        RowCode::Executed | RowCode::Unmapped => None,
        RowCode::Predecoded => Some(egui::Color32::from_rgb(30, 52, 36)),
        RowCode::Data => Some(egui::Color32::from_rgb(54, 52, 32)),
        RowCode::Unidentified => Some(egui::Color32::from_rgb(56, 34, 36)),
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// Body of the Debugger window. Returns what the user did this frame.
pub(crate) fn debugger_body(ui: &mut egui::Ui, view: &DebuggerView) -> Vec<DebuggerAction> {
    let mut out = Vec::new();
    let mut st: UiState = ui
        .ctx()
        .data_mut(|d| d.get_temp(state_id()).unwrap_or_default());
    shortcuts(ui, view, &mut st, &mut out);

    egui::Panel::top("dbg_menu").show_inside(ui, |ui| {
        menu_bar(ui, view, &mut st, &mut out);
        toolbar(ui, view, &mut st, &mut out);
    });
    egui::Panel::bottom("dbg_status_bar").show_inside(ui, |ui| status_bar(ui, view));
    if view.settings.show_settings {
        egui::Panel::right("dbg_settings")
            .resizable(true)
            .default_size(190.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| settings_panel(ui, view, &mut out));
            });
    }
    egui::Panel::bottom("dbg_bottom")
        .resizable(true)
        .default_size(170.0)
        .show_inside(ui, |ui| {
            ui.columns(3, |cols| {
                watch_pane(&mut cols[0], view, &mut st, &mut out);
                breakpoints_pane(&mut cols[1], view, &mut st, &mut out);
                call_stack_pane(&mut cols[2], view, &mut out);
            });
        });
    egui::Panel::right("dbg_right")
        .resizable(true)
        .default_size(330.0)
        .show_inside(ui, |ui| {
            egui::Panel::top("dbg_status")
                .resizable(true)
                .default_size(250.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("dbg_status_scroll")
                        .show(ui, |ui| status_pane(ui, view, &mut st, &mut out));
                });
            egui::CentralPanel::default().show_inside(ui, |ui| {
                tabs(ui, view, &mut st, &mut out);
            });
        });
    egui::CentralPanel::default().show_inside(ui, |ui| {
        code_view(ui, view, &mut st, &mut out);
    });
    dialogs(ui, view, &mut st, &mut out);
    ui.ctx().data_mut(|d| d.insert_temp(state_id(), st));
    out
}

fn shortcuts(ui: &egui::Ui, view: &DebuggerView, st: &mut UiState, out: &mut Vec<DebuggerAction>) {
    use egui::{Key, Modifiers};
    if ui.ctx().egui_wants_keyboard_input() && st.dialog != Dialog::None {
        return;
    }
    let typing = ui.ctx().egui_wants_keyboard_input();
    let sel = view.cursor.map(|c| c.addr);
    ui.input_mut(|i| {
        let mut take = |m: Modifiers, k: Key| i.consume_key(m, k);
        if take(Modifiers::NONE, Key::F5) {
            out.push(DebuggerAction::Continue);
        }
        if take(Modifiers::SHIFT, Key::F5) {
            out.push(DebuggerAction::Break);
        }
        if take(Modifiers::NONE, Key::F11) {
            out.push(DebuggerAction::Step(StepKind::Into));
        }
        if take(Modifiers::NONE, Key::F10) {
            out.push(DebuggerAction::Step(StepKind::Over));
        }
        if take(Modifiers::SHIFT, Key::F11) {
            out.push(DebuggerAction::Step(StepKind::Out));
        }
        if take(Modifiers::NONE, Key::F7) {
            out.push(DebuggerAction::Step(StepKind::Scanline));
        }
        if take(Modifiers::NONE, Key::F8) {
            out.push(DebuggerAction::Step(StepKind::Frame));
        }
        if take(Modifiers::ALT, Key::B) {
            st.dialog = Dialog::RunToScanline;
        }
        if take(Modifiers::COMMAND, Key::B) {
            st.dialog = Dialog::BreakIn;
        }
        if take(Modifiers::COMMAND, Key::G) {
            st.dialog = Dialog::GotoAddress;
        }
        if take(Modifiers::COMMAND, Key::Comma) {
            st.dialog = Dialog::GotoAll;
        }
        if take(Modifiers::COMMAND | Modifiers::SHIFT, Key::F) {
            st.dialog = Dialog::FindAll;
        } else if take(Modifiers::COMMAND, Key::F) {
            st.find_open = true;
        }
        if take(Modifiers::NONE, Key::F3) {
            out.push(DebuggerAction::Find(st.find_text.clone(), true));
        }
        if take(Modifiers::SHIFT, Key::F3) {
            out.push(DebuggerAction::Find(st.find_text.clone(), false));
        }
        if take(Modifiers::COMMAND, Key::Minus) {
            out.push(DebuggerAction::Back);
        }
        if take(Modifiers::COMMAND | Modifiers::SHIFT, Key::Minus) {
            out.push(DebuggerAction::Forward);
        }
        if take(Modifiers::COMMAND, Key::Q) {
            let mut s = view.settings;
            s.show_settings = !s.show_settings;
            out.push(DebuggerAction::Settings(s));
        }
        if !typing {
            if take(Modifiers::NONE, Key::Escape) {
                if st.find_open {
                    st.find_open = false;
                } else {
                    out.push(DebuggerAction::ToggleBreak);
                }
            }
            if let Some(a) = sel {
                if take(Modifiers::NONE, Key::F9) {
                    out.push(DebuggerAction::ToggleBreakpoint(a));
                }
                if take(Modifiers::COMMAND, Key::F11) {
                    out.push(DebuggerAction::RunTo(a));
                }
                if take(Modifiers::COMMAND | Modifiers::SHIFT, Key::F10) {
                    out.push(DebuggerAction::MovePc(a));
                }
            }
        }
    });
    if !typing
        && let Some(r) = view
            .cursor
            .and_then(|c| view.rows.iter().find(|r| r.pos == c))
    {
        let (f2, semicolon) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::F2),
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(t) if t == ";")),
            )
        });
        if f2 {
            st.dialog = label_dialog_for(r, view);
        }
        if semicolon && let Some(loc) = r.absolute {
            let text = view
                .labels
                .iter()
                .find(|l| l.label.memory == loc.kind && l.label.address == loc.offset)
                .map(|l| l.label.comment.clone())
                .unwrap_or_default();
            st.dialog = Dialog::EditComment {
                memory: loc.kind,
                address: loc.offset,
                text,
            };
        }
    }
}

fn label_dialog_for(r: &ListingRow, view: &DebuggerView) -> Dialog {
    let (memory, address) = r
        .absolute
        .map_or((MemoryKind::SnesMemory, r.pos.addr), |l| (l.kind, l.offset));
    let existing = view.labels.iter().find(|l| {
        l.label.memory == memory
            && (l.label.address..l.label.address + l.label.length).contains(&address)
    });
    match existing {
        Some(l) => Dialog::EditLabel {
            memory: l.label.memory,
            address: format!("{:X}", l.label.address),
            name: l.label.name.clone(),
            comment: l.label.comment.clone(),
            length: l.label.length.to_string(),
            existing: true,
        },
        None => Dialog::EditLabel {
            memory,
            address: format!("{address:X}"),
            name: String::new(),
            comment: String::new(),
            length: "1".into(),
            existing: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Menus and toolbar
// ---------------------------------------------------------------------------

fn menu_bar(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            ui.add_enabled(false, egui::Button::new("Code/Data Logger"))
                .on_disabled_hover_text("Comes with the code/data logger.");
            ui.separator();
            ui.menu_button("Workspace", |ui| {
                for (label, act) in [
                    ("Import labels…", DebuggerAction::ImportLabels),
                    ("Export labels…", DebuggerAction::ExportLabels),
                ] {
                    if ui.button(label).clicked() {
                        out.push(act);
                        ui.close();
                    }
                }
                ui.separator();
                for (label, act) in [
                    ("Import watch entries…", DebuggerAction::ImportWatches),
                    ("Export watch entries…", DebuggerAction::ExportWatches),
                ] {
                    if ui.button(label).clicked() {
                        out.push(act);
                        ui.close();
                    }
                }
                ui.separator();
                if ui
                    .button("Reset workspace")
                    .on_hover_text("Empty the watch list, remove every breakpoint and reset the labels to their defaults.")
                    .clicked()
                {
                    out.push(DebuggerAction::ResetWorkspace);
                    ui.close();
                }
            });
        });
        ui.menu_button("Debug", |ui| {
            debug_items(ui, view, st, out);
            ui.separator();
            if ui.add(egui::Button::new("Reset").shortcut_text("Ctrl+R")).clicked() {
                out.push(DebuggerAction::Reset);
                ui.close();
            }
            if ui.add(egui::Button::new("Reload ROM").shortcut_text("Ctrl+Shift+R")).clicked() {
                out.push(DebuggerAction::Reload);
                ui.close();
            }
        });
        ui.menu_button("Search", |ui| {
            ui.menu_button("Go to…", |ui| goto_items(ui, view, st, out));
            if ui.add(egui::Button::new("Go to all…").shortcut_text("Ctrl+,")).clicked() {
                st.dialog = Dialog::GotoAll;
                ui.close();
            }
            ui.separator();
            if ui.add(egui::Button::new("Find…").shortcut_text("Ctrl+F")).clicked() {
                st.find_open = true;
                ui.close();
            }
            if ui.add(egui::Button::new("Find previous").shortcut_text("Shift+F3")).clicked() {
                out.push(DebuggerAction::Find(st.find_text.clone(), false));
                ui.close();
            }
            if ui.add(egui::Button::new("Find next").shortcut_text("F3")).clicked() {
                out.push(DebuggerAction::Find(st.find_text.clone(), true));
                ui.close();
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Find all occurrences").shortcut_text("Ctrl+Shift+F"))
                .clicked()
            {
                st.dialog = Dialog::FindAll;
                ui.close();
            }
        });
        ui.menu_button("Settings", |ui| {
            let mut s = view.settings;
            if ui
                .checkbox(&mut s.show_settings, "Show settings panel (Ctrl+Q)")
                .changed()
            {
                out.push(DebuggerAction::Settings(s));
            }
        });
    });
}

fn debug_items(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    let p = view.paused;
    let mut item =
        |ui: &mut egui::Ui, enabled: bool, label: &str, keys: &str, act: Option<DebuggerAction>| {
            let r = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys));
            if r.clicked() {
                if let Some(a) = act {
                    out.push(a);
                }
                ui.close();
                return true;
            }
            false
        };
    item(ui, p, "Continue", "F5", Some(DebuggerAction::Continue));
    item(ui, !p, "Break", "Shift+F5", Some(DebuggerAction::Break));
    ui.separator();
    item(
        ui,
        true,
        "Step into",
        "F11",
        Some(DebuggerAction::Step(StepKind::Into)),
    );
    item(
        ui,
        true,
        "Step over",
        "F10",
        Some(DebuggerAction::Step(StepKind::Over)),
    );
    item(
        ui,
        true,
        "Step out",
        "Shift+F11",
        Some(DebuggerAction::Step(StepKind::Out)),
    );
    ui.separator();
    for label in ["Step back", "Step back (1 scanline)", "Step back (1 frame)"] {
        ui.add_enabled(false, egui::Button::new(label))
            .on_disabled_hover_text("Comes with rewind.");
    }
    ui.separator();
    ui.add_enabled(false, egui::Button::new("Run one CPU Cycle"))
        .on_disabled_hover_text("luna runs instructions whole.");
    ui.add_enabled(false, egui::Button::new("Run one PPU cycle"))
        .on_disabled_hover_text("luna runs instructions whole.");
    item(
        ui,
        true,
        "Run one scanline",
        "F7",
        Some(DebuggerAction::Step(StepKind::Scanline)),
    );
    item(
        ui,
        true,
        "Run one frame",
        "F8",
        Some(DebuggerAction::Step(StepKind::Frame)),
    );
    ui.separator();
    item(ui, true, "Run to NMI", "", Some(DebuggerAction::RunToNmi));
    item(ui, true, "Run to IRQ", "", Some(DebuggerAction::RunToIrq));
    if item(ui, true, "Run to scanline…", "Alt+B", None) {
        st.dialog = Dialog::RunToScanline;
    }
    ui.separator();
    if item(ui, true, "Break in…", "Ctrl+B", None) {
        st.dialog = Dialog::BreakIn;
    }
}

fn goto_items(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    if ui
        .add(egui::Button::new("Go to address…").shortcut_text("Ctrl+G"))
        .clicked()
    {
        st.dialog = Dialog::GotoAddress;
        ui.close();
    }
    if ui
        .add(egui::Button::new("Program counter").shortcut_text("Alt+*"))
        .clicked()
    {
        out.push(DebuggerAction::GotoPc);
        ui.close();
    }
    ui.separator();
    for &(name, _vector, target) in &view.vectors {
        if ui.button(format!("{name} (${target:04X})")).clicked() {
            out.push(DebuggerAction::Goto(u32::from(target)));
            ui.close();
        }
    }
}

fn toolbar(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    ui.horizontal(|ui| {
        let p = view.paused;
        let mut b =
            |ui: &mut egui::Ui, enabled: bool, text: &str, tip: &str, act: DebuggerAction| {
                if ui
                    .add_enabled(enabled, egui::Button::new(text))
                    .on_hover_text(tip)
                    .clicked()
                {
                    out.push(act);
                }
            };
        b(ui, p, "\u{25B6}", "Continue (F5)", DebuggerAction::Continue);
        b(
            ui,
            !p,
            "\u{23F8}",
            "Break (Shift+F5)",
            DebuggerAction::Break,
        );
        ui.separator();
        b(
            ui,
            true,
            "Into",
            "Step into (F11)",
            DebuggerAction::Step(StepKind::Into),
        );
        b(
            ui,
            true,
            "Over",
            "Step over (F10)",
            DebuggerAction::Step(StepKind::Over),
        );
        b(
            ui,
            true,
            "Out",
            "Step out (Shift+F11)",
            DebuggerAction::Step(StepKind::Out),
        );
        ui.separator();
        b(
            ui,
            true,
            "Line",
            "Run one scanline (F7)",
            DebuggerAction::Step(StepKind::Scanline),
        );
        b(
            ui,
            true,
            "Frame",
            "Run one frame (F8)",
            DebuggerAction::Step(StepKind::Frame),
        );
        ui.separator();
        b(ui, true, "NMI", "Run to NMI", DebuggerAction::RunToNmi);
        b(ui, true, "IRQ", "Run to IRQ", DebuggerAction::RunToIrq);
        if ui
            .button("Line#")
            .on_hover_text("Run to scanline… (Alt+B)")
            .clicked()
        {
            st.dialog = Dialog::RunToScanline;
        }
        ui.separator();
        if ui
            .button("Break in…")
            .on_hover_text("Break in… (Ctrl+B)")
            .clicked()
        {
            st.dialog = Dialog::BreakIn;
        }
        ui.menu_button("Go to…", |ui| goto_items(ui, view, st, out));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("\u{2699}")
                .on_hover_text("Toggle settings panel (Ctrl+Q)")
                .clicked()
            {
                let mut s = view.settings;
                s.show_settings = !s.show_settings;
                out.push(DebuggerAction::Settings(s));
            }
        });
    });
}

fn status_bar(ui: &mut egui::Ui, view: &DebuggerView) {
    ui.columns(3, |c| {
        if let Some(r) = &view.break_reason {
            c[0].label(format!("\u{25CF} {r}"));
        }
        if let Some(n) = view.elapsed.filter(|&n| n > 0) {
            let text = if n <= 999_999 {
                format!("{n}")
            } else {
                let units = [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "K")];
                let (d, u) = units
                    .iter()
                    .find(|(d, _)| n as f64 >= *d)
                    .copied()
                    .unwrap_or((1.0, ""));
                format!("{:.2} {u}", n as f64 / d)
            };
            c[2].label(format!("\u{23F1} {text} master clocks elapsed"))
                .on_hover_text(format!("{n} master clocks since the CPU last stopped"));
        }
    });
}

fn settings_panel(ui: &mut egui::Ui, view: &DebuggerView, out: &mut Vec<DebuggerAction>) {
    let mut s = view.settings;
    ui.strong("Disassembly settings");
    let block = |ui: &mut egui::Ui, label: &str, v: &mut BlockView| {
        ui.horizontal(|ui| {
            ui.label(label);
            egui::ComboBox::from_id_salt(label)
                .selected_text(if *v == BlockView::Show {
                    "Show"
                } else {
                    "Hide"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(v, BlockView::Hide, "Hide");
                    ui.selectable_value(v, BlockView::Show, "Show");
                });
        });
    };
    block(ui, "Data:", &mut s.listing.data);
    block(ui, "Unidentified:", &mut s.listing.unidentified);
    ui.horizontal(|ui| {
        ui.label("Margin:");
        egui::ComboBox::from_id_salt("dbg_margin")
            .selected_text(match s.margin {
                MarginMode::Cpu => "CPU address",
                MarginMode::Rom => "ROM address",
                MarginMode::Both => "Both",
                MarginMode::Compact => "Both (compact)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.margin, MarginMode::Cpu, "CPU address");
                ui.selectable_value(&mut s.margin, MarginMode::Rom, "ROM address");
                ui.selectable_value(&mut s.margin, MarginMode::Both, "Both");
                ui.selectable_value(&mut s.margin, MarginMode::Compact, "Both (compact)");
            });
    });
    ui.checkbox(&mut s.byte_code, "Show byte code");
    ui.checkbox(&mut s.listing.memory_values, "Show memory values");
    ui.checkbox(&mut s.listing.jump_labels, "Show jump/sub labels");
    ui.checkbox(&mut s.listing.lower_case, "Use lower case");
    ui.separator();
    ui.strong("Break on…");
    let mut on = view.break_on;
    for (i, name) in ["BRK", "COP", "WDM", "STP"].iter().enumerate() {
        ui.checkbox(&mut on[i], *name);
    }
    if on != view.break_on {
        out.push(DebuggerAction::BreakOn(on));
    }
    ui.checkbox(&mut s.break_on_open, "Debugger opened");
    ui.separator();
    ui.strong("Misc. settings");
    ui.checkbox(&mut s.bring_to_front, "Bring to front on break");
    ui.checkbox(&mut s.keep_centered, "Keep active statement in the center");
    ui.checkbox(&mut s.refresh_running, "Refresh while running");
    if s != view.settings {
        out.push(DebuggerAction::Settings(s));
    }
}

// ---------------------------------------------------------------------------
// The code view
// ---------------------------------------------------------------------------

/// The address the margin prints for a row.
fn margin_text(r: &ListingRow, mode: MarginMode) -> String {
    let abs = r.absolute.map(|l| l.offset);
    match (mode, abs) {
        (MarginMode::Cpu, _) | (_, None) => format!("{:06X}", r.pos.addr),
        (MarginMode::Rom, Some(a)) => format!("{a:06X}"),
        (MarginMode::Both, Some(a)) => format!("{:06X} [{a:06X}]", r.pos.addr),
        (MarginMode::Compact, Some(a)) => format!("{:06X} [{:X}]", r.pos.addr, a >> 12),
    }
}

/// The instruction text, coloured by token (Mesen2's roles).
fn code_job(
    r: &ListingRow,
    font: &egui::FontId,
    ink: Option<egui::Color32>,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    let mut push = |s: &str, c: egui::Color32| {
        job.append(
            s,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: ink.unwrap_or(c),
                ..Default::default()
            },
        );
    };
    match r.kind {
        RowKind::Instruction => {
            let (mn, rest) = r.text.split_once(' ').unwrap_or((r.text.as_str(), ""));
            push(mn, C_OPCODE);
            if !rest.is_empty() {
                push(" ", C_OPCODE);
                let colour = if rest.starts_with('#') {
                    C_IMMEDIATE
                } else if rest.contains('$') {
                    C_ADDRESS
                } else {
                    C_LABEL
                };
                push(rest, colour);
            }
            if let Some(e) = &r.effective {
                push(" ", C_OPCODE);
                push(e, C_EFFECTIVE);
            }
            if let Some(v) = &r.value {
                push(" = ", C_VALUE);
                push(v, C_VALUE);
            }
            if let Some(c) = &r.comment {
                push(" ;", C_COMMENT);
                push(c, C_COMMENT);
            }
        }
        RowKind::Label => push(&r.text, C_LABEL),
        RowKind::Comment => push(&r.text, C_COMMENT),
        RowKind::BlockBody => push(&r.text, C_VALUE),
        _ => {}
    }
    job
}

/// The address a double-click on a row goes to: its effective address,
/// else a full target written in its text.
fn row_target(r: &ListingRow) -> Option<u32> {
    if let Some(e) = &r.effective
        && let Some(h) = e.strip_prefix("[$").and_then(|e| e.strip_suffix(']'))
    {
        return u32::from_str_radix(h, 16).ok();
    }
    let i = r.text.find('$')?;
    let hex: String = r.text[i + 1..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (hex.len() == 6)
        .then(|| u32::from_str_radix(&hex, 16).ok())
        .flatten()
}

fn code_view(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let full = ui.available_rect_before_wrap();
    let bar_w = 14.0;
    let area = egui::Rect::from_min_max(full.min, egui::pos2(full.max.x - bar_w, full.max.y));
    let bar = egui::Rect::from_min_max(egui::pos2(area.max.x, full.min.y), full.max);
    let visible = ((area.height() / row_h).floor() as usize).max(1);
    out.push(DebuggerAction::VisibleRows(visible));
    let resp = ui.allocate_rect(area, egui::Sense::click_and_drag());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, egui::Color32::from_rgb(18, 18, 24));
    let gutter_w = char_w * 2.5;
    let margin_w = char_w
        * match view.settings.margin {
            MarginMode::Cpu | MarginMode::Rom => 7.0,
            MarginMode::Compact => 11.0,
            MarginMode::Both => 16.0,
        };
    let bytes_w = if view.settings.byte_code {
        char_w * 13.0
    } else {
        0.0
    };
    let code_x = area.min.x + gutter_w + margin_w + bytes_w + char_w;
    painter.rect_filled(
        egui::Rect::from_min_size(area.min, egui::vec2(gutter_w, area.height())),
        0.0,
        egui::Color32::from_rgb(34, 34, 42),
    );
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(area.min.x + gutter_w, area.min.y),
            egui::vec2(margin_w, area.height()),
        ),
        0.0,
        egui::Color32::from_rgb(26, 26, 33),
    );
    let hover_row = resp
        .hover_pos()
        .map(|p| ((p.y - area.min.y) / row_h) as usize);
    let find_lower = st.find_text.to_lowercase();
    for (i, r) in view.rows.iter().take(visible).enumerate() {
        let y = area.min.y + i as f32 * row_h;
        let row_rect =
            egui::Rect::from_min_size(egui::pos2(area.min.x, y), egui::vec2(area.width(), row_h));
        let code_rect = egui::Rect::from_min_max(
            egui::pos2(code_x - 2.0, y),
            egui::pos2(area.max.x, y + row_h),
        );
        if let Some(bg) = row_background(r.code)
            .filter(|_| r.kind == RowKind::Instruction || r.kind == RowKind::BlockBody)
        {
            painter.rect_filled(code_rect, 0.0, bg);
        }
        if view.cursor == Some(r.pos) {
            painter.rect_filled(
                code_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(70, 110, 200, 70),
            );
            painter.rect_stroke(
                code_rect,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 140, 240)),
                egui::StrokeKind::Inside,
            );
        }
        match r.kind {
            RowKind::FunctionStart
            | RowKind::FunctionEnd
            | RowKind::BlockStart
            | RowKind::BlockEnd => {
                let mid = y + row_h / 2.0;
                painter.line_segment(
                    [egui::pos2(code_x, mid), egui::pos2(area.max.x - 4.0, mid)],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 92)),
                );
                if !r.text.is_empty() {
                    let galley = painter.layout_no_wrap(
                        r.text.clone(),
                        egui::FontId::monospace(font.size * 0.85),
                        egui::Color32::from_rgb(200, 200, 210),
                    );
                    let cx = f32::midpoint(code_x, area.max.x);
                    let b = egui::Rect::from_center_size(
                        egui::pos2(cx, mid),
                        galley.size() + egui::vec2(10.0, 2.0),
                    );
                    painter.rect_filled(b, 2.0, egui::Color32::from_rgb(40, 40, 50));
                    painter.rect_stroke(
                        b,
                        2.0,
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 110, 125)),
                        egui::StrokeKind::Inside,
                    );
                    painter.galley(
                        b.center() - galley.size() / 2.0,
                        galley,
                        egui::Color32::WHITE,
                    );
                }
                continue;
            }
            _ => {}
        }
        if r.kind == RowKind::Instruction || r.kind == RowKind::BlockBody {
            painter.text(
                egui::pos2(area.min.x + gutter_w + margin_w - char_w * 0.5, y + 1.0),
                egui::Align2::RIGHT_TOP,
                margin_text(r, view.settings.margin),
                font.clone(),
                C_MARGIN,
            );
            if view.settings.byte_code && !r.bytes.is_empty() {
                let mut b: Vec<String> =
                    r.bytes.iter().take(4).map(|b| format!("{b:02X}")).collect();
                if r.bytes.len() > 4 {
                    b.push("..".into());
                }
                painter.text(
                    egui::pos2(area.min.x + gutter_w + margin_w + char_w * 0.5, y + 1.0),
                    egui::Align2::LEFT_TOP,
                    b.join(" "),
                    font.clone(),
                    C_VALUE,
                );
            }
        }
        // The PC: a yellow box behind the instruction, an arrow in the gutter.
        let text_x = if r.kind == RowKind::Instruction {
            code_x + char_w * 2.0
        } else {
            code_x
        };
        let mut ink = None;
        let job = code_job(r, &font, None);
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let text_rect = egui::Rect::from_min_size(
            egui::pos2(text_x - 2.0, y),
            egui::vec2(galley.size().x + 4.0, row_h),
        );
        if let Some(b) = r.breakpoint {
            let c = bp_colour(b.kind);
            let centre = egui::pos2(area.min.x + gutter_w / 2.0, y + row_h / 2.0);
            let rad = row_h * 0.32;
            if b.enabled {
                painter.circle_filled(centre, rad, c);
                painter.rect_filled(text_rect, 2.0, c);
                ink = Some(egui::Color32::WHITE);
            } else {
                painter.circle_stroke(centre, rad, egui::Stroke::new(1.5, c));
                painter.rect_stroke(
                    text_rect,
                    2.0,
                    egui::Stroke::new(1.0, c),
                    egui::StrokeKind::Inside,
                );
            }
            if b.kind == "forbid" {
                painter.line_segment(
                    [
                        centre + egui::vec2(-rad, rad),
                        centre + egui::vec2(rad, -rad),
                    ],
                    egui::Stroke::new(1.5, egui::Color32::WHITE),
                );
            } else if b.conditional {
                painter.text(
                    centre,
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::monospace(row_h * 0.6),
                    egui::Color32::WHITE,
                );
            }
            if b.mark {
                painter.circle_filled(
                    centre + egui::vec2(rad, -rad),
                    rad * 0.4,
                    egui::Color32::from_rgb(135, 206, 250),
                );
            }
        } else if hover_row == Some(i) && r.kind == RowKind::Instruction {
            painter.circle_stroke(
                egui::pos2(area.min.x + gutter_w / 2.0, y + row_h / 2.0),
                row_h * 0.32,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 110, 120)),
            );
        }
        if r.is_pc && view.paused {
            painter.rect_filled(text_rect, 2.0, C_PC);
            ink = Some(egui::Color32::BLACK);
            let tip = egui::pos2(area.min.x + gutter_w - 2.0, y + row_h / 2.0);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    tip,
                    tip + egui::vec2(-7.0, -5.0),
                    tip + egui::vec2(-7.0, 5.0),
                ],
                C_PC,
                egui::Stroke::new(1.0, egui::Color32::BLACK),
            ));
        }
        if !find_lower.is_empty() && r.text.to_lowercase().contains(&find_lower) && ink.is_none() {
            painter.rect_filled(text_rect, 2.0, egui::Color32::from_rgb(100, 149, 237));
            ink = Some(egui::Color32::WHITE);
        }
        let galley = if ink.is_some() {
            ui.fonts_mut(|f| f.layout_job(code_job(r, &font, ink)))
        } else {
            galley
        };
        painter.galley(egui::pos2(text_x, y + 1.0), galley, egui::Color32::WHITE);
        let _ = row_rect;
    }

    // Input: clicks, wheel, keys.
    let row_at = |p: egui::Pos2| view.rows.get(((p.y - area.min.y) / row_h) as usize);
    if let Some(p) = resp.interact_pointer_pos()
        && let Some(r) = row_at(p)
    {
        let in_gutter = p.x < area.min.x + gutter_w;
        if resp.clicked() {
            if in_gutter && r.kind == RowKind::Instruction {
                out.push(DebuggerAction::ToggleBreakpoint(r.pos.addr));
            } else {
                out.push(DebuggerAction::Select(r.pos));
            }
        }
        if resp.middle_clicked() && in_gutter && r.kind == RowKind::Instruction {
            out.push(DebuggerAction::ToggleForbid(r.pos.addr));
        }
        if resp.double_clicked()
            && !in_gutter
            && let Some(t) = row_target(r)
        {
            out.push(DebuggerAction::Goto(t));
        }
        if resp.secondary_clicked() {
            out.push(DebuggerAction::Select(r.pos));
        }
    }
    let selected = view
        .cursor
        .and_then(|c| view.rows.iter().find(|r| r.pos == c));
    resp.context_menu(|ui| code_menu(ui, view, selected, st, out));
    if resp.hovered() {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        st.wheel -= wheel / row_h;
        let whole = st.wheel.trunc();
        if whole != 0.0 {
            out.push(DebuggerAction::Scroll(whole as i64));
            st.wheel -= whole;
        }
        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1)) {
            out.push(DebuggerAction::Back);
        }
        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2)) {
            out.push(DebuggerAction::Forward);
        }
    }
    if !ui.ctx().egui_wants_keyboard_input() {
        let idx = view
            .cursor
            .and_then(|c| view.rows.iter().position(|r| r.pos == c));
        let (up, down, pgup, pgdn) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
            )
        });
        let page = visible.saturating_sub(2).max(1) as i64;
        let mut moved = |delta: i64| match idx {
            Some(i) => {
                let j = i as i64 + delta;
                if j < 0 || j >= visible as i64 {
                    out.push(DebuggerAction::Scroll(delta));
                } else if let Some(r) = view.rows.get(j as usize) {
                    out.push(DebuggerAction::Select(r.pos));
                }
            }
            None => out.push(DebuggerAction::Scroll(delta)),
        };
        if up {
            moved(-1);
        }
        if down {
            moved(1);
        }
        if pgup {
            moved(-page);
        }
        if pgdn {
            moved(page);
        }
    }

    // The scrollbar: the whole space, the PC and the breakpoints marked.
    let bresp = ui.allocate_rect(bar, egui::Sense::click_and_drag());
    let bp = ui.painter_at(bar);
    bp.rect_filled(bar, 0.0, egui::Color32::from_rgb(28, 28, 36));
    for &(f, enabled) in &view.bp_marks {
        let r = egui::Rect::from_min_size(
            egui::pos2(bar.min.x + 1.0, bar.min.y + f * bar.height()),
            egui::vec2(4.0, 4.0),
        );
        if enabled {
            bp.rect_filled(r, 0.0, C_BP_EXEC);
        } else {
            bp.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(1.0, C_BP_EXEC),
                egui::StrokeKind::Inside,
            );
        }
    }
    if let Some(f) = view.pc_mark {
        let y = bar.min.y + f * bar.height();
        bp.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(bar.min.x, y - 1.5),
                egui::pos2(bar.max.x, y + 1.5),
            ),
            0.0,
            C_PC,
        );
    }
    let thumb_y = bar.min.y + view.scroll * bar.height();
    bp.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(bar.min.x + 2.0, thumb_y),
            egui::pos2(bar.max.x - 2.0, (thumb_y + 18.0).min(bar.max.y)),
        ),
        2.0,
        egui::Color32::from_rgba_unmultiplied(160, 180, 230, 90),
    );
    if bresp.hovered() {
        for (k, name) in ["00", "20", "40", "60", "80", "A0", "C0", "E0", "FF"]
            .iter()
            .enumerate()
        {
            bp.text(
                egui::pos2(bar.max.x - 1.0, bar.min.y + k as f32 / 8.0 * bar.height()),
                egui::Align2::RIGHT_TOP,
                *name,
                egui::FontId::monospace(8.0),
                egui::Color32::from_rgb(120, 120, 135),
            );
        }
    }
    if (bresp.clicked() || bresp.dragged())
        && let Some(p) = bresp.interact_pointer_pos()
    {
        out.push(DebuggerAction::ScrollTo(
            ((p.y - bar.min.y) / bar.height()).clamp(0.0, 1.0),
        ));
    }

    // Find (Ctrl+F): a box at the top right of the view.
    if st.find_open {
        let mut open = st.find_open;
        egui::Area::new(egui::Id::new("dbg_find"))
            .fixed_pos(egui::pos2(area.max.x - 300.0, area.min.y + 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Search");
                        let mut edit =
                            egui::TextEdit::singleline(&mut st.find_text).desired_width(140.0);
                        if view.find_miss {
                            edit = edit.text_color(egui::Color32::from_rgb(255, 120, 120));
                        }
                        let r = ui.add(edit);
                        r.request_focus();
                        if r.changed() {
                            out.push(DebuggerAction::Find(st.find_text.clone(), true));
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            out.push(DebuggerAction::Find(st.find_text.clone(), true));
                        }
                        if ui.small_button("\u{25B2}").clicked() {
                            out.push(DebuggerAction::Find(st.find_text.clone(), false));
                        }
                        if ui.small_button("\u{25BC}").clicked() {
                            out.push(DebuggerAction::Find(st.find_text.clone(), true));
                        }
                        if ui.small_button("\u{2715}").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            open = false;
                            st.find_text.clear();
                        }
                    });
                });
            });
        st.find_open = open;
    }
}

fn code_menu(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    sel: Option<&ListingRow>,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    let target = sel.map(|r| r.pos.addr);
    let hint = target.map_or_else(String::new, |a| format!(" (${a:06X})"));
    let item = |ui: &mut egui::Ui, enabled: bool, label: String, keys: &str| {
        let r = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys));
        let c = r.clicked();
        if c {
            ui.close();
        }
        c
    };
    ui.add_enabled(false, egui::Button::new("Mark as…"))
        .on_disabled_hover_text("Comes with the code/data logger.");
    ui.add_enabled(false, egui::Button::new("Edit Selected Code"))
        .on_disabled_hover_text("Comes with the assembler.");
    ui.separator();
    if let Some(r) = sel
        && item(ui, true, "Copy".into(), "Ctrl+C")
    {
        let mut line = format!("{}  {}", margin_text(r, view.settings.margin), r.text);
        if let Some(e) = &r.effective {
            line = format!("{line} {e}");
        }
        if let Some(v) = &r.value {
            line = format!("{line} = {v}");
        }
        if let Some(c) = &r.comment {
            line = format!("{line} ;{c}");
        }
        ui.ctx().copy_text(line);
    }
    ui.separator();
    if let Some(a) = target {
        if item(ui, true, format!("Toggle Breakpoint{hint}"), "F9") {
            out.push(DebuggerAction::ToggleBreakpoint(a));
        }
        let watch = sel
            .and_then(|r| r.effective.clone())
            .unwrap_or_else(|| format!("[${a:06X}]"));
        if item(ui, true, "Add to Watch".into(), "") {
            out.push(DebuggerAction::AddWatch(watch));
        }
        if item(ui, true, "Edit Label".into(), "F2")
            && let Some(r) = sel
        {
            st.dialog = label_dialog_for(r, view);
        }
        ui.add_enabled(
            false,
            egui::Button::new("View in Memory Viewer").shortcut_text("F1"),
        )
        .on_disabled_hover_text("Comes with the memory tools.");
        ui.separator();
        let term = sel
            .and_then(|r| r.effective.clone())
            .unwrap_or_else(|| format!("${a:06X}"));
        if item(ui, true, format!("Find all occurrences ({term})"), "") {
            out.push(DebuggerAction::FindAll(term, true, true));
            st.tab = Tab::FindResults;
        }
        ui.separator();
        if item(
            ui,
            view.paused,
            format!("Move Program Counter{hint}"),
            "Ctrl+Shift+F10",
        ) {
            out.push(DebuggerAction::MovePc(a));
        }
        if item(ui, true, format!("Run to Location{hint}"), "Ctrl+F11") {
            out.push(DebuggerAction::RunTo(a));
        }
        ui.separator();
        if let Some(t) = sel.and_then(row_target)
            && item(ui, true, format!("Go to Location (${t:06X})"), "")
        {
            out.push(DebuggerAction::Goto(t));
        }
        ui.separator();
    }
    if item(ui, view.can_back, "Navigate Backward".into(), "Ctrl+-") {
        out.push(DebuggerAction::Back);
    }
    if item(
        ui,
        view.can_forward,
        "Navigate Forward".into(),
        "Ctrl+Shift+-",
    ) {
        out.push(DebuggerAction::Forward);
    }
}

// ---------------------------------------------------------------------------
// The Status pane
// ---------------------------------------------------------------------------

fn status_pane(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    let Some(s) = &view.status else {
        ui.label("(no ROM loaded)");
        return;
    };
    let c = &s.cpu;
    let editable = view.paused;
    let pc = (u32::from(c.pb) << 16) | u32::from(c.pc);
    let fields: [(&'static str, &'static str, u32, usize); 7] = [
        ("A", "a", u32::from(c.a), 4),
        ("X", "x", u32::from(c.x), 4),
        ("Y", "y", u32::from(c.y), 4),
        ("PC", "pc24", pc, 6),
        ("D", "d", u32::from(c.dp), 4),
        ("DB", "db", u32::from(c.db), 2),
        ("SP", "sp", u32::from(c.sp), 4),
    ];
    if !editable || st.regs.is_empty() {
        st.regs = fields
            .iter()
            .map(|&(_, k, v, w)| (k, format!("{v:0w$X}")))
            .collect();
    }
    egui::Grid::new("dbg_regs")
        .num_columns(8)
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (i, &(label, key, _, width)) in fields.iter().enumerate() {
                ui.monospace(format!("{label}:"));
                let buf = &mut st.regs[i].1;
                let r = ui.add_enabled(
                    editable,
                    egui::TextEdit::singleline(buf)
                        .desired_width(8.0 * width as f32 + 6.0)
                        .font(egui::TextStyle::Monospace),
                );
                if r.lost_focus()
                    && let Ok(v) = u32::from_str_radix(buf.trim(), 16)
                {
                    if key == "pc24" {
                        out.push(DebuggerAction::SetRegister("pb", v >> 16));
                        out.push(DebuggerAction::SetRegister("pc", v & 0xFFFF));
                    } else {
                        let reg = match key {
                            "a" => "a",
                            "x" => "x",
                            "y" => "y",
                            "d" => "d",
                            "db" => "db",
                            _ => "sp",
                        };
                        out.push(DebuggerAction::SetRegister(reg, v));
                    }
                }
                if i == 3 {
                    ui.end_row();
                }
            }
            ui.end_row();
        });
    ui.horizontal(|ui| {
        ui.monospace(format!("Clock: {}", s.master_clocks));
    });
    ui.separator();
    let mut p = c.p;
    ui.horizontal(|ui| {
        ui.monospace(format!("P: {:02X}", c.p));
    });
    egui::Grid::new("dbg_flags").num_columns(4).show(ui, |ui| {
        for (k, &(bit, name)) in [
            (0x01, "Carry"),
            (0x10, "Index"),
            (0x02, "Zero"),
            (0x20, "Memory"),
            (0x04, "Interrupt"),
            (0x40, "Overflow"),
            (0x08, "Decimal"),
            (0x80, "Negative"),
        ]
        .iter()
        .enumerate()
        {
            let mut on = p & bit != 0;
            if ui
                .add_enabled(editable, egui::Checkbox::new(&mut on, name))
                .changed()
            {
                p = if on { p | bit } else { p & !bit };
            }
            if k % 4 == 3 {
                ui.end_row();
            }
        }
    });
    if p != c.p {
        out.push(DebuggerAction::SetRegister("p", u32::from(p)));
    }
    let mut e = c.e;
    if ui
        .add_enabled(editable, egui::Checkbox::new(&mut e, "Emulation"))
        .changed()
    {
        out.push(DebuggerAction::SetRegister("e", u32::from(e)));
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("IRQs:");
        let (mut n, mut h, mut x) = (s.nmi, s.hv_irq, s.coprocessor_irq);
        ui.add_enabled(false, egui::Checkbox::new(&mut n, "NMI"));
        ui.add_enabled(false, egui::Checkbox::new(&mut h, "H/V"));
        ui.add_enabled(false, egui::Checkbox::new(&mut x, "Coprocessor"));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Stack:");
        ui.monospace(
            s.stack
                .iter()
                .map(|b| format!("${b:02X}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    });
    ui.separator();
    egui::Grid::new("dbg_ppu").num_columns(4).show(ui, |ui| {
        ui.monospace(format!("H Clock: {}", s.h_clock));
        ui.monospace(format!("VRAM Addr.: {:04X}", s.vram_addr));
        ui.end_row();
        ui.monospace(format!("Cycle: {}", s.dot));
        ui.monospace(format!("OAM Addr.: {:03X}", s.oam_addr));
        ui.end_row();
        ui.monospace(format!("Scanline: {}", s.scanline));
        ui.monospace(format!("CGRAM Addr.: {:02X}", s.cgram_addr));
        ui.end_row();
    });
}

// ---------------------------------------------------------------------------
// Tabs: Labels, Functions, Find Results, Controllers
// ---------------------------------------------------------------------------

fn addr_cell(a: Option<u32>) -> egui::RichText {
    a.map_or_else(
        || egui::RichText::new("<unavailable>").italics().weak(),
        |a| egui::RichText::new(format!("${a:06X}")).monospace(),
    )
}

fn rom_cell(memory: MemoryKind, offset: u32) -> String {
    let short = match memory {
        MemoryKind::SnesPrgRom => "PRG",
        MemoryKind::SnesWorkRam => "WRAM",
        MemoryKind::SnesSaveRam => "SRAM",
        MemoryKind::SnesRegister => "REG",
        MemoryKind::SnesVideoRam => "VRAM",
        MemoryKind::SnesSpriteRam => "OAM",
        MemoryKind::SnesCgRam => "CGRAM",
        _ => "CPU",
    };
    format!("${offset:04X} [{short}]")
}

fn tabs(ui: &mut egui::Ui, view: &DebuggerView, st: &mut UiState, out: &mut Vec<DebuggerAction>) {
    ui.horizontal(|ui| {
        for (t, name) in [
            (Tab::Labels, "Labels"),
            (Tab::Functions, "Functions"),
            (Tab::FindResults, "Find Results"),
            (Tab::Controllers, "Controllers"),
        ] {
            ui.selectable_value(&mut st.tab, t, name);
        }
    });
    ui.separator();
    match st.tab {
        Tab::Labels => labels_tab(ui, view, st, out),
        Tab::Functions => {
            egui::ScrollArea::vertical()
                .id_salt("dbg_fn")
                .show(ui, |ui| {
                    egui::Grid::new("dbg_fn_grid")
                        .striped(true)
                        .num_columns(3)
                        .show(ui, |ui| {
                            ui.strong("Function");
                            ui.strong("CPU Addr");
                            ui.strong("ROM Addr");
                            ui.end_row();
                            for f in &view.functions {
                                let name = f.name.clone().unwrap_or_else(|| "<no label>".into());
                                if ui.selectable_label(false, name).double_clicked()
                                    && let Some(a) = f.address
                                {
                                    out.push(DebuggerAction::Goto(a));
                                }
                                ui.label(addr_cell(f.address));
                                ui.monospace(rom_cell(MemoryKind::SnesPrgRom, f.rom));
                                ui.end_row();
                            }
                        });
                    if view.functions.is_empty() {
                        ui.label(egui::RichText::new("(no subroutine entered yet)").weak());
                    }
                });
        }
        Tab::FindResults => {
            egui::ScrollArea::vertical()
                .id_salt("dbg_find_res")
                .show(ui, |ui| {
                    egui::Grid::new("dbg_find_grid")
                        .striped(true)
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.strong("Address");
                            ui.strong("Result");
                            ui.end_row();
                            for r in &view.find_results {
                                if ui
                                    .selectable_label(
                                        false,
                                        egui::RichText::new(format!("${:06X}", r.pos.addr))
                                            .monospace(),
                                    )
                                    .double_clicked()
                                {
                                    out.push(DebuggerAction::Goto(r.pos.addr));
                                }
                                ui.monospace(&r.text);
                                ui.end_row();
                            }
                        });
                });
        }
        Tab::Controllers => {
            for port in 0..2 {
                ui.label(format!("Port {}", port + 1));
                let mask = view.pad_override[port];
                ui.horizontal_wrapped(|ui| {
                    for (bit, name) in [
                        (0x0800u16, "Up"),
                        (0x0400, "Down"),
                        (0x0200, "Left"),
                        (0x0100, "Right"),
                        (0x2000, "Select"),
                        (0x1000, "Start"),
                        (0x4000, "Y"),
                        (0x0040, "X"),
                        (0x8000, "B"),
                        (0x0080, "A"),
                        (0x0020, "L"),
                        (0x0010, "R"),
                    ] {
                        if ui.selectable_label(mask & bit != 0, name).clicked() {
                            out.push(DebuggerAction::PadOverride(port, mask ^ bit));
                        }
                    }
                });
                ui.separator();
            }
        }
    }
}

fn labels_tab(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    egui::ScrollArea::vertical()
        .id_salt("dbg_labels")
        .show(ui, |ui| {
            egui::Grid::new("dbg_labels_grid")
                .striped(true)
                .num_columns(4)
                .show(ui, |ui| {
                    ui.strong("Label");
                    ui.strong("CPU Addr");
                    ui.strong("ROM Addr");
                    ui.strong("Comment");
                    ui.end_row();
                    for l in &view.labels {
                        let resp = ui.selectable_label(
                            false,
                            egui::RichText::new(&l.label.name).monospace(),
                        );
                        if resp.double_clicked() {
                            st.dialog = Dialog::EditLabel {
                                memory: l.label.memory,
                                address: format!("{:X}", l.label.address),
                                name: l.label.name.clone(),
                                comment: l.label.comment.clone(),
                                length: l.label.length.to_string(),
                                existing: true,
                            };
                        }
                        resp.context_menu(|ui| {
                            if ui
                                .add(egui::Button::new("Add…").shortcut_text("Ins"))
                                .clicked()
                            {
                                st.dialog = Dialog::EditLabel {
                                    memory: MemoryKind::SnesWorkRam,
                                    address: String::new(),
                                    name: String::new(),
                                    comment: String::new(),
                                    length: "1".into(),
                                    existing: false,
                                };
                                ui.close();
                            }
                            if ui
                                .add(egui::Button::new("Delete").shortcut_text("Del"))
                                .clicked()
                            {
                                out.push(DebuggerAction::RemoveLabel(
                                    l.label.memory,
                                    l.label.address,
                                ));
                                ui.close();
                            }
                            if let Some(a) = l.address {
                                if ui.button("Toggle Breakpoint").clicked() {
                                    out.push(DebuggerAction::ToggleBreakpoint(a));
                                    ui.close();
                                }
                                if ui.button("Go to Location").clicked() {
                                    out.push(DebuggerAction::Goto(a));
                                    ui.close();
                                }
                            }
                            if ui.button("Add to Watch").clicked() {
                                out.push(DebuggerAction::AddWatch(format!("[{}]", l.label.name)));
                                ui.close();
                            }
                            if ui.button("Find all occurrences").clicked() {
                                out.push(DebuggerAction::FindAll(l.label.name.clone(), true, true));
                                st.tab = Tab::FindResults;
                                ui.close();
                            }
                        });
                        ui.label(addr_cell(l.address));
                        ui.monospace(rom_cell(l.label.memory, l.label.address));
                        ui.label(l.label.comment.lines().next().unwrap_or(""));
                        ui.end_row();
                    }
                });
        });
}

// ---------------------------------------------------------------------------
// Watch, Breakpoints, Call Stack
// ---------------------------------------------------------------------------

fn watch_pane(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    ui.horizontal(|ui| {
        ui.strong("Watch");
        ui.label("?").on_hover_text(
            "Expressions in Mesen2's language: [addr] a byte, {addr} a word, #addr a long, \
             :addr the offset behind addr, labels, registers (a x y ps sp pc db d), \
             frame, scanline, hclock, cycle. Add \", H2\" / \", S\" / \", U\" / \", B\" \
             and a byte count 1-4 for a format; [addr, n] shows n bytes.",
        );
    });
    egui::ScrollArea::vertical()
        .id_salt("dbg_watch")
        .show(ui, |ui| {
            egui::Grid::new("dbg_watch_grid")
                .striped(true)
                .num_columns(2)
                .show(ui, |ui| {
                    ui.strong("Name");
                    ui.strong("Value");
                    ui.end_row();
                    for (i, w) in view.watches.iter().enumerate() {
                        let editing = st.watch_edit.as_ref().is_some_and(|(j, _)| *j == i);
                        if editing {
                            let (_, buf) = st.watch_edit.as_mut().expect("editing");
                            let r = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                            r.request_focus();
                            if r.lost_focus() {
                                let text = buf.clone();
                                st.watch_edit = None;
                                if !ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                                    out.push(DebuggerAction::EditWatch(i, text));
                                }
                            }
                        } else {
                            let r = ui
                                .selectable_label(false, egui::RichText::new(&w.entry).monospace());
                            if r.clicked() {
                                st.watch_edit = Some((i, w.entry.clone()));
                            }
                            r.context_menu(|ui| watch_menu(ui, view, i, out));
                        }
                        let colour = if w.changed {
                            egui::Color32::from_rgb(255, 110, 110)
                        } else {
                            egui::Color32::from_rgb(220, 220, 230)
                        };
                        ui.monospace(egui::RichText::new(&w.value).color(colour));
                        ui.end_row();
                    }
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.new_watch)
                            .hint_text("add an expression")
                            .desired_width(150.0),
                    );
                    if r.lost_focus() && !st.new_watch.trim().is_empty() {
                        out.push(DebuggerAction::AddWatch(std::mem::take(&mut st.new_watch)));
                    }
                    ui.end_row();
                });
        });
}

fn watch_menu(ui: &mut egui::Ui, view: &DebuggerView, i: usize, out: &mut Vec<DebuggerAction>) {
    let entry = view.watches[i].entry.clone();
    let base = entry
        .rsplit_once(',')
        .filter(|(_, s)| {
            let s = s.trim();
            s.starts_with(['H', 'S', 'U', 'B']) && s.len() <= 2
        })
        .map_or(entry.as_str(), |(e, _)| e.trim())
        .to_string();
    if ui
        .add(egui::Button::new("Delete").shortcut_text("Del"))
        .clicked()
    {
        out.push(DebuggerAction::EditWatch(i, String::new()));
        ui.close();
    }
    ui.separator();
    if ui
        .add(egui::Button::new("Move Up").shortcut_text("Alt+Up"))
        .clicked()
    {
        out.push(DebuggerAction::MoveWatch(i, true));
        ui.close();
    }
    if ui
        .add(egui::Button::new("Move Down").shortcut_text("Alt+Down"))
        .clicked()
    {
        out.push(DebuggerAction::MoveWatch(i, false));
        ui.close();
    }
    ui.separator();
    ui.menu_button("Row Display Format", |ui| {
        for (label, suffix) in [
            ("Binary", ", B"),
            ("Hexadecimal (8-bit)", ", H1"),
            ("Hexadecimal (16-bit)", ", H2"),
            ("Hexadecimal (24-bit)", ", H3"),
            ("Hexadecimal (32-bit)", ", H4"),
            ("Signed decimal (8-bit)", ", S1"),
            ("Signed decimal (16-bit)", ", S2"),
            ("Signed decimal (24-bit)", ", S3"),
            ("Signed decimal (32-bit)", ", S4"),
            ("Unsigned decimal", ", U"),
            ("Clear", ""),
        ] {
            if ui.button(label).clicked() {
                out.push(DebuggerAction::EditWatch(i, format!("{base}{suffix}")));
                ui.close();
            }
        }
    });
    ui.separator();
    for (label, style) in [
        ("Decimal Display", WatchStyle::Decimal),
        ("Hexadecimal Display", WatchStyle::Hex),
        ("Binary Display", WatchStyle::Binary),
    ] {
        if ui.radio(view.watch_style == style, label).clicked() {
            out.push(DebuggerAction::WatchStyle(style));
            ui.close();
        }
    }
}

fn bp_type(b: &BreakpointInfo) -> String {
    use luna_api::WatchSpace;
    let mem = match b.space {
        WatchSpace::Bus => "CPU",
        WatchSpace::Vram => "VRAM",
        WatchSpace::Cgram => "CGRAM",
        WatchSpace::Oam => "OAM",
        WatchSpace::PrgRom => "PRG",
        WatchSpace::WorkRam => "WRAM",
        WatchSpace::SaveRam => "SRAM",
        _ => "MEM",
    };
    if b.forbid {
        return format!("{mem}:\u{1F6C7}");
    }
    let f = |on: bool, c: char| if on { c } else { '\u{2012}' };
    format!(
        "{mem}:{}{}{}",
        f(b.on_read, 'R'),
        f(b.on_write, 'W'),
        f(b.exec, 'X')
    )
}

fn bp_dialog(b: Option<&BreakpointInfo>, forbid: bool) -> Dialog {
    use luna_api::WatchSpace;
    match b {
        Some(b) => Dialog::EditBreakpoint {
            replace: Some(b.id),
            memory: match b.space {
                WatchSpace::Vram => MemoryKind::SnesVideoRam,
                WatchSpace::Cgram => MemoryKind::SnesCgRam,
                WatchSpace::Oam => MemoryKind::SnesSpriteRam,
                WatchSpace::PrgRom => MemoryKind::SnesPrgRom,
                WatchSpace::WorkRam => MemoryKind::SnesWorkRam,
                WatchSpace::SaveRam => MemoryKind::SnesSaveRam,
                _ => MemoryKind::SnesMemory,
            },
            lo: format!("{:X}", b.lo),
            hi: format!("{:X}", b.hi),
            rwx: [b.on_read, b.on_write, b.exec],
            condition: b.condition.clone().unwrap_or_default(),
            mark: b.mark,
            enabled: b.enabled,
            forbid: b.forbid,
        },
        None => Dialog::EditBreakpoint {
            replace: None,
            memory: MemoryKind::SnesMemory,
            lo: String::new(),
            hi: String::new(),
            rwx: [!forbid, !forbid, true],
            condition: String::new(),
            mark: false,
            enabled: true,
            forbid,
        },
    }
}

fn breakpoints_pane(
    ui: &mut egui::Ui,
    view: &DebuggerView,
    st: &mut UiState,
    out: &mut Vec<DebuggerAction>,
) {
    ui.strong("Breakpoints");
    egui::ScrollArea::vertical()
        .id_salt("dbg_bps")
        .show(ui, |ui| {
            egui::Grid::new("dbg_bps_grid")
                .striped(true)
                .num_columns(5)
                .show(ui, |ui| {
                    ui.strong("E");
                    ui.strong("M");
                    ui.strong("Type");
                    ui.strong("Address");
                    ui.strong("Condition");
                    ui.end_row();
                    for (b, label) in &view.breakpoints {
                        let mut e = b.enabled;
                        if ui.checkbox(&mut e, "").changed() {
                            out.push(DebuggerAction::BpEnabled(b.id, e));
                        }
                        if b.forbid {
                            ui.label("");
                        } else {
                            let mut m = b.mark;
                            if ui.checkbox(&mut m, "").changed() {
                                out.push(DebuggerAction::BpMark(b.id, m));
                            }
                        }
                        let r =
                            ui.selectable_label(false, egui::RichText::new(bp_type(b)).monospace());
                        if r.double_clicked() {
                            st.dialog = bp_dialog(Some(b), false);
                        }
                        r.context_menu(|ui| {
                            if ui
                                .add(egui::Button::new("Edit").shortcut_text("F2"))
                                .clicked()
                            {
                                st.dialog = bp_dialog(Some(b), false);
                                ui.close();
                            }
                            if ui
                                .add(egui::Button::new("Delete").shortcut_text("Del"))
                                .clicked()
                            {
                                out.push(DebuggerAction::BpRemove(b.id));
                                ui.close();
                            }
                            let (label, on) = if b.enabled {
                                ("Disable Breakpoint", false)
                            } else {
                                ("Enable Breakpoint", true)
                            };
                            if ui.button(label).clicked() {
                                out.push(DebuggerAction::BpEnabled(b.id, on));
                                ui.close();
                            }
                            if b.space == luna_api::WatchSpace::Bus
                                && ui.button("Go to Location").clicked()
                            {
                                out.push(DebuggerAction::Goto(b.lo));
                                ui.close();
                            }
                        });
                        let mut addr = if b.lo == b.hi {
                            format!("${:06X}", b.lo)
                        } else {
                            format!("${:06X} - ${:06X}", b.lo, b.hi)
                        };
                        if let Some(l) = label {
                            addr = format!("{addr} [{l}]");
                        }
                        ui.monospace(addr);
                        ui.monospace(b.condition.clone().unwrap_or_default());
                        ui.end_row();
                    }
                });
        });
    ui.horizontal(|ui| {
        if ui.small_button("Add…").on_hover_text("Insert").clicked() {
            st.dialog = bp_dialog(None, false);
        }
        if ui
            .small_button("Add forbid…")
            .on_hover_text("Ctrl+Insert")
            .clicked()
        {
            st.dialog = bp_dialog(None, true);
        }
    });
}

fn call_stack_pane(ui: &mut egui::Ui, view: &DebuggerView, out: &mut Vec<DebuggerAction>) {
    ui.strong("Call Stack");
    egui::ScrollArea::vertical()
        .id_salt("dbg_stack")
        .show(ui, |ui| {
            egui::Grid::new("dbg_stack_grid")
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    ui.strong("Function");
                    ui.strong("PC Address");
                    ui.strong("ROM Address");
                    ui.end_row();
                    for r in &view.call_stack {
                        let mapped = r.absolute.is_some();
                        let text = if mapped {
                            egui::RichText::new(&r.function).monospace()
                        } else {
                            egui::RichText::new(&r.function)
                                .monospace()
                                .italics()
                                .weak()
                        };
                        if ui.selectable_label(false, text).double_clicked() && mapped {
                            out.push(DebuggerAction::Goto(r.pc));
                        }
                        ui.monospace(format!("${:06X}", r.pc));
                        ui.monospace(
                            r.absolute
                                .map_or_else(String::new, |l| rom_cell(l.kind, l.offset)),
                        );
                        ui.end_row();
                    }
                });
            if view.call_stack.is_empty() {
                ui.label(
                    egui::RichText::new("(the call stack is tracked while this window is open)")
                        .weak(),
                );
            }
        });
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('$');
    u32::from_str_radix(s, 16).ok()
}

fn dialogs(ui: &egui::Ui, view: &DebuggerView, st: &mut UiState, out: &mut Vec<DebuggerAction>) {
    let ctx = ui.ctx().clone();
    let mut close = false;
    let mut window = |title: &str, body: &mut dyn FnMut(&mut egui::Ui) -> bool| {
        let mut open = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(&ctx, |ui| {
                if body(ui) {
                    close = true;
                }
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
    };
    let enter = |ui: &egui::Ui| ui.input(|i| i.key_pressed(egui::Key::Enter));
    match &mut st.dialog {
        Dialog::None => {}
        Dialog::GotoAddress => {
            let text = &mut st.goto_text;
            window("Go to Address…", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    ui.add(egui::TextEdit::singleline(text).desired_width(160.0)).request_focus();
                    ui.label("?").on_hover_text(
                        "A hex address (8000 is $8000), a label, or any expression the watch window takes.",
                    );
                });
                if ui.button("OK").clicked() || enter(ui) {
                    out.push(DebuggerAction::GotoText(text.clone()));
                    return true;
                }
                false
            });
        }
        Dialog::GotoAll => {
            let text = &mut st.goto_all_text;
            window("Go to All", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(220.0));
                    r.request_focus();
                    if r.changed() {
                        out.push(DebuggerAction::GotoAllQuery(text.clone()));
                    }
                });
                ui.label(format!("{} results", view.goto_all.len()));
                let mut picked = None;
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for r in &view.goto_all {
                            let line = r.address.map_or_else(
                                || format!("\u{26A0} {}   <unavailable>", r.name),
                                |a| format!("{}   ${a:06X}", r.name),
                            );
                            if ui
                                .add_enabled(
                                    r.address.is_some(),
                                    egui::Button::selectable(false, line),
                                )
                                .double_clicked()
                            {
                                picked = r.address;
                            }
                        }
                    });
                if (ui.button("Select").clicked() || enter(ui))
                    && let Some(a) =
                        picked.or_else(|| view.goto_all.first().and_then(|r| r.address))
                {
                    picked = Some(a);
                }
                if let Some(a) = picked {
                    out.push(DebuggerAction::Goto(a));
                    return true;
                }
                ui.button("Close").clicked()
            });
        }
        Dialog::BreakIn => {
            let (n, unit) = (&mut st.break_in, &mut st.break_in_unit);
            window("Break In…", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label("Break in:");
                    ui.add(egui::TextEdit::singleline(n).desired_width(80.0))
                        .request_focus();
                });
                for (i, name) in ["CPU instructions", "Scanlines", "Frames"]
                    .iter()
                    .enumerate()
                {
                    ui.radio_value(unit, i, *name);
                }
                if (ui.button("OK").clicked() || enter(ui))
                    && let Ok(count) = n.trim().parse::<u64>()
                {
                    out.push(DebuggerAction::Step(match unit {
                        0 => StepKind::Instructions(count),
                        1 => StepKind::Scanlines(count),
                        _ => StepKind::Frames(count),
                    }));
                    return true;
                }
                false
            });
        }
        Dialog::RunToScanline => {
            let text = &mut st.scanline;
            window("Run to scanline…", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label("Scanline #:");
                    ui.add(egui::TextEdit::singleline(text).desired_width(80.0))
                        .request_focus();
                });
                if (ui.button("OK").clicked() || enter(ui))
                    && let Ok(line) = text.trim().parse::<u16>()
                {
                    out.push(DebuggerAction::RunToScanline(line));
                    return true;
                }
                false
            });
        }
        Dialog::FindAll => {
            let (text, case, word) = (&mut st.find_all_text, &mut st.find_case, &mut st.find_word);
            let mut found = false;
            window("Find All Occurrences…", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.add(egui::TextEdit::singleline(text).desired_width(200.0))
                        .request_focus();
                });
                ui.checkbox(case, "Match case");
                ui.checkbox(word, "Match whole word");
                if ui.button("Find").clicked() || enter(ui) {
                    out.push(DebuggerAction::FindAll(text.clone(), *case, *word));
                    found = true;
                    return true;
                }
                ui.button("Cancel").clicked()
            });
            if found {
                st.tab = Tab::FindResults;
            }
        }
        Dialog::EditLabel {
            memory,
            address,
            name,
            comment,
            length,
            existing,
        } => {
            let existing = *existing;
            window("Edit label…", &mut |ui| {
                egui::Grid::new("dbg_label_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Memory type:");
                        egui::ComboBox::from_id_salt("dbg_label_mem")
                            .selected_text(memory.name())
                            .show_ui(ui, |ui| {
                                for k in [
                                    MemoryKind::SnesPrgRom,
                                    MemoryKind::SnesWorkRam,
                                    MemoryKind::SnesSaveRam,
                                    MemoryKind::SnesRegister,
                                    MemoryKind::SnesMemory,
                                ] {
                                    ui.selectable_value(memory, k, k.name());
                                }
                            });
                        ui.end_row();
                        ui.label("Address:");
                        ui.text_edit_singleline(address);
                        ui.end_row();
                        ui.label("Label:");
                        ui.text_edit_singleline(name).request_focus();
                        ui.end_row();
                        ui.label("Comment:");
                        ui.text_edit_multiline(comment);
                        ui.end_row();
                        ui.label("Length:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(length).desired_width(50.0));
                            ui.label("bytes");
                        });
                        ui.end_row();
                    });
                let valid_name = name.is_empty() || luna_api::labels::valid_name(name);
                if !valid_name {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 120, 120),
                        "Letters, digits, _ and @ only, not starting with a digit.",
                    );
                }
                let mut done = false;
                ui.horizontal(|ui| {
                    if existing
                        && ui.button("Delete Label").clicked()
                        && let Some(a) = parse_hex(address)
                    {
                        out.push(DebuggerAction::RemoveLabel(*memory, a));
                        done = true;
                    }
                    if ui
                        .add_enabled(valid_name, egui::Button::new("OK"))
                        .clicked()
                        && let Some(a) = parse_hex(address)
                    {
                        out.push(DebuggerAction::SetLabel(Label {
                            memory: *memory,
                            address: a,
                            length: length.trim().parse().unwrap_or(1),
                            name: name.clone(),
                            comment: comment.clone(),
                        }));
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
                done
            });
        }
        Dialog::EditComment {
            memory,
            address,
            text,
        } => {
            let (memory, address) = (*memory, *address);
            window("Edit comment…", &mut |ui| {
                ui.text_edit_multiline(text).request_focus();
                let mut done = false;
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        let name = view
                            .labels
                            .iter()
                            .find(|l| l.label.memory == memory && l.label.address == address)
                            .map(|l| (l.label.name.clone(), l.label.length));
                        let (name, length) = name.unwrap_or_default();
                        out.push(DebuggerAction::SetLabel(Label {
                            memory,
                            address,
                            length: length.max(1),
                            name,
                            comment: text.clone(),
                        }));
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
                done
            });
        }
        Dialog::EditBreakpoint {
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
            let title = if *forbid {
                "Edit forbid breakpoint…"
            } else {
                "Edit breakpoint…"
            };
            let forbid = *forbid;
            let replace = *replace;
            window(title, &mut |ui| {
                egui::Grid::new("dbg_bp_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Memory type:");
                        egui::ComboBox::from_id_salt("dbg_bp_mem")
                            .selected_text(memory.name())
                            .show_ui(ui, |ui| {
                                let kinds: &[MemoryKind] = if forbid {
                                    &[
                                        MemoryKind::SnesMemory,
                                        MemoryKind::SnesPrgRom,
                                        MemoryKind::SnesWorkRam,
                                    ]
                                } else {
                                    &[
                                        MemoryKind::SnesMemory,
                                        MemoryKind::SnesVideoRam,
                                        MemoryKind::SnesCgRam,
                                        MemoryKind::SnesSpriteRam,
                                    ]
                                };
                                for &k in kinds {
                                    ui.selectable_value(memory, k, k.name());
                                }
                            });
                        ui.end_row();
                        if !forbid {
                            ui.label("Break on:");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut rwx[0], "Read");
                                ui.checkbox(&mut rwx[1], "Write");
                                if *memory == MemoryKind::SnesMemory {
                                    ui.checkbox(&mut rwx[2], "Execution");
                                }
                            });
                            ui.end_row();
                        }
                        ui.label("Address:");
                        ui.horizontal(|ui| {
                            ui.label("$");
                            let before = lo.clone();
                            ui.add(egui::TextEdit::singleline(lo).desired_width(70.0));
                            if *lo != before && (hi.is_empty() || *hi == before) {
                                hi.clone_from(lo);
                            }
                            ui.label("to $");
                            ui.add(egui::TextEdit::singleline(hi).desired_width(70.0));
                        });
                        ui.end_row();
                        ui.label("Condition:");
                        ui.text_edit_multiline(condition);
                        ui.end_row();
                    });
                if forbid {
                    ui.checkbox(enabled, "Enabled");
                } else {
                    ui.checkbox(mark, "Mark on Event Viewer");
                    ui.checkbox(enabled, "Break Execution");
                }
                let parsed = parse_hex(lo).zip(parse_hex(if hi.is_empty() { lo } else { hi }));
                let mut done = false;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(parsed.is_some(), egui::Button::new("OK"))
                        .clicked()
                        && let Some((l, h)) = parsed
                    {
                        out.push(DebuggerAction::BpSave {
                            replace,
                            memory: *memory,
                            lo: l.min(h),
                            hi: l.max(h),
                            rwx: *rwx,
                            condition: condition.clone(),
                            mark: *mark,
                            enabled: *enabled,
                            forbid,
                        });
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
                done
            });
        }
    }
    if close {
        st.dialog = Dialog::None;
    }
}
