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
//!                       │  Consumer   │ → [`Resampler`] → device buffer
//!                       └─────────────┘
//! ```
//!
//! The host device's sample rate is usually 44.1 kHz or 48 kHz, not
//! 32 kHz. We request 32 kHz from cpal where supported; otherwise a
//! light-weight linear-interpolation [`Resampler`] sits between the
//! ring consumer and the device buffer. The resampler tracks a
//! sub-sample fractional position and steps it by
//! `32000 / device_rate` per output sample — yielding the right
//! pitch at the cost of a one-sample interpolation latency.

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

        let device_rate = config.sample_rate.0;
        let mut resampler = Resampler::new(TARGET_SAMPLE_RATE, device_rate);

        let stream = match chosen_format {
            SampleFormat::F32 => {
                let primed_inner = primed_cb;
                device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        fill_buffer_f32(data, &mut consumer, &mut resampler, &primed_inner);
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
                        fill_buffer_i16(data, &mut consumer, &mut resampler, &primed_inner);
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
    resampler: &mut Resampler,
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
        let (l, r) = resampler.pull(|| pop_input(consumer));
        data[idx] = l;
        data[idx + 1] = r;
        idx += 2;
    }
}

fn fill_buffer_i16(
    data: &mut [i16],
    consumer: &mut ringbuf::HeapCons<(i16, i16)>,
    resampler: &mut Resampler,
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
        let (l, r) = resampler.pull(|| pop_input(consumer));
        // The resampler works in normalised f32 [-1, 1]; round back
        // to i16. Clamp because the linear interp can mathematically
        // overshoot by an LSB on certain frac/sample combinations.
        data[idx] = (l * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        data[idx + 1] = (r * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        idx += 2;
    }
}

/// Pull one stereo sample from the ring and normalise to f32 in
/// `[-1.0, 1.0]`. Returns `None` on an empty ring so the resampler
/// can hold its current sample rather than ramp toward zero.
fn pop_input(consumer: &mut ringbuf::HeapCons<(i16, i16)>) -> Option<(f32, f32)> {
    consumer
        .try_pop()
        .map(|(l, r)| (i16_to_f32(l), i16_to_f32(r)))
}

fn i16_to_f32(v: i16) -> f32 {
    <f32 as Sample>::from_sample(v)
}

// =============================================================================
// Resampler
// =============================================================================

/// Linear-interpolation sample-rate converter from a fixed source
/// rate (the SPC APU's 32 kHz) to the host device rate (often 44.1
/// or 48 kHz). Keeps a 1-sample lookahead window (`cur` and `next`)
/// and a fractional position counter `frac ∈ [0, 1)` that walks by
/// `step = source_rate / device_rate` per output sample.
///
/// The interp output for each call is
/// `cur + (next − cur) · frac`; when `frac` rolls past `1.0` we
/// consume one more input sample and continue. If the input ring
/// runs dry mid-callback we hold the last `next` (= zero-order
/// hold on the underrun) — audible as a small click rather than a
/// hard discontinuity.
pub(crate) struct Resampler {
    cur: (f32, f32),
    next: (f32, f32),
    frac: f32,
    step: f32,
}

impl Resampler {
    pub(crate) fn new(source_rate: u32, device_rate: u32) -> Self {
        Self {
            cur: (0.0, 0.0),
            next: (0.0, 0.0),
            frac: 0.0,
            step: source_rate as f32 / device_rate as f32,
        }
    }

    pub(crate) fn pull(&mut self, mut pop_input: impl FnMut() -> Option<(f32, f32)>) -> (f32, f32) {
        let l = self.cur.0 + (self.next.0 - self.cur.0) * self.frac;
        let r = self.cur.1 + (self.next.1 - self.cur.1) * self.frac;
        self.frac += self.step;
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            self.cur = self.next;
            self.next = pop_input().unwrap_or(self.next);
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    #[test]
    fn resampler_identity_at_matching_rates() {
        // 32k → 32k: step = 1.0. Each output sample consumes one
        // input. The resampler starts with `cur = next = (0, 0)`
        // so there's a 2-sample priming latency (out[0] and out[1]
        // are zero) before the input echoes through.
        let mut r = Resampler::new(32_000, 32_000);
        let input = [(0.1, -0.1), (0.2, -0.2), (0.3, -0.3), (0.4, -0.4)];
        let mut it = input.iter().copied();
        let mut out = Vec::new();
        for _ in 0..6 {
            out.push(r.pull(|| it.next()));
        }
        // 2-sample priming latency, then bit-exact echo of the input.
        assert_eq!(out[0], (0.0, 0.0));
        assert_eq!(out[1], (0.0, 0.0));
        assert_eq!(out[2], input[0]);
        assert_eq!(out[3], input[1]);
        assert_eq!(out[4], input[2]);
        assert_eq!(out[5], input[3]);
    }

    #[test]
    fn resampler_step_is_below_one_when_upsampling() {
        // 32k → 48k: step = 0.667. Three output samples consume
        // two input samples.
        let r = Resampler::new(32_000, 48_000);
        assert!((r.step - 32_000.0 / 48_000.0).abs() < 1e-6);
    }

    #[test]
    fn resampler_consumes_one_input_per_step_at_44100() {
        // For 32k → 44.1k, step ≈ 0.7256. Pulling 1000 output
        // samples should consume ~ 0.7256 × 1000 ≈ 725 inputs.
        let mut r = Resampler::new(32_000, 44_100);
        let mut consumed = 0usize;
        for _ in 0..1000 {
            r.pull(|| {
                consumed += 1;
                Some((0.0, 0.0))
            });
        }
        assert!(
            (700..=750).contains(&consumed),
            "expected ~725 inputs consumed, got {consumed}",
        );
    }

    #[test]
    fn resampler_holds_on_underrun() {
        // Feed two known samples, then return None forever. The
        // resampler must keep emitting the last `next` value
        // instead of decaying toward zero — that's the "zero-order
        // hold" underrun behaviour.
        let mut r = Resampler::new(32_000, 48_000);
        let mut count = 0;
        for _ in 0..200 {
            r.pull(|| {
                count += 1;
                if count <= 2 { Some((0.5, -0.5)) } else { None }
            });
        }
        // After 200 outputs at step=0.667 we've asked for 0.667*200
        // ≈ 133 inputs — well past the 2 we provided. The hold
        // means `next` stays at (0.5, -0.5) and the output settles
        // there too.
        let final_sample = r.pull(|| None);
        assert!((final_sample.0 - 0.5).abs() < 0.01);
        assert!((final_sample.1 - (-0.5)).abs() < 0.01);
    }
}
