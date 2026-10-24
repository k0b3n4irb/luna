//! The Memory Search window, laid out as Mesen2's: the memory type,
//! format and value size on the left, the filter (Compare To, Operator)
//! on the right, Apply filter / Undo / Reset under them, and the table of
//! candidates with its live *Match* column.

// Screen coordinates: `a + b * c` reads as the layout it places.
#![allow(clippy::suboptimal_flops)]

use luna_api::{MemoryKind, SearchOperator};

use crate::memory_host::{
    CompareTo, MemorySearchAction, MemorySearchView, SearchColumn, SearchDisplay, SearchForm,
};

/// The table's columns: header, width in characters, sort key.
const COLUMNS: [(&str, f32, SearchColumn); 10] = [
    ("Address", 8.0, SearchColumn::Address),
    ("Value", 10.0, SearchColumn::Value),
    ("Last Value", 11.0, SearchColumn::LastValue),
    ("Reads", 9.0, SearchColumn::Reads),
    ("Last Read", 10.0, SearchColumn::LastRead),
    ("Writes", 9.0, SearchColumn::Writes),
    ("Last Write", 11.0, SearchColumn::LastWrite),
    ("Execs", 9.0, SearchColumn::Execs),
    ("Last Exec", 10.0, SearchColumn::LastExec),
    ("Match", 6.0, SearchColumn::Match),
];

/// The Memory Search window's body.
pub(crate) fn memory_search_body(
    ui: &mut egui::Ui,
    view: &MemorySearchView,
) -> Vec<MemorySearchAction> {
    let mut out = Vec::new();
    egui::Panel::top("ms_form").show_inside(ui, |ui| form(ui, view, &mut out));
    egui::CentralPanel::default().show_inside(ui, |ui| table(ui, view, &mut out));
    out
}

fn form(ui: &mut egui::Ui, view: &MemorySearchView, out: &mut Vec<MemorySearchAction>) {
    let mut f: SearchForm = view.form.clone();
    let before = f.clone();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            egui::Grid::new("ms_left").num_columns(2).show(ui, |ui| {
                ui.label("Memory type:");
                egui::ComboBox::from_id_salt("ms_kind")
                    .selected_text(view.kind.map_or("—", MemoryKind::name))
                    .show_ui(ui, |ui| {
                        for k in &view.kinds {
                            if ui
                                .selectable_label(Some(*k) == view.kind, k.name())
                                .clicked()
                            {
                                out.push(MemorySearchAction::SetKind(*k));
                            }
                        }
                    });
                ui.end_row();
                ui.label("Format:");
                egui::ComboBox::from_id_salt("ms_format")
                    .selected_text(match f.display {
                        SearchDisplay::Hex => "Hex",
                        SearchDisplay::Signed => "Signed",
                        SearchDisplay::Unsigned => "Unsigned",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.display, SearchDisplay::Hex, "Hex");
                        ui.selectable_value(&mut f.display, SearchDisplay::Signed, "Signed");
                        ui.selectable_value(&mut f.display, SearchDisplay::Unsigned, "Unsigned");
                    });
                ui.end_row();
                ui.label("Value size:");
                egui::ComboBox::from_id_salt("ms_size")
                    .selected_text(format!("{}-bit", u32::from(f.size) * 8))
                    .show_ui(ui, |ui| {
                        for s in [1u8, 2, 4] {
                            ui.selectable_value(
                                &mut f.size,
                                s,
                                format!("{}-bit", u32::from(s) * 8),
                            );
                        }
                    });
                ui.end_row();
            });
            if ui.button("Reset access stats").clicked() {
                out.push(MemorySearchAction::ResetCounters);
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.strong("Search Filter");
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label("Compare To");
                    ui.radio_value(
                        &mut f.compare,
                        CompareTo::PreviousSearch,
                        "Previous search value",
                    );
                    ui.radio_value(
                        &mut f.compare,
                        CompareTo::PreviousRefresh,
                        "Previous refresh value",
                    );
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut f.compare, CompareTo::Value, "Specific value:");
                        ui.add_enabled(
                            f.compare == CompareTo::Value,
                            egui::TextEdit::singleline(&mut f.value).desired_width(90.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut f.compare, CompareTo::Address, "Specific address:");
                        ui.add_enabled(
                            f.compare == CompareTo::Address,
                            egui::TextEdit::singleline(&mut f.address).desired_width(90.0),
                        );
                    });
                });
                ui.vertical(|ui| {
                    ui.label("Operator");
                    for (op, label) in [
                        (SearchOperator::Equal, "Equal"),
                        (SearchOperator::NotEqual, "Not equal"),
                        (SearchOperator::LessThan, "Less than"),
                        (SearchOperator::GreaterThan, "Greater than"),
                        (SearchOperator::LessThanOrEqual, "Less than or equal"),
                        (SearchOperator::GreaterThanOrEqual, "Greater than or equal"),
                    ] {
                        ui.radio_value(&mut f.operator, op, label);
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(view.filter_ok, egui::Button::new("Apply filter"))
                    .clicked()
                {
                    out.push(MemorySearchAction::Apply);
                }
                if ui
                    .add_enabled(view.undo > 0, egui::Button::new("Undo"))
                    .clicked()
                {
                    out.push(MemorySearchAction::Undo);
                }
                if ui.button("Reset").clicked() {
                    out.push(MemorySearchAction::Reset);
                }
                ui.label(format!("{} candidate(s)", view.candidates));
            });
        });
    });
    if f != before {
        out.push(MemorySearchAction::Form(f));
    }
}

fn table(ui: &mut egui::Ui, view: &MemorySearchView, out: &mut Vec<MemorySearchAction>) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let full = ui.available_rect_before_wrap();
    let bar_w = 14.0;
    let area = egui::Rect::from_min_max(full.min, egui::pos2(full.max.x - bar_w, full.max.y));
    let bar = egui::Rect::from_min_max(egui::pos2(area.max.x, full.min.y), full.max);
    let visible = (((area.height() - row_h) / row_h).floor() as usize).max(1);
    out.push(MemorySearchAction::VisibleRows(visible));
    let resp = ui.allocate_rect(area, egui::Sense::click());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, egui::Color32::from_rgb(18, 18, 24));
    let text = egui::Color32::from_rgb(205, 205, 215);
    let dim = egui::Color32::from_rgb(120, 120, 135);
    // The header: click to sort, again to sort the other way.
    let mut x = area.min.x + char_w * 0.5;
    for (name, w, col) in COLUMNS {
        let rect =
            egui::Rect::from_min_size(egui::pos2(x, area.min.y), egui::vec2(w * char_w, row_h));
        let arrow = match view.sort {
            Some((c, true)) if c == col => " \u{25B2}",
            Some((c, false)) if c == col => " \u{25BC}",
            _ => "",
        };
        painter.text(
            rect.min,
            egui::Align2::LEFT_TOP,
            format!("{name}{arrow}"),
            font.clone(),
            dim,
        );
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && rect.contains(p)
        {
            out.push(MemorySearchAction::Sort(col));
        }
        x += w * char_w + char_w;
    }
    let top = area.min.y + row_h;
    for (i, r) in view.rows.iter().enumerate().take(visible) {
        let y = top + i as f32 * row_h;
        let colour = if r.matches == Some(false) { dim } else { text };
        let cells = [
            &r.address,
            &r.value,
            &r.last_value,
            &r.reads,
            &r.last_read,
            &r.writes,
            &r.last_write,
            &r.execs,
            &r.last_exec,
        ];
        let mut x = area.min.x + char_w * 0.5;
        for ((_, w, _), cell) in COLUMNS.iter().zip(cells) {
            painter.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_TOP,
                cell,
                font.clone(),
                colour,
            );
            x += w * char_w + char_w;
        }
        let mark = match r.matches {
            Some(true) => "\u{2611}",
            Some(false) => "\u{2610}",
            None => "",
        };
        painter.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            mark,
            font.clone(),
            colour,
        );
    }
    let wheel = ui.input(|i| i.smooth_scroll_delta.y);
    if resp.hovered() && wheel.abs() > 0.5 {
        out.push(MemorySearchAction::Scroll(
            (-wheel / row_h * 3.0).round() as i64
        ));
    }
    // The scrollbar.
    let bresp = ui.allocate_rect(bar, egui::Sense::click_and_drag());
    let bp = ui.painter_at(bar);
    bp.rect_filled(bar, 0.0, egui::Color32::from_rgb(30, 30, 38));
    let total = view.candidates.max(1) as f32;
    let h = (visible as f32 / total * bar.height()).clamp(12.0, bar.height());
    let span = (total - visible as f32).max(1.0);
    let y0 = bar.min.y + (view.top as f32 / span).min(1.0) * (bar.height() - h).max(0.0);
    bp.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(bar.min.x + 2.0, y0),
            egui::vec2(bar.width() - 4.0, h),
        ),
        3.0,
        egui::Color32::from_rgb(90, 90, 110),
    );
    if (bresp.dragged() || bresp.clicked())
        && let Some(p) = bresp.interact_pointer_pos()
    {
        let f = ((p.y - bar.min.y - h / 2.0) / (bar.height() - h).max(1.0)).clamp(0.0, 1.0);
        out.push(MemorySearchAction::ScrollTo(
            (f * (total - visible as f32).max(0.0)).round() as usize,
        ));
    }
}
