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
    ToggleCpuMemory,
    ToggleCpuDisasm,
    ToggleSpc700,
    ToggleSpc700Memory,
    ToggleSpc700Disasm,
    ToggleSprites,
    ToggleRegisters,
}

/// A navigation request a debug panel's toolbar emits this frame, applied
/// by `LunaApp` to the panel's cursor state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PanelNav {
    /// Move a memory viewer's address cursor by a signed byte delta.
    MemAddr(i64),
    /// Re-anchor the disassembly at the live program counter.
    DisasmGotoPc,
    /// Set the disassembly start address (from the editable field).
    DisasmSetAddr(u32),
    /// Set the disassembly line count (from the editable field).
    DisasmSetLines(u16),
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
    pub show_cpu_memory: bool,
    pub show_cpu_disasm: bool,
    pub show_spc700: bool,
    pub show_spc700_memory: bool,
    pub show_spc700_disasm: bool,
    pub show_sprites: bool,
    pub show_registers: bool,
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

/// Restyle the current `ui`'s widget visuals so an interactive widget
/// (e.g. a `DragValue`) renders as a blue cell, matching [`value_chip`].
/// Call inside a `ui.scope(...)` so the override is local.
fn style_blue_field(ui: &mut egui::Ui) {
    let fill = egui::Color32::from_rgb(38, 60, 108);
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(96, 132, 205));
    let text = egui::Color32::from_rgb(226, 232, 248);
    let w = &mut ui.visuals_mut().widgets;
    for s in [&mut w.inactive, &mut w.hovered, &mut w.active] {
        s.weak_bg_fill = fill;
        s.bg_fill = fill;
        s.bg_stroke = stroke;
        s.fg_stroke.color = text;
    }
    w.hovered.weak_bg_fill = egui::Color32::from_rgb(50, 78, 140);
}

/// A register value rendered as a blue framed cell — the ness debugger's
/// signature look for live values.
fn value_chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(38, 60, 108))
        .stroke(egui::Stroke::new(
            1.5,
            egui::Color32::from_rgb(96, 132, 205),
        ))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(egui::CornerRadius::same(3))
        .show(ui, |ui| {
            ui.monospace(egui::RichText::new(text).color(egui::Color32::from_rgb(226, 232, 248)));
        });
}

/// `label:` followed by one or more blue value chips, as one grid row.
fn reg_row(ui: &mut egui::Ui, label: &str, values: &[String]) {
    ui.monospace(label);
    ui.horizontal(|ui| {
        for v in values {
            value_chip(ui, v);
        }
    });
    ui.end_row();
}

/// Processor-status flags as a row of blue 0/1 chips with the flag
/// letter centred beneath each chip (ness style). Each flag is a
/// fixed-width, centre-aligned mini-column so the chip and its letter
/// share an axis. `bits` is MSB→LSB.
fn psw_flags(ui: &mut egui::Ui, p: u8, bits: &[(u8, char)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for &(mask, name) in bits {
            ui.allocate_ui_with_layout(
                egui::vec2(22.0, 42.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    value_chip(ui, if p & mask != 0 { "1" } else { "0" });
                    ui.monospace(name.to_string());
                },
            );
        }
    });
}

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

/// One `$AAAA  NAME  <chip>` row in a register-viewer grid.
fn rv_row(ui: &mut egui::Ui, addr: &str, name: &str, value: &str) {
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
    let dsp = |r: usize| a.dsp_regs.get(r).copied().unwrap_or(0);
    egui::Grid::new("rv_dsp_voices")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for v in 0..8usize {
                let base = v << 4;
                rv_row(
                    ui,
                    &format!("${:02X}", base),
                    &format!("V{v} VOLL"),
                    &b(dsp(base)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 1),
                    &format!("V{v} VOLR"),
                    &b(dsp(base + 1)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 2),
                    &format!("V{v} PITCHL"),
                    &b(dsp(base + 2)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 3),
                    &format!("V{v} PITCHH"),
                    &b(dsp(base + 3)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 4),
                    &format!("V{v} SRCN"),
                    &b(dsp(base + 4)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 5),
                    &format!("V{v} ADSR1"),
                    &b(dsp(base + 5)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 6),
                    &format!("V{v} ADSR2"),
                    &b(dsp(base + 6)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 7),
                    &format!("V{v} GAIN"),
                    &b(dsp(base + 7)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 8),
                    &format!("V{v} ENVX"),
                    &b(dsp(base + 8)),
                );
                rv_row(
                    ui,
                    &format!("${:02X}", base + 9),
                    &format!("V{v} OUTX"),
                    &b(dsp(base + 9)),
                );
            }
        });
    egui::Grid::new("rv_dsp_global")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            rv_row(ui, "$0C", "MVOLL", &b(dsp(0x0C)));
            rv_row(ui, "$1C", "MVOLR", &b(dsp(0x1C)));
            rv_row(ui, "$2C", "EVOLL", &b(dsp(0x2C)));
            rv_row(ui, "$3C", "EVOLR", &b(dsp(0x3C)));
            rv_row(ui, "$4C", "KON", &b(dsp(0x4C)));
            rv_row(ui, "$5C", "KOF", &b(dsp(0x5C)));
            rv_row(ui, "$6C", "FLG", &b(dsp(0x6C)));
            rv_row(ui, "$7C", "ENDX", &b(dsp(0x7C)));
            rv_row(ui, "$0D", "EFB", &b(dsp(0x0D)));
            rv_row(ui, "$2D", "PMON", &b(dsp(0x2D)));
            rv_row(ui, "$3D", "NON", &b(dsp(0x3D)));
            rv_row(ui, "$4D", "EON", &b(dsp(0x4D)));
            rv_row(ui, "$5D", "DIR", &b(dsp(0x5D)));
            rv_row(ui, "$6D", "ESA", &b(dsp(0x6D)));
            rv_row(ui, "$7D", "EDL", &b(dsp(0x7D)));
            for n in 0..8usize {
                let r = (n << 4) | 0x0F;
                rv_row(ui, &format!("${r:02X}"), &format!("COEF{n}"), &b(dsp(r)));
            }
        });
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

/// Render `bytes` as a Mesen2-style hex grid: a flat zero-padded address
/// (`addr_digits` wide) per 16-byte row, `00` bytes dimmed and non-zero
/// bytes bright, a vertical rule, then the ASCII gutter. The view's first
/// byte (the cursor) gets a blue background. Per-byte colouring is built
/// with a `LayoutJob` so each row is one galley.
fn mem_hex_grid(ui: &mut egui::Ui, start: u32, bytes: &[u8], addr_digits: usize) {
    use egui::text::{LayoutJob, TextFormat};

    // Never wrap: a row's hex and ASCII must stay on one aligned line (the
    // window/scroll-area handles any overflow).
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    let mono = egui::FontId::monospace(13.0);
    let c_addr = egui::Color32::from_rgb(118, 128, 150);
    let c_zero = egui::Color32::from_rgb(74, 78, 92);
    let c_byte = egui::Color32::from_rgb(220, 224, 236);
    let c_sep = egui::Color32::from_rgb(64, 68, 86);
    let c_dot = egui::Color32::from_rgb(74, 78, 92);
    let c_chr = egui::Color32::from_rgb(176, 198, 168);
    let cursor_bg = egui::Color32::from_rgb(38, 60, 108);

    let fmt = |color: egui::Color32| TextFormat {
        font_id: mono.clone(),
        color,
        ..Default::default()
    };

    for (r, chunk) in bytes.chunks(16).enumerate() {
        let addr = start.wrapping_add((r as u32) * 16) & 0xFF_FFFF;
        let mut job = LayoutJob::default();
        job.append(&format!("{addr:0addr_digits$X}: "), 0.0, fmt(c_addr));
        for (i, &b) in chunk.iter().enumerate() {
            let mut f = fmt(if b == 0 { c_zero } else { c_byte });
            if r == 0 && i == 0 {
                f.background = cursor_bg;
            }
            job.append(&format!("{b:02X}"), 0.0, f);
            job.append(" ", 0.0, fmt(c_zero));
        }
        for _ in chunk.len()..16 {
            job.append("   ", 0.0, fmt(c_zero));
        }
        job.append("\u{2502} ", 0.0, fmt(c_sep));
        for &b in chunk {
            let (ch, col) = if (0x20..0x7F).contains(&b) {
                (b as char, c_chr)
            } else {
                ('.', c_dot)
            };
            job.append(&ch.to_string(), 0.0, fmt(col));
        }
        ui.label(job);
    }
}

/// Bottom toolbar for a memory view: the current address as a blue chip
/// plus ±$100 / ±$1000 nav buttons. Returns the nav request if clicked.
fn mem_nav(ui: &mut egui::Ui, start: u32, addr_digits: usize) -> Option<PanelNav> {
    let mut nav = None;
    ui.separator();
    ui.horizontal(|ui| {
        value_chip(ui, &format!("{start:0addr_digits$X}"));
        ui.add_space(8.0);
        if ui
            .button("\u{25c0}\u{25c0}")
            .on_hover_text("\u{2212}$1000")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(-0x1000));
        }
        if ui
            .button("\u{25c0}")
            .on_hover_text("\u{2212}$100")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(-0x100));
        }
        if ui.button("\u{25b6}").on_hover_text("+$100").clicked() {
            nav = Some(PanelNav::MemAddr(0x100));
        }
        if ui
            .button("\u{25b6}\u{25b6}")
            .on_hover_text("+$1000")
            .clicked()
        {
            nav = Some(PanelNav::MemAddr(0x1000));
        }
    });
    nav
}

/// Body of the CPU Memory view — 256 bytes of the 24-bit CPU bus at the
/// current address. Returns the nav request from the toolbar, if any.
pub(crate) fn cpu_memory_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some((addr, bytes)) = snap.cpu_memory.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    mem_hex_grid(ui, *addr, bytes, 6);
    mem_nav(ui, *addr, 6)
}

/// Body of the SPC700 Memory view — 256 bytes of the flat 64 KiB ARAM.
pub(crate) fn spc700_memory_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some((addr, bytes)) = snap.spc_memory.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    mem_hex_grid(ui, u32::from(*addr), bytes, 4);
    mem_nav(ui, u32::from(*addr), 4)
}

/// Body of the SPC700 disassembly view — a live listing from the current
/// PC (`ADDR: BYTES   MNEMONIC`), the PC line given the blue cursor
/// background. Read-only / follow-execution.
/// Shared body for the SPC700 / CPU disassembly panels: a toolbar
/// (Follow-PC, address, scroll, line count) over a live instruction
/// listing with the PC line highlighted. `addr_digits` is 4 for ARAM,
/// 6 for the 24-bit CPU bus. Returns the toolbar's nav request.
fn disasm_body(
    ui: &mut egui::Ui,
    lines: &[luna_api::DisasmLine],
    line_count: u16,
    addr_digits: usize,
    max_addr: u32,
) -> Option<PanelNav> {
    use egui::text::{LayoutJob, TextFormat};

    let start = lines.first().map_or(0, |l| l.addr);
    let mut nav = None;

    // Toolbar (ness layout): jump-to-PC button, an editable hex start
    // address, and an editable line count. Type an address + Enter, or
    // double-click the value to edit / drag to scrub.
    ui.horizontal(|ui| {
        if ui
            .button("Disassemble at PC")
            .on_hover_text("Re-anchor the listing at the live program counter")
            .clicked()
        {
            nav = Some(PanelNav::DisasmGotoPc);
        }
        let mut addr = start;
        let addr_resp = ui
            .scope(|ui| {
                style_blue_field(ui);
                ui.add(
                    egui::DragValue::new(&mut addr)
                        .range(0..=max_addr)
                        .hexadecimal(addr_digits, false, true)
                        .speed(1.0),
                )
            })
            .inner;
        if addr_resp
            .on_hover_text("Start address (double-click to type)")
            .changed()
        {
            nav = Some(PanelNav::DisasmSetAddr(addr));
        }
        ui.add_space(8.0);
        ui.label("Lines:");
        let mut n = line_count;
        let lines_resp = ui
            .scope(|ui| {
                style_blue_field(ui);
                ui.add(egui::DragValue::new(&mut n).range(4..=128).speed(0.25))
            })
            .inner;
        if lines_resp.changed() {
            nav = Some(PanelNav::DisasmSetLines(n));
        }
    });
    ui.separator();
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    let mono = egui::FontId::monospace(13.0);
    let c_addr = egui::Color32::from_rgb(118, 128, 150);
    let c_bytes = egui::Color32::from_rgb(120, 124, 140);
    let c_text = egui::Color32::from_rgb(222, 226, 238);
    let cursor_bg = egui::Color32::from_rgb(38, 60, 108);

    for line in lines {
        let bg = if line.is_pc {
            cursor_bg
        } else {
            egui::Color32::TRANSPARENT
        };
        let fmt = |color| TextFormat {
            font_id: mono.clone(),
            color,
            background: bg,
            ..Default::default()
        };
        let mut raw = String::with_capacity(line.bytes.len() * 3);
        for b in &line.bytes {
            use std::fmt::Write;
            let _ = write!(raw, "{b:02X} ");
        }
        let mut job = LayoutJob::default();
        job.append(
            &format!("{:0addr_digits$X}:  ", line.addr),
            0.0,
            fmt(c_addr),
        );
        job.append(&format!("{raw:<13}"), 0.0, fmt(c_bytes));
        job.append(&line.text, 0.0, fmt(c_text));
        ui.label(job);
    }
    nav
}

/// Body of the SPC700 disassembly view.
pub(crate) fn spc700_disasm_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(lines) = snap.spc_disasm.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    disasm_body(ui, lines, snap.spc_disasm_lines, 4, 0xFFFF)
}

/// Body of the CPU (65c816) disassembly view.
pub(crate) fn cpu_disasm_body(ui: &mut egui::Ui, snap: &DebugSnapshot) -> Option<PanelNav> {
    let Some(lines) = snap.cpu_disasm.as_ref() else {
        ui.label("(no ROM loaded)");
        return None;
    };
    disasm_body(ui, lines, snap.cpu_disasm_lines, 6, 0xFF_FFFF)
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
                    // Grouped by subsystem (CPU / SPC700 / PPU), separated by
                    // a light rule, Mesen2-style.
                    ui.label(egui::RichText::new("CPU").weak().small());
                    if ui
                        .selectable_label(state.show_cpu_state, "CPU state")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuState);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_cpu_memory, "CPU memory")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuMemory);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_cpu_disasm, "CPU disassembly")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuDisasm);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("SPC700").weak().small());
                    if ui
                        .selectable_label(state.show_spc700, "SPC700 state")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc700_memory, "SPC700 memory")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700Memory);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc700_disasm, "SPC700 disassembly")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700Disasm);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("PPU").weak().small());
                    if ui
                        .selectable_label(state.show_sprites, "Sprites (OAM)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSprites);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("System").weak().small());
                    if ui
                        .selectable_label(state.show_registers, "Registers")
                        .clicked()
                    {
                        emit(MenuAction::ToggleRegisters);
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
