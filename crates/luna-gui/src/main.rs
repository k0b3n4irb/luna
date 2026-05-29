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

use luna_bus::MapperKind;
use luna_cartridge::Cartridge;
use luna_core::Snes;
use luna_ppu::{FRAME_H, FRAME_W};
use pixels::{Pixels, SurfaceTexture};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::audio::AudioStreamArtifacts;
use crate::emu_thread::EmuShared;
use crate::input::KeyBindings;
use crate::ui::{MenuAction, UiOverlay, UiState};

const WINDOW_TITLE: &str = "Luna — SNES Emulator";
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
    snes: Arc<Mutex<Option<Snes>>>,
    /// Handle to the spawned emu thread (one per loaded ROM). The
    /// thread returns the audio producer + primed gate on exit so the
    /// next ROM's spawn can reuse the same cpal stream.
    emu_join: Option<JoinHandle<emu_thread::AudioReclaim>>,
    /// State shared between UI, emu thread, and the cpal callback —
    /// shutdown / pause flags + the unpark handle that the cpal
    /// callback uses to wake the emu thread.
    emu_shared: Arc<EmuShared>,

    /// Latest RGBA framebuffer published by the emu thread. The UI
    /// thread memcpy's this out under a brief lock and blits it.
    framebuffer_rgba: Arc<Mutex<Vec<u8>>>,

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

    /// `true` after a successful `load_rom` until `unload_snes`.
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
    /// Pending async file-dialog result. The dialog runs on a worker
    /// thread so the winit event loop keeps redrawing — otherwise the
    /// WM flags the window as "not responding" within ~2 seconds and
    /// pops a "Force Quit / Wait" prompt. Polled each `about_to_wait`.
    rom_picker_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// `true` while the egui input-config modal is open.
    show_input_config: bool,
    /// When `Some(button)`, the next key press is captured as that
    /// SNES button's new binding and the field is cleared.
    pending_rebind: Option<crate::input::SnesButton>,
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
            snes: Arc::new(Mutex::new(None)),
            emu_join: None,
            emu_shared,
            framebuffer_rgba: Arc::new(Mutex::new(vec![0u8; FRAME_W * FRAME_H * 4])),
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
            rom_picker_rx: None,
            show_input_config: false,
            pending_rebind: None,
        };
        if let Some(path) = auto_rom {
            app.load_rom(&path);
        }
        app
    }

    fn load_rom(&mut self, path: &Path) {
        let cart = match Cartridge::load(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("luna-gui: failed to load ROM: {e}");
                return;
            }
        };
        match cart.header.mapper_kind {
            MapperKind::LoRom | MapperKind::HiRom | MapperKind::ExHiRom | MapperKind::Sa1 => {}
            other => {
                eprintln!("luna-gui: cartridge needs unsupported coprocessor: {other:?}");
                self.unload_snes();
                return;
            }
        }
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // Tear down the previous emulator before installing the new one.
        // This reclaims the audio producer + primed gate from the old
        // emu thread so the new thread can be spawned with them.
        self.unload_snes();
        if let Ok(mut guard) = self.snes.lock() {
            *guard = Some(snes);
        }
        if let (Some(producer), Some(primed)) =
            (self.audio_producer.take(), self.audio_primed.take())
        {
            self.emu_join = Some(crate::emu_thread::spawn(
                self.snes.clone(),
                self.emu_shared.clone(),
                producer,
                primed,
                self.framebuffer_rgba.clone(),
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
    }

    fn unload_snes(&mut self) {
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
        if let Ok(mut guard) = self.snes.lock() {
            *guard = None;
        }
        if let Ok(mut fb) = self.framebuffer_rgba.lock() {
            fb.iter_mut().for_each(|b| *b = 0);
        }
        self.rom_loaded = false;
    }

    /// Push the current keyboard mask into the loaded Snes.
    fn push_joypad(&self) {
        if !self.rom_loaded {
            return;
        }
        let mask = self.key_bindings.mask_from_pressed(&self.pressed_keys);
        if let Ok(mut guard) = self.snes.lock() {
            if let Some(snes) = guard.as_mut() {
                snes.set_joypad(0, mask);
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
        if let Ok(mut guard) = self.snes.lock() {
            if let Some(snes) = guard.as_mut() {
                snes.reset();
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                self.unload_snes();
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
                // Rebind capture: when the input-config modal armed
                // `pending_rebind`, swallow the next key press and
                // assign it to that button. Esc cancels.
                if pressed
                    && !repeat
                    && let Some(button) = self.pending_rebind
                {
                    if code != KeyCode::Escape {
                        self.key_bindings.set(button, code);
                    }
                    self.pending_rebind = None;
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
                            self.unload_snes();
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
        // Request a redraw every ~16 ms to keep the framebuffer current.
        if let Some(win) = self.window.as_ref() {
            win.request_redraw();
        }
    }
}

impl LunaApp {
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // Disjoint borrows: pixels and ui live in separate fields so the
        // closure passed to render_with can capture `&mut ui` without
        // aliasing `self`.
        let Some(pixels) = self.pixels.as_mut() else {
            return;
        };
        let ui = self.ui.as_mut();
        // Copy the latest published SNES frame into the pixels canvas
        // (256×224). pixels handles the upscaling to the window.
        if let Ok(fb) = self.framebuffer_rgba.lock() {
            let dst = pixels.frame_mut();
            let len = FRAME_W * FRAME_H * 4;
            dst[..len].copy_from_slice(&fb[..len]);
        }
        let pending: Mutex<Vec<MenuAction>> = Mutex::new(Vec::new());
        let ui_state = UiState {
            paused: self.emu_shared.paused.load(Ordering::Acquire),
            rom_title: self.rom_title.clone(),
            show_input_config: self.show_input_config,
            key_bindings: &self.key_bindings,
            pending_rebind: self.pending_rebind,
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

    fn dispatch_menu_action(&mut self, action: MenuAction, event_loop: &ActiveEventLoop) {
        match action {
            MenuAction::OpenRom => self.open_rom_dialog(),
            MenuAction::Quit => {
                self.unload_snes();
                event_loop.exit();
            }
            MenuAction::PauseToggle => self.toggle_pause(),
            MenuAction::Reset => self.reset(),
            MenuAction::ToggleInputConfig => {
                self.show_input_config = !self.show_input_config;
                if !self.show_input_config {
                    self.pending_rebind = None;
                }
            }
            MenuAction::StartRebind(button) => {
                self.pending_rebind = Some(button);
            }
            MenuAction::SaveBindings => {
                if let Err(e) = self.key_bindings.save() {
                    eprintln!("luna-gui: save bindings failed: {e}");
                }
            }
        }
    }
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
