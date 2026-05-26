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
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

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
    pub(crate) fn new() -> Self {
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
pub(crate) fn spawn(
    snes: Arc<Mutex<Option<Snes>>>,
    shared: Arc<EmuShared>,
    mut producer: HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("luna-emu".into())
        .spawn(move || {
            run(snes, shared, &mut producer, primed);
        })
        .expect("failed to spawn luna-emu thread")
}

fn run(
    snes: Arc<Mutex<Option<Snes>>>,
    shared: Arc<EmuShared>,
    producer: &mut HeapProd<(i16, i16)>,
    primed: Arc<AtomicBool>,
) {
    if let Ok(mut g) = shared.thread_handle.lock() {
        *g = Some(thread::current());
    }
    eprintln!("luna-emu: started");

    const BATCH: usize = 1024;

    // Diagnostic counters — log every second so we can tell whether
    // the emu thread is actually making progress when the user
    // reports a black screen.
    let mut last_report = Instant::now();
    let mut batches_since_report = 0u64;
    let mut samples_since_report = 0u64;
    let mut ring_full_count = 0u64;

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
            // the whole thread. The shared.shutdown flag stays false
            // so the UI sees the emu thread is "alive but idle"; we
            // surface the panic by storing it for the next snapshot.
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
                Err(_payload) => {
                    eprintln!("luna-emu: emulator panic — stopping batch");
                    // Stop the emulator so we don't loop hard.
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
}
