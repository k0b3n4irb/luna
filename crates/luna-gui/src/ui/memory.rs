//! CPU and SPC700 memory panels.

use super::widgets::value_chip;
use super::{DebugSnapshot, PanelNav};

/// Render `bytes` as a Mesen2-style hex grid: a flat zero-padded address
/// (`addr_digits` wide) per 16-byte row, `00` bytes dimmed and non-zero
/// bytes bright, a vertical rule, then the ASCII gutter. The view's first
/// byte (the cursor) gets a blue background. Per-byte colouring is built
/// with a `LayoutJob` so each row is one galley.
fn mem_hex_grid(ui: &mut egui::Ui, start: u32, bytes: &[u8], addr_digits: usize) {
    use egui::text::{LayoutJob, TextFormat};
    use std::fmt::Write as _;

    // Never wrap: a row's hex and ASCII must stay on one aligned line (the
    // window/scroll-area handles any overflow).
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    let mono = egui::FontId::monospace(13.0);
    let c_addr = egui::Color32::from_rgb(118, 128, 150);
    let c_zero = egui::Color32::from_rgb(74, 78, 92);
    let c_byte = egui::Color32::from_rgb(220, 224, 236);
    let c_sep = egui::Color32::from_rgb(64, 68, 86);
    let c_dot = egui::Color32::from_rgb(74, 78, 92);
    let c_chr = egui::Color32::from_rgb(176, 198, 168);
    let cursor_bg = egui::Color32::from_rgb(38, 60, 108);

    let fmt = |color: egui::Color32| TextFormat {
        font_id: mono.clone(),
        color,
        ..Default::default()
    };

    // One reusable scratch buffer for the per-byte hex / per-char ASCII
    // cells — `LayoutJob::append` copies the &str, so clearing and reusing
    // this avoids a heap allocation per cell (32 per row) on every repaint.
    let mut cell = String::with_capacity(3);

    for (r, chunk) in bytes.chunks(16).enumerate() {
        let addr = start.wrapping_add((r as u32) * 16) & 0xFF_FFFF;
        let mut job = LayoutJob::default();
        job.append(&format!("{addr:0addr_digits$X}: "), 0.0, fmt(c_addr));
        for (i, &b) in chunk.iter().enumerate() {
            let mut f = fmt(if b == 0 { c_zero } else { c_byte });
            if r == 0 && i == 0 {
                f.background = cursor_bg;
            }
            cell.clear();
            let _ = write!(cell, "{b:02X}");
            job.append(&cell, 0.0, f);
            job.append(" ", 0.0, fmt(c_zero));
        }
        for _ in chunk.len()..16 {
            job.append("   ", 0.0, fmt(c_zero));
        }
        job.append("\u{2502} ", 0.0, fmt(c_sep));
        for &b in chunk {
            let (ch, col) = if (0x20..0x7F).contains(&b) {
                (b as char, c_chr)
            } else {
                ('.', c_dot)
            };
            cell.clear();
            cell.push(ch);
            job.append(&cell, 0.0, fmt(col));
        }
        ui.label(job);
    }
}

/// Bottom toolbar for a memory view: the current address as a blue chip
/// plus ±$100 / ±$1000 nav buttons. Returns the nav request if clicked.
fn mem_nav(ui: &mut egui::Ui, start: u32, addr_digits: usize) -> Option<PanelNav> {
    let mut nav = None;
    ui.separator();
    ui.horizontal(|ui| {
        value_chip(ui, &format!("{start:0addr_digits$X}"));
        ui.add_space(8.0);
        if ui
            .button("\u{25c0}\u{25c0}")
            .on_hover_text("\u{2212}$1000")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(-0x1000));
        }
        if ui
            .button("\u{25c0}")
            .on_hover_text("\u{2212}$100")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(-0x100));
        }
        if ui.button("\u{25b6}").on_hover_text("+$100").clicked() {
            nav = Some(PanelNav::MemAddr(0x100));
        }
        if ui
            .button("\u{25b6}\u{25b6}")
            .on_hover_text("+$1000")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(0x1000));
        }
    });
    nav
}

/// Body of the CPU Memory view — 256 bytes of the 24-bit CPU bus at the
/// current address. Returns the nav request from the toolbar, if any.
pub(crate) fn cpu_memory_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some((addr, bytes)) = snap.cpu_memory.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    mem_hex_grid(ui, *addr, bytes, 6);
    mem_nav(ui, *addr, 6)
}

/// Body of the SPC700 Memory view — 256 bytes of the flat 64 KiB ARAM.
pub(crate) fn spc700_memory_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some((addr, bytes)) = snap.spc_memory.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    mem_hex_grid(ui, u32::from(*addr), bytes, 4);
    mem_nav(ui, u32::from(*addr), 4)
}
