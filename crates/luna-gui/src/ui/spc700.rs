//! SPC700 state panel.

use super::DebugSnapshot;
use super::widgets::{psw_flags, reg_row, value_chip};

/// Body of the SPC700 (audio CPU) debug view — register file + flags.
pub(crate) fn spc700_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(c) = snap.spc700.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    egui::Grid::new("spc_regs")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            reg_row(ui, "A", &[format!("{:02X}", c.a)]);
            reg_row(ui, "X", &[format!("{:02X}", c.x)]);
            reg_row(ui, "Y", &[format!("{:02X}", c.y)]);
            reg_row(
                ui,
                "YA",
                &[format!("{:04X}", (u16::from(c.y) << 8) | u16::from(c.a))],
            );
            reg_row(ui, "SP", &[format!("01{:02X}", c.sp)]);
            reg_row(ui, "PC", &[format!("{:04X}", c.pc)]);
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.monospace("PSW");
        value_chip(ui, &format!("{:02X}", c.psw));
    });
    ui.add_space(4.0);
    // SPC700 PSW: N V P B H I Z C.
    psw_flags(
        ui,
        c.psw,
        &[
            (0x80, 'N'),
            (0x40, 'V'),
            (0x20, 'P'),
            (0x10, 'B'),
            (0x08, 'H'),
            (0x04, 'I'),
            (0x02, 'Z'),
            (0x01, 'C'),
        ],
    );
    if c.stopped {
        ui.colored_label(egui::Color32::RED, "STOPPED");
    }
    if c.sleeping {
        ui.colored_label(egui::Color32::YELLOW, "SLEEPING");
    }
}
