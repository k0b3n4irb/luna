//! Separate native OS windows for the debug panels.
//!
//! Unlike `egui::Window`s (clamped to their host window's rect), each
//! debug panel here is a **real winit window** with its own wgpu surface
//! and egui context, so it can be dragged anywhere on the desktop —
//! outside the game window — like the ness debugger.
//!
//! The game window keeps the `pixels` stack untouched; these debug
//! windows run on an independent wgpu device created lazily on first
//! open. All emulator data still comes through `luna_api` (api-first):
//! `LunaApp` builds a [`crate::ui::DebugSnapshot`] and hands it to
//! [`DebugWindows::render`].

use std::collections::HashMap;
use std::sync::Arc;

use egui_wgpu::ScreenDescriptor;
use egui_wgpu::wgpu;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{ResizeDirection, Window, WindowAttributes, WindowId};

use crate::ui::{self, DebugSnapshot, PanelNav};

/// Height (logical px) of the custom egui title bar drawn at the top of
/// each borderless debug window.
const TITLE_BAR_H: f32 = 26.0;
/// Window height (logical px) when collapsed to just the title bar.
const COLLAPSED_H: f32 = TITLE_BAR_H;

/// Which debug view a window shows.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub(crate) enum DebugPanel {
    Cpu,
    CpuMemory,
    CpuDisasm,
    Spc700,
    Spc700Memory,
    Spc700Disasm,
    Sprites,
    Registers,
    Palette,
}

impl DebugPanel {
    const fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU — 65c816",
            Self::CpuMemory => "CPU memory",
            Self::CpuDisasm => "CPU disassembly",
            Self::Spc700 => "SPC700 — audio CPU",
            Self::Spc700Memory => "SPC700 memory",
            Self::Spc700Disasm => "SPC700 disassembly",
            Self::Sprites => "Sprites (OAM)",
            Self::Registers => "Registers",
            Self::Palette => "Palette (CGRAM)",
        }
    }

    /// Default window size (logical px) — sized to each panel's content.
    const fn default_size(self) -> (u32, u32) {
        match self {
            Self::Cpu => (250, 340),
            Self::Spc700 => (250, 320),
            Self::CpuMemory | Self::Spc700Memory => (660, 420),
            Self::Spc700Disasm => (420, 440),
            Self::CpuDisasm => (460, 440),
            Self::Sprites => (340, 460),
            Self::Registers => (360, 520),
            Self::Palette => (380, 440),
        }
    }
}

/// The shared, lazily-initialised wgpu device for all debug windows.
struct Gpu {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// One native debug window: its winit window, wgpu surface, and the
/// per-window egui state needed to paint it.
struct DebugWin {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    panel: DebugPanel,
    /// Folded to just the title bar (the title-bar triangle toggles it).
    collapsed: bool,
    /// Logical inner height to restore when un-collapsing.
    expanded_h: f32,
}

/// Owns every open debug window plus their shared wgpu device.
pub(crate) struct DebugWindows {
    instance: wgpu::Instance,
    gpu: Option<Gpu>,
    wins: HashMap<WindowId, DebugWin>,
    by_panel: HashMap<DebugPanel, WindowId>,
}

impl DebugWindows {
    pub(crate) fn new() -> Self {
        Self {
            instance: wgpu::Instance::default(),
            gpu: None,
            wins: HashMap::new(),
            by_panel: HashMap::new(),
        }
    }

    /// Is the given panel's window currently open?
    pub(crate) fn is_open(&self, panel: DebugPanel) -> bool {
        self.by_panel.contains_key(&panel)
    }

    /// Does `id` belong to one of our debug windows?
    pub(crate) fn owns(&self, id: WindowId) -> bool {
        self.wins.contains_key(&id)
    }

    /// The panel a debug window renders, if `id` is one of ours.
    pub(crate) fn panel_of(&self, id: WindowId) -> Option<DebugPanel> {
        self.wins.get(&id).map(|w| w.panel)
    }

    /// Open the panel if closed, close it if open (the menu toggle).
    pub(crate) fn toggle(&mut self, event_loop: &ActiveEventLoop, panel: DebugPanel) {
        if let Some(&id) = self.by_panel.get(&panel) {
            self.wins.remove(&id);
            self.by_panel.remove(&panel);
        } else {
            self.open(event_loop, panel);
        }
    }

    /// Ask each open debug window to repaint (called from `about_to_wait`
    /// so live register/memory values keep updating).
    pub(crate) fn request_redraw_all(&self) {
        for win in self.wins.values() {
            win.window.request_redraw();
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, panel: DebugPanel) {
        let (w, h) = panel.default_size();
        let attrs = WindowAttributes::default()
            .with_title(format!("Luna — {}", panel.title()))
            // Borderless: the egui chrome (title bar + triangle + resize
            // grip) is drawn by us, ness-style. Dragging / resizing is
            // forwarded to the window manager from the egui widgets.
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(w, h))
            // Allow folding down to just the title bar (some WMs otherwise
            // clamp a borderless window to a larger minimum).
            .with_min_inner_size(LogicalSize::new(160.0_f32, TITLE_BAR_H));
        let window = match event_loop.create_window(attrs) {
            Ok(win) => Arc::new(win),
            Err(e) => {
                eprintln!("luna-gui: debug window create failed: {e}");
                return;
            }
        };
        let surface = match self.instance.create_surface(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("luna-gui: debug surface create failed: {e}");
                return;
            }
        };
        // Create the wgpu device on first open, picking an adapter that
        // supports this first surface.
        if self.gpu.is_none() {
            let adapter = match pollster::block_on(self.instance.request_adapter(
                &wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: Some(&surface),
                },
            )) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("luna-gui: debug adapter request failed: {e}");
                    return;
                }
            };
            let (device, queue) =
                match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("luna-debug"),
                    ..Default::default()
                })) {
                    Ok(dq) => dq,
                    Err(e) => {
                        eprintln!("luna-gui: debug device request failed: {e}");
                        return;
                    }
                };
            self.gpu = Some(Gpu {
                adapter,
                device,
                queue,
            });
        }
        let gpu = self.gpu.as_ref().expect("gpu just initialised");
        let size = window.inner_size();
        let Some(config) =
            surface.get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
        else {
            eprintln!("luna-gui: debug surface unsupported by adapter");
            return;
        };
        surface.configure(&gpu.device, &config);

        let egui_ctx = egui::Context::default();
        ui::install_dark_theme(&egui_ctx);
        let viewport_id = egui_ctx.viewport_id();
        let egui_state =
            egui_winit::State::new(egui_ctx.clone(), viewport_id, &window, None, None, None);
        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        let id = window.id();
        self.wins.insert(
            id,
            DebugWin {
                window,
                surface,
                config,
                egui_ctx,
                egui_state,
                renderer,
                panel,
                collapsed: false,
                expanded_h: h as f32,
            },
        );
        self.by_panel.insert(panel, id);
    }

    /// Route a window event to the matching debug window (caller has
    /// already confirmed `owns(id)`). Handles close + resize internally;
    /// the menu checkmark just reads [`Self::is_open`] afterwards.
    pub(crate) fn on_window_event(&mut self, id: WindowId, event: &WindowEvent) {
        let Some(win) = self.wins.get_mut(&id) else {
            return;
        };
        let _ = win.egui_state.on_window_event(&win.window, event);
        match event {
            WindowEvent::CloseRequested => {
                let panel = win.panel;
                self.wins.remove(&id);
                self.by_panel.remove(&panel);
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_ref() {
                    win.config.width = size.width.max(1);
                    win.config.height = size.height.max(1);
                    win.surface.configure(&gpu.device, &win.config);
                }
            }
            _ => {}
        }
    }

    /// Repaint one debug window with the freshest snapshot. Returns the
    /// signed byte delta a memory panel's nav toolbar requested (if any) and
    /// whether the title-bar ✕ asked to close the window.
    pub(crate) fn render(
        &mut self,
        id: WindowId,
        snap: &DebugSnapshot,
    ) -> (Option<PanelNav>, bool) {
        let mut nav: Option<PanelNav> = None;
        // Disjoint field borrows: `gpu` (immutable) and `wins` (mutable).
        let Some(gpu) = self.gpu.as_ref() else {
            return (nav, false);
        };
        let Some(win) = self.wins.get_mut(&id) else {
            return (nav, false);
        };

        // Reconcile the surface to the window's current size BEFORE acquiring
        // a frame (no `SurfaceTexture` is alive here, so `configure` is safe —
        // configuring while one is alive panics). This also covers compositors
        // that apply `request_inner_size` synchronously and emit no `Resized`
        // event (the collapse fold), where the surface would otherwise stay at
        // the old size.
        let phys = win.window.inner_size();
        let (pw, ph) = (phys.width.max(1), phys.height.max(1));
        if (pw, ph) != (win.config.width, win.config.height) {
            win.config.width = pw;
            win.config.height = ph;
            win.surface.configure(&gpu.device, &win.config);
        }

        // Acquire the swapchain image, reconfiguring and retrying once if the
        // surface went stale so we never present a blank frame.
        let frame = match win.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                win.surface.configure(&gpu.device, &win.config);
                match win.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    _ => return (nav, false),
                }
            }
            _ => return (nav, false),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let raw_input = win.egui_state.take_egui_input(&win.window);
        let panel = win.panel;
        let collapsed = win.collapsed;
        let window = win.window.clone();
        let mut want_close = false;
        let mut want_toggle = false;
        let full_output = win.egui_ctx.run_ui(raw_input, |ui| {
            // One custom frame filling the borderless window (egui's
            // `custom_window_frame` example pattern).
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(20, 20, 28))
                .show(ui, |ui| {
                    let app = ui.max_rect();
                    ui.expand_to_include_rect(app);

                    // Title bar. When collapsed it fills the whole window so
                    // there is never a blank strip (some WMs clamp the
                    // collapsed height up); the title content stays in the
                    // top `TITLE_BAR_H` strip either way.
                    let bar_fill_h = if collapsed { app.height() } else { TITLE_BAR_H };
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(app.min, egui::vec2(app.width(), bar_fill_h)),
                        0.0,
                        egui::Color32::from_rgb(30, 30, 42),
                    );
                    let bar =
                        egui::Rect::from_min_size(app.min, egui::vec2(app.width(), TITLE_BAR_H));

                    // Collapse triangle (left): ▶ when collapsed, ▼ otherwise.
                    let tri =
                        egui::Rect::from_min_size(bar.min, egui::vec2(TITLE_BAR_H, TITLE_BAR_H));
                    let tri_resp = ui.interact(tri, ui.id().with("collapse"), egui::Sense::click());
                    let tri_col = if tri_resp.hovered() {
                        egui::Color32::from_rgb(150, 120, 255)
                    } else {
                        egui::Color32::from_rgb(205, 205, 220)
                    };
                    let c = tri.center();
                    let pts = if collapsed {
                        vec![
                            c + egui::vec2(-3.0, -5.0),
                            c + egui::vec2(4.0, 0.0),
                            c + egui::vec2(-3.0, 5.0),
                        ]
                    } else {
                        vec![
                            c + egui::vec2(-5.0, -3.0),
                            c + egui::vec2(5.0, -3.0),
                            c + egui::vec2(0.0, 4.0),
                        ]
                    };
                    ui.painter().add(egui::Shape::convex_polygon(
                        pts,
                        tri_col,
                        egui::Stroke::NONE,
                    ));
                    if tri_resp.clicked() {
                        want_toggle = true;
                    }

                    // Close ✕ (right).
                    let close = egui::Rect::from_min_size(
                        egui::pos2(bar.max.x - TITLE_BAR_H, bar.min.y),
                        egui::vec2(TITLE_BAR_H, TITLE_BAR_H),
                    );
                    let close_resp =
                        ui.interact(close, ui.id().with("close"), egui::Sense::click());
                    let close_col = if close_resp.hovered() {
                        egui::Color32::from_rgb(240, 120, 120)
                    } else {
                        egui::Color32::from_rgb(205, 205, 220)
                    };
                    let cc = close.center();
                    let r = 4.0;
                    let s = egui::Stroke::new(1.6, close_col);
                    ui.painter()
                        .line_segment([cc + egui::vec2(-r, -r), cc + egui::vec2(r, r)], s);
                    ui.painter()
                        .line_segment([cc + egui::vec2(r, -r), cc + egui::vec2(-r, r)], s);
                    if close_resp.clicked() {
                        want_close = true;
                    }

                    // Title text.
                    ui.painter().text(
                        egui::pos2(tri.max.x + 2.0, bar.center().y),
                        egui::Align2::LEFT_CENTER,
                        panel.title(),
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_rgb(228, 228, 238),
                    );

                    // Drag the OS window via the bar (between the buttons).
                    let drag = egui::Rect::from_min_max(
                        egui::pos2(tri.max.x, bar.min.y),
                        egui::pos2(close.min.x, bar.max.y),
                    );
                    let drag_resp =
                        ui.interact(drag, ui.id().with("drag"), egui::Sense::click_and_drag());
                    if drag_resp.drag_started() {
                        let _ = window.drag_window();
                    }

                    if !collapsed {
                        // Body in its own child UI under the title bar, kept
                        // clear of the bottom-right resize grip.
                        let content = egui::Rect::from_min_max(
                            egui::pos2(app.min.x + 8.0, bar.max.y + 6.0),
                            egui::pos2(app.max.x - 8.0, app.max.y - 18.0),
                        );
                        let mut body = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(content)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        egui::ScrollArea::both().show(&mut body, |ui| match panel {
                            DebugPanel::Cpu => ui::cpu_state_body(ui, snap),
                            DebugPanel::Spc700 => ui::spc700_body(ui, snap),
                            DebugPanel::Sprites => ui::sprites_body(ui, snap),
                            DebugPanel::Registers => ui::registers_body(ui, snap),
                            DebugPanel::Palette => ui::palette_body(ui, snap),
                            DebugPanel::Spc700Disasm => nav = ui::spc700_disasm_body(ui, snap),
                            DebugPanel::CpuDisasm => nav = ui::cpu_disasm_body(ui, snap),
                            DebugPanel::CpuMemory => nav = ui::cpu_memory_body(ui, snap),
                            DebugPanel::Spc700Memory => {
                                nav = ui::spc700_memory_body(ui, snap);
                            }
                        });

                        // Blue resize grip (bottom-right), ness-style.
                        let g = 16.0;
                        let grip = egui::Rect::from_min_max(
                            egui::pos2(app.max.x - g, app.max.y - g),
                            app.max,
                        );
                        let grip_resp =
                            ui.interact(grip, ui.id().with("resize"), egui::Sense::drag());
                        let grip_col = if grip_resp.hovered() || grip_resp.dragged() {
                            egui::Color32::from_rgb(96, 140, 225)
                        } else {
                            egui::Color32::from_rgb(56, 86, 150)
                        };
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                app.max,
                                egui::pos2(app.max.x, app.max.y - g),
                                egui::pos2(app.max.x - g, app.max.y),
                            ],
                            grip_col,
                            egui::Stroke::NONE,
                        ));
                        if grip_resp.drag_started() {
                            let _ = window.drag_resize_window(ResizeDirection::SouthEast);
                        }
                    }
                });
        });
        win.egui_state
            .handle_platform_output(&win.window, full_output.platform_output);

        let ppp = full_output.pixels_per_point;
        let paint_jobs = win.egui_ctx.tessellate(full_output.shapes, ppp);
        let screen = ScreenDescriptor {
            size_in_pixels: [win.config.width, win.config.height],
            pixels_per_point: ppp,
        };
        for (tid, delta) in &full_output.textures_delta.set {
            win.renderer
                .update_texture(&gpu.device, &gpu.queue, *tid, delta);
        }
        for tid in &full_output.textures_delta.free {
            win.renderer.free_texture(tid);
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luna-debug-encoder"),
            });
        win.renderer
            .update_buffers(&gpu.device, &gpu.queue, &mut encoder, &paint_jobs, &screen);
        {
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("luna-debug-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // Surface format may be sRGB; this near-black
                            // matches the egui panel fill closely enough
                            // that any uncovered edge is invisible.
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.02,
                                g: 0.02,
                                b: 0.03,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    multiview_mask: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            win.renderer.render(&mut rpass, &paint_jobs, &screen);
        }
        gpu.queue.submit(Some(encoder.finish()));
        win.window.pre_present_notify();
        frame.present();

        // Fold / unfold by resizing the OS window — done AFTER `present()`, so
        // the just-acquired `SurfaceTexture` is already consumed and we never
        // reconfigure a live surface (that panics and took the whole app down).
        // The next render reconciles the surface to the new size at its top.
        if want_toggle {
            win.collapsed = !win.collapsed;
            let scale = win.window.scale_factor() as f32;
            let logical_w = win.config.width as f32 / scale;
            let target_h = if win.collapsed {
                win.expanded_h = win.config.height as f32 / scale;
                COLLAPSED_H
            } else {
                win.expanded_h
            };
            let _ = win
                .window
                .request_inner_size(LogicalSize::new(logical_w, target_h));
        }

        (nav, want_close)
    }

    /// Close (and drop) the debug window for `id`, if present.
    pub(crate) fn close(&mut self, id: WindowId) {
        if let Some(win) = self.wins.remove(&id) {
            self.by_panel.remove(&win.panel);
        }
    }
}
