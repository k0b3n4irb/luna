//! Event viewer panel.

use super::{DebugSnapshot, EventViewerAction, PanelNav};

/// The 18 Event Viewer categories in legend order.
const EVENT_LEGEND: [luna_api::EventCategory; 18] = [
    luna_api::EventCategory::PpuVramWrite,
    luna_api::EventCategory::PpuCgramWrite,
    luna_api::EventCategory::PpuOamWrite,
    luna_api::EventCategory::PpuMode7Write,
    luna_api::EventCategory::PpuBgOptionWrite,
    luna_api::EventCategory::PpuBgScrollWrite,
    luna_api::EventCategory::PpuWindowWrite,
    luna_api::EventCategory::PpuOtherWrite,
    luna_api::EventCategory::PpuRead,
    luna_api::EventCategory::CpuWrite,
    luna_api::EventCategory::CpuRead,
    luna_api::EventCategory::ApuWrite,
    luna_api::EventCategory::ApuRead,
    luna_api::EventCategory::WorkRamWrite,
    luna_api::EventCategory::WorkRamRead,
    luna_api::EventCategory::Nmi,
    luna_api::EventCategory::Irq,
    luna_api::EventCategory::MarkedBreakpoint,
];

/// "PPU Register Writes" filter group — `(category, Mesen2 label)`. These are
/// the eight `EventCategory::Ppu*Write` variants in Mesen2's group order.
const FILTER_PPU_WRITES: [(luna_api::EventCategory, &str); 8] = [
    (luna_api::EventCategory::PpuVramWrite, "VRAM"),
    (luna_api::EventCategory::PpuCgramWrite, "CGRAM"),
    (luna_api::EventCategory::PpuOamWrite, "OAM"),
    (luna_api::EventCategory::PpuMode7Write, "Mode 7"),
    (luna_api::EventCategory::PpuBgOptionWrite, "BG Options"),
    (luna_api::EventCategory::PpuBgScrollWrite, "BG Scroll"),
    (luna_api::EventCategory::PpuWindowWrite, "Window"),
    (luna_api::EventCategory::PpuOtherWrite, "Others"),
];

/// "Other events" filter group — `(category, Mesen2 label)`. Mesen2 names the
/// APU rows "SPC Reg…", not "APU…".
const FILTER_OTHER: [(luna_api::EventCategory, &str); 10] = [
    (luna_api::EventCategory::PpuRead, "PPU Reg Read"),
    (luna_api::EventCategory::ApuRead, "SPC Reg Read"),
    (luna_api::EventCategory::ApuWrite, "SPC Reg Write"),
    (luna_api::EventCategory::CpuRead, "CPU Reg Read"),
    (luna_api::EventCategory::CpuWrite, "CPU Reg Write"),
    (luna_api::EventCategory::WorkRamRead, "WRAM Reg Read"),
    (luna_api::EventCategory::WorkRamWrite, "WRAM Reg Write"),
    (luna_api::EventCategory::Irq, "IRQ"),
    (luna_api::EventCategory::Nmi, "NMI"),
    (
        luna_api::EventCategory::MarkedBreakpoint,
        "Marked Breakpoints",
    ),
];

/// A category checkbox with its color swatch. Returns an action when toggled.
fn filter_category_check(
    ui: &mut egui::Ui,
    cfg: &luna_api::EventViewerConfig,
    cat: luna_api::EventCategory,
    label: &str,
) -> Option<EventViewerAction> {
    let mut on = cfg.visible[cat.index()];
    let mut action = None;
    ui.horizontal(|ui| {
        let (r, g, b) = cat.color();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(2),
            egui::Color32::from_rgb(r, g, b),
        );
        if ui.checkbox(&mut on, label).changed() {
            action = Some(EventViewerAction::Category(cat.index(), on));
        }
    });
    action
}

/// The Event Viewer filter side-panel (Mesen2 layout): three grouped boxes of
/// color-swatched checkboxes (PPU writes / Other events / DMA channels), then
/// the previous-frame + list-view toggles and Select/Deselect-all buttons.
/// `show_list` is the panel-local list-view flag (mutated in place). Returns a
/// config-mutation action if any checkbox/button changed this frame.
fn event_filter_panel(
    ui: &mut egui::Ui,
    cfg: &luna_api::EventViewerConfig,
    show_list: &mut bool,
) -> Option<EventViewerAction> {
    let mut action = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(egui::RichText::new("PPU Register Writes").strong());
        for (cat, label) in FILTER_PPU_WRITES {
            if let Some(a) = filter_category_check(ui, cfg, cat, label) {
                action = Some(a);
            }
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Other events").strong());
        for (cat, label) in FILTER_OTHER {
            if let Some(a) = filter_category_check(ui, cfg, cat, label) {
                action = Some(a);
            }
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("DMA Filters").strong());
        for ch in 0..8usize {
            let mut on = cfg.show_dma_channels[ch];
            if ui.checkbox(&mut on, format!("Channel {ch}")).changed() {
                action = Some(EventViewerAction::DmaChannel(ch, on));
            }
        }
        ui.add_space(8.0);
        ui.separator();
        let mut prev = cfg.show_previous_frame;
        if ui
            .checkbox(&mut prev, "Show previous frame events")
            .changed()
        {
            action = Some(EventViewerAction::PreviousFrame(prev));
        }
        ui.checkbox(show_list, "Show list view");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Select all").clicked() {
                action = Some(EventViewerAction::All(true));
            }
            if ui.button("Deselect all").clicked() {
                action = Some(EventViewerAction::All(false));
            }
        });
    });
    action
}

/// The Event Viewer list view (Mesen2 columns: Scanline | Cycle | PC | Type |
/// Address, with a category color swatch leading each row). Capped to a few
/// hundred rows so a write-heavy frame never blows the layout cost.
fn event_list_panel(ui: &mut egui::Ui, events: &[luna_api::EventViewerEvent]) {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("event_list_grid")
                .num_columns(6)
                .striped(true)
                .spacing([14.0, 3.0])
                .show(ui, |ui| {
                    for h in ["", "Scanline", "Cycle", "PC", "Type", "Address"] {
                        ui.label(egui::RichText::new(h).strong().small());
                    }
                    ui.end_row();
                    for ev in events.iter().take(400) {
                        // Color swatch.
                        let (r, g, b) = ev.category.color();
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            egui::CornerRadius::same(2),
                            egui::Color32::from_rgb(r, g, b),
                        );
                        ui.monospace(format!("{}", ev.scanline));
                        ui.monospace(format!("{}", ev.cycle));
                        ui.monospace(format!("${:06X}", ev.pc));
                        // Type: register name (or category label) + R/W marker.
                        let rw = match ev.category {
                            luna_api::EventCategory::PpuRead
                            | luna_api::EventCategory::ApuRead
                            | luna_api::EventCategory::CpuRead
                            | luna_api::EventCategory::WorkRamRead => " (R)",
                            luna_api::EventCategory::Nmi
                            | luna_api::EventCategory::Irq
                            | luna_api::EventCategory::MarkedBreakpoint => "",
                            _ => " (W)",
                        };
                        let name =
                            luna_api::register_name(ev.addr).unwrap_or_else(|| ev.category.label());
                        ui.monospace(format!("{name}{rw}"));
                        // Address: $AAAA plus register name when known.
                        let addr_label = match luna_api::register_name(ev.addr) {
                            Some(n) => format!("${:04X} {n}", ev.addr),
                            None => format!("${:04X}", ev.addr),
                        };
                        ui.monospace(addr_label);
                        ui.end_row();
                    }
                });
        });
}

/// Event Viewer — the Mesen2 layout: the event overlay (composited over the
/// live framebuffer in `build_panel_snapshot`) in the centre, a grouped filter
/// side-panel on the right, and an optional decoded-event list at the bottom.
/// Config mutations are returned as a [`PanelNav::EventViewer`] action (the
/// body is read-only; the main loop applies it to `event_config_mut`).
pub(crate) fn event_viewer_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(cfg) = snap.event_config.as_ref() else {
        ui.label(egui::RichText::new("(no ROM loaded — open while a game runs)").weak());
        return None;
    };
    let mut action: Option<EventViewerAction> = None;

    // "Show list view" is panel-local UI state, kept in egui memory.
    let list_id = egui::Id::new("event_viewer_show_list");
    let mut show_list = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(list_id).unwrap_or(true));

    // Filter side-panel (right).
    egui::Panel::right("event_filter_panel")
        .resizable(true)
        .default_size(190.0)
        .show_inside(ui, |ui| {
            if let Some(a) = event_filter_panel(ui, cfg, &mut show_list) {
                action = Some(a);
            }
        });
    ui.ctx().data_mut(|d| d.insert_temp(list_id, show_list));

    // List view (bottom), when enabled.
    if show_list {
        egui::Panel::bottom("event_list_panel")
            .resizable(true)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                if let Some(events) = snap.event_list.as_ref() {
                    event_list_panel(ui, events);
                } else {
                    ui.label(egui::RichText::new("(no events)").weak());
                }
            });
    }

    // Overlay image fills the remaining central area.
    egui::CentralPanel::default().show_inside(ui, |ui| {
        let Some((rgba, w, h)) = snap.event_overlay.as_ref() else {
            ui.label(egui::RichText::new("(running…)").weak());
            return;
        };
        let img = egui::ColorImage::from_rgba_unmultiplied([*w, *h], rgba);
        let tex = ui
            .ctx()
            .load_texture("event_view", img, egui::TextureOptions::NEAREST);
        // Keep the handle alive past this paint (same reason as the Tilemap view).
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("event_tex_keepalive"), tex.clone()));
        // Fit the buffer to the available width (capped at ~1.1×).
        let scale = (ui.available_width().max(200.0) / *w as f32).min(1.1);
        let size = egui::vec2(*w as f32 * scale, *h as f32 * scale);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for cat in EVENT_LEGEND {
                let (r, g, b) = cat.color();
                ui.label(
                    egui::RichText::new(format!("■ {}", cat.label()))
                        .color(egui::Color32::from_rgb(r, g, b))
                        .small(),
                );
            }
        });
    });

    action.map(PanelNav::EventViewer)
}
