//! Breakpoints panel.

use super::widgets::style_blue_field;
use super::{DebugSnapshot, PanelNav};

/// Body of the Breakpoints panel (issue #68): the live registry with
/// per-row remove, clear-all, and add forms for exec breakpoints and
/// memory watchpoints. Form fields persist in egui temp memory.
pub(crate) fn breakpoints_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(rows) = snap.breakpoints.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    let mut nav = None;

    // --- add-exec form ---
    let exec_id = ui.id().with("bp-add-exec");
    let mut exec_addr: u32 = ui.data_mut(|d| *d.get_temp_mut_or(exec_id, 0x00_8000_u32));
    ui.horizontal(|ui| {
        ui.label("Exec at");
        ui.scope(|ui| {
            style_blue_field(ui);
            ui.add(
                egui::DragValue::new(&mut exec_addr)
                    .range(0..=0xFF_FFFF_u32)
                    .hexadecimal(6, false, true)
                    .speed(1.0),
            );
        });
        if ui.button("Add").clicked() {
            nav = Some(PanelNav::BpAddExec(exec_addr));
        }
    });
    ui.data_mut(|d| d.insert_temp(exec_id, exec_addr));

    // --- add-watch form ---
    let lo_id = ui.id().with("bp-watch-lo");
    let hi_id = ui.id().with("bp-watch-hi");
    let r_id = ui.id().with("bp-watch-r");
    let w_id = ui.id().with("bp-watch-w");
    let mut lo: u32 = ui.data_mut(|d| *d.get_temp_mut_or(lo_id, 0x7E_0000_u32));
    let mut hi: u32 = ui.data_mut(|d| *d.get_temp_mut_or(hi_id, 0x7E_0000_u32));
    let mut on_r: bool = ui.data_mut(|d| *d.get_temp_mut_or(r_id, false));
    let mut on_w: bool = ui.data_mut(|d| *d.get_temp_mut_or(w_id, true));
    ui.horizontal(|ui| {
        ui.label("Watch");
        ui.scope(|ui| {
            style_blue_field(ui);
            ui.add(
                egui::DragValue::new(&mut lo)
                    .range(0..=0xFF_FFFF_u32)
                    .hexadecimal(6, false, true)
                    .speed(1.0),
            );
            ui.label("..");
            ui.add(
                egui::DragValue::new(&mut hi)
                    .range(0..=0xFF_FFFF_u32)
                    .hexadecimal(6, false, true)
                    .speed(1.0),
            );
        });
        ui.checkbox(&mut on_r, "R");
        ui.checkbox(&mut on_w, "W");
        if ui.button("Add").clicked() && (on_r || on_w) && lo <= hi {
            nav = Some(PanelNav::BpAddMem(lo, hi, on_r, on_w));
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(lo_id, lo);
        d.insert_temp(hi_id, hi);
        d.insert_temp(r_id, on_r);
        d.insert_temp(w_id, on_w);
    });

    ui.separator();
    if rows.is_empty() {
        ui.label("No breakpoints. Click a CPU-disassembly row or use the forms above.");
        return nav;
    }
    if ui.button("Clear all").clicked() {
        nav = Some(PanelNav::BpClearAll);
    }
    ui.add_space(4.0);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    let mono = egui::FontId::monospace(13.0);
    for (info, sym) in rows {
        ui.horizontal(|ui| {
            if ui.small_button("\u{2715}").clicked() {
                nav = Some(PanelNav::BpRemove(info.id));
            }
            let desc = if info.exec {
                format!("#{:<3} exec  ${:06X}", info.id, info.lo)
            } else {
                let kinds = match (info.on_read, info.on_write) {
                    (true, true) => "RW",
                    (true, false) => "R ",
                    _ => " W",
                };
                if info.lo == info.hi {
                    format!("#{:<3} watch ${:06X}       {kinds}", info.id, info.lo)
                } else {
                    format!(
                        "#{:<3} watch ${:06X}-${:06X} {kinds}",
                        info.id, info.lo, info.hi
                    )
                }
            };
            let text = match sym {
                Some(s) => format!("{desc}  ({s})"),
                None => desc,
            };
            ui.label(egui::RichText::new(text).font(mono.clone()));
        });
    }
    nav
}
