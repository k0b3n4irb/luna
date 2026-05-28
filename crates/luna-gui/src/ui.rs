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
    SaveBindings,
}

/// State the egui overlay reads to drive its widgets — passed in by
/// `LunaApp` on every frame.
pub(crate) struct UiState<'a> {
    pub paused: bool,
    pub rom_title: Option<String>,
    pub show_input_config: bool,
    pub key_bindings: &'a crate::input::KeyBindings,
    /// When `Some`, the input modal is waiting on the user to press a
    /// key to rebind the named SNES button.
    pub pending_rebind: Option<crate::input::SnesButton>,
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
fn install_dark_theme(ctx: &egui::Context) {
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
    use crate::input::SnesButton;
    egui::Window::new("Controller bindings")
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
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
            egui::Grid::new("luna-bindings-grid")
                .num_columns(3)
                .spacing([16.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for &button in SnesButton::ALL.iter() {
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
                if let Some(title) = state.rom_title.as_deref() {
                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new(title)
                            .color(egui::Color32::from_rgb(150, 150, 170))
                            .italics(),
                    );
                }
            });
        });
}
