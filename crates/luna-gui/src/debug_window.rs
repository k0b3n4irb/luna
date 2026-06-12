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
use winit::window::{Window, WindowAttributes, WindowId};

use crate::ui::{self, DebugSnapshot, MenuAction};

/// Which debug view a window shows.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub(crate) enum DebugPanel {
    Cpu,
    Spc700,
    Sprites,
    Memory,
}

impl DebugPanel {
    const fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU — 65c816",
            Self::Spc700 => "SPC700 — audio CPU",
            Self::Sprites => "Sprites (OAM)",
            Self::Memory => "Memory (hex)",
        }
    }

    /// Default window size (logical px) — sized to each panel's content.
    const fn default_size(self) -> (u32, u32) {
        match self {
            Self::Cpu => (240, 300),
            Self::Spc700 => (240, 280),
            Self::Sprites => (340, 460),
            Self::Memory => (530, 380),
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
            .with_inner_size(LogicalSize::new(w, h));
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

    /// Repaint one debug window with the freshest snapshot. Returns any
    /// menu actions its body emitted (memory page / bank navigation).
    pub(crate) fn render(&mut self, id: WindowId, snap: &DebugSnapshot) -> Vec<MenuAction> {
        let mut actions: Vec<MenuAction> = Vec::new();
        // Disjoint field borrows: `gpu` (immutable) and `wins` (mutable).
        let Some(gpu) = self.gpu.as_ref() else {
            return actions;
        };
        let Some(win) = self.wins.get_mut(&id) else {
            return actions;
        };

        let frame = match win.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                win.surface.configure(&gpu.device, &win.config);
                return actions;
            }
            _ => return actions,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let raw_input = win.egui_state.take_egui_input(&win.window);
        let panel = win.panel;
        let full_output = win.egui_ctx.run_ui(raw_input, |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| match panel {
                DebugPanel::Cpu => ui::cpu_state_body(ui, snap),
                DebugPanel::Spc700 => ui::spc700_body(ui, snap),
                DebugPanel::Sprites => {
                    egui::ScrollArea::vertical().show(ui, |ui| ui::sprites_body(ui, snap));
                }
                DebugPanel::Memory => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(a) = ui::memory_body(ui, snap) {
                            actions.push(a);
                        }
                    });
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

        actions
    }
}
