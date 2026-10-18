//! Tilemap viewer panel.

use super::{DebugSnapshot, PanelNav};

/// Body of the Tilemap Viewer — renders the selected BG layer's full
/// tilemap (Mode 7 = the 128×128 field) as a texture, with a red rectangle
/// overlay marking the on-screen viewport (from the layer's scroll).
/// Returns a nav action when the BG selector is clicked.
pub(crate) fn tilemap_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let mut nav = None;
    // Mode 7 has a single BG plane, so only BG1 is meaningful — disable the
    // others and say so, rather than letting all four look selectable.
    let mode7 = snap.tilemap.as_ref().is_some_and(|t| t.is_mode7);
    ui.horizontal(|ui| {
        ui.monospace("BG:");
        for bg in 0..4usize {
            let enabled = !mode7 || bg == 0;
            let selected = if mode7 {
                bg == 0
            } else {
                snap.tilemap_bg == bg
            };
            let resp = ui.add_enabled(
                enabled,
                egui::Button::selectable(selected, format!("{}", bg + 1)),
            );
            if resp.clicked() {
                nav = Some(PanelNav::TilemapSetBg(bg));
            }
        }
        if mode7 {
            ui.label(egui::RichText::new("Mode 7 — single plane").weak());
        }
    });
    let Some(t) = snap.tilemap.as_ref() else {
        ui.label("(no ROM loaded)");
        return nav;
    };
    if t.bpp == 0 {
        ui.label("(layer disabled in the current BG mode)");
        return nav;
    }
    ui.monospace(format!(
        "{}{}×{} tiles · {}bpp · {}×{} px",
        if t.is_mode7 { "Mode 7 · " } else { "" },
        t.cols,
        t.rows,
        t.bpp,
        t.width,
        t.height
    ));
    ui.separator();

    let img =
        egui::ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &t.rgba);
    let tex = ui
        .ctx()
        .load_texture("tilemap_view", img, egui::TextureOptions::NEAREST);
    // Keep the handle alive past this frame: the debug-window renderer
    // creates textures then frees them in the same pass, so a handle
    // dropped here would be freed before the paint (→ black image).
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new("tilemap_tex_keepalive"), tex.clone()));
    let size = egui::vec2(t.width as f32, t.height as f32);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    // Viewport rectangle (skipped for Mode 7, view_w == 0): a single clean
    // box at the scroll origin. It may run off the right/bottom edge when
    // the camera straddles the tilemap wrap; the clip rect trims it rather
    // than drawing a confusing second wrapped copy.
    if t.view_w > 0 {
        let painter = ui.painter().with_clip_rect(rect);
        let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 90, 90));
        let min = rect.min + egui::vec2(t.view_x as f32, t.view_y as f32);
        let vrect = egui::Rect::from_min_size(min, egui::vec2(t.view_w as f32, t.view_h as f32));
        painter.rect_stroke(
            vrect,
            egui::CornerRadius::ZERO,
            stroke,
            egui::StrokeKind::Inside,
        );
    }
    nav
}
