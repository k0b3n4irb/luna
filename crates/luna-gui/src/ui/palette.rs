//! Palette (CGRAM) panel.

use super::DebugSnapshot;

/// 5-bit colour channel → 8-bit, replicating the high bits (matches
/// `luna_ppu::tile::scale_5_to_8`; duplicated here so the GUI stays off
/// the lower crates — pure presentation maths on an API-provided value).
const fn c5_to_8(c5: u8) -> u8 {
    (c5 << 3) | (c5 >> 2)
}

/// Body of the Palette Viewer — the 256 CGRAM entries as a 16×16 swatch
/// grid (index 0 top-left = backdrop). Hover a swatch for its index, raw
/// BGR555 word, and decoded RGB triplet.
pub(crate) fn palette_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(cgram) = snap.palette.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    let cell = 20.0_f32;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(1.0, 1.0);
        for row in 0..16usize {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(1.0, 1.0);
                for col in 0..16usize {
                    let idx = row * 16 + col;
                    let c = cgram.get(idx).copied().unwrap_or(0);
                    let r = c5_to_8((c & 0x1F) as u8);
                    let g = c5_to_8(((c >> 5) & 0x1F) as u8);
                    let b = c5_to_8(((c >> 10) & 0x1F) as u8);
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::hover());
                    let painter = ui.painter();
                    painter.rect_filled(
                        rect,
                        egui::CornerRadius::same(2),
                        egui::Color32::from_rgb(r, g, b),
                    );
                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::same(2),
                        egui::Stroke::new(1.0, egui::Color32::from_gray(40)),
                        egui::StrokeKind::Inside,
                    );
                    resp.on_hover_text(format!(
                        "#{idx} (${idx:02X})\n${c:04X} BGR555\nRGB {r:02X} {g:02X} {b:02X}"
                    ));
                }
            });
        }
    });
}
