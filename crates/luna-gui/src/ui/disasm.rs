//! CPU and SPC700 disassembly panels.

use super::widgets::style_blue_field;
use super::{DebugSnapshot, PanelNav};

/// Body of the SPC700 disassembly view — a live listing from the current
/// PC (`ADDR: BYTES   MNEMONIC`), the PC line given the blue cursor
/// background. Read-only / follow-execution.
/// Shared body for the SPC700 / CPU disassembly panels: a toolbar
/// (Follow-PC, address, scroll, line count) over a live instruction
/// listing with the PC line highlighted. `addr_digits` is 4 for ARAM,
/// 6 for the 24-bit CPU bus. Returns the toolbar's nav request.
fn disasm_body(
    ui: &mut egui::Ui,
    lines: &[luna_api::DisasmLine],
    line_count: u16,
    addr_digits: usize,
    max_addr: u32,
    bp_addrs: Option<&[u32]>,
) -> Option<PanelNav> {
    use egui::text::{LayoutJob, TextFormat};

    let start = lines.first().map_or(0, |l| l.addr);
    let mut nav = None;

    // Toolbar (ness layout): jump-to-PC button, an editable hex start
    // address, and an editable line count. Type an address + Enter, or
    // double-click the value to edit / drag to scrub.
    ui.horizontal(|ui| {
        if ui
            .button("Disassemble at PC")
            .on_hover_text("Re-anchor the listing at the live program counter")
            .clicked()
        {
            nav = Some(PanelNav::DisasmGotoPc);
        }
        let mut addr = start;
        let addr_resp = ui
            .scope(|ui| {
                style_blue_field(ui);
                ui.add(
                    egui::DragValue::new(&mut addr)
                        .range(0..=max_addr)
                        .hexadecimal(addr_digits, false, true)
                        .speed(1.0),
                )
            })
            .inner;
        if addr_resp
            .on_hover_text("Start address (double-click to type)")
            .changed()
        {
            nav = Some(PanelNav::DisasmSetAddr(addr));
        }
        ui.add_space(8.0);
        ui.label("Lines:");
        let mut n = line_count;
        let lines_resp = ui
            .scope(|ui| {
                style_blue_field(ui);
                ui.add(egui::DragValue::new(&mut n).range(4..=128).speed(0.25))
            })
            .inner;
        if lines_resp.changed() {
            nav = Some(PanelNav::DisasmSetLines(n));
        }
    });
    ui.separator();
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    let mono = egui::FontId::monospace(13.0);
    let c_addr = egui::Color32::from_rgb(118, 128, 150);
    let c_bytes = egui::Color32::from_rgb(120, 124, 140);
    let c_text = egui::Color32::from_rgb(222, 226, 238);
    let cursor_bg = egui::Color32::from_rgb(38, 60, 108);

    let c_bp = egui::Color32::from_rgb(220, 60, 60);
    for line in lines {
        let bg = if line.is_pc {
            cursor_bg
        } else {
            egui::Color32::TRANSPARENT
        };
        let fmt = |color| TextFormat {
            font_id: mono.clone(),
            color,
            background: bg,
            ..Default::default()
        };
        let mut raw = String::with_capacity(line.bytes.len() * 3);
        for b in &line.bytes {
            use std::fmt::Write;
            let _ = write!(raw, "{b:02X} ");
        }
        let mut job = LayoutJob::default();
        // Breakpoint gutter (issue #68): a red dot on breakpointed rows;
        // two spaces otherwise so the columns stay aligned. Only the CPU
        // view gets one (the registry watches the CPU bus).
        if let Some(bps) = bp_addrs {
            let marked = bps.contains(&line.addr);
            job.append(
                if marked { "\u{25cf} " } else { "  " },
                0.0,
                fmt(if marked { c_bp } else { c_addr }),
            );
        }
        job.append(
            &format!("{:0addr_digits$X}:  ", line.addr),
            0.0,
            fmt(c_addr),
        );
        job.append(&format!("{raw:<13}"), 0.0, fmt(c_bytes));
        job.append(&line.text, 0.0, fmt(c_text));
        if bp_addrs.is_some() {
            // Clickable row: toggle an exec breakpoint at this address.
            let resp = ui
                .add(egui::Label::new(job).sense(egui::Sense::click()))
                .on_hover_text("Click: toggle breakpoint");
            if resp.clicked() {
                nav = Some(PanelNav::DisasmToggleBreakpoint(line.addr));
            }
        } else {
            ui.label(job);
        }
    }
    nav
}

/// Body of the SPC700 disassembly view.
pub(crate) fn spc700_disasm_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(lines) = snap.spc_disasm.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    disasm_body(ui, lines, snap.spc_disasm_lines, 4, 0xFFFF, None)
}

/// Body of the CPU (65c816) disassembly view.
pub(crate) fn cpu_disasm_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(lines) = snap.cpu_disasm.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    disasm_body(
        ui,
        lines,
        snap.cpu_disasm_lines,
        6,
        0xFF_FFFF,
        Some(&snap.cpu_bp_addrs),
    )
}
