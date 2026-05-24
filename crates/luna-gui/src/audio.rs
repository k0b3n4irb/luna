//! Host audio output for the SNES APU.
//!
//! The APU generates a steady 32 kHz stereo sample stream (see
//! `luna_apu::Apu::tick_one_sample`). We connect that to the host
//! audio device via [cpal] and a lock-free SPSC ring buffer
//! (the `ringbuf` crate) so the audio callback thread can drain
//! samples without ever blocking on the emulation thread:
//!
//! ```text
//!   Emulation thread                                Audio callback
//! ────────────────────                              ──────────────
//!   Apu::drain_audio →  feed(samples)
//!                            │
//!                            ▼
//!                       ┌─────────────┐
//!                       │  ringbuf    │
//!                       │  Producer   │
//!                       └─────────────┘
//!                            │
//!                            ▼
//!                       ┌─────────────┐
//!                       │  ringbuf    │
//!                       │  Consumer   │  ← pull samples, fill device buffer
//!                       └─────────────┘
//! ```
//!
//! The host device's sample rate is usually 44.1 kHz or 48 kHz, not
//! 32 kHz. We request 32 kHz from cpal where supported; if the
//! device only supports higher rates we play at the device rate and
//! the pitch is slightly off (≈ +30 % for 48 kHz). A small linear
//! resampler would fix that, but it's outside the MVP.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

/// Target SNES audio sample rate. The host stream is configured to
/// this rate when the device supports it.
pub(crate) const TARGET_SAMPLE_RATE: u32 = 32_000;

/// Maximum number of stereo samples the SPSC ring can hold (~125 ms
/// at 32 kHz — comfortable safety margin against UI hitches without
/// adding too much latency).
const RING_CAPACITY: usize = 4_096;

/// Host audio output. Owns the cpal stream + the ring-buffer
/// producer; the consumer is moved into the audio callback.
pub(crate) struct AudioBackend {
    producer: ringbuf::HeapProd<(i16, i16)>,
    /// Held just to keep the stream alive — dropping the `Stream`
    /// stops the callback.
    _stream: Stream,
    /// `true` after at least one successful `feed` call. The audio
    /// callback emits silence until this flips, avoiding a startup
    /// pop while the ring buffer is empty.
    primed: Arc<AtomicBool>,
    /// Output device sample rate (might not match
    /// [`TARGET_SAMPLE_RATE`]). Stored for diagnostics; the Stubs
    /// panel will surface it once the GUI gets a "host SR" row.
    #[allow(dead_code)]
    pub(crate) host_sample_rate: u32,
}

impl AudioBackend {
    /// Try to start an audio stream on the default output device.
    /// Returns `None` (with a logged warning) if any setup step
    /// fails — emulation continues silently in that case.
    #[must_use]
    pub(crate) fn try_start() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let default_cfg = device.default_output_config().ok()?;
        // Pick the best matching config: target rate first, else
        // device default.
        let mut config: StreamConfig = default_cfg.config();
        let mut chosen_format = default_cfg.sample_format();
        if let Ok(supported) = device.supported_output_configs() {
            for c in supported {
                if c.channels() == 2
                    && c.min_sample_rate().0 <= TARGET_SAMPLE_RATE
                    && c.max_sample_rate().0 >= TARGET_SAMPLE_RATE
                {
                    config = c
                        .with_sample_rate(cpal::SampleRate(TARGET_SAMPLE_RATE))
                        .config();
                    chosen_format = c.sample_format();
                    break;
                }
            }
        }
        config.channels = 2;

        let rb = HeapRb::<(i16, i16)>::new(RING_CAPACITY);
        let (producer, mut consumer) = rb.split();
        let primed = Arc::new(AtomicBool::new(false));
        let primed_cb = primed.clone();

        let stream = match chosen_format {
            SampleFormat::F32 => {
                let primed_inner = primed_cb;
                device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        fill_buffer_f32(data, &mut consumer, &primed_inner);
                    },
                    |err| eprintln!("luna-gui audio error: {err}"),
                    None,
                )
            }
            SampleFormat::I16 => {
                let primed_inner = primed_cb;
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| {
                        fill_buffer_i16(data, &mut consumer, &primed_inner);
                    },
                    |err| eprintln!("luna-gui audio error: {err}"),
                    None,
                )
            }
            other => {
                eprintln!("luna-gui audio: unsupported sample format {other:?}");
                return None;
            }
        }
        .ok()?;

        stream.play().ok()?;
        Some(Self {
            producer,
            _stream: stream,
            primed,
            host_sample_rate: config.sample_rate.0,
        })
    }

    /// Push as many of the given samples into the SPSC ring as fit.
    /// Anything that doesn't fit is dropped (ring already full =
    /// audio thread is ahead of consumption rate — extremely rare
    /// in practice; we'd rather drop than starve emulation).
    pub(crate) fn feed(&mut self, samples: &[(i16, i16)]) {
        if samples.is_empty() {
            return;
        }
        for s in samples {
            if self.producer.try_push(*s).is_err() {
                // Ring full — drop the oldest by reading one out via
                // a peek-style trick isn't trivial with our crate;
                // we just stop pushing. Emulation continues; the
                // audio thread will catch up shortly.
                break;
            }
        }
        self.primed.store(true, Ordering::Relaxed);
    }
}

fn fill_buffer_f32(
    data: &mut [f32],
    consumer: &mut ringbuf::HeapCons<(i16, i16)>,
    primed: &AtomicBool,
) {
    if !primed.load(Ordering::Relaxed) {
        for s in data.iter_mut() {
            *s = 0.0;
        }
        return;
    }
    let mut idx = 0;
    while idx + 1 < data.len() {
        let (l, r) = consumer.try_pop().unwrap_or((0, 0));
        data[idx] = i16_to_f32(l);
        data[idx + 1] = i16_to_f32(r);
        idx += 2;
    }
}

fn fill_buffer_i16(
    data: &mut [i16],
    consumer: &mut ringbuf::HeapCons<(i16, i16)>,
    primed: &AtomicBool,
) {
    if !primed.load(Ordering::Relaxed) {
        for s in data.iter_mut() {
            *s = 0;
        }
        return;
    }
    let mut idx = 0;
    while idx + 1 < data.len() {
        let (l, r) = consumer.try_pop().unwrap_or((0, 0));
        data[idx] = l;
        data[idx + 1] = r;
        idx += 2;
    }
}

fn i16_to_f32(v: i16) -> f32 {
    <f32 as Sample>::from_sample(v)
}
