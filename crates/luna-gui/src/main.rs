//! Luna SNES emulator — desktop GUI entry point.
//!
//! Single-window winit + pixels stack. The emulator runs on a
//! dedicated thread paced by the cpal audio callback (audio-as-clock);
//! the main thread owns the winit event loop and just blits the
//! shared 256×224 RGBA framebuffer that the emu thread publishes.
//!
//! Replaces the previous eframe + egui + wgpu stack on 2026-05-28.
//! That stack's wgpu state caching and immediate-mode redraws
//! interfered with ROM swaps (multi-second stalls), made every
//! perceived rendering bug ambiguous between core and GUI, and pulled
//! in 200+ MB of build cache for what is fundamentally a 224 KiB
//! framebuffer presentation.
//!
//! Controls (default Mesen2 "Arrow keys" preset):
//!   D-pad : Arrow keys
//!   B / Y : A / Z
//!   A / X : S / X
//!   L / R : Q / W
//!   Select / Start : E / D
//!
//! Keyboard shortcuts:
//!   Ctrl+O : Open ROM
//!   Ctrl+R : Reset
//!   Ctrl+P : Pause / Resume
//!   Ctrl+Q : Quit

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod debug_window;
mod emu_thread;
mod input;
mod ui;

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use luna_api::{Emulator, FRAME_H, FRAME_W};
use pixels::{Pixels, SurfaceTexture};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::audio::AudioStreamArtifacts;
use crate::debug_window::{DebugPanel, DebugWindows};
use crate::emu_thread::EmuShared;
use crate::input::{Hotkey, KeyBindings};
use crate::ui::{DebugSnapshot, MenuAction, PanelNav, UiOverlay, UiState};

const WINDOW_TITLE: &str = "Luna — SNES Emulator";
/// Interval between periodic battery-SRAM auto-flushes (dirty-write only).
const SRAM_FLUSH_SECS: u64 = 30;
const INITIAL_SCALE: u32 = 3;
/// Logical height of the egui menu bar reserved at the top of the
/// window. The pixels canvas sits below it.
const MENU_BAR_LOGICAL_H: u32 = 28;
/// Pixels canvas dimensions = the SNES framebuffer, no margin.
const CANVAS_W: usize = FRAME_W;
const CANVAS_H: usize = FRAME_H;

/// Application state owned by the winit event loop.
struct LunaApp {
    /// Window + pixels surface. Created on first `resumed` event.
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,

    /// Currently-loaded emulator, behind a Mutex shared with the
    /// dedicated emu thread (`emu_thread.rs`).
    emu: Arc<Mutex<Option<Emulator>>>,
    /// Handle to the spawned emu thread (one per loaded ROM). The
    /// thread returns the audio producer + primed gate on exit so the
    /// next ROM's spawn can reuse the same cpal stream.
    emu_join: Option<JoinHandle<emu_thread::AudioReclaim>>,
    /// State shared between UI, emu thread, and the cpal callback —
    /// shutdown / pause flags + the unpark handle that the cpal
    /// callback uses to wake the emu thread.
    emu_shared: Arc<EmuShared>,

    /// Read side of a lock-free triple buffer the emu thread publishes
    /// completed RGBA frames into (replaces the old `Arc<Mutex<Vec<u8>>>`).
    /// The producer never blocks on the UI and the UI never sees a
    /// half-written frame — no lock contention between the two ~60 Hz
    /// clocks (ness `triple_buffer`).
    framebuffer_out: triple_buffer::Output<Vec<u8>>,

    /// Audio backend kept alive for the program's lifetime. cpal owns
    /// the ring's consumer end internally; we hand the producer to the
    /// emu thread and reclaim it on join for the next ROM swap.
    #[allow(dead_code)]
    audio: Option<crate::audio::AudioBackend>,
    audio_producer: Option<ringbuf::HeapProd<(i16, i16)>>,
    audio_primed: Option<Arc<AtomicBool>>,

    /// Set of keys currently held down — recomputed each
    /// `KeyboardInput` event, sampled before every joypad push.
    pressed_keys: HashSet<KeyCode>,
    /// Modifier state for menu shortcuts.
    modifiers: ModifiersState,
    /// Keyboard → SNES button mapping, remappable through Input →
    /// Configure controller… and persisted to
    /// `~/.config/luna/input.json`.
    key_bindings: KeyBindings,

    /// `true` after a successful `load_rom` until `unload_emu`.
    rom_loaded: bool,
    /// Last-opened ROM directory, persisted to
    /// `~/.config/luna/last_rom_dir` so the file dialog re-opens
    /// where the user left off.
    last_rom_dir: Option<PathBuf>,

    /// egui overlay (menu bar + dropdowns) rendered on the same wgpu
    /// device as `pixels`. `None` until the first `resumed` event.
    ui: Option<UiOverlay>,
    /// Friendly ROM name surfaced in the menu bar after a load.
    rom_title: Option<String>,
    /// Sidecar `.srm` path for the loaded ROM's battery SRAM. Restored on
    /// load, written back on `unload_emu` (ROM change + app exit both route
    /// through it), so in-game saves persist across runs.
    srm_path: Option<PathBuf>,
    /// Selected device on controller ports 1 and 2 (Settings → Devices). Fed
    /// to the core each frame; reflected in the menu radios.
    port_device: [luna_api::PortDevice; 2],
    /// Host cursor in SNES framebuffer pixels (`(-1, -1)` = off the screen),
    /// and the previous frame's, so a Mouse gets per-frame deltas.
    cursor_snes: (i32, i32),
    prev_cursor_snes: (i32, i32),
    /// Host pointer buttons: bit 0 = left, bit 1 = right.
    pointer_buttons: u8,
    /// Last battery-SRAM bytes written to `srm_path`; the periodic flush
    /// skips the disk write when the SRAM is unchanged.
    last_srm_written: Vec<u8>,
    /// When the battery SRAM was last auto-flushed (periodic dirty-write).
    last_srm_flush: std::time::Instant,
    /// Pending async file-dialog result. The dialog runs on a worker
    /// thread so the winit event loop keeps redrawing — otherwise the
    /// WM flags the window as "not responding" within ~2 seconds and
    /// pops a "Force Quit / Wait" prompt. Polled each `about_to_wait`.
    rom_picker_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Pending coprocessor-firmware file-dialog result (same async pattern
    /// as `rom_picker_rx`), and the ROM to reload once the user supplies it.
    firmware_picker_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    pending_firmware_rom: Option<PathBuf>,
    /// `true` while the egui input-config modal is open.
    show_input_config: bool,
    /// `true` while the egui hotkey-config modal is open.
    show_hotkey_config: bool,
    /// When `Some(button)`, the next key press is captured as that
    /// SNES button's new binding and the field is cleared.
    pending_rebind: Option<(usize, crate::input::SnesButton)>,
    /// When `Some(hotkey)`, the next key press is captured as that
    /// hotkey's new binding (screenshot, …) and the field is cleared.
    pending_hotkey_rebind: Option<Hotkey>,
    /// Last screenshot filename, surfaced in the menu bar as feedback.
    screenshot_status: Option<String>,
    /// Currently-selected save-state slot (1..=9). The save/load hotkeys
    /// act on this slot; picking a slot from either menu sets it.
    current_slot: u8,
    /// Transient save/load-state feedback surfaced in the menu bar.
    save_state_status: Option<String>,

    /// Debug panels (Debug menu) — each is its own native OS window,
    /// draggable anywhere on the desktop. Data is pulled through
    /// `luna-api` each frame only while a panel is open.
    debug_windows: DebugWindows,
    /// CPU-memory viewer cursor — full 24-bit CPU-bus address.
    cpu_mem_addr: u32,
    /// SPC700-memory viewer cursor — 16-bit ARAM address.
    spc_mem_addr: u16,
    /// SPC700 disassembly: start address + line count.
    spc_disasm_addr: u16,
    spc_disasm_lines: u16,
    /// CPU (65c816) disassembly: 24-bit start address + line count.
    cpu_disasm_addr: u32,
    cpu_disasm_lines: u16,
    /// Tilemap Viewer: which BG layer (0..3) to render.
    tilemap_bg: usize,
}

impl LunaApp {
    fn new(auto_rom: Option<PathBuf>) -> Self {
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
        let mut app = Self {
            window: None,
            pixels: None,
            emu: Arc::new(Mutex::new(None)),
            emu_join: None,
            emu_shared,
            framebuffer_out: triple_buffer::triple_buffer(&vec![0u8; FRAME_W * FRAME_H * 4]).1,
            audio,
            audio_producer,
            audio_primed,
            pressed_keys: HashSet::new(),
            modifiers: ModifiersState::empty(),
            key_bindings: KeyBindings::load_or_default(),
            rom_loaded: false,
            last_rom_dir: load_last_rom_dir(),
            ui: None,
            rom_title: None,
            srm_path: None,
            port_device: [luna_api::PortDevice::Pad; 2],
            cursor_snes: (-1, -1),
            prev_cursor_snes: (-1, -1),
            pointer_buttons: 0,
            last_srm_written: Vec::new(),
            last_srm_flush: std::time::Instant::now(),
            rom_picker_rx: None,
            firmware_picker_rx: None,
            pending_firmware_rom: None,
            show_input_config: false,
            show_hotkey_config: false,
            pending_rebind: None,
            pending_hotkey_rebind: None,
            screenshot_status: None,
            current_slot: 1,
            save_state_status: None,
            debug_windows: DebugWindows::new(),
            cpu_mem_addr: 0x7E_0000,
            spc_mem_addr: 0x0000,
            spc_disasm_addr: 0x0000,
            spc_disasm_lines: 32,
            cpu_disasm_addr: 0x00_8000,
            cpu_disasm_lines: 32,
            tilemap_bg: 0,
        };
        if let Some(path) = auto_rom {
            app.load_rom(&path);
        }
        app
    }

    fn load_rom(&mut self, path: &Path) {
        // Tear down the previous emulator first — this reclaims the audio
        // producer + primed gate from the old emu thread so the new thread
        // can be spawned with them.
        self.unload_emu();
        // Drive everything through luna-api (.claude/rules/api-first.md).
        // `Emulator::load_rom` parses the cart, builds the core, and
        // returns RomInfo; it returns Err for a missing/bad file OR an
        // unsupported coprocessor cart (it catches `from_cartridge`'s
        // panic), so one check covers both.
        let mut em = Emulator::new();
        let info = match em.load_rom(path) {
            Ok(info) => info,
            Err(e) => {
                eprintln!("luna-gui: cannot load ROM (bad file or unsupported coprocessor): {e}");
                return;
            }
        };
        // Restore battery SRAM from the sidecar `<rom>.srm`, if present, so
        // in-game saves survive across runs (written back in `unload_emu`).
        let srm_path = path.with_extension("srm");
        if srm_path.is_file() {
            match std::fs::read(&srm_path) {
                Ok(data) => {
                    if let Err(e) = em.load_sram(&data) {
                        eprintln!(
                            "luna-gui: ignoring battery SRAM {}: {e}",
                            srm_path.display()
                        );
                    }
                }
                Err(e) => eprintln!("luna-gui: could not read {}: {e}", srm_path.display()),
            }
        }
        self.srm_path = Some(srm_path);
        // Seed the periodic-flush baseline with the just-loaded SRAM so the
        // first auto-flush only writes once the game actually modifies it.
        self.last_srm_written = em.sram();
        self.last_srm_flush = std::time::Instant::now();
        if let Ok(mut guard) = self.emu.lock() {
            *guard = Some(em);
        }
        if let (Some(producer), Some(primed)) =
            (self.audio_producer.take(), self.audio_primed.take())
        {
            let (fb_in, fb_out) = triple_buffer::triple_buffer(&vec![0u8; FRAME_W * FRAME_H * 4]);
            self.framebuffer_out = fb_out;
            self.emu_join = Some(crate::emu_thread::spawn(
                self.emu.clone(),
                self.emu_shared.clone(),
                producer,
                primed,
                fb_in,
            ));
        }
        self.rom_loaded = true;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        if let Some(win) = self.window.as_ref() {
            win.set_title(&format!("{WINDOW_TITLE} — {name}"));
        }
        self.rom_title = Some(name);
        if let Some(dir) = path.parent() {
            self.last_rom_dir = Some(dir.to_path_buf());
            let _ = save_last_rom_dir(dir);
        }
        // DSP / coprocessor cart missing its firmware: the game runs inert
        // (e.g. flat Mode 7) until the user supplies it. Prompt them to
        // locate it, Mesen2-style; once installed, reload so it takes effect.
        if let Some(fw_name) = info.missing_firmware {
            self.prompt_for_firmware(path.to_path_buf(), fw_name);
        }
    }

    /// Pop a file dialog asking the user to locate a coprocessor firmware
    /// file (e.g. `dsp1b.rom`). On pick it is installed into luna's firmware
    /// folder and the ROM reloaded. Mirrors [`Self::open_rom_dialog`]'s
    /// off-thread pattern so the event loop keeps redrawing.
    fn prompt_for_firmware(&mut self, rom: PathBuf, fw_name: String) {
        if self.firmware_picker_rx.is_some() {
            return;
        }
        eprintln!(
            "luna-gui: this cartridge needs coprocessor firmware '{fw_name}' — \
             prompting for it (the coprocessor is inert until supplied)."
        );
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("luna-firmware-picker".into())
            .spawn(move || {
                let dialog = rfd::FileDialog::new()
                    .set_title(format!("Locate coprocessor firmware ({fw_name})"))
                    .add_filter("Firmware ROM", &["rom", "bin"])
                    .add_filter("All files", &["*"]);
                let _ = tx.send(dialog.pick_file());
            })
            .expect("spawn luna-firmware-picker thread");
        self.firmware_picker_rx = Some(rx);
        self.pending_firmware_rom = Some(rom);
    }

    /// Pump the firmware file-dialog channel. Called every `about_to_wait`.
    fn poll_firmware_picker(&mut self) {
        let Some(rx) = self.firmware_picker_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Some(fw)) => {
                self.firmware_picker_rx = None;
                match Emulator::install_firmware(&fw, "dsp1b.rom") {
                    Ok(dest) => {
                        eprintln!("luna-gui: installed firmware → {}", dest.display());
                        if let Some(rom) = self.pending_firmware_rom.take() {
                            self.load_rom(&rom); // reload — now the firmware is found
                        }
                    }
                    Err(e) => eprintln!("luna-gui: could not install firmware: {e}"),
                }
            }
            Ok(None) => {
                // Cancelled — leave the game running inert.
                self.firmware_picker_rx = None;
                self.pending_firmware_rom = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.firmware_picker_rx = None;
                self.pending_firmware_rom = None;
            }
        }
    }

    fn unload_emu(&mut self) {
        self.emu_shared.shutdown.store(true, Ordering::Release);
        self.emu_shared.unpark_emu();
        if let Some(join) = self.emu_join.take() {
            // Reclaim producer + primed for the next ROM. Keep primed
            // as the thread left it: the cpal callback can continue
            // draining stale samples from the ring; the next emu
            // thread starts pushing into the freed slots immediately.
            if let Ok((producer, primed)) = join.join() {
                self.audio_producer = Some(producer);
                self.audio_primed = Some(primed);
            }
        }
        self.emu_shared.shutdown.store(false, Ordering::Release);
        // Final battery-SRAM flush before tearing down. Both paths into
        // `unload_emu` — a ROM change and the app close — force-flush here,
        // so the latest in-game save is never lost.
        self.persist_sram(true);
        if let Ok(mut guard) = self.emu.lock() {
            *guard = None;
        }
        self.srm_path = None;
        // Black the screen with a fresh empty triple buffer (the old one's
        // producer was the emu thread we just joined).
        self.framebuffer_out = triple_buffer::triple_buffer(&vec![0u8; FRAME_W * FRAME_H * 4]).1;
        self.rom_loaded = false;
    }

    /// Write the loaded cart's battery SRAM to its `<rom>.srm` sidecar.
    /// With `force`, always writes (final flush on unload); otherwise only
    /// when the bytes changed since the last write — a cheap periodic
    /// auto-save (every [`SRAM_FLUSH_SECS`]) so a save survives an unclean
    /// exit (crash / kill), not just a clean close. Carts without battery
    /// SRAM return empty bytes and write nothing.
    fn persist_sram(&mut self, force: bool) {
        let Some(srm) = self.srm_path.clone() else {
            return;
        };
        let data = {
            let Ok(guard) = self.emu.lock() else {
                return;
            };
            let Some(em) = guard.as_ref() else {
                return;
            };
            em.sram()
        };
        if data.is_empty() || (!force && data == self.last_srm_written) {
            return;
        }
        if let Err(e) = std::fs::write(&srm, &data) {
            eprintln!(
                "luna-gui: could not save battery SRAM to {}: {e}",
                srm.display()
            );
            return;
        }
        self.last_srm_written = data;
    }

    /// Push the current keyboard mask into the loaded Snes.
    fn push_joypad(&self) {
        if !self.rom_loaded {
            return;
        }
        let p1 = self.key_bindings.mask_from_pressed(0, &self.pressed_keys);
        let p2 = self.key_bindings.mask_from_pressed(1, &self.pressed_keys);
        if let Ok(mut guard) = self.emu.lock() {
            if let Some(em) = guard.as_mut() {
                let _ = em.set_joypad(0, p1);
                let _ = em.set_joypad(1, p2);
            }
        }
    }

    /// Feed the host pointer to a Mouse / Super Scope on the selected ports:
    /// a Mouse gets the per-frame motion delta, a Super Scope the absolute aim
    /// (both with the host's left/right buttons). No-op for plain pads.
    fn push_devices(&mut self) {
        if !self.rom_loaded || self.port_device == [luna_api::PortDevice::Pad; 2] {
            return;
        }
        let devices = self.port_device;
        let (cx, cy) = self.cursor_snes;
        let onscreen = cx >= 0 && cy >= 0 && self.prev_cursor_snes.0 >= 0;
        let (dx, dy) = if onscreen {
            (cx - self.prev_cursor_snes.0, cy - self.prev_cursor_snes.1)
        } else {
            (0, 0)
        };
        self.prev_cursor_snes = self.cursor_snes;
        let buttons = self.pointer_buttons;
        if let Ok(mut guard) = self.emu.lock() {
            if let Some(em) = guard.as_mut() {
                for dev in devices {
                    match dev {
                        luna_api::PortDevice::Mouse => {
                            let _ = em.set_mouse(dx, dy, buttons);
                        }
                        luna_api::PortDevice::SuperScope => {
                            let _ = em.set_superscope(cx, cy, buttons);
                        }
                        luna_api::PortDevice::Pad => {}
                    }
                }
            }
        }
    }

    /// File → Open ROM… via rfd, run on a worker thread so the winit
    /// event loop keeps redrawing while the OS dialog is open. The
    /// chosen path (or `None` on cancel) comes back through an mpsc
    /// channel polled in `about_to_wait`.
    ///
    /// `rfd::FileDialog::pick_file()` is synchronous and blocks the
    /// calling thread until the user dismisses the dialog. Calling it
    /// directly from the winit handler froze the main thread for the
    /// dialog's lifetime → the WM flagged the window as "not
    /// responding" within ~2 seconds and popped a Force-Quit prompt.
    fn open_rom_dialog(&mut self) {
        if self.rom_picker_rx.is_some() {
            // A dialog is already in flight — ignore the second click.
            return;
        }
        let last_dir = self.last_rom_dir.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("luna-rom-picker".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("SNES ROM", &["sfc", "smc"])
                    .add_filter("All files", &["*"]);
                if let Some(dir) = last_dir.as_deref() {
                    dialog = dialog.set_directory(dir);
                }
                let _ = tx.send(dialog.pick_file());
            })
            .expect("spawn luna-rom-picker thread");
        self.rom_picker_rx = Some(rx);
    }

    /// Pump the file-dialog channel. Called every `about_to_wait`.
    fn poll_rom_picker(&mut self) {
        if let Some(rx) = self.rom_picker_rx.as_ref() {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.rom_picker_rx = None;
                    self.load_rom(&path);
                }
                Ok(None) => {
                    self.rom_picker_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rom_picker_rx = None;
                }
            }
        }
    }

    fn toggle_pause(&self) {
        let was = self.emu_shared.paused.load(Ordering::Acquire);
        self.emu_shared.paused.store(!was, Ordering::Release);
        self.emu_shared.unpark_emu();
    }

    fn reset(&self) {
        if let Ok(mut guard) = self.emu.lock() {
            if let Some(em) = guard.as_mut() {
                let _ = em.reset();
            }
        }
        self.emu_shared.paused.store(false, Ordering::Release);
        self.emu_shared.unpark_emu();
    }
}

impl ApplicationHandler for LunaApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Window is the SNES image area + a logical 28 px menu strip
        // at the top hosted by the egui overlay. Pixels fills the
        // whole window with the SNES framebuffer; the egui menu bar
        // draws over the top strip with a solid background so the
        // overlap with the game image is invisible to the user.
        let scaled_w = (CANVAS_W as u32) * INITIAL_SCALE;
        let scaled_h = (CANVAS_H as u32) * INITIAL_SCALE + MENU_BAR_LOGICAL_H;
        let mut attrs = WindowAttributes::default()
            .with_title(WINDOW_TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(scaled_w, scaled_h))
            .with_min_inner_size(winit::dpi::LogicalSize::new(
                CANVAS_W as u32,
                (CANVAS_H as u32) + MENU_BAR_LOGICAL_H,
            ));
        // Set the desktop-environment application id so GNOME / KWin /
        // sway label the window as "Luna" rather than the generic
        // "Unknown". Without this, the WM also disowns the window when
        // it goes briefly unresponsive (e.g. waiting on rfd) and pops
        // an "Unknown is not responding" prompt with Force-Quit.
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = WindowAttributesExtWayland::with_name(attrs, "luna-gui", "");
            attrs = WindowAttributesExtX11::with_name(attrs, "luna-gui", "Luna");
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("luna-gui: create_window failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        // Pacing diagnostic (vsync-lock work): the display refresh rate vs the
        // emu's ~60.099 fps (NTSC) sets the frame-presentation cadence — a
        // mismatch is the root of motion judder. Log it so we fix the right case.
        match window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
        {
            Some(mhz) => eprintln!(
                "luna-gui: display refresh = {:.3} Hz (emu target ≈ 60.099 NTSC / 50.007 PAL)",
                f64::from(mhz) / 1000.0
            ),
            None => eprintln!("luna-gui: display refresh rate unknown (monitor query None)"),
        }
        let surface = SurfaceTexture::new(size.width, size.height, window.clone());
        let pixels = match Pixels::new(CANVAS_W as u32, CANVAS_H as u32, surface) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("luna-gui: pixels init failed: {e}");
                event_loop.exit();
                return;
            }
        };
        // egui overlay shares the wgpu device + surface format with pixels.
        let device = pixels.device();
        let format = pixels.render_texture_format();
        let ui = UiOverlay::new(&window, device, format);
        self.window = Some(window);
        self.pixels = Some(pixels);
        self.ui = Some(ui);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // A debug panel's own native window: route the event to it and
        // stop — none of the game-window handling below applies.
        if self.debug_windows.owns(id) {
            if matches!(event, WindowEvent::RedrawRequested) {
                self.redraw_debug_window(id);
            } else {
                self.debug_windows.on_window_event(id, &event);
            }
            return;
        }
        // Let egui consume the event first so menu clicks / hovers
        // don't leak into the game-side joypad path.
        let consumed_by_ui = if let (Some(ui), Some(win)) = (self.ui.as_mut(), self.window.as_ref())
        {
            ui.on_window_event(win, &event)
        } else {
            false
        };
        match event {
            WindowEvent::CloseRequested => {
                self.unload_emu();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(pixels) = self.pixels.as_mut() {
                    let w = NonZeroU32::new(size.width).map_or(1, NonZeroU32::get);
                    let h = NonZeroU32::new(size.height).map_or(1, NonZeroU32::get);
                    if let Err(e) = pixels.resize_surface(w, h) {
                        eprintln!("luna-gui: pixels resize failed: {e}");
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Map the host cursor to SNES framebuffer pixels (Err = the
                // pointer is outside the game area → offscreen for a scope).
                self.cursor_snes = self
                    .pixels
                    .as_ref()
                    .and_then(|p| {
                        p.window_pos_to_pixel((position.x as f32, position.y as f32))
                            .ok()
                    })
                    .map_or((-1, -1), |(px, py)| (px as i32, py as i32));
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                let bit = match button {
                    winit::event::MouseButton::Left => 0x01,
                    winit::event::MouseButton::Right => 0x02,
                    _ => 0,
                };
                if btn_state == winit::event::ElementState::Pressed {
                    self.pointer_buttons |= bit;
                } else {
                    self.pointer_buttons &= !bit;
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                // Rebind capture: when the input-config modal armed a
                // pending rebind, swallow the next key press and assign
                // it to that button / hotkey. Esc cancels.
                if pressed
                    && !repeat
                    && let Some((player, button)) = self.pending_rebind
                {
                    if code != KeyCode::Escape {
                        self.key_bindings.set(player, button, code);
                    }
                    self.pending_rebind = None;
                    return;
                }
                if pressed
                    && !repeat
                    && let Some(hotkey) = self.pending_hotkey_rebind
                {
                    if code != KeyCode::Escape {
                        self.key_bindings.set_hotkey(hotkey, code);
                    }
                    self.pending_hotkey_rebind = None;
                    return;
                }
                // Emulator hotkeys (remappable; default Screenshot = F12).
                if pressed
                    && !repeat
                    && let Some(hotkey) = self.key_bindings.hotkey_for(code)
                {
                    match hotkey {
                        Hotkey::Screenshot => self.take_screenshot(),
                        Hotkey::SaveState => self.save_state_to_slot(self.current_slot),
                        Hotkey::LoadState => self.load_state_from_slot(self.current_slot),
                        Hotkey::Pause => self.toggle_pause(),
                        Hotkey::Reset => self.reset(),
                    }
                    return;
                }
                if consumed_by_ui {
                    return;
                }
                if pressed && !repeat && self.modifiers.control_key() {
                    match code {
                        KeyCode::KeyO => self.open_rom_dialog(),
                        KeyCode::KeyR => self.reset(),
                        KeyCode::KeyP => self.toggle_pause(),
                        KeyCode::KeyQ => {
                            self.unload_emu();
                            event_loop.exit();
                            return;
                        }
                        _ => {}
                    }
                }
                if pressed {
                    self.pressed_keys.insert(code);
                } else {
                    self.pressed_keys.remove(&code);
                }
                self.push_joypad();
            }
            WindowEvent::DroppedFile(path) => {
                self.load_rom(&path);
            }
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.poll_rom_picker();
        self.poll_firmware_picker();
        // Feed the host pointer to a Mouse / Super Scope once per loop tick.
        self.push_devices();
        // Periodic battery-SRAM auto-flush (dirty-write only) so an in-game
        // save survives an unclean exit (crash / kill), not just a clean
        // close. The byte-compare in `persist_sram` makes idle frames free.
        if self.last_srm_flush.elapsed() >= std::time::Duration::from_secs(SRAM_FLUSH_SECS) {
            self.persist_sram(false);
            self.last_srm_flush = std::time::Instant::now();
        }
        // Request a redraw every ~16 ms to keep the framebuffer current.
        if let Some(win) = self.window.as_ref() {
            win.request_redraw();
        }
        // Keep open debug windows refreshing their live register/memory views.
        self.debug_windows.request_redraw_all();
    }
}

impl LunaApp {
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // Compute the occupied-slot flags before borrowing `pixels` mutably
        // (it scans `self`), so the borrow checker stays happy.
        let occupied_slots = self.occupied_slots();
        // Disjoint borrows: pixels and ui live in separate fields so the
        // closure passed to render_with can capture `&mut ui` without
        // aliasing `self`.
        let Some(pixels) = self.pixels.as_mut() else {
            return;
        };
        let ui = self.ui.as_mut();
        // Copy the latest published SNES frame into the pixels canvas
        // (256×224). pixels handles the upscaling to the window.
        {
            let len = FRAME_W * FRAME_H * 4;
            // Lock-free fetch of the latest published frame (no Mutex).
            let fb = self.framebuffer_out.read();
            pixels.frame_mut()[..len].copy_from_slice(&fb[..len]);
        }
        // Debug panels live in their own native windows now (rendered in
        // `redraw_debug_window`), so the main window only needs the open/
        // closed state to tick the Debug-menu checkmarks.
        let pending: Mutex<Vec<MenuAction>> = Mutex::new(Vec::new());
        let ui_state = UiState {
            paused: self.emu_shared.paused.load(Ordering::Acquire),
            rom_title: self.rom_title.clone(),
            port_device: self.port_device,
            show_input_config: self.show_input_config,
            show_hotkey_config: self.show_hotkey_config,
            key_bindings: &self.key_bindings,
            show_cpu_state: self.debug_windows.is_open(DebugPanel::Cpu),
            show_cpu_memory: self.debug_windows.is_open(DebugPanel::CpuMemory),
            show_cpu_disasm: self.debug_windows.is_open(DebugPanel::CpuDisasm),
            show_spc700: self.debug_windows.is_open(DebugPanel::Spc700),
            show_spc700_memory: self.debug_windows.is_open(DebugPanel::Spc700Memory),
            show_spc700_disasm: self.debug_windows.is_open(DebugPanel::Spc700Disasm),
            show_sprites: self.debug_windows.is_open(DebugPanel::Sprites),
            show_registers: self.debug_windows.is_open(DebugPanel::Registers),
            show_palette: self.debug_windows.is_open(DebugPanel::Palette),
            show_tilemap: self.debug_windows.is_open(DebugPanel::Tilemap),
            show_event_viewer: self.debug_windows.is_open(DebugPanel::EventViewer),
            pending_rebind: self.pending_rebind,
            pending_hotkey_rebind: self.pending_hotkey_rebind,
            screenshot_status: self.screenshot_status.clone(),
            save_state_status: self.save_state_status.clone(),
            occupied_slots,
        };
        let win_size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let result = pixels.render_with(|encoder, target_view, ctx| {
            ctx.scaling_renderer.render(encoder, target_view);
            if let Some(ui) = ui {
                let mut sink = pending.lock().unwrap();
                ui.render(
                    &window,
                    &ctx.device,
                    &ctx.queue,
                    encoder,
                    target_view,
                    scale,
                    (win_size.width, win_size.height),
                    &ui_state,
                    |action| sink.push(action),
                );
            }
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("luna-gui: pixels render_with failed: {e}");
        }
        for action in pending.into_inner().unwrap_or_default() {
            self.dispatch_menu_action(action, event_loop);
        }
    }

    /// Build the `luna-api` snapshot for a single debug panel (api-first).
    /// Only the data that panel renders is fetched, so a CPU/SPC window is
    /// a couple of cheap register reads, not a full `state()` clone.
    fn build_panel_snapshot(&self, panel: DebugPanel) -> DebugSnapshot {
        let cpu_addr = self.cpu_mem_addr & 0xFF_FFFF;
        let spc_addr = self.spc_mem_addr;
        self.emu
            .lock()
            .ok()
            .and_then(|mut g| {
                g.as_mut().map(|em| {
                    let mut snap = DebugSnapshot::default();
                    match panel {
                        DebugPanel::Cpu => snap.cpu = em.cpu_state().ok(),
                        DebugPanel::Spc700 => snap.spc700 = em.spc700_state().ok(),
                        DebugPanel::Sprites => snap.sprites = em.decode_sprites().ok(),
                        DebugPanel::Registers => snap.registers = Some(em.state()),
                        DebugPanel::Palette => snap.palette = em.peek_cgram().ok(),
                        DebugPanel::Tilemap => {
                            snap.tilemap = em.render_tilemap_rgba(self.tilemap_bg).ok();
                            snap.tilemap_bg = self.tilemap_bg;
                        }
                        DebugPanel::EventViewer => {
                            let events = em.event_snapshot();
                            if let Ok(fb) = em.render_frame_rgba(false) {
                                snap.event_overlay = Some(composite_event_overlay(&fb, &events));
                            }
                            snap.event_config = Some(em.event_config().clone());
                            snap.event_list = Some(events);
                        }
                        DebugPanel::CpuMemory => {
                            let (bank, off) = ((cpu_addr >> 16) as u8, cpu_addr as u16);
                            snap.cpu_memory =
                                em.peek_memory(bank, off, 256).ok().map(|b| (cpu_addr, b));
                        }
                        DebugPanel::Spc700Memory => {
                            snap.spc_memory =
                                em.peek_aram(spc_addr, 256).ok().map(|b| (spc_addr, b));
                        }
                        DebugPanel::Spc700Disasm => {
                            snap.spc_disasm = em
                                .disassemble_spc(self.spc_disasm_addr, self.spc_disasm_lines)
                                .ok();
                            snap.spc_disasm_lines = self.spc_disasm_lines;
                        }
                        DebugPanel::CpuDisasm => {
                            let (m8, x8) = em.cpu_state().ok().map_or((true, true), |c| {
                                (c.e || c.p & 0x20 != 0, c.e || c.p & 0x10 != 0)
                            });
                            snap.cpu_disasm = em
                                .disassemble_cpu(
                                    self.cpu_disasm_addr,
                                    self.cpu_disasm_lines,
                                    m8,
                                    x8,
                                )
                                .ok();
                            snap.cpu_disasm_lines = self.cpu_disasm_lines;
                        }
                    }
                    snap
                })
            })
            .unwrap_or_default()
    }

    /// Repaint one debug window with fresh data and apply any memory-nav
    /// actions its body emitted (page / bank buttons).
    fn redraw_debug_window(&mut self, id: WindowId) {
        let Some(panel) = self.debug_windows.panel_of(id) else {
            return;
        };
        let snap = self.build_panel_snapshot(panel);
        let (nav, close) = self.debug_windows.render(id, &snap);
        match nav {
            Some(PanelNav::MemAddr(d)) => match panel {
                DebugPanel::CpuMemory => {
                    self.cpu_mem_addr =
                        (i64::from(self.cpu_mem_addr) + d).rem_euclid(0x100_0000) as u32;
                }
                DebugPanel::Spc700Memory => {
                    self.spc_mem_addr =
                        (i64::from(self.spc_mem_addr) + d).rem_euclid(0x1_0000) as u16;
                }
                _ => {}
            },
            Some(PanelNav::DisasmGotoPc) => {
                // Re-anchor at the live PC, read fresh from the emulator.
                let pc = self.emu.lock().ok().and_then(|mut g| {
                    g.as_mut().and_then(|em| {
                        if panel == DebugPanel::CpuDisasm {
                            em.cpu_state()
                                .ok()
                                .map(|c| (u32::from(c.pb) << 16) | u32::from(c.pc))
                        } else {
                            em.spc700_state().ok().map(|s| u32::from(s.pc))
                        }
                    })
                });
                if let Some(pc) = pc {
                    if panel == DebugPanel::CpuDisasm {
                        self.cpu_disasm_addr = pc & 0xFF_FFFF;
                    } else {
                        self.spc_disasm_addr = pc as u16;
                    }
                }
            }
            Some(PanelNav::DisasmSetAddr(a)) => {
                if panel == DebugPanel::CpuDisasm {
                    self.cpu_disasm_addr = a & 0xFF_FFFF;
                } else {
                    self.spc_disasm_addr = a as u16;
                }
            }
            Some(PanelNav::DisasmSetLines(n)) => {
                let n = n.clamp(4, 128);
                if panel == DebugPanel::CpuDisasm {
                    self.cpu_disasm_lines = n;
                } else {
                    self.spc_disasm_lines = n;
                }
            }
            Some(PanelNav::TilemapSetBg(bg)) => {
                self.tilemap_bg = bg.min(3);
            }
            Some(PanelNav::EventViewer(act)) => {
                use crate::ui::EventViewerAction;
                if let Ok(mut g) = self.emu.lock() {
                    if let Some(em) = g.as_mut() {
                        let cfg = em.event_config_mut();
                        match act {
                            EventViewerAction::Category(i, on) => {
                                if let Some(v) = cfg.visible.get_mut(i) {
                                    *v = on;
                                }
                            }
                            EventViewerAction::DmaChannel(ch, on) => {
                                if let Some(v) = cfg.show_dma_channels.get_mut(ch) {
                                    *v = on;
                                }
                            }
                            EventViewerAction::PreviousFrame(on) => {
                                cfg.show_previous_frame = on;
                            }
                            EventViewerAction::All(on) => {
                                cfg.visible = [on; luna_api::CATEGORY_COUNT];
                                cfg.show_dma_channels = [on; 8];
                            }
                        }
                    }
                }
            }
            None => {}
        }
        if close {
            self.debug_windows.close(id);
        }
    }

    fn dispatch_menu_action(&mut self, action: MenuAction, event_loop: &ActiveEventLoop) {
        match action {
            MenuAction::OpenRom => self.open_rom_dialog(),
            MenuAction::Quit => {
                self.unload_emu();
                event_loop.exit();
            }
            MenuAction::PauseToggle => self.toggle_pause(),
            MenuAction::Reset => self.reset(),
            MenuAction::ToggleCpuState => self.debug_windows.toggle(event_loop, DebugPanel::Cpu),
            MenuAction::ToggleCpuMemory => {
                self.debug_windows.toggle(event_loop, DebugPanel::CpuMemory);
            }
            MenuAction::ToggleCpuDisasm => {
                self.debug_windows.toggle(event_loop, DebugPanel::CpuDisasm);
            }
            MenuAction::ToggleSpc700 => self.debug_windows.toggle(event_loop, DebugPanel::Spc700),
            MenuAction::ToggleSpc700Memory => {
                self.debug_windows
                    .toggle(event_loop, DebugPanel::Spc700Memory);
            }
            MenuAction::ToggleSpc700Disasm => {
                self.debug_windows
                    .toggle(event_loop, DebugPanel::Spc700Disasm);
            }
            MenuAction::ToggleSprites => self.debug_windows.toggle(event_loop, DebugPanel::Sprites),
            MenuAction::ToggleRegisters => {
                self.debug_windows.toggle(event_loop, DebugPanel::Registers);
            }
            MenuAction::TogglePalette => {
                self.debug_windows.toggle(event_loop, DebugPanel::Palette);
            }
            MenuAction::ToggleTilemap => {
                self.debug_windows.toggle(event_loop, DebugPanel::Tilemap);
            }
            MenuAction::ToggleEventViewer => {
                self.debug_windows
                    .toggle(event_loop, DebugPanel::EventViewer);
                // Capture only runs while the panel is open (it adds per-access
                // tracing overhead).
                let on = self.debug_windows.is_open(DebugPanel::EventViewer);
                if let Ok(mut g) = self.emu.lock() {
                    if let Some(em) = g.as_mut() {
                        let _ = em.enable_event_capture(on);
                    }
                }
            }
            MenuAction::ToggleInputConfig => {
                self.show_input_config = !self.show_input_config;
                if !self.show_input_config {
                    self.pending_rebind = None;
                }
            }
            MenuAction::ToggleHotkeyConfig => {
                self.show_hotkey_config = !self.show_hotkey_config;
                if !self.show_hotkey_config {
                    self.pending_hotkey_rebind = None;
                }
            }
            MenuAction::StartRebind(player, button) => {
                self.pending_rebind = Some((player, button));
                self.pending_hotkey_rebind = None;
            }
            MenuAction::StartRebindHotkey(hotkey) => {
                self.pending_hotkey_rebind = Some(hotkey);
                self.pending_rebind = None;
            }
            MenuAction::TakeScreenshot => self.take_screenshot(),
            MenuAction::SaveBindings => {
                if let Err(e) = self.key_bindings.save() {
                    eprintln!("luna-gui: save bindings failed: {e}");
                }
            }
            MenuAction::ResetBindings(player) => {
                self.key_bindings.reset_bindings(player);
                self.pending_rebind = None;
            }
            MenuAction::ApplyPreset(player, preset) => {
                self.key_bindings.apply_preset(player, preset);
                self.pending_rebind = None;
            }
            MenuAction::ResetHotkeys => {
                self.key_bindings.reset_hotkeys();
                self.pending_hotkey_rebind = None;
            }
            MenuAction::SaveState(slot) => self.save_state_to_slot(slot),
            MenuAction::LoadState(slot) => self.load_state_from_slot(slot),
            MenuAction::SetPortDevice(port, dev) => self.set_port_device(port, dev),
        }
    }

    /// Assign a device to a controller port (Settings → Devices), driving the
    /// core through `luna_api::Emulator::set_port_device` (api-first).
    fn set_port_device(&mut self, port: u8, dev: luna_api::PortDevice) {
        self.port_device[port as usize] = dev;
        if let Ok(mut guard) = self.emu.lock() {
            if let Some(em) = guard.as_mut() {
                let _ = em.set_port_device(port, dev);
            }
        }
    }

    /// Save the current on-screen frame to a PNG. Ports Mesen2's
    /// `BaseVideoFilter::TakeScreenshot(romName, …)`: snapshot the
    /// output buffer, then write `<rom>_NNN.png` with a zero-padded
    /// auto-incrementing counter into [`screenshot_dir`]
    /// (`$HOME/.local/luna/screenshots`).
    ///
    /// We capture the GUI's published RGBA framebuffer (256×224) — the
    /// exact pixels on screen — so the PNG matches what the user sees.
    fn take_screenshot(&mut self) {
        let buf = self.framebuffer_out.read().clone();
        let Some(img) = image::RgbaImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf) else {
            eprintln!("luna-gui: screenshot skipped — framebuffer size mismatch");
            return;
        };
        // ROM filename without extension, or "luna" if nothing loaded.
        let base = self.rom_title.as_deref().unwrap_or("luna");
        let base = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
        let path = next_screenshot_path(base);
        match img.save(&path) {
            Ok(()) => {
                let shown = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                eprintln!("luna-gui: screenshot saved → {}", path.display());
                self.screenshot_status = Some(format!("\u{1F4F7} {shown}"));
            }
            Err(e) => eprintln!("luna-gui: screenshot save failed: {e}"),
        }
    }

    /// Lowercase-kebab slug for the current ROM, used in slot filenames.
    /// Returns `None` when no ROM is loaded.
    fn rom_slug(&self) -> Option<String> {
        let title = self.rom_title.as_deref()?;
        // Drop the extension, then slugify the stem.
        let stem = title.rsplit_once('.').map_or(title, |(s, _)| s);
        let s = slug(stem);
        if s.is_empty() { None } else { Some(s) }
    }

    /// Which slots (1..=9, returned indexed 0..9) have a state file on disk
    /// for the current ROM. Cheap file-existence checks, recomputed per
    /// frame for the menu's occupied marker.
    fn occupied_slots(&self) -> [bool; 9] {
        let mut out = [false; 9];
        let Some(slug) = self.rom_slug() else {
            return out;
        };
        let dir = states_dir();
        for (i, flag) in out.iter_mut().enumerate() {
            let slot = (i + 1) as u8;
            *flag = dir.join(slot_filename(&slug, slot)).exists();
        }
        out
    }

    /// Save emulator state to `slot` (1..=9) and make it the current slot.
    /// Goes through `luna_api::Emulator::save_state` (api-first); the GUI
    /// only adds the file write. No-op with a status message if no ROM.
    fn save_state_to_slot(&mut self, slot: u8) {
        self.current_slot = slot;
        let Some(slug) = self.rom_slug() else {
            self.save_state_status = Some("No ROM loaded".to_string());
            return;
        };
        // Lock the emu and snapshot through the API (api-first).
        let encoded = self
            .emu
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(luna_api::Emulator::save_state));
        let bytes = match encoded {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                eprintln!("luna-gui: save state failed: {e}");
                self.save_state_status = Some(format!("Save failed: {e}"));
                return;
            }
            None => {
                self.save_state_status = Some("No ROM loaded".to_string());
                return;
            }
        };
        let dir = states_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("luna-gui: cannot create states dir: {e}");
            self.save_state_status = Some(format!("Save failed: {e}"));
            return;
        }
        let path = dir.join(slot_filename(&slug, slot));
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                eprintln!("luna-gui: state saved → {}", path.display());
                self.save_state_status = Some(format!("\u{1F4BE} Slot {slot} saved"));
            }
            Err(e) => {
                eprintln!("luna-gui: state write failed: {e}");
                self.save_state_status = Some(format!("Save failed: {e}"));
            }
        }
    }

    /// Load emulator state from `slot` (1..=9) and make it the current slot.
    /// Goes through `luna_api::Emulator::load_state` (api-first); a
    /// wrong-ROM / corrupt blob surfaces as a status message, never a crash.
    fn load_state_from_slot(&mut self, slot: u8) {
        self.current_slot = slot;
        let Some(slug) = self.rom_slug() else {
            self.save_state_status = Some("No ROM loaded".to_string());
            return;
        };
        let path = states_dir().join(slot_filename(&slug, slot));
        let Ok(bytes) = std::fs::read(&path) else {
            self.save_state_status = Some(format!("Slot {slot} empty"));
            return;
        };
        // Lock the emu and restore through the API (api-first).
        let result = self
            .emu
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().map(|em| em.load_state(&bytes)));
        match result {
            Some(Ok(())) => {
                eprintln!("luna-gui: state loaded ← {}", path.display());
                self.save_state_status = Some(format!("\u{1F4C2} Slot {slot} loaded"));
            }
            Some(Err(e)) => {
                eprintln!("luna-gui: load state failed: {e}");
                self.save_state_status = Some(format!("Load failed: {e}"));
            }
            None => {
                self.save_state_status = Some("No ROM loaded".to_string());
            }
        }
    }
}

/// Directory screenshots are written to: `$HOME/.local/luna/screenshots`
/// (a fixed location, like Mesen2's `~/Screenshots`, so captures land in
/// the same place regardless of the launch directory). Falls back to a
/// cwd-relative `screenshots/` if `$HOME` is unset.
fn screenshot_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("screenshots"),
        |home| {
            PathBuf::from(home)
                .join(".local")
                .join("luna")
                .join("screenshots")
        },
    )
}

/// Directory save states are written to: `$HOME/.local/luna/states`, a
/// sibling of [`screenshot_dir`]. Falls back to a cwd-relative `states/`
/// if `$HOME` is unset.
fn states_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("states"),
        |home| {
            PathBuf::from(home)
                .join(".local")
                .join("luna")
                .join("states")
        },
    )
}

/// Filename for a ROM's save-state slot: `<rom-slug>.slot<N>.luna`.
fn slot_filename(slug: &str, slot: u8) -> String {
    format!("{slug}.slot{slot}.luna")
}

/// Lowercase-kebab slug of `name`: alphanumerics lowercased, every run of
/// non-alphanumeric characters collapsed to a single `-`, and leading /
/// trailing `-` trimmed. Used to derive a stable, filesystem-safe filename
/// stem from a ROM title.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Pick `<screenshot_dir>/<base>_NNN.png` with the lowest free 3-digit
/// counter, creating the folder if needed (Mesen2's numbering scheme).
fn next_screenshot_path(base: &str) -> PathBuf {
    let dir = screenshot_dir();
    let _ = std::fs::create_dir_all(&dir);
    for counter in 0..1000 {
        let candidate = dir.join(format!("{base}_{counter:03}.png"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{base}_overflow.png"))
}

fn main() {
    let auto_rom = std::env::args().nth(1).map(PathBuf::from);
    let event_loop = match EventLoop::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("luna-gui: event loop init failed: {e}");
            std::process::exit(1);
        }
    };
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
    let mut app = LunaApp::new(auto_rom);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("luna-gui: event loop error: {e}");
    }
}

/// `~/.config/luna/last_rom_dir` — single-line text file holding the
/// directory the user last opened a ROM from.
fn last_rom_dir_path() -> Option<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return None;
    };
    Some(base.join("luna").join("last_rom_dir"))
}

fn load_last_rom_dir() -> Option<PathBuf> {
    let path = last_rom_dir_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let dir = PathBuf::from(raw.trim());
    if dir.is_dir() { Some(dir) } else { None }
}

fn save_last_rom_dir(dir: &Path) -> std::io::Result<()> {
    let path = last_rom_dir_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, dir.to_string_lossy().as_ref())
}

/// Composite the Event Viewer overlay — the game framebuffer with the frame's
/// captured events drawn over it. Faithful port of Mesen2's `BaseEventManager`
/// `GetDisplayBuffer` / `DrawScreen` / `DrawEvents` (see
/// `docs/event_viewer_reference.md` §C). Returns `(rgba, width, height)`.
fn composite_event_overlay(
    fb: &[u8],
    events: &[luna_api::EventViewerEvent],
) -> (Vec<u8>, usize, usize) {
    // Display buffer: 682 wide (1364 H-clocks / 2) × scanlines*2 (262 NTSC).
    const W: usize = 682;
    const H: usize = 524;
    const FB_W: usize = 256;
    const FB_H: usize = 224;

    // Clear to 0xFF555555 (dark-gray border).
    let mut buf = vec![0u8; W * H * 4];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&[0x55, 0x55, 0x55, 0xFF]);
    }

    // DrawScreen: blit the 256×224 framebuffer with a lo-res 2×2 upsample,
    // centred at col +44 / row +2 inside the 682-wide buffer.
    if fb.len() >= FB_W * FB_H * 4 {
        for dy in 0..(FB_H * 2) {
            for dx in 0..(FB_W * 2) {
                let s = ((dy >> 1) * FB_W + (dx >> 1)) * 4;
                let d = ((dy + 2) * W + (dx + 44)) * 4;
                buf[d..d + 4].copy_from_slice(&fb[s..s + 4]);
            }
        }
    }

    // DrawEvents: two passes (all halos, then all cores) so cores sit on top.
    for draw_background in [true, false] {
        for ev in events {
            // ConvertScanlineCycleToRowColumn: x = cycle/2, y = scanline*2.
            let x = (ev.cycle / 2) as i32;
            let y = (ev.scanline as i32) * 2;
            draw_event_dot(&mut buf, W, H, x, y, ev.category.color(), draw_background);
        }
    }
    // TODO: pause-mode current-scanline line (yellow) + cursor dot (magenta) —
    // Mesen2 draws these only when broken/paused; luna runs live, so deferred
    // until an Event Viewer pause/step mode exists.
    (buf, W, H)
}

/// `BaseEventManager::DrawDot` — a 6×6 half-bright halo (`draw_background`) or a
/// 2×2 opaque core. Ported verbatim from `BaseEventManager.cpp:32-60`.
fn draw_event_dot(
    buf: &mut [u8],
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    color: (u8, u8, u8),
    draw_background: bool,
) {
    let (r, g, b) = color;
    let (r, g, b) = if draw_background {
        ((r >> 1) & 0x7F, (g >> 1) & 0x7F, (b >> 1) & 0x7F)
    } else {
        (r, g, b)
    };
    let (lo, hi) = if draw_background {
        (-2i32, 3i32)
    } else {
        (0i32, 1i32)
    };
    for i in lo..=hi {
        for j in lo..=hi {
            let (px, py) = (x + j, y + i);
            if px < 0 || px >= w as i32 || py < 0 || py >= h as i32 {
                continue;
            }
            let d = (py as usize * w + px as usize) * 4;
            buf[d..d + 4].copy_from_slice(&[r, g, b, 0xFF]);
        }
    }
}
