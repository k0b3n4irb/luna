//! The Luna application state and `eframe::App` implementation.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::App;
use egui::{
    Align, Color32, ColorImage, Context, Layout, MenuBar, RichText, TextureHandle, TextureOptions,
    UiKind,
};
use luna_cartridge::Cartridge;
use luna_core::Snes;
use luna_ppu::{FRAME_H, FRAME_W};

/// Hard ceiling on instructions per UI frame — purely a safety belt
/// against runaway loops. The real budget is wall-clock time
/// ([`FRAME_TIME_BUDGET_MS`]) so the UI stays responsive even on slow
/// hardware or ROMs that spend a lot of cycles per instruction.
const STEPS_PER_FRAME: u32 = 200_000;

/// How many milliseconds we're willing to spend stepping the CPU
/// before yielding back to the UI thread. ~8 ms leaves half of a 60 Hz
/// frame for compositing, layout, and event handling, which is what
/// keeps the window from showing "Not Responding" under heavy load.
const FRAME_TIME_BUDGET_MS: u128 = 8;

/// The Luna desktop application.
pub(crate) struct LunaApp {
    /// Currently-loaded emulator, if any.
    snes: Option<Snes>,
    /// Path of the loaded ROM (for the title bar / recents).
    rom_path: Option<PathBuf>,
    /// Title extracted from the cartridge header.
    rom_title: Option<String>,

    /// Rendered framebuffer texture, refreshed every UI frame.
    framebuffer: Option<TextureHandle>,
    /// Last error message — shown in a banner if set.
    last_error: Option<String>,

    /// Pause toggle (true = CPU isn't stepped on update()).
    paused: bool,
    /// Total instructions executed since load.
    instructions_executed: u64,
    /// FPS bookkeeping.
    last_frame: Instant,
    fps: f32,

    /// UI panel toggles.
    show_cpu_panel: bool,
    show_ppu_panel: bool,
    show_stubs_panel: bool,

    /// Debug: render with `INIDISP` forced-blank ignored and master
    /// brightness clamped to $0F. Lets the user see whatever the game
    /// has uploaded to VRAM/CGRAM even when boot init keeps the screen
    /// blanked. Off by default.
    force_display: bool,

    /// Snapshot of the 8 bytes at `PB:PC`, captured once per UI frame
    /// and displayed in the CPU panel. Lets the user (and Claude
    /// looking at screenshots) tell at a glance what the CPU is
    /// looping on. Zero-initialised; refreshed after each frame's
    /// `step_cpu`.
    pc_bytes: [u8; 8],
}

impl LunaApp {
    pub(crate) fn new() -> Self {
        Self {
            snes: None,
            rom_path: None,
            rom_title: None,
            framebuffer: None,
            last_error: None,
            paused: false,
            instructions_executed: 0,
            last_frame: Instant::now(),
            fps: 0.0,
            show_cpu_panel: true,
            show_ppu_panel: true,
            show_stubs_panel: true,
            force_display: false,
            pc_bytes: [0; 8],
        }
    }

    /// Load a ROM from disk and reset the emulator.
    ///
    /// Wrapped in `catch_unwind` so that unsupported cartridge types
    /// (e.g. HiROM, SA-1, Super FX — anything past P0.6) surface as a
    /// friendly error in the status bar instead of crashing the GUI.
    fn load_rom(&mut self, path: &Path) {
        let cart = match Cartridge::load(path) {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(format!("Failed to load ROM: {e}"));
                return;
            }
        };
        // Mapper compatibility check.
        // LoROM / HiROM / ExHiROM are wired through `Snes::from_cartridge`.
        // Coprocessor carts (SA-1 / Super FX / S-DD1 / SPC7110) need
        // their own subsystem implementation — refuse them with a
        // clear message rather than half-loading.
        use luna_bus::MapperKind;
        match cart.header.mapper_kind {
            MapperKind::LoRom | MapperKind::HiRom | MapperKind::ExHiRom => {}
            other => {
                self.last_error = Some(format!(
                    "Cartridge needs the {other:?} coprocessor, which isn't yet \
                     emulated. Star Fox / Yoshi's Island (Super FX), Super Mario \
                     RPG / Kirby Super Star (SA-1) and friends will land in \
                     their own dedicated phase. Plain LoROM, HiROM and ExHiROM \
                     carts work today."
                ));
                self.snes = None;
                self.rom_title = Some(cart.header.title.clone());
                self.rom_path = Some(path.to_path_buf());
                self.framebuffer = None;
                return;
            }
        }
        let title = cart.header.title.clone();
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut snes = Snes::from_cartridge(cart);
            snes.reset();
            snes
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(snes) => {
                self.snes = Some(snes);
                self.rom_title = Some(title);
                self.rom_path = Some(path.to_path_buf());
                self.instructions_executed = 0;
                self.last_error = None;
                self.framebuffer = None;
            }
            Err(payload) => {
                self.snes = None;
                self.last_error = Some(format!(
                    "Could not initialise emulator for this ROM: {}",
                    payload_to_string(payload)
                ));
                self.rom_title = Some(title);
                self.rom_path = Some(path.to_path_buf());
                self.framebuffer = None;
            }
        }
    }

    /// Step the CPU under a **wall-clock time budget** so the UI
    /// thread never starves. Panics are caught so an unimplemented
    /// opcode just pauses emulation instead of killing the window.
    ///
    /// We check the clock every `BATCH_SIZE` steps rather than every
    /// step — calling `Instant::now()` 60 000+ times per frame would
    /// itself eat the budget. A batch of 256 is small enough to
    /// stay within budget even on slow ROMs.
    fn step_cpu(&mut self) {
        if self.paused {
            return;
        }
        let Some(snes) = self.snes.as_mut() else {
            return;
        };
        const BATCH_SIZE: u32 = 256;
        let deadline =
            Instant::now() + std::time::Duration::from_millis(FRAME_TIME_BUDGET_MS as u64);
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut executed: u32 = 0;
            while executed < STEPS_PER_FRAME && !snes.cpu.stopped {
                for _ in 0..BATCH_SIZE {
                    if snes.cpu.stopped {
                        break;
                    }
                    snes.step();
                    executed += 1;
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            executed
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(n) => self.instructions_executed += u64::from(n),
            Err(payload) => {
                let msg = payload_to_string(payload);
                self.last_error = Some(format!("CPU panic: {msg}"));
                self.paused = true;
            }
        }
        // Diagnostic: snapshot the 8 bytes at PB:PC for the CPU panel.
        if let Some(snes) = self.snes.as_mut() {
            let bytes = snes.peek_pc_bytes(8);
            for (slot, b) in self.pc_bytes.iter_mut().zip(bytes.iter()) {
                *slot = *b;
            }
        }
    }

    /// Render the PPU framebuffer into an egui texture.
    ///
    /// Most SNES games toggle `INIDISP` bit 7 every frame to *force-
    /// blank* the screen during their VBlank handler so they can
    /// upload tiles / palette / OAM safely, then clear bit 7 before
    /// rendering resumes. Our UI thread samples the PPU at an
    /// arbitrary moment within each emulated second — when that
    /// happens to land *inside* a forced-blank window we'd see an
    /// all-black frame, which to the user looks like the screen is
    /// blinking once per second.
    ///
    /// Fix: when forced-blank is on (and the user hasn't asked for
    /// bypass), keep the previous good texture. The eye sees a
    /// stable rendered frame; the game's blanking is invisible.
    fn refresh_framebuffer(&mut self, ctx: &Context) {
        let Some(snes) = self.snes.as_ref() else {
            return;
        };
        if !self.force_display && snes.ppu.inidisp & 0x80 != 0 && self.framebuffer.is_some() {
            // Forced blank — preserve the last non-blanked texture so
            // the screen doesn't flicker every NMI handler tick.
            return;
        }
        let opts = luna_ppu::RenderOptions {
            bypass_forced_blank: self.force_display,
        };
        let frame = luna_ppu::render_frame_with(&snes.ppu, opts);
        let mut rgba = Vec::with_capacity(FRAME_W * FRAME_H * 4);
        for px in frame {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
        }
        let image = ColorImage::from_rgba_unmultiplied([FRAME_W, FRAME_H], &rgba);
        if let Some(tex) = self.framebuffer.as_mut() {
            tex.set(image, TextureOptions::NEAREST);
        } else {
            self.framebuffer =
                Some(ctx.load_texture("luna-framebuffer", image, TextureOptions::NEAREST));
        }
    }
}

impl App for LunaApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // ---------------- File-drop handling ----------------
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped_files.into_iter().find_map(|f| f.path) {
            self.load_rom(&file);
        }

        // ---------------- FPS bookkeeping ----------------
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .max(0.0001);
        self.fps = 0.9 * self.fps + 0.1 * (1.0 / dt);
        self.last_frame = now;

        // ---------------- Emulation step ----------------
        self.step_cpu();
        self.refresh_framebuffer(ctx);

        // ---------------- Top menu bar ----------------
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("SNES ROM", &["sfc", "smc"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                        {
                            self.load_rom(&path);
                        }
                        ui.close_kind(UiKind::Menu);
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Emulation", |ui| {
                    let label = if self.paused { "Resume" } else { "Pause" };
                    if ui.button(label).clicked() {
                        self.paused = !self.paused;
                        ui.close_kind(UiKind::Menu);
                    }
                    if ui.button("Reset").clicked() {
                        if let Some(snes) = self.snes.as_mut() {
                            snes.reset();
                            self.instructions_executed = 0;
                            self.last_error = None;
                            self.paused = false;
                        }
                        ui.close_kind(UiKind::Menu);
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_cpu_panel, "CPU panel");
                    ui.checkbox(&mut self.show_ppu_panel, "PPU panel");
                    ui.checkbox(&mut self.show_stubs_panel, "Stubs panel");
                    ui.separator();
                    ui.checkbox(&mut self.force_display, "Force display (bypass INIDISP)")
                        .on_hover_text(
                            "Render even when the game has set forced-blank \
                         (INIDISP bit 7). Master brightness is also \
                         clamped to $0F. Useful to see what's in VRAM \
                         when a game stays blanked during boot.",
                        );
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Luna SNES — 2026");
                    ui.hyperlink_to("github", "https://github.com/k0b3n4irb/luna");
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(format!("{:>5.1} FPS", self.fps));
                    if self.paused {
                        ui.label(RichText::new("⏸ PAUSED").color(Color32::from_rgb(255, 200, 80)));
                    }
                });
            });
        });

        // ---------------- Right side panel (debug) ----------------
        if self.show_cpu_panel || self.show_ppu_panel || self.show_stubs_panel {
            egui::SidePanel::right("debug_panel")
                .resizable(true)
                .default_width(280.0)
                .min_width(220.0)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    if let Some(snes) = self.snes.as_ref() {
                        if self.show_cpu_panel {
                            egui::CollapsingHeader::new(RichText::new("CPU 65C816").strong())
                                .default_open(true)
                                .show(ui, |ui| {
                                    cpu_panel(ui, snes, &self.pc_bytes);
                                });
                        }
                        if self.show_ppu_panel {
                            egui::CollapsingHeader::new(RichText::new("PPU").strong())
                                .default_open(true)
                                .show(ui, |ui| {
                                    ppu_panel(ui, snes);
                                });
                        }
                        if self.show_stubs_panel {
                            egui::CollapsingHeader::new(RichText::new("Stubs & diag").strong())
                                .default_open(true)
                                .show(ui, |ui| {
                                    stubs_panel(ui, snes);
                                });
                        }
                    } else {
                        ui.label("Open a ROM to inspect.");
                    }
                });
        }

        // ---------------- Status bar ----------------
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(title) = &self.rom_title {
                    ui.label(RichText::new(format!("📀 {title}")).strong());
                    ui.separator();
                }
                ui.label(format!("Instructions: {}", self.instructions_executed));
                if let Some(snes) = self.snes.as_ref() {
                    ui.separator();
                    ui.label(format!("MCycles: {}", snes.total_mclk));
                }
                if let Some(err) = &self.last_error {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(255, 120, 120), err);
                }
            });
        });

        // ---------------- Error banner ----------------
        if let Some(err) = self.last_error.clone() {
            egui::TopBottomPanel::top("error_banner")
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgb(60, 24, 30))
                        .inner_margin(8.0),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("⚠")
                                .size(18.0)
                                .color(Color32::from_rgb(255, 140, 140)),
                        );
                        ui.label(RichText::new(&err).color(Color32::from_rgb(255, 200, 200)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.last_error = None;
                            }
                        });
                    });
                });
        }

        // ---------------- Central panel (screen) ----------------
        let mut requested_path: Option<PathBuf> = None;
        let cpu_stopped = self.snes.as_ref().map(|s| s.cpu.stopped).unwrap_or(false);
        let cpu_paused = self.paused;
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.snes.is_none() {
                requested_path = draw_landing_page(ui);
                return;
            }
            draw_screen(ui, self.framebuffer.as_ref(), cpu_stopped, cpu_paused);
        });
        if let Some(path) = requested_path {
            self.load_rom(&path);
        }

        // 60 fps repaint target.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn cpu_panel(ui: &mut egui::Ui, snes: &Snes, pc_bytes: &[u8; 8]) {
    let cpu = &snes.cpu;
    let mono = |s: String| RichText::new(s).monospace();
    egui::Grid::new("cpu_regs")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label("A");
            ui.label(mono(format!("${:04X}", cpu.a)));
            ui.end_row();
            ui.label("X");
            ui.label(mono(format!("${:04X}", cpu.x)));
            ui.end_row();
            ui.label("Y");
            ui.label(mono(format!("${:04X}", cpu.y)));
            ui.end_row();
            ui.label("SP");
            ui.label(mono(format!("${:04X}", cpu.sp)));
            ui.end_row();
            ui.label("PC");
            ui.label(mono(format!("${:02X}:{:04X}", cpu.pb, cpu.pc)));
            ui.end_row();
            ui.label("DP");
            ui.label(mono(format!("${:04X}", cpu.dp)));
            ui.end_row();
            ui.label("DB");
            ui.label(mono(format!("${:02X}", cpu.db)));
            ui.end_row();
            ui.label("P");
            ui.label(mono(format!(
                "${:02X}  {}",
                cpu.p.bits(),
                flag_string(cpu.p.bits(), cpu.e)
            )));
            ui.end_row();
            ui.label("@PC");
            ui.label(mono(format!(
                "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                pc_bytes[0],
                pc_bytes[1],
                pc_bytes[2],
                pc_bytes[3],
                pc_bytes[4],
                pc_bytes[5],
                pc_bytes[6],
                pc_bytes[7],
            )));
            ui.end_row();
        });
}

fn ppu_panel(ui: &mut egui::Ui, snes: &Snes) {
    let p = &snes.ppu;
    let mono = |s: String| RichText::new(s).monospace();
    egui::Grid::new("ppu_regs")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label("INIDISP");
            ui.label(mono(format!(
                "${:02X} {}",
                p.inidisp,
                if p.inidisp & 0x80 != 0 {
                    "(blanked)"
                } else {
                    ""
                }
            )));
            ui.end_row();
            ui.label("Brightness");
            ui.label(mono(format!("{}/15", p.inidisp & 0x0F)));
            ui.end_row();
            ui.label("BGMODE");
            ui.label(mono(format!("${:02X}", p.bgmode)));
            ui.end_row();
            ui.label("VRAM addr");
            ui.label(mono(format!("${:04X} (word)", p.vram.address)));
            ui.end_row();
            for (i, bg) in p.bg.iter().enumerate() {
                ui.label(format!("BG{}", i + 1));
                ui.label(mono(format!(
                    "tile=${:04X} chr=${:04X}",
                    bg.tilemap_addr_words, bg.char_addr_words
                )));
                ui.end_row();
            }
        });
}

fn stubs_panel(ui: &mut egui::Ui, snes: &Snes) {
    let mono = |s: String| RichText::new(s).monospace();
    ui.label(
        RichText::new("Compat stubs — replaced once real APU/scheduler land.")
            .small()
            .italics()
            .color(Color32::from_rgb(160, 160, 180)),
    );
    egui::Grid::new("stubs_regs")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (i, v) in snes.apu.ports().iter().enumerate() {
                ui.label(format!("APU $214{i}"));
                ui.label(mono(format!("${v:02X}")));
                ui.end_row();
            }
            ui.label("APU phase");
            ui.label(mono(format!("{:?}", snes.apu.phase())));
            ui.end_row();
            ui.label("Fake frames");
            ui.label(mono(snes.fake_frame_count.to_string()));
            ui.end_row();
            ui.label("NMIs served");
            ui.label(mono(snes.nmis_serviced.to_string()));
            ui.end_row();
            ui.label("NMITIMEN");
            let nmiten = snes.cpu_regs.nmitimen;
            ui.label(mono(format!(
                "${:02X} {}",
                nmiten,
                if nmiten & 0x80 != 0 {
                    "(NMI on)"
                } else {
                    "(masked)"
                }
            )));
            ui.end_row();
            ui.label("INIDISP writes");
            ui.label(mono(snes.ppu.inidisp_write_count.to_string()));
            ui.end_row();
            ui.label("Backdrop");
            let bg0 = snes.ppu.cgram.color(0);
            ui.label(mono(format!("${bg0:04X}")));
            ui.end_row();
            ui.label("HVBJOY");
            ui.label(mono(format!("${:02X}", snes.cpu_regs.hvbjoy)));
            ui.end_row();
        });
}

fn flag_string(p: u8, e: bool) -> String {
    let bit = |mask: u8, c: char, fallback: char| if p & mask != 0 { c } else { fallback };
    format!(
        "{}{}{}{}{}{}{}{}{}",
        bit(0b1000_0000, 'N', 'n'),
        bit(0b0100_0000, 'V', 'v'),
        bit(0b0010_0000, 'M', 'm'),
        bit(0b0001_0000, 'X', 'x'),
        bit(0b0000_1000, 'D', 'd'),
        bit(0b0000_0100, 'I', 'i'),
        bit(0b0000_0010, 'Z', 'z'),
        bit(0b0000_0001, 'C', 'c'),
        if e { 'E' } else { 'e' },
    )
}

fn draw_screen(ui: &mut egui::Ui, texture: Option<&TextureHandle>, stopped: bool, paused: bool) {
    let Some(tex) = texture else {
        ui.centered_and_justified(|ui| {
            ui.label(
                RichText::new("Waiting for first frame…").color(Color32::from_rgb(160, 160, 180)),
            );
        });
        return;
    };
    // Compute the largest integer scale that fits the available rect
    // while preserving the 8:7 SNES native pixel ratio (we keep 1:1
    // for now — square pixels — and let the user resize freely).
    let avail = ui.available_size();
    let scale_x = (avail.x / FRAME_W as f32).floor().max(1.0);
    let scale_y = (avail.y / FRAME_H as f32).floor().max(1.0);
    let scale = scale_x.min(scale_y);
    let size = egui::vec2(FRAME_W as f32 * scale, FRAME_H as f32 * scale);

    ui.centered_and_justified(|ui| {
        let response = ui.add(egui::Image::new(tex).fit_to_exact_size(size));
        // Subtle rounded frame around the screen.
        let rect = response.rect;
        ui.painter().rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(6),
            egui::Stroke::new(1.5, Color32::from_rgb(80, 80, 110)),
            egui::StrokeKind::Outside,
        );
        // Overlay: STOPPED / PAUSED badge — tells the user why the
        // screen looks frozen.
        if stopped || paused {
            let (text, color) = if stopped {
                ("CPU HALTED (STP)", Color32::from_rgb(255, 120, 120))
            } else {
                ("PAUSED", Color32::from_rgb(255, 200, 80))
            };
            let painter = ui.painter();
            let pos = rect.center_bottom() + egui::vec2(0.0, -24.0);
            painter.text(
                pos,
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(16.0),
                color,
            );
        }
    });
}

/// Draw the no-ROM startup screen. Returns the picked path if the
/// user clicked "Open ROM…", so the caller (which owns `self`) can
/// call `load_rom` without running into closure-borrow conflicts.
fn draw_landing_page(ui: &mut egui::Ui) -> Option<PathBuf> {
    let mut picked: Option<PathBuf> = None;
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.heading(RichText::new("Luna").size(56.0).strong());
        ui.label(
            RichText::new("A modern SNES emulator with an introspection API")
                .size(16.0)
                .color(Color32::from_rgb(180, 180, 200)),
        );
        ui.add_space(36.0);
        if ui
            .add(
                egui::Button::new(RichText::new("📂  Open ROM…").size(18.0))
                    .min_size(egui::vec2(240.0, 48.0)),
            )
            .clicked()
        {
            picked = rfd::FileDialog::new()
                .add_filter("SNES ROM", &["sfc", "smc"])
                .add_filter("All files", &["*"])
                .pick_file();
        }
        ui.add_space(16.0);
        ui.label(
            RichText::new("…or drop a .sfc / .smc file here")
                .size(13.0)
                .italics()
                .color(Color32::from_rgb(140, 140, 160)),
        );
    });
    picked
}

fn payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(unknown panic payload)".to_string()
    }
}
