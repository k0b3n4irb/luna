//! Dedicated emulator thread, paced by the cpal audio callback.
//!
//! The thread owns the [`Snes`] state behind an `Arc<Mutex<>>` shared
//! with the UI thread. It loops:
//!
//!   1. Acquire the Snes lock briefly,
//!   2. Step a short batch of CPU instructions,
//!   3. Drain the APU's audio queue into the cpal producer ring,
//!   4. Release the lock,
//!   5. If the ring filled up before the queue drained, [`park`] until
//!      the cpal callback [`unpark`]s us (signal: "I consumed samples,
//!      you can produce more"). Otherwise [`yield_now`] so the UI
//!      thread gets a chance to lock for its framebuffer snapshot.
//!
//! This is the standard "audio-as-clock" pattern: the host audio
//! device's wall-clock rate (the cpal callback frequency) sets the
//! emulator's pace. The UI's repaint cadence has no influence on
//! emulation speed — it just snapshots whatever state is current.
//!
//! [`park`]: std::thread::park
//! [`unpark`]: std::thread::Thread::unpark
//! [`yield_now`]: std::thread::yield_now

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use luna_api::Emulator;
use ringbuf::HeapProd;
use ringbuf::traits::{Observer, Producer};

/// Reclaimed audio-side state the emu thread hands back to the UI on
/// exit, so the next ROM's [`spawn`] can reuse the same cpal-bound
/// producer + the silence-gate flag.
pub(crate) type AudioReclaim = (HeapProd<(i16, i16)>, Arc<AtomicBool>);

/// State shared between the UI thread, the emu thread, and the cpal
/// callback. Cheaply clonable via `Arc<Self>`.
pub(crate) struct EmuShared {
    /// Set by the UI thread on ROM unload / app exit. The emu thread
    /// checks this every batch and exits its loop.
    pub shutdown: AtomicBool,
    /// Pause flag — flipped by the UI's Pause menu item. While set,
    /// the emu thread sleeps in a short `park_timeout` cycle.
    pub paused: AtomicBool,
    /// Handle to the emu thread, registered once at startup. The cpal
    /// callback calls `.unpark()` on this after consuming samples,
    /// waking the emu thread if it parked on a full producer ring.
    pub thread_handle: Mutex<Option<thread::Thread>>,
}

impl EmuShared {
    pub(crate) const fn new() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            thread_handle: Mutex::new(None),
        }
    }

    /// Wake the emu thread if it is currently parked. Called from the
    /// cpal audio callback every time it consumes samples — the
    /// frequency of this matches the host device's pull rate, so the
    /// emu thread's effective production rate equals the cpal drain
    /// rate.
    pub(crate) fn unpark_emu(&self) {
        if let Ok(g) = self.thread_handle.lock() {
            if let Some(t) = g.as_ref() {
                t.unpark();
            }
        }
    }
}

/// Spawn the emu thread. Returns its `JoinHandle` so the UI can `.join()`
/// on shutdown / ROM unload to ensure the thread releases its `Snes`
/// borrow before the UI drops the Mutex. The `primed` flag is shared
/// with the cpal callback — the emu thread flips it on first push so
/// the callback's "silence until ready" gate opens.
///
/// `framebuffer_rgba` is a shared 256×224×4 RGBA byte buffer the emu
/// thread fills once per emulated frame; the UI thread just memcpy's
/// it out under a brief lock and uploads to the GPU. Decouples the
/// UI's repaint cadence from the Snes Mutex (which the emu thread
/// holds for the duration of each ~1024-instruction batch). This is
/// the visual analogue of the audio-as-clock decoupling.
///
/// Forced-blank (INIDISP bit 7) frames are NOT published — the UI
/// keeps showing the previous good frame. Most games toggle bit 7
/// every `VBlank` to upload tiles/OAM safely, so without this the
/// screen would flash black once per second.
pub(crate) fn spawn(
    emu: Arc<Mutex<Option<Emulator>>>,
    shared: Arc<EmuShared>,
    producer: HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
    framebuffer_in: triple_buffer::Input<Vec<u8>>,
) -> JoinHandle<AudioReclaim> {
    thread::Builder::new()
        .name("luna-emu".into())
        .spawn(move || run(emu, shared, producer, primed, framebuffer_in))
        .expect("failed to spawn luna-emu thread")
}

/// The `Arc<...>` params take ownership so the thread closure can move
/// them across the `spawn` boundary; pedantic flags them as "not
/// consumed" because the body only borrows, but a `&Arc` signature
/// would force every caller to outlive the thread, defeating the
/// owned-handle design.
#[allow(clippy::needless_pass_by_value)]
fn run(
    emu: Arc<Mutex<Option<Emulator>>>,
    shared: Arc<EmuShared>,
    mut producer: HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
    mut framebuffer_in: triple_buffer::Input<Vec<u8>>,
) -> AudioReclaim {
    const BATCH: usize = 1024;
    // Hold the last content frame across up to this many consecutive
    // full-frame forced-blank frames (absorbs the transient per-frame blanks
    // of double-buffered Super FX titles); a longer run is a real
    // transition/fade and is shown as black, like Mesen2.
    const BLANK_HOLD_FRAMES: u32 = 8;

    if let Ok(mut g) = shared.thread_handle.lock() {
        *g = Some(thread::current());
    }
    eprintln!("luna-emu: started");

    // Diagnostic counters — log every second so we can tell whether
    // the emu thread is actually making progress when the user
    // reports a black screen.
    let mut last_report = Instant::now();
    let mut batches_since_report = 0u64;
    let mut samples_since_report = 0u64;
    let mut ring_full_count = 0u64;

    // Last emulated frame_count we copied into the shared RGBA buffer.
    // We only refresh the shared buffer when the PPU's frame_count has
    // advanced, so we don't pay the conversion cost on every batch.
    let mut last_emu_frame: u64 = u64::MAX;
    // Consecutive full-frame forced-blank frames seen so far. A short run is a
    // transient double-buffer blank (Super FX) → hold the last content frame so
    // the screen doesn't strobe; a sustained run is a real transition/fade →
    // publish the black so it matches Mesen instead of freezing greyed.
    let mut blank_run: u32 = 0;
    // Set once the emulator panics: we stop stepping it (to avoid
    // re-panicking) but keep the thread alive so the UI stays responsive.
    let mut emu_dead = false;

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if shared.paused.load(Ordering::Acquire) {
            thread::park_timeout(Duration::from_millis(50));
            continue;
        }

        let outcome = {
            let Ok(mut guard) = emu.lock() else {
                break;
            };
            let Some(em) = guard.as_mut() else {
                drop(guard);
                thread::park_timeout(Duration::from_millis(50));
                continue;
            };

            // Step a batch through the API. `Emulator::step` catches an
            // emulator panic and halts on STP internally; on a panic we
            // stop stepping (to avoid re-panicking) but keep the thread
            // alive so the UI stays responsive. The PB:PC is read back
            // from the API state snapshot for the freeze-debug log.
            let done: u64 = if emu_dead {
                0
            } else {
                match em.step(BATCH as u64) {
                    Ok(n) => n,
                    Err(e) => {
                        let st = em.state();
                        eprintln!(
                            "luna-emu: EMULATOR PANIC at PB:PC=${:02X}:{:04X} — {}",
                            st.cpu.pb, st.cpu.pc, e
                        );
                        emu_dead = true;
                        0
                    }
                }
            };

            // Audio (audio-as-clock backpressure): drain only as many
            // samples as the host ring can accept, so none are lost. If
            // the API queue still holds more afterwards, the ring (not
            // the queue) was the limiter → `full` → we park until cpal
            // consumes and unparks us.
            let free = producer.vacant_len();
            let samples = em.drain_audio(free).unwrap_or_default();
            let pushed = samples.len() as u64;
            for s in samples {
                let _ = producer.try_push(s);
            }
            let full = em.audio_queue_len().unwrap_or(0) > 0;
            // Open the cpal callback's silence gate the moment we have
            // pushed any sample at all. Without this, the callback
            // returns zeros forever, the SPSC ring stays full of the
            // initial burst, and the emu thread parks indefinitely.
            if pushed > 0 && !primed.load(Ordering::Relaxed) {
                primed.store(true, Ordering::Release);
            }

            // Framebuffer publication via the SAME API render path the
            // CLI/MCP use (`render_frame_rgba`) — so the GUI and CLI cannot
            // disagree on pixels. The hard part is the full-frame forced-blank:
            //   - a SUSTAINED blank is a real transition / fade / loading screen
            //     → show the black, like Mesen2 (holding the last content frame
            //     there froze it as a grey "stuck" screen — the original bug);
            //   - a TRANSIENT blank (1-few frames) is a double-buffered Super FX
            //     title swapping buffers; the visible frame is momentarily
            //     blank ~every other frame → publishing it would strobe the
            //     whole screen. So HOLD across short blank runs.
            // We tell them apart by run length: hold up to BLANK_HOLD_FRAMES
            // consecutive blanks, then publish the black once. (Verified
            // 2026-06-10: the CLI render path, which never held, shows
            // Williams→black→DOOM at full brightness — the core is correct.)
            let cur_frame = em.frame_count().unwrap_or(last_emu_frame);
            if cur_frame != last_emu_frame {
                last_emu_frame = cur_frame;
                let publish = if em.frame_showed_content().unwrap_or(true) {
                    blank_run = 0;
                    true
                } else {
                    blank_run += 1;
                    // Publish the black exactly once, when the run crosses the
                    // threshold (a genuine transition); hold otherwise.
                    blank_run == BLANK_HOLD_FRAMES
                };
                if publish {
                    if let Ok(rgba) = em.render_frame_rgba(false) {
                        // Lock-free publish into the triple buffer (moves
                        // `rgba`, no copy; the UI reads the latest contention-
                        // free).
                        framebuffer_in.write(rgba);
                    }
                }
            }
            (done, pushed, full)
        };

        let (done, pushed, ring_full) = outcome;
        batches_since_report += 1;
        samples_since_report += pushed;
        if ring_full {
            ring_full_count += 1;
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "luna-emu: {} batches/s, {} samples/s, ring_full×{} (last batch: {} steps)",
                batches_since_report, samples_since_report, ring_full_count, done,
            );
            last_report = Instant::now();
            batches_since_report = 0;
            samples_since_report = 0;
            ring_full_count = 0;
        }

        if ring_full {
            thread::park_timeout(Duration::from_millis(50));
        } else {
            thread::yield_now();
        }
    }

    eprintln!("luna-emu: exiting");
    if let Ok(mut g) = shared.thread_handle.lock() {
        *g = None;
    }
    // Hand back the audio-side ownership so the next ROM's emu thread
    // can re-spawn with the same producer + primed gate. cpal's
    // consumer is permanently held by the audio callback (registered
    // once at app start), so we can't recreate the ring; the producer
    // must round-trip across ROM swaps.
    (producer, primed)
}
