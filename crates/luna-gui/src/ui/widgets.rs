//! Small widgets shared by the debug panels.

/// Restyle the current `ui`'s widget visuals so an interactive widget
/// (e.g. a `DragValue`) renders as a blue cell, matching [`value_chip`].
/// Call inside a `ui.scope(...)` so the override is local.
pub(super) fn style_blue_field(ui: &mut egui::Ui) {
    let fill = egui::Color32::from_rgb(38, 60, 108);
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(96, 132, 205));
    let text = egui::Color32::from_rgb(226, 232, 248);
    let w = &mut ui.visuals_mut().widgets;
    for s in [&mut w.inactive, &mut w.hovered, &mut w.active] {
        s.weak_bg_fill = fill;
        s.bg_fill = fill;
        s.bg_stroke = stroke;
        s.fg_stroke.color = text;
    }
    w.hovered.weak_bg_fill = egui::Color32::from_rgb(50, 78, 140);
}

/// A register value rendered as a blue framed cell — the ness debugger's
/// signature look for live values.
pub(super) fn value_chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(38, 60, 108))
        .stroke(egui::Stroke::new(
            1.5,
            egui::Color32::from_rgb(96, 132, 205),
        ))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(egui::CornerRadius::same(3))
        .show(ui, |ui| {
            ui.monospace(egui::RichText::new(text).color(egui::Color32::from_rgb(226, 232, 248)));
        });
}

/// `label:` followed by one or more blue value chips, as one grid row.
pub(super) fn reg_row(ui: &mut egui::Ui, label: &str, values: &[String]) {
    ui.monospace(label);
    ui.horizontal(|ui| {
        for v in values {
            value_chip(ui, v);
        }
    });
    ui.end_row();
}

/// Processor-status flags as a row of blue 0/1 chips with the flag
/// letter centred beneath each chip (ness style). Each flag is a
/// fixed-width, centre-aligned mini-column so the chip and its letter
/// share an axis. `bits` is MSB→LSB.
pub(super) fn psw_flags(ui: &mut egui::Ui, p: u8, bits: &[(u8, char)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for &(mask, name) in bits {
            ui.allocate_ui_with_layout(
                egui::vec2(22.0, 42.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    value_chip(ui, if p & mask != 0 { "1" } else { "0" });
                    ui.monospace(name.to_string());
                },
            );
        }
    });
}
