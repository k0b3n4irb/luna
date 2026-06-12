//! egui overlay rendered on top of the pixels surface.
//!
//! Architecture:
//!   - winit owns the window + events.
//!   - pixels owns the wgpu device + a surface texture that renders
//!     the 256×224 SNES framebuffer with nearest-neighbour upscaling.
//!   - egui (this module) renders UI primitives via `egui-wgpu` as a
//!     second pass on the SAME wgpu surface, so the menu bar floats
//!     over the top of the game image with native-quality text and
//!     proper hit-testing.
//!
//! Inspired by `pixels/examples/minimal-egui`.

use egui_wgpu::ScreenDescriptor;
use egui_wgpu::wgpu;
use winit::event::WindowEvent;
use winit::window::Window;

/// User-driven menu commands the event loop dispatches into [`crate::LunaApp`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MenuAction {
    OpenRom,
    Quit,
    PauseToggle,
    Reset,
    ToggleInputConfig,
    StartRebind(crate::input::SnesButton),
    StartRebindHotkey(crate::input::Hotkey),
    TakeScreenshot,
    SaveBindings,
    // Debug panels (api-first: data comes from `luna_api::Emulator`).
    ToggleCpuState,
    ToggleSpc700,
    ToggleSprites,
    ToggleMemory,
    MemPagePrev,
    MemPageNext,
    MemBankToggle,
}

/// Per-frame snapshot of `luna-api` introspection, built by `LunaApp` only
/// when a debug panel is open (closed panels cost nothing). All data is
/// pulled through `luna_api::Emulator` — the GUI never touches `luna-core`.
#[derive(Default)]
pub(crate) struct DebugSnapshot {
    pub cpu: Option<luna_api::CpuState>,
    pub spc700: Option<luna_api::Spc700State>,
    pub sprites: Option<Vec<luna_api::SpriteInfo>>,
    /// `(bank, offset, bytes)` for the hex viewer.
    pub memory: Option<(u8, u16, Vec<u8>)>,
}

/// State the egui overlay reads to drive its widgets — passed in by
/// `LunaApp` on every frame.
pub(crate) struct UiState<'a> {
    pub paused: bool,
    pub rom_title: Option<String>,
    pub show_input_config: bool,
    pub key_bindings: &'a crate::input::KeyBindings,
    /// Which debug panels are open + their snapshotted data.
    pub show_cpu_state: bool,
    pub show_spc700: bool,
    pub show_sprites: bool,
    pub show_memory: bool,
    /// When `Some`, the input modal is waiting on the user to press a
    /// key to rebind the named SNES button.
    pub pending_rebind: Option<crate::input::SnesButton>,
    /// When `Some`, the input modal is waiting on a key to rebind the
    /// named hotkey (screenshot, …).
    pub pending_hotkey_rebind: Option<crate::input::Hotkey>,
    /// Last screenshot filename, shown briefly in the menu bar.
    pub screenshot_status: Option<String>,
}

/// All the egui plumbing wired up against pixels' wgpu device.
pub(crate) struct UiOverlay {
    ctx: egui::Context,
    winit_state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
}

impl UiOverlay {
    pub(crate) fn new(
        window: &Window,
        device: &wgpu::Device,
        output_format: wgpu::TextureFormat,
    ) -> Self {
        let ctx = egui::Context::default();
        install_dark_theme(&ctx);
        let viewport_id = ctx.viewport_id();
        let winit_state =
            egui_winit::State::new(ctx.clone(), viewport_id, window, None, None, None);
        let renderer =
            egui_wgpu::Renderer::new(device, output_format, egui_wgpu::RendererOptions::default());
        Self {
            ctx,
            winit_state,
            renderer,
        }
    }

    /// Forward a winit event to egui. Returns `true` if egui wants the
    /// event consumed (the main loop should skip its own handling).
    pub(crate) fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.winit_state.on_window_event(window, event);
        response.consumed
    }

    /// Build the frame's egui content + render it to `target_view`
    /// over the same wgpu command encoder that pixels used.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render<F>(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        pixels_per_point: f32,
        screen_size_px: (u32, u32),
        state: &UiState<'_>,
        mut emit: F,
    ) where
        F: FnMut(MenuAction),
    {
        let raw_input = self.winit_state.take_egui_input(window);
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            draw_menu_bar(ui.ctx(), state, &mut emit);
            if state.show_input_config {
                draw_input_config(ui.ctx(), state, &mut emit);
            }
            // Debug views live in their own native OS windows (see
            // `crate::debug_window`), not in this overlay — so they can be
            // dragged anywhere on the desktop, outside the game window.
        });
        self.winit_state
            .handle_platform_output(window, full_output.platform_output);
        let paint_jobs = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen = ScreenDescriptor {
            size_in_pixels: [screen_size_px.0, screen_size_px.1],
            pixels_per_point,
        };
        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        self.renderer
            .update_buffers(device, queue, encoder, &paint_jobs, &screen);
        let mut rpass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("luna-egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        self.renderer.render(&mut rpass, &paint_jobs, &screen);
        drop(rpass);
    }
}

#[allow(deprecated)]
pub(crate) fn install_dark_theme(ctx: &egui::Context) {
    use egui::{Color32, Stroke, Visuals, epaint::Shadow};
    let mut visuals = Visuals::dark();
    // Luna palette — quiet purple-blue accent on near-black surfaces.
    let accent = Color32::from_rgb(140, 110, 255);
    let surface = Color32::from_rgb(18, 18, 24);
    let surface_alt = Color32::from_rgb(26, 26, 36);
    let text = Color32::from_rgb(222, 222, 232);
    let border = Color32::from_rgb(48, 48, 60);

    visuals.window_fill = surface;
    visuals.panel_fill = surface;
    visuals.faint_bg_color = surface_alt;
    visuals.extreme_bg_color = Color32::from_rgb(10, 10, 16);
    visuals.override_text_color = Some(text);
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent.linear_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, accent);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 36, 48);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(28, 28, 38);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 50, 68);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(40, 40, 56);
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent.linear_multiply(0.6);
    visuals.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(160),
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    visuals.menu_corner_radius = egui::CornerRadius::same(6);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    ctx.set_style(style);
}

fn draw_input_config<F: FnMut(MenuAction)>(ctx: &egui::Context, state: &UiState<'_>, emit: &mut F) {
    use crate::input::{Hotkey, SnesButton};
    egui::Window::new("Controller bindings")
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .max_height(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "Click a row's button to rebind it. Defaults match the Mesen2 \
                     \"Arrow keys\" preset.",
                )
                .color(egui::Color32::from_rgb(160, 160, 180)),
            );
            ui.add_space(8.0);
            // Scroll so the Hotkeys section below the 12 pad rows stays
            // reachable on short windows.
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .show(ui, |ui| {
                    egui::Grid::new("luna-bindings-grid")
                        .num_columns(3)
                        .spacing([16.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for &button in &SnesButton::ALL {
                                ui.label(egui::RichText::new(button.label()).strong());
                                let key = state.key_bindings.get(button);
                                let label = if state.pending_rebind == Some(button) {
                                    "Press a key…".to_string()
                                } else {
                                    format!("{key:?}")
                                };
                                if ui
                                    .button(egui::RichText::new(label).monospace())
                                    .on_hover_text("Click to rebind")
                                    .clicked()
                                {
                                    emit(MenuAction::StartRebind(button));
                                }
                                ui.allocate_space(egui::vec2(1.0, 1.0));
                                ui.end_row();
                            }
                        });
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Hotkeys").strong());
                    ui.add_space(4.0);
                    egui::Grid::new("luna-hotkeys-grid")
                        .num_columns(3)
                        .spacing([16.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for &hotkey in &Hotkey::ALL {
                                ui.label(egui::RichText::new(hotkey.label()).strong());
                                let key = state.key_bindings.get_hotkey(hotkey);
                                let label = if state.pending_hotkey_rebind == Some(hotkey) {
                                    "Press a key…".to_string()
                                } else {
                                    format!("{key:?}")
                                };
                                if ui
                                    .button(egui::RichText::new(label).monospace())
                                    .on_hover_text("Click to rebind")
                                    .clicked()
                                {
                                    emit(MenuAction::StartRebindHotkey(hotkey));
                                }
                                ui.allocate_space(egui::vec2(1.0, 1.0));
                                ui.end_row();
                            }
                        });
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save & close").clicked() {
                    emit(MenuAction::SaveBindings);
                    emit(MenuAction::ToggleInputConfig);
                }
                if ui.button("Close").clicked() {
                    emit(MenuAction::ToggleInputConfig);
                }
            });
        });
}

/// Body of the CPU-state debug view (65c816 register file + decoded flags).
pub(crate) fn cpu_state_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(c) = snap.cpu.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    egui::Grid::new("cpu_regs").num_columns(2).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.monospace(k);
            ui.monospace(v);
            ui.end_row();
        };
        row(ui, "A", format!("{:04X}", c.a));
        row(ui, "X", format!("{:04X}", c.x));
        row(ui, "Y", format!("{:04X}", c.y));
        row(ui, "SP", format!("{:04X}", c.sp));
        row(ui, "DP", format!("{:04X}", c.dp));
        row(ui, "PB:PC", format!("{:02X}:{:04X}", c.pb, c.pc));
        row(ui, "DB", format!("{:02X}", c.db));
    });
    ui.separator();
    let p = c.p;
    let f = |bit: u8, name: char| {
        if p & bit != 0 {
            name.to_ascii_uppercase()
        } else {
            name.to_ascii_lowercase()
        }
    };
    ui.monospace(format!(
        "P={:02X}  {}{}{}{}{}{}{}{}  E={}",
        p,
        f(0x80, 'N'),
        f(0x40, 'V'),
        f(0x20, 'M'),
        f(0x10, 'X'),
        f(0x08, 'D'),
        f(0x04, 'I'),
        f(0x02, 'Z'),
        f(0x01, 'C'),
        u8::from(c.e),
    ));
    if c.stopped {
        ui.colored_label(egui::Color32::RED, "STOPPED");
    }
    if c.waiting {
        ui.colored_label(egui::Color32::YELLOW, "WAITING (WAI)");
    }
}

/// Body of the SPC700 (audio CPU) debug view — register file + flags.
pub(crate) fn spc700_body(ui: &mut egui::Ui, snap: &DebugSnapshot) {
    let Some(c) = snap.spc700.as_ref() else {
        ui.label("(no ROM loaded)");
        return;
    };
    egui::Grid::new("spc_regs").num_columns(2).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.monospace(k);
            ui.monospace(v);
            ui.end_row();
        };
        row(ui, "A", format!("{:02X}", c.a));
        row(ui, "X", format!("{:02X}", c.x));
        row(ui, "Y", format!("{:02X}", c.y));
        row(
            ui,
            "YA",
            format!("{:04X}", (u16::from(c.y) << 8) | u16::from(c.a)),
        );
        row(ui, "SP", format!("01{:02X}", c.sp));
        row(ui, "PC", format!("{:04X}", c.pc));
    });
    ui.separator();
    let p = c.psw;
    let f = |bit: u8, name: char| {
        if p & bit != 0 {
            name.to_ascii_uppercase()
        } else {
            name.to_ascii_lowercase()
        }
    };
    // SPC700 PSW: N V P B H I Z C.
    ui.monospace(format!(
        "PSW={:02X}  {}{}{}{}{}{}{}{}",
        p,
        f(0x80, 'N'),
        f(0x40, 'V'),
        f(0x20, 'P'),
        f(0x10, 'B'),
        f(0x08, 'H'),
        f(0x04, 'I'),
        f(0x02, 'Z'),
        f(0x01, 'C'),
    ));
    if c.stopped {
        ui.colored_label(egui::Color32::RED, "STOPPED");
    }
    if c.sleeping {
        ui.colored_label(egui::Color32::YELLOW, "SLEEPING");
    }
}

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

/// Body of the memory hex-dump view. Returns the toolbar action (page / bank
/// navigation) clicked this frame, if any, so the caller can emit it without
/// the body needing a mutable `emit` borrow.
pub(crate) fn memory_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<MenuAction> {
    let Some((bank, offset, bytes)) = snap.memory.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    let (bank, offset) = (*bank, *offset);
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("\u{25c0} \u{2212}256").clicked() {
            action = Some(MenuAction::MemPagePrev);
        }
        if ui.button("+256 \u{25b6}").clicked() {
            action = Some(MenuAction::MemPageNext);
        }
        if ui.button(format!("bank ${bank:02X}")).clicked() {
            action = Some(MenuAction::MemBankToggle);
        }
    });
    ui.separator();
    for (r, chunk) in bytes.chunks(16).enumerate() {
        let addr = offset.wrapping_add((r * 16) as u16);
        let hex = chunk
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        ui.monospace(format!("{bank:02X}:{addr:04X}  {hex:<48}{ascii}"));
    }
    action
}

#[allow(deprecated)]
fn draw_menu_bar<F: FnMut(MenuAction)>(ctx: &egui::Context, state: &UiState<'_>, emit: &mut F) {
    egui::TopBottomPanel::top("luna-menu")
        .exact_height(28.0)
        .show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM…").clicked() {
                        emit(MenuAction::OpenRom);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        emit(MenuAction::Quit);
                        ui.close();
                    }
                });
                ui.menu_button("Emulation", |ui| {
                    let label = if state.paused { "Resume" } else { "Pause" };
                    if ui.button(label).clicked() {
                        emit(MenuAction::PauseToggle);
                        ui.close();
                    }
                    if ui.button("Reset").clicked() {
                        emit(MenuAction::Reset);
                        ui.close();
                    }
                });
                ui.menu_button("Input", |ui| {
                    if ui.button("Configure controller…").clicked() {
                        emit(MenuAction::ToggleInputConfig);
                        ui.close();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    let key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::Screenshot);
                    if ui.button(format!("Take screenshot ({key:?})")).clicked() {
                        emit(MenuAction::TakeScreenshot);
                        ui.close();
                    }
                });
                ui.menu_button("Debug", |ui| {
                    if ui
                        .selectable_label(state.show_cpu_state, "CPU state (65c816)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuState);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc700, "SPC700 state (audio CPU)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_sprites, "Sprites (OAM)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSprites);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_memory, "Memory (hex)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleMemory);
                        ui.close();
                    }
                });
                if let Some(title) = state.rom_title.as_deref() {
                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new(title)
                            .color(egui::Color32::from_rgb(150, 150, 170))
                            .italics(),
                    );
                }
                if let Some(status) = state.screenshot_status.as_deref() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(status).color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                }
            });
        });
}
