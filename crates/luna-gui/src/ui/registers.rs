//! Register viewer panel.

use super::DebugSnapshot;
use super::widgets::value_chip;

/// Egui-memory id holding the Register Viewer's live search filter.
fn rv_filter_id() -> egui::Id {
    egui::Id::new("reg_viewer_filter")
}

/// One `$AAAA  NAME  <chip>` row in a register-viewer grid. Skipped
/// entirely when the live search filter (egui memory) matches neither
/// the register name nor its address.
fn rv_row(ui: &mut egui::Ui, addr: &str, name: &str, value: &str) {
    let filter: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(rv_filter_id()))
        .unwrap_or_default();
    if !filter.is_empty() {
        let f = filter.to_ascii_lowercase();
        if !name.to_ascii_lowercase().contains(&f) && !addr.to_ascii_lowercase().contains(&f) {
            return;
        }
    }
    ui.monospace(addr);
    ui.monospace(name);
    value_chip(ui, value);
    ui.end_row();
}

/// A faint section header for the register viewer.
fn rv_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).weak().small());
    ui.separator();
}

/// Body of the Register Viewer — raw memory-mapped I/O register values
/// grouped by component (CPU `$42xx`, PPU `$21xx`, DMA `$43xx`, APU/DSP).
/// v1 is read-only and shows the raw value only (no per-field decoding).
pub(crate) fn registers_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(s) = snap.registers.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    // Search box (filters rows by register name or address). The filter
    // string lives in egui memory so each rv_row can consult it.
    let mut filter: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(rv_filter_id()))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.monospace("Search:");
        ui.add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text("name or $addr")
                .desired_width(170.0),
        );
        if ui.button("Clear").clicked() {
            filter.clear();
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(rv_filter_id(), filter));

    let b = |v: u8| format!("${v:02X}");
    let w = |v: u16| format!("${v:04X}");

    // --- CPU $4200-$421F ---
    let c = &s.cpu_regs;
    rv_header(ui, "CPU  $4200-$421F");
    egui::Grid::new("rv_cpu")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            rv_row(ui, "$4200", "NMITIMEN", &b(c.nmitimen));
            rv_row(ui, "$4201", "WRIO", &b(c.wrio));
            rv_row(ui, "$4202", "WRMPYA", &b(c.wrmpya));
            rv_row(ui, "$4203", "WRMPYB", &b(c.wrmpyb));
            rv_row(ui, "$4204", "WRDIV", &w(c.wrdiv));
            rv_row(ui, "$4206", "WRDVDD", &b(c.wrdvdd));
            rv_row(ui, "$4207", "HTIME", &w(c.htime));
            rv_row(ui, "$4209", "VTIME", &w(c.vtime));
            rv_row(ui, "$420B", "MDMAEN", &b(s.dma.mdmaen));
            rv_row(ui, "$420C", "HDMAEN", &b(s.dma.hdmaen));
            rv_row(ui, "$420D", "MEMSEL", &b(c.memsel));
            rv_row(ui, "$4210", "RDNMI", &b(u8::from(c.nmi_flag)));
            rv_row(ui, "$4211", "TIMEUP", &b(u8::from(c.irq_flag)));
            rv_row(ui, "$4212", "HVBJOY", &b(c.hvbjoy));
            rv_row(ui, "$4214", "RDDIV", &w(c.rddiv));
            rv_row(ui, "$4216", "RDMPY", &w(c.rdmpy));
            rv_row(ui, "$4218", "JOY1", &w(c.joy1));
            rv_row(ui, "$421A", "JOY2", &w(c.joy2));
        });

    // --- PPU $2100-$213F ---
    let p = &s.ppu;
    rv_header(ui, "PPU  $2100-$213F");
    egui::Grid::new("rv_ppu")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            rv_row(ui, "$2100", "INIDISP", &b(p.inidisp));
            rv_row(ui, "$2101", "OBSEL", &b(p.obsel));
            rv_row(ui, "$2105", "BGMODE", &b(p.bgmode));
            rv_row(ui, "$2106", "MOSAIC", &b(p.mosaic));
            for (i, bg) in p.bgs.iter().enumerate() {
                let n = i + 1;
                rv_row(
                    ui,
                    &format!("$210{}", 6 + n),
                    &format!("BG{n}SC"),
                    &w(bg.tilemap_addr_words),
                );
                rv_row(
                    ui,
                    &format!("$210{:X}", 0xB + i / 2),
                    &format!("BG{n}NBA"),
                    &w(bg.char_addr_words),
                );
                rv_row(ui, "", &format!("BG{n}HOFS"), &w(bg.h_scroll));
                rv_row(ui, "", &format!("BG{n}VOFS"), &w(bg.v_scroll));
            }
            rv_row(ui, "$211A", "M7SEL", &b(p.m7sel));
            rv_row(ui, "$210D", "M7HOFS", &w(p.m7_hofs as u16));
            rv_row(ui, "$210E", "M7VOFS", &w(p.m7_vofs as u16));
            rv_row(ui, "$211B", "M7A", &w(p.m7a as u16));
            rv_row(ui, "$211C", "M7B", &w(p.m7b as u16));
            rv_row(ui, "$211D", "M7C", &w(p.m7c as u16));
            rv_row(ui, "$211E", "M7D", &w(p.m7d as u16));
            rv_row(ui, "$211F", "M7X", &w(p.m7x as u16));
            rv_row(ui, "$2120", "M7Y", &w(p.m7y as u16));
            rv_row(ui, "$2123", "W12SEL", &b(p.w12sel));
            rv_row(ui, "$2124", "W34SEL", &b(p.w34sel));
            rv_row(ui, "$2125", "WOBJSEL", &b(p.wobjsel));
            rv_row(ui, "$2126", "WH0", &b(p.windows[0]));
            rv_row(ui, "$2127", "WH1", &b(p.windows[1]));
            rv_row(ui, "$2128", "WH2", &b(p.windows[2]));
            rv_row(ui, "$2129", "WH3", &b(p.windows[3]));
            rv_row(ui, "$212A", "WBGLOG", &b(p.wbglog));
            rv_row(ui, "$212B", "WOBJLOG", &b(p.wobjlog));
            rv_row(ui, "$212C", "TM", &b(p.tm));
            rv_row(ui, "$212D", "TS", &b(p.ts));
            rv_row(ui, "$212E", "TMW", &b(p.tmw));
            rv_row(ui, "$212F", "TSW", &b(p.tsw));
            rv_row(ui, "$2130", "CGWSEL", &b(p.cgwsel));
            rv_row(ui, "$2131", "CGADSUB", &b(p.cgadsub));
            rv_row(
                ui,
                "$2132",
                "COLDATA",
                &format!(
                    "R{:02X} G{:02X} B{:02X}",
                    p.coldata_r, p.coldata_g, p.coldata_b
                ),
            );
            rv_row(ui, "$2133", "SETINI", &b(p.setini));
            rv_row(
                ui,
                "$2134",
                "MPY",
                &format!("${:06X}", p.mpy as u32 & 0xFF_FFFF),
            );
            rv_row(ui, "$213E", "STAT77", &b(p.stat77));
            rv_row(ui, "$213F", "STAT78", &b(p.stat78));
            rv_row(ui, "$213C", "OPHCT", &w(p.ophct));
            rv_row(ui, "$213D", "OPVCT", &w(p.opvct));
        });

    // --- DMA / HDMA $4300-$437F ---
    rv_header(ui, "DMA / HDMA  $4300-$437F");
    egui::Grid::new("rv_dma")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (i, ch) in s.dma.channels.iter().enumerate() {
                rv_row(ui, &format!("$43{i}0"), &format!("DMAP{i}"), &b(ch.params));
                rv_row(ui, &format!("$43{i}1"), &format!("BBAD{i}"), &b(ch.bbad));
                rv_row(
                    ui,
                    &format!("$43{i}2"),
                    &format!("A1T{i}"),
                    &format!("${:02X}:{:04X}", ch.a_bank, ch.a_addr),
                );
                rv_row(ui, &format!("$43{i}5"), &format!("DAS{i}"), &w(ch.das));
                rv_row(ui, &format!("$43{i}7"), &format!("DASB{i}"), &b(ch.dasb));
                rv_row(ui, &format!("$43{i}8"), &format!("A2A{i}"), &w(ch.a2a));
                rv_row(ui, &format!("$43{i}A"), &format!("NTLR{i}"), &b(ch.ntlr));
            }
        });

    // --- APU / DSP ---
    let a = &s.apu;
    rv_header(ui, "APU ports  $2140-$2143");
    egui::Grid::new("rv_apu")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for i in 0..4 {
                rv_row(
                    ui,
                    &format!("$214{i}"),
                    &format!("APUIO{i}"),
                    &b(a.to_cpu_ports[i]),
                );
            }
        });
    rv_header(ui, "S-DSP registers");
    // The rows are `luna_api::dsp_register_table`: which registers, in
    // which order, under which name is the API's say (the names are the
    // ones `--dsp-trace` and `[asserts.dsp]` use), and its test covers it.
    let dsp = |r: u8| a.dsp_regs.get(usize::from(r)).copied().unwrap_or(0);
    let table = luna_api::dsp_register_table();
    let (voices, rest) = table.split_at(80);
    egui::Grid::new("rv_dsp_voices")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (r, name) in voices {
                rv_row(ui, &format!("${r:02X}"), name, &b(dsp(*r)));
            }
        });
    egui::Grid::new("rv_dsp_global")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (r, name) in rest {
                rv_row(ui, &format!("${r:02X}"), name, &b(dsp(*r)));
            }
        });

    // --- DSP-1 (uPD7725), only for DSP cartridges ---
    if let Some(d) = s.dsp1.as_ref() {
        rv_header(ui, "DSP-1  (uPD7725)");
        egui::Grid::new("rv_dsp1")
            .num_columns(3)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                rv_row(ui, "", "PC", &format!("${:04X}", d.pc));
                rv_row(ui, "", "SR", &w(d.sr));
                rv_row(ui, "", "DR", &w(d.dr));
                rv_row(ui, "", "A", &w(d.a as u16));
                rv_row(ui, "", "B", &w(d.b as u16));
                rv_row(ui, "", "RQM", &b(u8::from(d.rqm)));
            });
    }
}
