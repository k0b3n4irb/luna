//! Luna SNES emulator — desktop GUI entry point.
//!
//! Modern eframe-based interface: dark theme, drag-and-drop ROM
//! loading, scaled framebuffer display, side debug panel, and a
//! status bar. See `ARCHITECTURE.md` §3.2 ("standalone mode").

// On Windows we want to avoid the console flashing when double-clicking
// the .exe. The attribute is a no-op on other platforms.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use app::LunaApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Luna — SNES Emulator")
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([640.0, 480.0])
            .with_drag_and_drop(true)
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Luna",
        options,
        Box::new(|cc| {
            install_dark_theme(&cc.egui_ctx);
            Ok(Box::new(LunaApp::new()))
        }),
    )
}

/// Embedded application icon. For now a 1×1 placeholder — the
/// asset round-trip lives behind a build-time include later.
fn load_icon() -> egui::IconData {
    egui::IconData {
        rgba: vec![0x6E, 0x5A, 0xFF, 0xFF],
        width: 1,
        height: 1,
    }
}

fn install_dark_theme(ctx: &egui::Context) {
    use egui::{Color32, Stroke, Visuals, epaint::Shadow};

    let mut visuals = Visuals::dark();
    // Luna's accent palette — quiet purple/blue with a touch of life.
    let accent = Color32::from_rgb(110, 90, 255);
    let surface = Color32::from_rgb(18, 18, 24);
    let surface_alt = Color32::from_rgb(26, 26, 36);
    let text = Color32::from_rgb(220, 220, 230);

    visuals.window_fill = surface;
    visuals.panel_fill = surface;
    visuals.faint_bg_color = surface_alt;
    visuals.extreme_bg_color = Color32::from_rgb(10, 10, 16);
    visuals.override_text_color = Some(text);
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent.linear_multiply(0.4);
    visuals.selection.stroke = Stroke::new(1.0, accent);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(48, 48, 60));
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 36, 48);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 48, 64);
    visuals.widgets.active.bg_fill = accent;
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    ctx.set_style(style);
}
