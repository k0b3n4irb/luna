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

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use luna_core::Snes;
use luna_ppu::{FRAME_H, FRAME_W};
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

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
    snes: Arc<Mutex<Option<Snes>>>,
    shared: Arc<EmuShared>,
    producer: HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
    framebuffer_rgba: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<AudioReclaim> {
    thread::Builder::new()
        .name("luna-emu".into())
        .spawn(move || run(snes, shared, producer, primed, framebuffer_rgba))
        .expect("failed to spawn luna-emu thread")
}

/// The `Arc<...>` params take ownership so the thread closure can move
/// them across the `spawn` boundary; pedantic flags them as "not
/// consumed" because the body only borrows, but a `&Arc` signature
/// would force every caller to outlive the thread, defeating the
/// owned-handle design.
#[allow(clippy::needless_pass_by_value)]
fn run(
    snes: Arc<Mutex<Option<Snes>>>,
    shared: Arc<EmuShared>,
    mut producer: HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
    framebuffer_rgba: Arc<Mutex<Vec<u8>>>,
) -> AudioReclaim {
    const BATCH: usize = 1024;

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
    // Local scratch buffer for the RGBA conversion. Built outside the
    // shared lock, then swapped in with a single memcpy.
    let mut rgba_scratch: Vec<u8> = vec![0; FRAME_W * FRAME_H * 4];

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if shared.paused.load(Ordering::Acquire) {
            thread::park_timeout(Duration::from_millis(50));
            continue;
        }

        let outcome = {
            let Ok(mut guard) = snes.lock() else {
                break;
            };
            let Some(snes_ref) = guard.as_mut() else {
                drop(guard);
                thread::park_timeout(Duration::from_millis(50));
                continue;
            };

            // catch_unwind so an emulator panic kills the batch, not
            // the whole thread. We surface the actual panic message
            // (instead of dropping it silently) so freeze-debug runs
            // can identify the exact site that died.
            let stepped: Result<usize, _> = catch_unwind(AssertUnwindSafe(|| {
                let mut done = 0usize;
                while done < BATCH && !snes_ref.cpu.stopped {
                    snes_ref.step();
                    done += 1;
                }
                done
            }));
            let done = match stepped {
                Ok(n) => n,
                Err(payload) => {
                    let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                        (*s).to_string()
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "(unknown panic payload)".to_string()
                    };
                    eprintln!(
                        "luna-emu: EMULATOR PANIC at PB:PC=${:02X}:{:04X} — {}",
                        snes_ref.cpu.pb, snes_ref.cpu.pc, msg
                    );
                    snes_ref.cpu.stopped = true;
                    0
                }
            };

            let mut pushed = 0u64;
            let mut full = false;
            while let Some(sample) = snes_ref.apu_real.audio_queue.pop_front() {
                if producer.try_push(sample).is_err() {
                    snes_ref.apu_real.audio_queue.push_front(sample);
                    full = true;
                    break;
                }
                pushed += 1;
            }
            // Open the cpal callback's silence gate the moment we have
            // pushed any sample at all. Without this, the callback
            // returns zeros forever, the SPSC ring stays full of the
            // initial burst, and the emu thread parks indefinitely.
            if pushed > 0 && !primed.load(Ordering::Relaxed) {
                primed.store(true, Ordering::Release);
            }

            // Producer-side framebuffer publication. Done while we
            // still hold the Snes lock — the conversion reads PPU
            // state directly. Only refreshes when the emulated frame
            // has advanced.
            //
            // Forced-blank handling: when INIDISP bit 7 is set, skip
            // the publish so the consumer keeps showing the last good
            // frame (most games toggle bit 7 every VBlank during their
            // tile / OAM upload; without this we'd flash black once
            // per second).
            let cur_frame = snes_ref.frame_count;
            let blanked = snes_ref.ppu.inidisp & 0x80 != 0;
            if cur_frame != last_emu_frame && !blanked {
                for (i, px) in snes_ref.ppu.framebuffer().iter().enumerate() {
                    let off = i * 4;
                    rgba_scratch[off] = px[0];
                    rgba_scratch[off + 1] = px[1];
                    rgba_scratch[off + 2] = px[2];
                    rgba_scratch[off + 3] = 0xFF;
                }
                last_emu_frame = cur_frame;
                if let Ok(mut shared_fb) = framebuffer_rgba.lock() {
                    shared_fb.copy_from_slice(&rgba_scratch);
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
