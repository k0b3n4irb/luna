//! CPU (65C816) state panel.

use super::DebugSnapshot;
use super::widgets::{psw_flags, reg_row, value_chip};

/// Body of the CPU-state debug view (65c816 register file + decoded flags).
pub(crate) fn cpu_state_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(c) = snap.cpu.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    egui::Grid::new("cpu_regs")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            reg_row(ui, "A", &[format!("{:04X}", c.a)]);
            reg_row(ui, "X", &[format!("{:04X}", c.x)]);
            reg_row(ui, "Y", &[format!("{:04X}", c.y)]);
            reg_row(ui, "SP", &[format!("{:04X}", c.sp)]);
            reg_row(ui, "DP", &[format!("{:04X}", c.dp)]);
            reg_row(
                ui,
                "PB:PC",
                &[format!("{:02X}", c.pb), format!("{:04X}", c.pc)],
            );
            reg_row(ui, "DB", &[format!("{:02X}", c.db)]);
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.monospace("P");
        value_chip(ui, &format!("{:02X}", c.p));
        ui.add_space(8.0);
        ui.monospace("E");
        value_chip(ui, &format!("{}", u8::from(c.e)));
    });
    ui.add_space(4.0);
    psw_flags(
        ui,
        c.p,
        &[
            (0x80, 'N'),
            (0x40, 'V'),
            (0x20, 'M'),
            (0x10, 'X'),
            (0x08, 'D'),
            (0x04, 'I'),
            (0x02, 'Z'),
            (0x01, 'C'),
        ],
    );
    if c.stopped {
        ui.colored_label(egui::Color32::RED, "STOPPED");
    }
    if c.waiting {
        ui.colored_label(egui::Color32::YELLOW, "WAITING (WAI)");
    }
}
