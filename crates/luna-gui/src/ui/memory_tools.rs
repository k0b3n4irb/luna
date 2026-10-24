//! The Memory Tools window, laid out as Mesen2's Memory Viewer: File /
//! View / Search menus and a toolbar (the memory type, Import, Export,
//! Reset access stats), the hex grid with its text column, the options
//! panel at the right edge, and a status bar.
//!
//! Everything shown comes from `luna-api` through
//! [`crate::memory_host::MemoryToolsView`]; everything done goes back as
//! [`MemoryToolsAction`]s. The panel keeps only what a widget needs
//! between two frames (the open dialog, text being typed) in egui's memory.

// Screen coordinates: `a + b * c` reads as the layout it places.
#![allow(clippy::suboptimal_flops)]

use luna_api::{AccessKind, ByteShade, FadeSpeed, FindData, FindFilters, MemoryKind, NavTarget};

use crate::memory_host::{
    COLUMNS, FindRequest, MemoryToolsAction, MemoryToolsSettings, MemoryToolsView,
};

/// The open dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Dialog {
    #[default]
    None,
    Goto,
    Find,
    Label,
}

/// Find's data types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FindType {
    #[default]
    Hex,
    Text,
    Integer,
}

/// What the panel keeps between frames.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // The Find dialog's boxes.
struct UiState {
    dialog: Dialog,
    goto_text: String,
    find_text: String,
    find_type: FindType,
    /// 0: any size, else 1 / 2 / 4 bytes.
    find_size: u8,
    case_sensitive: bool,
    use_tbl: bool,
    filters: FindFilters,
    last_frames: String,
    label_name: String,
    label_comment: String,
    /// Dragging the scrollbar.
    dragging_bar: bool,
}

fn state_id() -> egui::Id {
    egui::Id::new("memory_tools_state")
}

/// The colours (Mesen2's, on a dark background).
const TEXT: egui::Color32 = egui::Color32::from_rgb(205, 205, 215);
const DIM: egui::Color32 = egui::Color32::from_rgb(120, 120, 135);
const READ: egui::Color32 = egui::Color32::from_rgb(80, 150, 255);
const WRITE: egui::Color32 = egui::Color32::from_rgb(240, 80, 80);
const EXEC: egui::Color32 = egui::Color32::from_rgb(70, 210, 100);
const FROZEN: egui::Color32 = egui::Color32::from_rgb(230, 70, 230);
const CODE_BG: egui::Color32 = egui::Color32::from_rgb(38, 70, 48);
const DATA_BG: egui::Color32 = egui::Color32::from_rgb(40, 56, 84);
const LABEL_BG: egui::Color32 = egui::Color32::from_rgb(92, 46, 70);
const SELECT_BG: egui::Color32 = egui::Color32::from_rgb(52, 84, 140);
const SELECT_DIM_BG: egui::Color32 = egui::Color32::from_rgb(44, 64, 100);

const fn access_colour(k: AccessKind) -> egui::Color32 {
    match k {
        AccessKind::Read => READ,
        AccessKind::Write => WRITE,
        AccessKind::Exec => EXEC,
    }
}

/// `TEXT` toward `c` by `t` (the access fade).
fn mix(c: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
    egui::Color32::from_rgb(l(TEXT.r(), c.r()), l(TEXT.g(), c.g()), l(TEXT.b(), c.b()))
}

/// The Memory Tools window's body.
pub(crate) fn memory_tools_body(
    ui: &mut egui::Ui,
    view: &MemoryToolsView,
) -> Vec<MemoryToolsAction> {
    let mut out = Vec::new();
    let mut st: UiState = ui
        .ctx()
        .data_mut(|d| d.get_temp(state_id()).unwrap_or_default());
    if let Some(text) = &view.clipboard {
        ui.ctx().copy_text(text.clone());
    }
    shortcuts(ui, view, &mut st, &mut out);
    egui::Panel::top("mem_menu").show_inside(ui, |ui| {
        menu_bar(ui, view, &mut st, &mut out);
        toolbar(ui, view, &mut out);
    });
    egui::Panel::bottom("mem_status").show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            ui.monospace(&view.status);
            if view.find_miss {
                ui.colored_label(WRITE, "No match found!");
            }
        });
    });
    if view.settings.show_settings {
        egui::Panel::right("mem_settings")
            .resizable(true)
            .default_size(200.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| settings_panel(ui, view, &mut out));
            });
    }
    egui::CentralPanel::default().show_inside(ui, |ui| grid(ui, view, &mut st, &mut out));
    dialogs(ui, view, &mut st, &mut out);
    ui.ctx().data_mut(|d| d.insert_temp(state_id(), st));
    out
}

fn shortcuts(
    ui: &egui::Ui,
    view: &MemoryToolsView,
    st: &mut UiState,
    out: &mut Vec<MemoryToolsAction>,
) {
    use egui::{Event, Key};
    if st.dialog != Dialog::None || ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let cols = i64::from(view.settings.columns);
    let page = cols * (view.rows.len().max(1) as i64);
    let events = ui.input(|i| i.events.clone());
    for ev in events {
        match ev {
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let shift = modifiers.shift;
                let ctrl = modifiers.command;
                let row_start = i64::from(view.cursor) - i64::from(view.cursor) % cols;
                match (key, ctrl) {
                    (Key::ArrowLeft, false) => out.push(MemoryToolsAction::Move(-1, shift)),
                    (Key::ArrowRight, false) => out.push(MemoryToolsAction::Move(1, shift)),
                    (Key::ArrowUp, false) => out.push(MemoryToolsAction::Move(-cols, shift)),
                    (Key::ArrowDown, false) => out.push(MemoryToolsAction::Move(cols, shift)),
                    (Key::PageUp, false) => out.push(MemoryToolsAction::Move(-page, shift)),
                    (Key::PageDown, false) => out.push(MemoryToolsAction::Move(page, shift)),
                    (Key::Home, false) => {
                        out.push(MemoryToolsAction::MoveTo(row_start as u32, shift));
                    }
                    (Key::End, false) => {
                        out.push(MemoryToolsAction::MoveTo(
                            (row_start + cols - 1) as u32,
                            shift,
                        ));
                    }
                    (Key::Home, true) => out.push(MemoryToolsAction::MoveTo(0, shift)),
                    (Key::End, true) => out.push(MemoryToolsAction::MoveTo(u32::MAX, shift)),
                    (Key::A, true) => out.push(MemoryToolsAction::SelectAll),
                    (Key::C, true) => out.push(MemoryToolsAction::Copy),
                    (Key::Z, true) => out.push(MemoryToolsAction::Undo),
                    (Key::G, true) => st.dialog = Dialog::Goto,
                    (Key::F, true) => st.dialog = Dialog::Find,
                    (Key::O, true) => out.push(MemoryToolsAction::Import),
                    (Key::S, true) => out.push(MemoryToolsAction::Export),
                    (Key::Q, true) if view.freezable => out.push(MemoryToolsAction::Freeze(true)),
                    (Key::W, true) if view.freezable => out.push(MemoryToolsAction::Freeze(false)),
                    (Key::F3, _) => {
                        if view.can_find_again {
                            out.push(MemoryToolsAction::FindAgain(shift));
                        } else {
                            st.dialog = Dialog::Find;
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if !ui.input(|i| i.modifiers.command) => {
                for c in t.chars() {
                    if view.text_focus {
                        out.push(MemoryToolsAction::Char(c));
                    } else if let Some(d) = c.to_digit(16) {
                        out.push(MemoryToolsAction::HexDigit(d as u8));
                    }
                }
            }
            Event::Paste(t) => out.push(MemoryToolsAction::Paste(t)),
            Event::Copy => out.push(MemoryToolsAction::Copy),
            _ => {}
        }
    }
}

fn menu_bar(
    ui: &mut egui::Ui,
    view: &MemoryToolsView,
    st: &mut UiState,
    out: &mut Vec<MemoryToolsAction>,
) {
    let bus = matches!(
        view.kind,
        Some(
            MemoryKind::SnesMemory
                | MemoryKind::SpcMemory
                | MemoryKind::Sa1Memory
                | MemoryKind::GsuMemory
        )
    );
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui
                .add_enabled(!bus, egui::Button::new("Import...").shortcut_text("Ctrl+O"))
                .clicked()
            {
                out.push(MemoryToolsAction::Import);
            }
            if ui
                .add(egui::Button::new("Export...").shortcut_text("Ctrl+S"))
                .clicked()
            {
                out.push(MemoryToolsAction::Export);
            }
            ui.separator();
            if ui.button("Reset access stats").clicked() {
                out.push(MemoryToolsAction::ResetCounters);
            }
            ui.separator();
            if ui.button("Load TBL file...").clicked() {
                out.push(MemoryToolsAction::LoadTbl);
            }
            if ui
                .add_enabled(view.tbl, egui::Button::new("Reset TBL mappings"))
                .clicked()
            {
                out.push(MemoryToolsAction::ResetTbl);
            }
        });
        ui.menu_button("View", |ui| {
            let mut s = view.settings.clone();
            if ui
                .checkbox(&mut s.show_settings, "Show settings panel")
                .changed()
            {
                out.push(MemoryToolsAction::Settings(s));
            }
        });
        ui.menu_button("Search", |ui| {
            if ui
                .add(egui::Button::new("Go to address...").shortcut_text("Ctrl+G"))
                .clicked()
            {
                st.dialog = Dialog::Goto;
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Find...").shortcut_text("Ctrl+F"))
                .clicked()
            {
                st.dialog = Dialog::Find;
            }
            if ui
                .add_enabled(
                    view.can_find_again,
                    egui::Button::new("Find next").shortcut_text("F3"),
                )
                .clicked()
            {
                out.push(MemoryToolsAction::FindAgain(false));
            }
            if ui
                .add_enabled(
                    view.can_find_again,
                    egui::Button::new("Find previous").shortcut_text("Shift+F3"),
                )
                .clicked()
            {
                out.push(MemoryToolsAction::FindAgain(true));
            }
            ui.separator();
            ui.menu_button("Navigate to...", |ui| {
                for (label, target, forward) in [
                    ("Previous read block", NavTarget::Read, false),
                    ("Next read block", NavTarget::Read, true),
                    ("Previous write block", NavTarget::Write, false),
                    ("Next write block", NavTarget::Write, true),
                    ("Previous exec block", NavTarget::Exec, false),
                    ("Next exec block", NavTarget::Exec, true),
                    ("Previous code block", NavTarget::Code, false),
                    ("Next code block", NavTarget::Code, true),
                    ("Previous data block", NavTarget::Data, false),
                    ("Next data block", NavTarget::Data, true),
                    (
                        "Previous unidentified block",
                        NavTarget::Unidentified,
                        false,
                    ),
                    ("Next unidentified block", NavTarget::Unidentified, true),
                ] {
                    if ui.button(label).clicked() {
                        out.push(MemoryToolsAction::Navigate(target, forward));
                    }
                }
            });
        });
    });
}

fn toolbar(ui: &mut egui::Ui, view: &MemoryToolsView, out: &mut Vec<MemoryToolsAction>) {
    ui.horizontal(|ui| {
        ui.label("Memory Type:");
        let current = view.kind.map_or("—", MemoryKind::name);
        egui::ComboBox::from_id_salt("mem_kind")
            .selected_text(current)
            .width(170.0)
            .show_ui(ui, |ui| {
                for (k, size) in &view.kinds {
                    if ui
                        .selectable_label(Some(*k) == view.kind, k.name())
                        .on_hover_text(format!("{size} bytes"))
                        .clicked()
                    {
                        out.push(MemoryToolsAction::SetKind(*k));
                    }
                }
            });
        if ui.button("Import").clicked() {
            out.push(MemoryToolsAction::Import);
        }
        if ui.button("Export").clicked() {
            out.push(MemoryToolsAction::Export);
        }
        ui.separator();
        if ui
            .button("Reset")
            .on_hover_text("Reset access stats")
            .clicked()
        {
            out.push(MemoryToolsAction::ResetCounters);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(view.settings.show_settings, "\u{2699}")
                .on_hover_text("Toggle settings panel")
                .clicked()
            {
                let mut s = view.settings.clone();
                s.show_settings = !s.show_settings;
                out.push(MemoryToolsAction::Settings(s));
            }
        });
    });
}

fn settings_panel(ui: &mut egui::Ui, view: &MemoryToolsView, out: &mut Vec<MemoryToolsAction>) {
    let mut s: MemoryToolsSettings = view.settings.clone();
    let before = s.clone();
    let nav = |ui: &mut egui::Ui, target: NavTarget, out: &mut Vec<MemoryToolsAction>| {
        if ui
            .small_button("\u{25C1}")
            .on_hover_text("Previous block")
            .clicked()
        {
            out.push(MemoryToolsAction::Navigate(target, false));
        }
        if ui
            .small_button("\u{25B7}")
            .on_hover_text("Next block")
            .clicked()
        {
            out.push(MemoryToolsAction::Navigate(target, true));
        }
    };
    let swatch = |ui: &mut egui::Ui, c: egui::Color32| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(r, 2.0, c);
    };
    ui.strong("Access highlighting");
    let h = &mut s.highlight;
    for (flag, target, colour, label) in [
        (&mut h.read, NavTarget::Read, READ, "Reads"),
        (&mut h.write, NavTarget::Write, WRITE, "Writes"),
        (&mut h.exec, NavTarget::Exec, EXEC, "Execution"),
    ] {
        ui.horizontal(|ui| {
            nav(ui, target, out);
            ui.checkbox(flag, "");
            swatch(ui, colour);
            ui.label(label);
        });
    }
    ui.horizontal(|ui| {
        ui.label("Fade speed:");
        egui::ComboBox::from_id_salt("mem_fade")
            .selected_text(h.fade.label())
            .show_ui(ui, |ui| {
                for f in FadeSpeed::ALL {
                    ui.selectable_value(&mut h.fade, f, f.label());
                }
            });
    });
    ui.add_space(6.0);
    ui.strong("Data type highlighting");
    ui.horizontal(|ui| {
        nav(ui, NavTarget::Code, out);
        ui.checkbox(&mut h.code, "");
        swatch(ui, CODE_BG);
        ui.label("Code");
    });
    ui.horizontal(|ui| {
        nav(ui, NavTarget::Data, out);
        ui.checkbox(&mut h.data, "");
        swatch(ui, DATA_BG);
        ui.label("Data");
    });
    ui.horizontal(|ui| {
        nav(ui, NavTarget::Unidentified, out);
        ui.label("Unidentified");
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut h.labels, "");
        swatch(ui, LABEL_BG);
        ui.label("Labels");
    });
    if view.freezable {
        ui.horizontal(|ui| {
            ui.checkbox(&mut h.frozen, "");
            swatch(ui, FROZEN);
            ui.label("Frozen addresses");
        });
    }
    ui.checkbox(&mut h.breakpoints, "Breakpoints");
    ui.add_space(6.0);
    ui.strong("Misc. settings");
    ui.horizontal(|ui| {
        ui.label("Columns:");
        egui::ComboBox::from_id_salt("mem_cols")
            .selected_text(s.columns.to_string())
            .show_ui(ui, |ui| {
                for c in COLUMNS {
                    ui.selectable_value(&mut s.columns, c, c.to_string());
                }
            });
    });
    ui.checkbox(&mut s.high_density, "Use high-density text");
    ui.checkbox(&mut s.show_chars, "Show characters");
    ui.checkbox(&mut s.tooltips, "Show tooltips");
    if s != before {
        out.push(MemoryToolsAction::Settings(s));
    }
}

/// The hex grid with its scrollbar and text column.
#[allow(clippy::too_many_lines)] // One painter pass over the grid.
fn grid(
    ui: &mut egui::Ui,
    view: &MemoryToolsView,
    st: &mut UiState,
    out: &mut Vec<MemoryToolsAction>,
) {
    let Some(kind) = view.kind else {
        ui.label("No ROM loaded.");
        return;
    };
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let line = ui.fonts_mut(|f| f.row_height(&font));
    let row_h = if view.settings.high_density {
        line * 0.85
    } else {
        line + 2.0
    };
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let full = ui.available_rect_before_wrap();
    let bar_w = 14.0;
    let area = egui::Rect::from_min_max(full.min, egui::pos2(full.max.x - bar_w, full.max.y));
    let bar = egui::Rect::from_min_max(egui::pos2(area.max.x, full.min.y), full.max);
    let visible = (((area.height() - row_h) / row_h).floor() as usize).max(1);
    out.push(MemoryToolsAction::VisibleRows(visible));
    let resp = ui.allocate_rect(area, egui::Sense::click_and_drag());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, egui::Color32::from_rgb(18, 18, 24));
    let cols = usize::from(view.settings.columns);
    let digits = format!("{:X}", view.size.saturating_sub(1)).len().max(4);
    let hex_x = area.min.x + char_w * (digits as f32 + 1.5);
    let cell_w = char_w * 3.0;
    let text_x = hex_x + cell_w * cols as f32 + char_w;
    let top = area.min.y + row_h;
    // The column header.
    for c in 0..cols {
        painter.text(
            egui::pos2(hex_x + c as f32 * cell_w, area.min.y),
            egui::Align2::LEFT_TOP,
            format!("{c:02X}"),
            font.clone(),
            DIM,
        );
    }
    let (sel_a, sel_b) = view.selection;
    let selected = |a: u32| a >= sel_a && a <= sel_b;
    for (r, (addr, bytes)) in view.rows.iter().enumerate().take(visible) {
        let y = top + r as f32 * row_h;
        painter.text(
            egui::pos2(area.min.x + char_w * 0.5, y),
            egui::Align2::LEFT_TOP,
            format!("{addr:0digits$X}"),
            font.clone(),
            DIM,
        );
        for (c, b) in bytes.iter().enumerate() {
            let a = addr + c as u32;
            let cell = egui::Rect::from_min_size(
                egui::pos2(hex_x + c as f32 * cell_w - char_w * 0.25, y),
                egui::vec2(char_w * 2.5, row_h),
            );
            let bg = if selected(a) {
                Some(if view.text_focus {
                    SELECT_DIM_BG
                } else {
                    SELECT_BG
                })
            } else {
                b.shade.map(|s| match s {
                    ByteShade::Code => CODE_BG,
                    ByteShade::Data => DATA_BG,
                    ByteShade::Label => LABEL_BG,
                })
            };
            if let Some(bg) = bg {
                painter.rect_filled(cell, 0.0, bg);
            }
            if let Some(k) = b.breakpoint {
                painter.rect_stroke(
                    cell,
                    0.0,
                    egui::Stroke::new(1.0, access_colour(k)),
                    egui::StrokeKind::Inside,
                );
            }
            if a == view.cursor && !view.text_focus {
                painter.rect_stroke(
                    cell,
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
            let colour = if b.frozen {
                FROZEN
            } else {
                b.access.map_or(TEXT, |(k, s)| mix(access_colour(k), s))
            };
            let text = match view.pending {
                Some(hi) if a == view.cursor => format!("{hi:X}{:X}", b.value & 0xF),
                _ => format!("{:02X}", b.value),
            };
            painter.text(
                egui::pos2(hex_x + c as f32 * cell_w, y),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                if view.pending.is_some() && a == view.cursor {
                    egui::Color32::YELLOW
                } else {
                    colour
                },
            );
        }
        if view.settings.show_chars
            && let Some(glyphs) = view.text.get(r)
        {
            let mut x = text_x;
            let mut a = *addr;
            for (g, n) in glyphs {
                let w = char_w * g.chars().count().max(1) as f32;
                let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, row_h));
                let span = a..a + *n as u32;
                if span.clone().any(selected) {
                    painter.rect_filled(
                        rect,
                        0.0,
                        if view.text_focus {
                            SELECT_BG
                        } else {
                            SELECT_DIM_BG
                        },
                    );
                }
                if span.contains(&view.cursor) && view.text_focus {
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0, egui::Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_TOP,
                    g,
                    font.clone(),
                    TEXT,
                );
                x += w;
                a += *n as u32;
            }
        }
    }
    // Pointer → byte (hex or text column).
    let hit = |p: egui::Pos2| -> Option<(u32, bool)> {
        if p.y < top {
            return None;
        }
        let r = ((p.y - top) / row_h) as usize;
        let (addr, bytes) = view.rows.get(r)?;
        if p.x >= hex_x - char_w * 0.25 && p.x < hex_x + cell_w * cols as f32 {
            let c = ((p.x - hex_x + char_w * 0.25) / cell_w) as usize;
            return (c < bytes.len()).then(|| (addr + c as u32, false));
        }
        if view.settings.show_chars && p.x >= text_x {
            let glyphs = view.text.get(r)?;
            let mut x = text_x;
            let mut a = *addr;
            for (g, n) in glyphs {
                let w = char_w * g.chars().count().max(1) as f32;
                if p.x < x + w {
                    return Some((a, true));
                }
                x += w;
                a += *n as u32;
            }
        }
        None
    };
    let shift = ui.input(|i| i.modifiers.shift);
    if let Some(p) = resp.interact_pointer_pos()
        && let Some((a, text)) = hit(p)
    {
        if resp.drag_started() || resp.clicked() || resp.secondary_clicked() {
            let inside = a >= sel_a && a <= sel_b && sel_a != sel_b;
            if !(resp.secondary_clicked() && inside) {
                out.push(MemoryToolsAction::Click {
                    addr: a,
                    shift: shift && !resp.secondary_clicked(),
                    text,
                });
            }
        } else if resp.dragged() {
            out.push(MemoryToolsAction::MoveTo(a, true));
        }
    }
    let hovered = resp.hover_pos().and_then(hit).map(|(a, _)| a);
    out.push(MemoryToolsAction::Hover(hovered));
    if let (Some(a), Some((ta, tip))) = (hovered, &view.tooltip)
        && a == *ta
    {
        resp.clone().on_hover_text_at_pointer(tip);
    }
    let wheel = ui.input(|i| i.smooth_scroll_delta.y);
    if resp.hovered() && wheel.abs() > 0.5 {
        out.push(MemoryToolsAction::Scroll(
            (-wheel / row_h * 3.0).round() as i64
        ));
    }
    resp.context_menu(|ui| context_menu(ui, view, kind, st, out));
    // The scrollbar.
    let bresp = ui.allocate_rect(bar, egui::Sense::click_and_drag());
    let bp = ui.painter_at(bar);
    bp.rect_filled(bar, 0.0, egui::Color32::from_rgb(30, 30, 38));
    let total = view.total_rows.max(1) as f32;
    let h = (visible as f32 / total * bar.height()).clamp(12.0, bar.height());
    let y0 = bar.min.y
        + view.top_row as f32 / total * (bar.height() - h).max(0.0) * total
            / (total - visible as f32).max(1.0);
    let thumb = egui::Rect::from_min_size(
        egui::pos2(bar.min.x + 2.0, y0.min(bar.max.y - h)),
        egui::vec2(bar.width() - 4.0, h),
    );
    bp.rect_filled(thumb, 3.0, egui::Color32::from_rgb(90, 90, 110));
    if bresp.drag_started() {
        st.dragging_bar = true;
    }
    if (bresp.dragged() || bresp.clicked())
        && let Some(p) = bresp.interact_pointer_pos()
    {
        let f = ((p.y - bar.min.y - h / 2.0) / (bar.height() - h).max(1.0)).clamp(0.0, 1.0);
        let row = (f * (total - visible as f32).max(0.0)).round() as u64;
        out.push(MemoryToolsAction::ScrollTo(row));
    }
    if bresp.drag_stopped() {
        st.dragging_bar = false;
    }
}

fn context_menu(
    ui: &mut egui::Ui,
    view: &MemoryToolsView,
    kind: MemoryKind,
    st: &mut UiState,
    out: &mut Vec<MemoryToolsAction>,
) {
    let bus = matches!(kind, MemoryKind::SnesMemory | MemoryKind::SpcMemory);
    if ui
        .add_enabled(bus, egui::Button::new("Add to Watch"))
        .clicked()
    {
        out.push(MemoryToolsAction::AddWatch);
        ui.close();
    }
    if ui
        .button("Edit Breakpoint")
        .on_hover_text("Toggle a read / write breakpoint on the selection")
        .clicked()
    {
        out.push(MemoryToolsAction::ToggleBreakpoint);
        ui.close();
    }
    if ui.button("Edit Label").clicked() {
        st.label_name.clear();
        st.label_comment.clear();
        st.dialog = Dialog::Label;
        ui.close();
    }
    if ui
        .add_enabled(bus, egui::Button::new("View in Debugger"))
        .clicked()
    {
        out.push(MemoryToolsAction::ViewInDebugger);
        ui.close();
    }
    let other = if bus {
        "View in memory"
    } else {
        "View in CPU memory"
    };
    if ui.button(other).clicked() {
        out.push(MemoryToolsAction::ViewIn);
        ui.close();
    }
    ui.separator();
    if view.freezable {
        if ui
            .add(egui::Button::new("Freeze").shortcut_text("Ctrl+Q"))
            .clicked()
        {
            out.push(MemoryToolsAction::Freeze(true));
            ui.close();
        }
        if ui
            .add(egui::Button::new("Unfreeze").shortcut_text("Ctrl+W"))
            .clicked()
        {
            out.push(MemoryToolsAction::Freeze(false));
            ui.close();
        }
        ui.separator();
    }
    if ui
        .add_enabled(
            view.undo_depth > 0,
            egui::Button::new("Undo").shortcut_text("Ctrl+Z"),
        )
        .clicked()
    {
        out.push(MemoryToolsAction::Undo);
        ui.close();
    }
    ui.separator();
    if ui
        .add(egui::Button::new("Copy").shortcut_text("Ctrl+C"))
        .clicked()
    {
        out.push(MemoryToolsAction::Copy);
        ui.close();
    }
    if ui
        .add(egui::Button::new("Paste").shortcut_text("Ctrl+V"))
        .clicked()
    {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        ui.close();
    }
    if ui
        .add(egui::Button::new("Select All").shortcut_text("Ctrl+A"))
        .clicked()
    {
        out.push(MemoryToolsAction::SelectAll);
        ui.close();
    }
}

/// The Find query the dialog's fields describe, `None` when it does not
/// parse.
fn find_request(st: &UiState) -> Option<FindRequest> {
    let data = match st.find_type {
        FindType::Hex => FindData::Hex(luna_api::memory_view::parse_hex_query(&st.find_text)?),
        FindType::Text => {
            if st.find_text.is_empty() {
                return None;
            }
            FindData::Text {
                text: st.find_text.clone(),
                case_sensitive: st.case_sensitive,
            }
        }
        FindType::Integer => FindData::Integer {
            value: st.find_text.trim().parse().ok()?,
            size: (st.find_size != 0).then_some(st.find_size),
        },
    };
    let mut filters = st.filters;
    filters.last_frames = st.last_frames.trim().parse().ok();
    Some(FindRequest {
        data,
        use_tbl: st.use_tbl,
        filters,
    })
}

#[allow(clippy::too_many_lines)] // The three dialogs.
fn dialogs(
    ui: &egui::Ui,
    view: &MemoryToolsView,
    st: &mut UiState,
    out: &mut Vec<MemoryToolsAction>,
) {
    let ctx = ui.ctx().clone();
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut close = escape;
    let mut open = true;
    match st.dialog {
        Dialog::None => return,
        Dialog::Goto => {
            egui::Window::new("Go to address")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Address: $");
                        ui.add(egui::TextEdit::singleline(&mut st.goto_text).desired_width(120.0))
                            .request_focus();
                    });
                    if ui.button("OK").clicked() || enter {
                        out.push(MemoryToolsAction::Goto(st.goto_text.clone()));
                        close = true;
                    }
                });
        }
        Dialog::Label => {
            egui::Window::new("Edit Label")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(&ctx, |ui| {
                    egui::Grid::new("mem_label").show(ui, |ui| {
                        ui.label("Name:");
                        ui.add(egui::TextEdit::singleline(&mut st.label_name).desired_width(200.0))
                            .request_focus();
                        ui.end_row();
                        ui.label("Comment:");
                        ui.add(egui::TextEdit::multiline(&mut st.label_comment).desired_rows(2));
                        ui.end_row();
                    });
                    ui.label(
                        egui::RichText::new(format!(
                            "{} byte(s) from the selection start",
                            view.selection.1 - view.selection.0 + 1
                        ))
                        .weak(),
                    );
                    if ui.button("OK").clicked() {
                        out.push(MemoryToolsAction::SetLabel(
                            st.label_name.trim().to_string(),
                            st.label_comment.clone(),
                        ));
                        close = true;
                    }
                });
        }
        Dialog::Find => {
            egui::Window::new("Find...")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Search:");
                        let r = ui.add(egui::TextEdit::singleline(&mut st.find_text).desired_width(260.0));
                        r.request_focus();
                        if st.find_type == FindType::Hex {
                            r.on_hover_text(
                                "When searching hexadecimal values, a question mark (?) can be used to \
                                 define a byte that can take any value. e.g: \"FF ? 55\" will search for \
                                 any set of three bytes that starts with FF and ends with 55.",
                            );
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Data Type:");
                        egui::ComboBox::from_id_salt("mem_find_type")
                            .selected_text(match st.find_type {
                                FindType::Hex => "Hexadecimal",
                                FindType::Text => "String",
                                FindType::Integer => "Integer",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut st.find_type, FindType::Hex, "Hexadecimal");
                                ui.selectable_value(&mut st.find_type, FindType::Text, "String");
                                ui.selectable_value(&mut st.find_type, FindType::Integer, "Integer");
                            });
                        if st.find_type == FindType::Integer {
                            ui.label("Format:");
                            egui::ComboBox::from_id_salt("mem_find_size")
                                .selected_text(match st.find_size {
                                    1 => "8-bit integer",
                                    2 => "16-bit integer",
                                    4 => "32-bit integer",
                                    _ => "Any size",
                                })
                                .show_ui(ui, |ui| {
                                    for (v, l) in [
                                        (0, "Any size"),
                                        (1, "8-bit integer"),
                                        (2, "16-bit integer"),
                                        (4, "32-bit integer"),
                                    ] {
                                        ui.selectable_value(&mut st.find_size, v, l);
                                    }
                                });
                        }
                    });
                    if st.find_type == FindType::Text {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut st.case_sensitive, "Case sensitive");
                            ui.add_enabled(view.tbl, egui::Checkbox::new(&mut st.use_tbl, "Use TBL mappings"));
                        });
                    }
                    ui.columns(2, |c| {
                        c[0].strong("Data Type Filters");
                        c[0].checkbox(&mut st.filters.code, "Code");
                        c[0].checkbox(&mut st.filters.data, "Data");
                        c[0].checkbox(&mut st.filters.unidentified, "Unidentified");
                        c[1].strong("Access Filters");
                        c[1].checkbox(&mut st.filters.not_accessed, "Not accessed");
                        c[1].checkbox(&mut st.filters.read, "Read");
                        c[1].checkbox(&mut st.filters.written, "Written");
                        c[1].checkbox(&mut st.filters.executed, "Executed");
                        c[1].horizontal(|ui| {
                            ui.label("In the last");
                            ui.add(egui::TextEdit::singleline(&mut st.last_frames).desired_width(40.0));
                            ui.label("frames");
                        });
                    });
                    let req = find_request(st);
                    ui.horizontal(|ui| {
                        if view.find_miss {
                            ui.colored_label(WRITE, "No match found!");
                        }
                        if ui.add_enabled(req.is_some(), egui::Button::new("Previous")).clicked()
                            && let Some(r) = req.clone()
                        {
                            out.push(MemoryToolsAction::Find(r, true));
                        }
                        if (ui.add_enabled(req.is_some(), egui::Button::new("Next")).clicked() || enter)
                            && let Some(r) = req.clone()
                        {
                            out.push(MemoryToolsAction::Find(r, false));
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
        }
    }
    if close || !open {
        st.dialog = Dialog::None;
    }
}
