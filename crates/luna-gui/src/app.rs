//! The Luna application state and `eframe::App` implementation.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Instant;

use eframe::App;
use egui::{
    Align, Color32, ColorImage, Context, Layout, MenuBar, RichText, TextureHandle, TextureOptions,
    UiKind,
};
use luna_cartridge::Cartridge;
use luna_core::Snes;
use luna_ppu::{FRAME_H, FRAME_W};

use crate::audio::AudioStreamArtifacts;
use crate::emu_thread::EmuShared;

/// Convenience alias — the Snes lives behind an `Arc<Mutex<>>` shared
/// between the UI thread and the dedicated emu thread.
type SharedSnes = Arc<Mutex<Option<Snes>>>;

/// The Luna desktop application.
pub(crate) struct LunaApp {
    /// Currently-loaded emulator, behind a Mutex shared with the
    /// dedicated emu thread (see `emu_thread.rs`). The UI uses
    /// `try_lock` to read state without ever blocking the redraw.
    snes: SharedSnes,
    /// Handle to the spawned emu thread (one per loaded ROM).
    emu_join: Option<JoinHandle<()>>,
    /// State shared between UI, emu thread, and the cpal callback —
    /// shutdown / pause flags + the unpark handle that the cpal
    /// callback uses to wake the emu thread.
    emu_shared: Arc<EmuShared>,
    /// Path of the loaded ROM (for the title bar / recents).
    rom_path: Option<PathBuf>,
    /// Title extracted from the cartridge header.
    rom_title: Option<String>,

    /// Rendered framebuffer texture, refreshed every UI frame.
    framebuffer: Option<TextureHandle>,
    /// Last error message — shown in a banner if set.
    last_error: Option<String>,

    /// Pause toggle. Mirrored into `emu_shared.paused` so the emu
    /// thread sleeps when set.
    paused: bool,
    /// Cached "a ROM is loaded" flag. We can't poll the Snes Mutex
    /// every frame to answer "should the central panel show the
    /// landing page?" — `try_lock` fails half the time when the emu
    /// thread is busy, which would flicker the central panel between
    /// the rendered ROM and the landing page at the UI's repaint
    /// rate. This bool is set on load_rom success and cleared on
    /// unload_snes.
    rom_loaded: bool,
    /// Cumulative main-CPU instruction counter, surfaced in the status
    /// bar. Now informational only — the emu thread tracks this
    /// internally; the UI just polls it (TODO).
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

    /// Host audio backend. `None` if cpal couldn't open the default
    /// output device — emulation keeps going silently. Held by the
    /// app even when the emu thread is the actual producer, so the
    /// cpal stream stays alive for the program's lifetime.
    #[allow(dead_code)]
    audio: Option<crate::audio::AudioBackend>,
    /// Producer end of the cpal SPSC ring, held here only until the
    /// emu thread is spawned (which moves it into the thread). `None`
    /// after the first ROM load, or if audio init failed.
    audio_producer: Option<ringbuf::HeapProd<(i16, i16)>>,
    /// Shared "audio primed" flag handed to the emu thread alongside
    /// the producer. The emu flips it on first push so the cpal
    /// callback's silence-until-primed gate opens.
    audio_primed: Option<Arc<std::sync::atomic::AtomicBool>>,

    /// Keyboard → SNES button mapping. Defaults to the Mesen2 "Arrow
    /// keys" preset; users can remap via Input → Configure controller.
    key_bindings: crate::input::KeyBindings,
    /// `true` while the Input → Configure controller modal is open.
    show_input_modal: bool,
    /// When the user clicks "Rebind" next to a SNES button, this is
    /// set to the button being remapped — the next key press in the
    /// modal becomes its binding.
    pending_rebind: Option<crate::input::SnesButton>,
}

impl LunaApp {
    pub(crate) fn new() -> Self {
        let emu_shared = Arc::new(EmuShared::new());
        let (audio, audio_producer, audio_primed) =
            match crate::audio::AudioBackend::try_start(emu_shared.clone()) {
                Some(AudioStreamArtifacts {
                    backend,
                    producer,
                    primed,
                }) => (Some(backend), Some(producer), Some(primed)),
                None => (None, None, None),
            };
        Self {
            snes: Arc::new(Mutex::new(None)),
            emu_join: None,
            emu_shared,
            rom_path: None,
            rom_title: None,
            framebuffer: None,
            last_error: None,
            paused: false,
            rom_loaded: false,
            instructions_executed: 0,
            last_frame: Instant::now(),
            fps: 0.0,
            show_cpu_panel: true,
            show_ppu_panel: true,
            show_stubs_panel: true,
            force_display: false,
            pc_bytes: [0; 8],
            audio,
            audio_producer,
            audio_primed,
            key_bindings: crate::input::KeyBindings::load_or_default(),
            show_input_modal: false,
            pending_rebind: None,
        }
    }

    /// Load a ROM from disk and reset the emulator.
    ///
    /// Wrapped in `catch_unwind` so that unsupported cartridge types
    /// (Super FX / S-DD1 / SPC7110 etc., none of which luna ships
    /// yet) surface as a friendly error in the status bar instead of
    /// crashing the GUI. LoROM / HiROM / ExHiROM / SA-1 are all
    /// wired through [`Snes::from_cartridge`].
    /// Public wrapper around `load_rom` so `main.rs` can auto-load
    /// a ROM passed on the command line at startup.
    pub(crate) fn load_rom_path(&mut self, path: &Path) {
        self.load_rom(path);
    }

    fn load_rom(&mut self, path: &Path) {
        let cart = match Cartridge::load(path) {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(format!("Failed to load ROM: {e}"));
                return;
            }
        };
        use luna_bus::MapperKind;
        match cart.header.mapper_kind {
            MapperKind::LoRom | MapperKind::HiRom | MapperKind::ExHiRom | MapperKind::Sa1 => {}
            other => {
                self.last_error = Some(format!(
                    "Cartridge needs the {other:?} coprocessor, which isn't yet \
                     emulated. Star Fox / Yoshi's Island (Super FX), Street \
                     Fighter Alpha 2 (S-DD1) and Far East of Eden Zero (SPC7110) \
                     will land in their own dedicated phases. LoROM, HiROM, \
                     ExHiROM and SA-1 carts work today."
                ));
                self.unload_snes();
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
                self.unload_snes();
                if let Ok(mut guard) = self.snes.lock() {
                    *guard = Some(snes);
                }
                // Spawn the dedicated emu thread now that a Snes is
                // present. It pulls samples into the producer ring
                // (held aside since AudioBackend::try_start) at the
                // rate cpal drains it; the UI thread does not pace.
                if let (Some(producer), Some(primed)) =
                    (self.audio_producer.take(), self.audio_primed.take())
                {
                    self.emu_join = Some(crate::emu_thread::spawn(
                        self.snes.clone(),
                        self.emu_shared.clone(),
                        producer,
                        primed,
                    ));
                }
                self.rom_title = Some(title);
                self.rom_path = Some(path.to_path_buf());
                self.instructions_executed = 0;
                self.last_error = None;
                self.framebuffer = None;
                self.rom_loaded = true;
            }
            Err(payload) => {
                self.unload_snes();
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

    /// Tear down the emu thread (if running) and clear the Snes slot
    /// so a fresh ROM can be loaded.
    fn unload_snes(&mut self) {
        // Tell the emu thread to exit, then wake it in case it's parked.
        self.emu_shared.shutdown.store(true, Ordering::Release);
        self.emu_shared.unpark_emu();
        if let Some(join) = self.emu_join.take() {
            let _ = join.join();
        }
        self.emu_shared.shutdown.store(false, Ordering::Release);
        if let Ok(mut guard) = self.snes.lock() {
            *guard = None;
        }
        self.rom_loaded = false;
    }

    /// Snapshot per-UI-frame state (FPS, PC bytes) from the Snes
    /// without blocking. If the emu thread currently holds the lock,
    /// the snapshot is skipped this frame — stale data shown until
    /// the next successful `try_lock`. Imperceptible at 60 Hz UI.
    fn snapshot_state(&mut self) {
        // Mirror the pause flag into the emu thread.
        self.emu_shared.paused.store(self.paused, Ordering::Release);

        if let Ok(mut guard) = self.snes.lock() {
            if let Some(snes) = guard.as_mut() {
                let bytes = snes.peek_pc_bytes(8);
                for (slot, b) in self.pc_bytes.iter_mut().zip(bytes.iter()) {
                    *slot = *b;
                }
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
        // Blocking lock: brief wait (≤ 1 ms) for the emu thread to
        // finish its current batch. Cheaper than the flicker that
        // a try_lock-and-skip would produce.
        let Ok(guard) = self.snes.lock() else {
            return;
        };
        let Some(snes) = guard.as_ref() else {
            return;
        };
        if !self.force_display && snes.ppu.inidisp & 0x80 != 0 && self.framebuffer.is_some() {
            return;
        }
        let mut rgba = Vec::with_capacity(FRAME_W * FRAME_H * 4);
        if self.force_display {
            let opts = luna_ppu::RenderOptions {
                bypass_forced_blank: true,
            };
            let frame = luna_ppu::render_frame_with(&snes.ppu, opts);
            for px in frame {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        } else {
            for px in snes.ppu.framebuffer() {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        drop(guard);
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

        // ---------------- Joypad polling ----------------
        //
        // Push the current keyboard state into the SPC700-side
        // joypad latch every frame. The SNES driver actually sees
        // this mask only at the next VBlank auto-read (~16 ms).
        //
        // Default layout matches Mesen2's "Arrow keys" preset
        // (`UI/Config/KeyPresets.cs::ApplyArrowLayout`); users can
        // remap via Input → Configure controller. See `crate::input`.
        {
            let mask = ctx.input(|i| self.key_bindings.mask_from_input(i));
            if let Ok(mut guard) = self.snes.lock() {
                if let Some(snes) = guard.as_mut() {
                    snes.set_joypad(0, mask);
                }
            }
        }

        // ---------------- FPS bookkeeping ----------------
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .max(0.0001);
        self.fps = 0.9 * self.fps + 0.1 * (1.0 / dt);
        self.last_frame = now;

        // ---------------- Snapshot + framebuffer ----------------
        // Emulation runs on the dedicated emu thread; the UI only
        // snapshots state under `try_lock` and re-renders the
        // framebuffer.
        self.snapshot_state();
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
                        // Mirror into the shared flag so the emu thread
                        // sleeps / wakes. Also unpark so a parked thread
                        // notices the new pause state immediately.
                        self.emu_shared.paused.store(self.paused, Ordering::Release);
                        self.emu_shared.unpark_emu();
                        ui.close_kind(UiKind::Menu);
                    }
                    if ui.button("Reset").clicked() {
                        // Reset goes through the emu thread by briefly
                        // taking the Snes lock. Safe to block here —
                        // it's a menu click, not the per-frame path.
                        if let Ok(mut guard) = self.snes.lock() {
                            if let Some(snes) = guard.as_mut() {
                                snes.reset();
                                self.instructions_executed = 0;
                                self.last_error = None;
                                self.paused = false;
                                self.emu_shared.paused.store(false, Ordering::Release);
                            }
                        }
                        self.emu_shared.unpark_emu();
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
                ui.menu_button("Input", |ui| {
                    if ui.button("Configure controller…").clicked() {
                        self.show_input_modal = true;
                        self.pending_rebind = None;
                        ui.close();
                    }
                    if ui.button("Reset to Mesen2 default (arrow keys)").clicked() {
                        self.key_bindings = crate::input::KeyBindings::default();
                        let _ = self.key_bindings.save();
                        ui.close();
                    }
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
                    // Blocking lock for the panels. The emu thread
                    // holds the lock for at most ~1 batch (≤ 1 ms),
                    // so the UI waits a negligible amount and gets a
                    // consistent snapshot every frame — no flicker.
                    let Ok(guard) = self.snes.lock() else {
                        return;
                    };
                    if let Some(snes) = guard.as_ref() {
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
                if let Ok(guard) = self.snes.lock() {
                    if let Some(snes) = guard.as_ref() {
                        ui.separator();
                        ui.label(format!("MCycles: {}", snes.total_mclk));
                    }
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
        let cpu_stopped = self
            .snes
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.cpu.stopped))
            .unwrap_or(false);
        let cpu_paused = self.paused;
        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.rom_loaded {
                requested_path = draw_landing_page(ui);
                return;
            }
            draw_screen(ui, self.framebuffer.as_ref(), cpu_stopped, cpu_paused);
        });
        if let Some(path) = requested_path {
            self.load_rom(&path);
        }

        self.draw_input_modal(ctx);

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
            for (i, v) in snes.apu_real.to_cpu_ports.iter().enumerate() {
                ui.label(format!("APU $214{i}"));
                ui.label(mono(format!("${v:02X}")));
                ui.end_row();
            }
            ui.label("SPC PC");
            ui.label(mono(format!("${:04X}", snes.apu_real.cpu.pc)));
            ui.end_row();
            ui.label("APU panicked");
            ui.label(mono(snes.apu_panicked.to_string()));
            ui.end_row();
            ui.label("Past IPL");
            ui.label(mono(snes.apu_real.past_iplrom.to_string()));
            ui.end_row();
            ui.label("Frames");
            ui.label(mono(snes.frame_count.to_string()));
            ui.end_row();
            ui.label("PPU line");
            ui.label(mono(snes.ppu_line.to_string()));
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

impl LunaApp {
    /// Render the **Input → Configure controller** modal: a table
    /// of the 12 SNES buttons with the currently-bound keyboard
    /// key and a `Rebind` button. While a rebind is pending the
    /// next non-modifier key press becomes the new binding. The
    /// bindings are persisted to disk on every successful rebind.
    fn draw_input_modal(&mut self, ctx: &Context) {
        if !self.show_input_modal {
            return;
        }
        // If a rebind is pending, capture the next key press as the
        // new binding. We grab it from the raw input so it works
        // even when the modal doesn't focus a widget.
        if let Some(button) = self.pending_rebind {
            let captured = ctx.input(|i| {
                i.events.iter().find_map(|ev| match ev {
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => Some(*key),
                    _ => None,
                })
            });
            if let Some(key) = captured {
                if key != egui::Key::Escape {
                    self.key_bindings.set(button, key);
                    let _ = self.key_bindings.save();
                }
                self.pending_rebind = None;
            }
        }

        let mut open = true;
        egui::Window::new("Configure controller (Player 1)")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(
                    "Click Rebind, then press the new key (Escape to cancel). \
                     Defaults match Mesen2's \"Arrow keys\" SNES preset.",
                );
                ui.add_space(4.0);
                egui::Grid::new("input_grid")
                    .num_columns(3)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for &button in crate::input::SnesButton::ALL.iter() {
                            ui.label(button.label());
                            let key = self.key_bindings.get(button);
                            let label = if self.pending_rebind == Some(button) {
                                "press a key…".to_owned()
                            } else {
                                key.name().to_owned()
                            };
                            ui.monospace(label);
                            if ui.button("Rebind").clicked() {
                                self.pending_rebind = Some(button);
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Reset to defaults").clicked() {
                        self.key_bindings = crate::input::KeyBindings::default();
                        let _ = self.key_bindings.save();
                        self.pending_rebind = None;
                    }
                    if ui.button("Close").clicked() {
                        self.show_input_modal = false;
                        self.pending_rebind = None;
                    }
                });
            });
        if !open {
            self.show_input_modal = false;
            self.pending_rebind = None;
        }
    }
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
