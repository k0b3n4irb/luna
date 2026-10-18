//! Sprite viewer panel.

use super::DebugSnapshot;

/// Body of the sprite (OAM) debug view -- the 128 decoded sprites.
pub(crate) fn sprites_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(sprites) = snap.sprites.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    egui::Grid::new("spr")
        .num_columns(6)
        .striped(true)
        .show(ui, |ui| {
            for h in ["#", "X", "Y", "tile", "pal", "pri"] {
                ui.monospace(h);
            }
            ui.end_row();
            for s in sprites {
                ui.monospace(format!("{:3}", s.index));
                ui.monospace(format!("{:4}", s.x));
                ui.monospace(format!("{:3}", s.y));
                ui.monospace(format!("{:03X}", s.tile));
                ui.monospace(format!("{}", s.palette));
                ui.monospace(format!("{}", s.priority));
                ui.end_row();
            }
        });
}
