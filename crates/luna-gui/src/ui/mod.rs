//! egui overlay rendered on top of the pixels surface.
//!
//! Architecture:
//!   - winit owns the window + events.
//!   - pixels owns the wgpu device, the surface and the 256×239 frame
//!     texture the SNES framebuffer is uploaded to.
//!   - egui (this module) renders via `egui-wgpu` on the SAME wgpu
//!     surface: the menu bar at the top, and the game frame itself,
//!     aspect-fit below the bar with nearest-neighbour sampling
//!     (pixels' own scaling renderer is not used).
//!
//! Inspired by `pixels/examples/minimal-egui`.

use egui_wgpu::ScreenDescriptor;
use egui_wgpu::wgpu;
use winit::event::WindowEvent;
use winit::window::Window;

mod breakpoints;
mod config;
mod cpu_state;
mod debugger;
mod disasm;
mod event_viewer;
mod memory;
mod memory_search;
mod memory_tools;
mod menu;
mod widgets;

pub(crate) use breakpoints::breakpoints_body;
pub(crate) use cpu_state::cpu_state_body;
pub(crate) use debugger::{
    DebuggerAction, DebuggerSettings, DebuggerView, WatchLine, debugger_body,
};
pub(crate) use disasm::{cpu_disasm_body, spc700_disasm_body};
pub(crate) use event_viewer::event_viewer_body;
pub(crate) use memory::{cpu_memory_body, spc700_memory_body};
pub(crate) use memory_search::memory_search_body;
pub(crate) use memory_tools::memory_tools_body;
pub(crate) use palette::palette_body;
pub(crate) use registers::registers_body;
pub(crate) use spc700::spc700_body;
pub(crate) use sprites::sprites_body;
pub(crate) use tilemap::tilemap_body;

use config::{draw_hotkey_config, draw_input_config};
use menu::{draw_force_mapper_prompt, draw_menu_bar};
mod palette;
mod registers;
mod spc700;
mod sprites;
mod tilemap;

/// User-driven menu commands the event loop dispatches into [`crate::LunaApp`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MenuAction {
    OpenRom,
    Quit,
    PauseToggle,
    Reset,
    ToggleInputConfig,
    ToggleHotkeyConfig,
    /// Arm a rebind for `(player, button)` — player 0 = P1, 1 = P2.
    StartRebind(usize, crate::input::SnesButton),
    StartRebindHotkey(crate::input::Hotkey),
    TakeScreenshot,
    SaveBindings,
    /// Reset one player's pad bindings to defaults.
    ResetBindings(usize),
    /// Apply a named layout preset (Arrows / WASD) to a player's pad.
    ApplyPreset(usize, crate::input::KeyPreset),
    ResetHotkeys,
    /// Save emulator state to numbered slot `1..=9` (also sets it current).
    SaveState(u8),
    /// Load emulator state from numbered slot `1..=9` (also sets it current).
    LoadState(u8),
    /// Set controller port `0`/`1` to a device (pad / mouse / super scope).
    SetPortDevice(u8, luna_api::PortDevice),
    /// Toggle borderless fullscreen (also on the F11 hotkey).
    ToggleFullscreen,
    /// Master volume slider (0..=100), applied live in the cpal callback.
    SetVolume(u8),
    /// Mute toggle (keeps the slider position).
    ToggleMute,
    /// Show or hide the on-screen frame-rate counter.
    ToggleShowFps,
    /// Settings ▸ Video Size: show the picture at this whole scale (1..=10).
    SetVideoScale(u8),
    /// Settings ▸ Video Filter.
    SetVideoFilter(luna_api::video::VideoFilter),
    /// Settings ▸ Video Filter ▸ Use bilinear interpolation.
    ToggleBilinear,
    /// Settings ▸ Aspect Ratio.
    SetAspectRatio(luna_api::video::AspectRatio),
    /// The Custom aspect ratio, in thousandths.
    SetCustomAspect(u32),
    // Debug panels (api-first: data comes from `luna_api::Emulator`).
    ToggleCpuState,
    ToggleCpuMemory,
    ToggleCpuDisasm,
    ToggleSpc700,
    ToggleSpc700Memory,
    ToggleSpc700Disasm,
    ToggleSprites,
    ToggleRegisters,
    TogglePalette,
    ToggleTilemap,
    ToggleEventViewer,
    ToggleBreakpoints,
    /// Open / close the Debugger window (Mesen2's).
    ToggleDebugger,
    /// Open / close the SPC Debugger window.
    ToggleSpcDebugger,
    ToggleMemoryTools,
    ToggleMemorySearch,
    /// Debugger: execute one instruction (pauses first if running).
    StepInstruction,
    /// Debugger: run to the next frame boundary (pauses first if running).
    StepFrame,
    /// Toggle joypad input recording (issue #83): start capturing, or stop
    /// and export the `frame:mask` script.
    ToggleInputRecording,
    /// Force a cartridge mapper on the next ROM load (issue #88); `None` =
    /// auto-detect. When a load is awaiting a mapper choice, this also retries
    /// it immediately.
    SetForcedMapper(Option<luna_api::MapperKind>),
    /// Dismiss the "couldn't detect the mapper" prompt without loading (issue #88).
    DismissForcePrompt,
    /// Reload the current ROM from disk in place (issue #93).
    ReloadRom,
    /// Toggle watch-mode auto-reload on ROM-file change (issue #93).
    ToggleAutoReload,
}

/// A navigation request a debug panel's toolbar emits this frame, applied
/// by `LunaApp` to the panel's cursor state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PanelNav {
    /// Toggle an exec breakpoint at this 24-bit address (disasm row click).
    DisasmToggleBreakpoint(u32),
    /// Remove one breakpoint by registry id (Breakpoints panel ✕).
    BpRemove(u32),
    /// Remove every breakpoint (Breakpoints panel "Clear all").
    BpClearAll,
    /// Add an exec breakpoint at this 24-bit PB:PC (panel form).
    BpAddExec(u32),
    /// Add a memory watchpoint `lo..=hi` firing on `(read, write)`.
    BpAddMem(u32, u32, bool, bool),
    /// Move a memory viewer's address cursor by a signed byte delta.
    MemAddr(i64),
    /// Re-anchor the disassembly at the live program counter.
    DisasmGotoPc,
    /// Set the disassembly start address (from the editable field).
    DisasmSetAddr(u32),
    /// Set the disassembly line count (from the editable field).
    DisasmSetLines(u16),
    /// Select which BG layer (0..3) the Tilemap Viewer renders.
    TilemapSetBg(usize),
    /// An Event Viewer filter-panel change to apply to `event_config_mut()`.
    EventViewer(EventViewerAction),
    /// What the Debugger window did this frame.
    Debugger(Vec<DebuggerAction>),
    /// What the Memory Tools window did this frame.
    MemoryTools(Vec<crate::memory_host::MemoryToolsAction>),
    /// What the Memory Search window did this frame.
    MemorySearch(Vec<crate::memory_host::MemorySearchAction>),
}

/// A change the Event Viewer's filter panel requests this frame, applied by
/// `LunaApp` to `em.event_config_mut()`. Mirrors the `PanelNav` pattern: the
/// read-only panel body cannot mutate the config directly, so it returns one
/// of these and the main loop applies it under the emulator lock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum EventViewerAction {
    /// Toggle one category's visibility (`visible[index] = on`).
    Category(usize, bool),
    /// Toggle one DMA channel's visibility (`show_dma_channels[ch] = on`).
    DmaChannel(usize, bool),
    /// Toggle the "show previous frame events" flag.
    PreviousFrame(bool),
    /// Set every category + DMA-channel flag to `on` (Select all / Deselect all).
    All(bool),
}

/// Per-frame snapshot of `luna-api` introspection, built by `LunaApp` only
/// when a debug panel is open (closed panels cost nothing). All data is
/// pulled through `luna_api::Emulator` — the GUI never touches `luna-core`.
#[derive(Default)]
pub(crate) struct DebugSnapshot {
    pub cpu: Option<luna_api::CpuState>,
    pub spc700: Option<luna_api::Spc700State>,
    pub sprites: Option<Vec<luna_api::SpriteInfo>>,
    /// `(addr24, bytes)` — CPU-bus hex viewer at a full 24-bit address.
    pub cpu_memory: Option<(u32, Vec<u8>)>,
    /// `(addr16, bytes)` — SPC700 ARAM hex viewer.
    pub spc_memory: Option<(u16, Vec<u8>)>,
    /// SPC700 disassembly lines from the current start address.
    pub spc_disasm: Option<Vec<luna_api::DisasmLine>>,
    /// The SPC700 disassembly line count (for the toolbar).
    pub spc_disasm_lines: u16,
    /// 65c816 disassembly lines from the current start address.
    pub cpu_disasm: Option<Vec<luna_api::DisasmLine>>,
    /// The CPU disassembly line count (for the toolbar).
    pub cpu_disasm_lines: u16,
    /// Full emulator snapshot for the Register Viewer (raw I/O values).
    pub registers: Option<luna_api::EmulatorState>,
    /// 256 raw BGR555 CGRAM entries for the Palette Viewer.
    pub palette: Option<Vec<u16>>,
    /// Decoded tilemap image for the Tilemap Viewer.
    pub tilemap: Option<luna_api::TilemapImage>,
    /// Which BG layer (0..3) the Tilemap Viewer currently shows.
    pub tilemap_bg: usize,
    /// Event Viewer overlay: `(rgba, width, height)` — events composited over
    /// the game framebuffer (Mesen2-style). Built in `build_panel_snapshot`.
    pub event_overlay: Option<(Vec<u8>, usize, usize)>,
    /// A copy of the live `EventViewerConfig`, so the filter-panel checkboxes
    /// can show their current state (the panel body is read-only; changes are
    /// returned as an [`EventViewerAction`] and applied to the real config).
    pub event_config: Option<luna_api::EventViewerConfig>,
    /// Decoded Event Viewer events for the list view (already filtered by the
    /// live config in `event_snapshot`).
    pub event_list: Option<Vec<luna_api::EventViewerEvent>>,
    /// Exec-breakpoint addresses for the CPU disassembly gutter.
    pub cpu_bp_addrs: Vec<u32>,
    /// Breakpoints panel rows: each registry entry + the resolved symbol
    /// for its (start) address when a `.sym` table is loaded.
    pub breakpoints: Option<Vec<(luna_api::BreakpointInfo, Option<String>)>>,
    /// The Debugger window's content.
    pub debugger: Option<Box<DebuggerView>>,
    /// The Memory Tools window's view.
    pub memory_tools: Option<Box<crate::memory_host::MemoryToolsView>>,
    /// The Memory Search window's view.
    pub memory_search: Option<Box<crate::memory_host::MemorySearchView>>,
}

/// State the egui overlay reads to drive its widgets — passed in by
/// `LunaApp` on every frame.
pub(crate) struct UiState<'a> {
    /// Rows of the current frame (224, or 239 under overscan): the part
    /// of the pixels canvas the game image shows, and its aspect.
    pub frame_height: usize,
    pub paused: bool,
    /// Debugger halt banner: set when a breakpoint auto-paused the
    /// emulation (`⏸ Break: …`), cleared on resume/reset.
    pub break_status: Option<String>,
    pub rom_title: Option<String>,
    /// Current device on controller ports 1 and 2 (drives the menu's radios).
    pub port_device: [luna_api::PortDevice; 2],
    /// Master volume slider position (0..=100).
    pub volume_percent: u8,
    /// Mute state (menu checkbox).
    pub muted: bool,
    /// Settings ▸ Video (frame-rate counter, filter, aspect ratio).
    pub video: &'a crate::video::VideoSettings,
    /// Width over height of the shown picture (Settings ▸ Aspect Ratio).
    pub aspect_ratio: f64,
    /// Picture size in physical pixels a Video Size entry set while the
    /// window is maximised or fullscreen; `None` = fill the game area.
    pub renderer_size: Option<(f64, f64)>,
    /// The Video Size entry the picture's height ticks (`0` = none).
    pub checked_scale: u32,
    /// Emulated frames per second in tenths, as last reported by the emu
    /// thread; `0` before the first report.
    pub fps_tenths: u32,
    pub show_input_config: bool,
    pub show_hotkey_config: bool,
    pub key_bindings: &'a crate::input::KeyBindings,
    /// Which debug panels are open + their snapshotted data.
    pub show_cpu_state: bool,
    pub show_cpu_memory: bool,
    pub show_cpu_disasm: bool,
    pub show_spc700: bool,
    pub show_spc700_memory: bool,
    pub show_spc700_disasm: bool,
    pub show_sprites: bool,
    pub show_registers: bool,
    pub show_palette: bool,
    pub show_tilemap: bool,
    pub show_event_viewer: bool,
    pub show_debugger: bool,
    pub show_spc_debugger: bool,
    pub show_memory_tools: bool,
    pub show_memory_search: bool,
    /// When `Some((player, button))`, the input modal is waiting on the
    /// user to press a key to rebind that player's SNES button.
    pub pending_rebind: Option<(usize, crate::input::SnesButton)>,
    /// When `Some`, the input modal is waiting on a key to rebind the
    /// named hotkey (screenshot, …).
    pub pending_hotkey_rebind: Option<crate::input::Hotkey>,
    /// Last screenshot filename, shown briefly in the menu bar.
    pub screenshot_status: Option<String>,
    /// Transient save/load-state feedback shown in the menu bar.
    pub save_state_status: Option<String>,
    /// `true` while joypad input is being recorded (issue #83) — drives the
    /// menu toggle label and the red "● REC" indicator.
    pub recording_input: bool,
    /// Transient input-recording feedback (saved-script path) in the menu bar.
    pub input_record_status: Option<String>,
    /// Forced cartridge mapper for the next load, or `None` = auto-detect
    /// (issue #88); drives the File ▸ Force mapper submenu's checkmarks.
    pub forced_mapper: Option<luna_api::MapperKind>,
    /// Transient load feedback (auto-detect failure hint) in the menu bar.
    pub load_notice: Option<String>,
    /// When `Some(filename)`, a ROM failed auto-detection and the inline
    /// "load as…?" mapper prompt is shown for it (issue #88).
    pub force_prompt_file: Option<String>,
    /// Watch-mode auto-reload state (issue #93) — drives the File menu toggle.
    pub auto_reload: bool,
    /// Which save-state slots (1..=9, indexed 0..9) have a file on disk for
    /// the current ROM. Drives the " ●" occupied marker in the slot menus.
    pub occupied_slots: [bool; 9],
}

/// All the egui plumbing wired up against pixels' wgpu device.
pub(crate) struct UiOverlay {
    ctx: egui::Context,
    winit_state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// The SNES framebuffer texture (pixels' 256 × 239 upload target),
    /// registered with egui so the game frame is laid out by egui itself —
    /// aspect-fit in the space **under** the menu bar. `None` until the first
    /// frame registers it.
    game_tex: Option<egui::TextureId>,
    /// Size and bilinear flag of the texture `game_tex` was made from: a
    /// filter changes the size, the bilinear setting the sampler.
    game_tex_key: (u32, u32, bool),
    /// Where the game image landed this frame, in logical points — the
    /// mouse→SNES-pixel mapping (Super Scope / SNES Mouse) reads it.
    last_game_rect: Option<egui::Rect>,
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
            game_tex: None,
            game_tex_key: (0, 0, false),
            last_game_rect: None,
        }
    }

    /// Register pixels' frame texture with the egui renderer, again when a
    /// video filter resized it or the bilinear setting changed. Nearest
    /// filtering keeps the upscale crisp; bilinear smooths it (Mesen2's
    /// "Use bilinear interpolation").
    pub(crate) fn ensure_game_texture(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::Texture,
        bilinear: bool,
    ) {
        let key = (texture.width(), texture.height(), bilinear);
        if self.game_tex.is_some() && self.game_tex_key == key {
            return;
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mode = if bilinear {
            wgpu::FilterMode::Linear
        } else {
            wgpu::FilterMode::Nearest
        };
        match self.game_tex {
            Some(id) => self
                .renderer
                .update_egui_texture_from_wgpu_texture(device, &view, mode, id),
            None => {
                self.game_tex = Some(self.renderer.register_native_texture(device, &view, mode));
            }
        }
        self.game_tex_key = key;
    }

    /// The game image's on-screen rectangle (logical points) from the last
    /// rendered frame — `None` before the first frame.
    pub(crate) const fn game_rect(&self) -> Option<egui::Rect> {
        self.last_game_rect
    }

    /// Forward a winit event to egui. Returns `true` if egui wants the
    /// event consumed (the main loop should skip its own handling).
    pub(crate) fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.winit_state.on_window_event(window, event);
        response.consumed
    }

    /// Build the frame's egui content + render it to `target_view`
    /// over the same wgpu command encoder that pixels used.
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
        let game_tex = self.game_tex;
        let game_rect: std::cell::Cell<Option<egui::Rect>> = std::cell::Cell::new(None);
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            let menu_bottom = draw_menu_bar(ui.ctx(), state, &mut emit);
            // The game frame, laid out by egui: aspect-fit (8:7) in the space
            // BELOW the menu strip. This replaces pixels' built-in scaling
            // renderer, which centred the image in the WHOLE window — with
            // the menu strip added to the window height, that centring put
            // ~half the strip's height as a black bar at the bottom and slid
            // the top of the image under the menu (the "black bar at the
            // bottom" bug). The area is computed EXPLICITLY from the strip's
            // real bottom edge — `ctx.content_rect()` is the whole window
            // (it subtracts OS notches, NOT egui panels; painting it black
            // was the "menu bar disappeared" regression). At the default
            // window size the fit is exact; a resized window letterboxes
            // symmetrically within the game area.
            if let Some(tex) = game_tex {
                let screen = ui.ctx().content_rect();
                let avail =
                    egui::Rect::from_min_max(egui::pos2(screen.min.x, menu_bottom), screen.max);
                let painter = ui.painter();
                painter.rect_filled(avail, 0.0, egui::Color32::BLACK);
                // The frame is `frame_height` rows of the 239-row canvas:
                // aspect-fit those rows and sample only that part of the
                // texture (a 224-row frame shows no overscan band).
                // Its shape is the aspect ratio's (Mesen2
                // `RendererPanel_LayoutUpdated`): as tall as the area, or as a
                // Video Size entry set it in a maximised / fullscreen window.
                let frame_h = state.frame_height.clamp(1, crate::CANVAS_H) as f32;
                let ppp = f64::from(pixels_per_point);
                let (avail_w, avail_h) = (f64::from(avail.width()), f64::from(avail.height()));
                let (area_w, area_h) = state.renderer_size.map_or((avail_w, avail_h), |(w, h)| {
                    ((w / ppp).min(avail_w), (h / ppp).min(avail_h))
                });
                let (w, h) = crate::video::fit(area_w, area_h, state.aspect_ratio);
                let size = egui::vec2(w as f32, h as f32);
                let rect = egui::Rect::from_center_size(avail.center(), size);
                painter.image(
                    tex,
                    rect,
                    egui::Rect::from_min_max(
                        egui::pos2(0.0, 0.0),
                        egui::pos2(1.0, frame_h / crate::CANVAS_H as f32),
                    ),
                    egui::Color32::WHITE,
                );
                game_rect.set(Some(rect));
                if state.video.show_fps
                    && let Some(text) = fps_label(state.fps_tenths, state.paused)
                {
                    draw_fps_counter(painter, rect, &text);
                }
            }
            if state.show_input_config {
                draw_input_config(ui.ctx(), state, &mut emit);
            }
            if state.show_hotkey_config {
                draw_hotkey_config(ui.ctx(), state, &mut emit);
            }
            if let Some(file) = state.force_prompt_file.as_deref() {
                draw_force_mapper_prompt(ui.ctx(), file, &mut emit);
            }
            // Debug views live in their own native OS windows (see
            // `crate::debug_window`), not in this overlay — so they can be
            // dragged anywhere on the desktop, outside the game window.
        });
        self.last_game_rect = game_rect.get();
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
                        // This pass paints the whole window now (menu + game
                        // image + black letterbox), so it clears — pixels'
                        // scaling pass, which used to clear, no longer runs.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

/// Text of the on-screen frame-rate counter: the emulated frame rate
/// with one decimal (`60.1 fps`). `None` while paused or before the emu
/// thread's first report, when there is no rate to show.
fn fps_label(fps_tenths: u32, paused: bool) -> Option<String> {
    if paused || fps_tenths == 0 {
        return None;
    }
    Some(format!("{}.{} fps", fps_tenths / 10, fps_tenths % 10))
}

/// Paint the frame-rate counter in the top-right corner of the game
/// image, on a translucent plate so it reads over any picture.
fn draw_fps_counter(painter: &egui::Painter, game: egui::Rect, text: &str) {
    const MARGIN: f32 = 6.0;
    const PAD: egui::Vec2 = egui::vec2(5.0, 2.0);
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::monospace(14.0),
        egui::Color32::from_rgb(0xFF, 0xE0, 0x40),
    );
    let size = galley.size() + PAD * 2.0;
    let plate = egui::Rect::from_min_size(
        egui::pos2(game.max.x - MARGIN - size.x, game.min.y + MARGIN),
        size,
    );
    painter.rect_filled(plate, 3.0, egui::Color32::from_black_alpha(170));
    painter.galley(plate.min + PAD, galley, egui::Color32::WHITE);
}

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

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    ctx.set_global_style(style);
}

#[cfg(test)]
mod fps_tests {
    use super::fps_label;

    #[test]
    fn the_counter_shows_one_decimal_and_hides_when_there_is_no_rate() {
        assert_eq!(fps_label(601, false).as_deref(), Some("60.1 fps"));
        assert_eq!(fps_label(500, false).as_deref(), Some("50.0 fps"));
        assert_eq!(fps_label(87, false).as_deref(), Some("8.7 fps"));
        // Paused: the last rate is stale. Before the first report: none yet.
        assert_eq!(fps_label(601, true), None);
        assert_eq!(fps_label(0, false), None);
    }
}
