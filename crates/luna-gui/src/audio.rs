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
//! 6-point cubic Hermite [`Resampler`] (Niemitalo 2009) sits between
//! the ring consumer and the device buffer. The resampler tracks a
//! sub-sample fractional position and steps it by
//! `32000 / device_rate` per output sample — well below the audible
//! stopband ripple of a naïve linear interpolator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Split};

/// Target SNES audio sample rate. The host stream is configured to
/// this rate when the device supports it.
pub(crate) const TARGET_SAMPLE_RATE: u32 = 32_000;

/// Maximum number of stereo samples the SPSC ring can hold (~125 ms
/// at 32 kHz — comfortable safety margin against UI hitches without
/// adding too much latency).
const RING_CAPACITY: usize = 4_096;

/// Host audio output. Owns the cpal stream + a flag that flips on
/// first sample push; the consumer end of the SPSC ring lives inside
/// the cpal callback, and the producer end is handed to the dedicated
/// emulator thread (see `emu_thread.rs`).
pub(crate) struct AudioBackend {
    /// Held just to keep the stream alive — dropping the `Stream`
    /// stops the callback.
    _stream: Stream,
    /// `true` after the emu thread has pushed at least one sample.
    /// The audio callback emits silence until this flips, avoiding a
    /// startup pop while the ring buffer is empty. Cloned into the
    /// callback closures; this end of the Arc is currently unused
    /// (kept so a future UI status row can display it).
    #[allow(dead_code)]
    pub(crate) primed: Arc<AtomicBool>,
    /// Output device sample rate (might not match
    /// [`TARGET_SAMPLE_RATE`]). Stored for diagnostics; the Stubs
    /// panel surfaces it as "host SR".
    #[allow(dead_code)]
    pub(crate) host_sample_rate: u32,
}

/// What [`AudioBackend::try_start`] returns — the backend itself, the
/// producer end of the SPSC ring (handed to the dedicated emu thread),
/// and the shared `primed` flag (also handed to the emu thread so it
/// can flip on first push, unblocking the cpal callback's gated
/// "emit silence until ready" branch).
pub(crate) struct AudioStreamArtifacts {
    pub backend: AudioBackend,
    pub producer: ringbuf::HeapProd<(i16, i16)>,
    pub primed: Arc<AtomicBool>,
}

impl AudioBackend {
    /// Try to start an audio stream on the default output device.
    /// Returns `None` (with a logged reason) if any setup step
    /// fails — emulation continues silently in that case.
    #[must_use]
    pub(crate) fn try_start(
        emu_shared: Arc<crate::emu_thread::EmuShared>,
    ) -> Option<AudioStreamArtifacts> {
        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            eprintln!("luna-gui audio: no default output device — running silent");
            return None;
        };
        let default_cfg = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("luna-gui audio: default_output_config failed: {e} — running silent");
                return None;
            }
        };
        // Pick the best matching config: target rate first, else
        // device default. Prefer higher-fidelity formats (F32 → I16 →
        // U8) so a device whose *default* is U8 (some PulseAudio /
        // ALSA configs) still picks I16/F32 when available.
        let mut config: StreamConfig = default_cfg.config();
        let mut chosen_format = default_cfg.sample_format();
        let format_rank = |fmt: SampleFormat| -> u8 {
            match fmt {
                SampleFormat::F32 => 3,
                SampleFormat::I16 => 2,
                SampleFormat::U8 => 1,
                _ => 0,
            }
        };
        if let Ok(supported) = device.supported_output_configs() {
            for c in supported {
                if c.channels() == 2
                    && c.min_sample_rate().0 <= TARGET_SAMPLE_RATE
                    && c.max_sample_rate().0 >= TARGET_SAMPLE_RATE
                    && format_rank(c.sample_format()) > format_rank(chosen_format)
                {
                    config = c
                        .with_sample_rate(cpal::SampleRate(TARGET_SAMPLE_RATE))
                        .config();
                    chosen_format = c.sample_format();
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

        let stream_result = match chosen_format {
            SampleFormat::F32 => {
                let primed_inner = primed_cb;
                let emu_inner = emu_shared.clone();
                device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        fill_buffer_f32(data, &mut consumer, &mut resampler, &primed_inner);
                        // Audio-as-clock: each callback drained samples
                        // from the ring → tell the emu thread it can
                        // push more (it parks on a full ring).
                        emu_inner.unpark_emu();
                    },
                    |err| eprintln!("luna-gui audio error: {err}"),
                    None,
                )
            }
            SampleFormat::I16 => {
                let primed_inner = primed_cb;
                let emu_inner = emu_shared.clone();
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| {
                        fill_buffer_i16(data, &mut consumer, &mut resampler, &primed_inner);
                        emu_inner.unpark_emu();
                    },
                    |err| eprintln!("luna-gui audio error: {err}"),
                    None,
                )
            }
            SampleFormat::U8 => {
                let primed_inner = primed_cb;
                let emu_inner = emu_shared.clone();
                device.build_output_stream(
                    &config,
                    move |data: &mut [u8], _| {
                        fill_buffer_u8(data, &mut consumer, &mut resampler, &primed_inner);
                        emu_inner.unpark_emu();
                    },
                    |err| eprintln!("luna-gui audio error: {err}"),
                    None,
                )
            }
            other => {
                eprintln!("luna-gui audio: unsupported sample format {other:?} — running silent");
                return None;
            }
        };
        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("luna-gui audio: build_output_stream failed: {e} — running silent");
                return None;
            }
        };
        if let Err(e) = stream.play() {
            eprintln!("luna-gui audio: stream.play failed: {e} — running silent");
            return None;
        }
        eprintln!(
            "luna-gui audio: started ({:?}, {} Hz, 2 ch)",
            chosen_format, config.sample_rate.0
        );
        Some(AudioStreamArtifacts {
            backend: Self {
                _stream: stream,
                primed: primed.clone(),
                host_sample_rate: config.sample_rate.0,
            },
            producer,
            primed,
        })
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

/// U8 device-format variant. cpal's u8 sample convention is offset-
/// binary: silence is `128` (`0x80`), the full positive scale is
/// `255`, and the full negative scale is `0`. Maps the resampled
/// `f32 ∈ [-1, 1]` into that range.
fn fill_buffer_u8(
    data: &mut [u8],
    consumer: &mut ringbuf::HeapCons<(i16, i16)>,
    resampler: &mut Resampler,
    primed: &AtomicBool,
) {
    if !primed.load(Ordering::Relaxed) {
        for s in data.iter_mut() {
            *s = 0x80;
        }
        return;
    }
    let mut idx = 0;
    while idx + 1 < data.len() {
        let (l, r) = resampler.pull(|| pop_input(consumer));
        data[idx] = (l * 128.0 + 128.0).round().clamp(0.0, 255.0) as u8;
        data[idx + 1] = (r * 128.0 + 128.0).round().clamp(0.0, 255.0) as u8;
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

/// Sample-rate converter from the SPC APU's fixed 32 kHz output to the
/// host device rate (typically 44.1 or 48 kHz). Uses **6-point cubic
/// Hermite interpolation** based on the closed-form polynomial fit
/// derived in Olli Niemitalo's *"Polynomial Interpolators for
/// High-Quality Resampling of Oversampled Audio"* (2009,
/// <https://yehar.com/blog/wp-content/uploads/2009/08/deip.pdf>),
/// section "6-point, 3rd-order Hermite".
///
/// Compared to the previous linear interpolator, 6-point cubic Hermite
/// has much lower passband ripple and lower aliasing energy above the
/// Nyquist of the source rate. At 32 k → 48 k (step ≈ 0.667), linear
/// interpolation aliases noticeably above ~ 10 kHz; the cubic version's
/// stopband is well below the SNES's own DAC noise floor.
///
/// State: a 6-sample circular history of the input stream plus a
/// fractional position `frac ∈ [0, 1)` that walks by `step =
/// source_rate / device_rate` per output sample. On `frac >= 1` we
/// rotate one new sample into the history; on input underrun we
/// stop rotating and keep emitting samples interpolated from the
/// last good window (graceful degradation rather than zero-output
/// click).
pub(crate) struct Resampler {
    /// Stereo history, oldest-first: indices `[y-2, y-1, y0, y1, y2, y3]`
    /// in Niemitalo's notation. `y0` and `y1` are the two samples
    /// straddling the current interpolation point; the other four are
    /// the neighbours needed by the cubic fit.
    history: [(f32, f32); 6],
    /// Fractional position between `history[2]` (= y0) and `history[3]`
    /// (= y1). `frac == 0` means "output exactly y0"; `frac` close to
    /// 1 means "output close to y1". Updated by adding `step` per pull.
    frac: f32,
    /// Per-output-sample step in input-sample units.
    /// `source_rate / device_rate`. For 32 k → 48 k → `≈ 0.667`.
    step: f32,
}

impl Resampler {
    pub(crate) fn new(source_rate: u32, device_rate: u32) -> Self {
        Self {
            history: [(0.0, 0.0); 6],
            frac: 0.0,
            step: source_rate as f32 / device_rate as f32,
        }
    }

    /// Closed-form 6-point cubic Hermite interpolation in one
    /// dimension. Coefficients `c0..c3` from Niemitalo 2009, evaluated
    /// via Horner's method. `x` is the fractional position between
    /// `y0` and `y1` (= `history[2]` and `history[3]`).
    #[inline]
    fn hermite6(ym2: f32, ym1: f32, y0: f32, y1: f32, y2: f32, y3: f32, x: f32) -> f32 {
        let c0 = y0;
        let c1 = (ym2 - y2) * (1.0 / 12.0) + (y1 - ym1) * (2.0 / 3.0);
        let c2 = ym1 * (5.0 / 4.0) - y0 * (7.0 / 3.0) + y1 * (5.0 / 3.0) - y2 * 0.5
            + y3 * (1.0 / 12.0)
            - ym2 * (1.0 / 6.0);
        let c3 = (ym2 - y3) * (1.0 / 12.0) + (y2 - ym1) * (7.0 / 12.0) + (y0 - y1) * (4.0 / 3.0);
        ((c3 * x + c2) * x + c1) * x + c0
    }

    pub(crate) fn pull(&mut self, mut pop_input: impl FnMut() -> Option<(f32, f32)>) -> (f32, f32) {
        let h = &self.history;
        let l = Self::hermite6(h[0].0, h[1].0, h[2].0, h[3].0, h[4].0, h[5].0, self.frac);
        let r = Self::hermite6(h[0].1, h[1].1, h[2].1, h[3].1, h[4].1, h[5].1, self.frac);
        self.frac += self.step;
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            // Shift oldest out, append newest. On underrun, repeat the
            // newest known sample — this keeps the cubic well-behaved
            // (no zero-jump) at the cost of a low-passed echo for the
            // duration of the dry spell.
            let next = pop_input().unwrap_or(self.history[5]);
            self.history[0] = self.history[1];
            self.history[1] = self.history[2];
            self.history[2] = self.history[3];
            self.history[3] = self.history[4];
            self.history[4] = self.history[5];
            self.history[5] = next;
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    #[test]
    fn resampler_step_is_below_one_when_upsampling() {
        // 32 k → 48 k: step = 0.667. Three output samples consume
        // two input samples on average.
        let r = Resampler::new(32_000, 48_000);
        assert!((r.step - 32_000.0 / 48_000.0).abs() < 1e-6);
    }

    #[test]
    fn resampler_consumes_one_input_per_step_at_44100() {
        // 32 k → 44.1 k, step ≈ 0.7256. 1000 output samples should
        // consume ~ 0.7256 × 1000 ≈ 725 inputs (within ±25 for the
        // initial priming).
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
    fn resampler_passes_dc_signal_unchanged_after_priming() {
        // A constant input must produce that same constant on the
        // output (post-priming). The 6-point Hermite formula is
        // exact for constants — its coefficients are derived such
        // that `c0 = y0` and all higher-order coefficients sum to
        // zero when all `yk` are equal.
        let mut r = Resampler::new(32_000, 48_000);
        let v = (0.42_f32, -0.17_f32);
        // Prime the history: 8 inputs is enough at step 0.667 since
        // 8 × 1.5 > 6 (the history window).
        for _ in 0..32 {
            r.pull(|| Some(v));
        }
        let out = r.pull(|| Some(v));
        assert!(
            (out.0 - v.0).abs() < 1e-5,
            "L: got {} expected {}",
            out.0,
            v.0
        );
        assert!(
            (out.1 - v.1).abs() < 1e-5,
            "R: got {} expected {}",
            out.1,
            v.1
        );
    }

    #[test]
    fn resampler_holds_on_underrun() {
        // Feed two known samples, then return None forever. The
        // history rotates the latest known sample into every new
        // slot on underrun, so once it propagates through the whole
        // 6-slot window the output settles on that value.
        let mut r = Resampler::new(32_000, 48_000);
        let mut count = 0;
        for _ in 0..200 {
            r.pull(|| {
                count += 1;
                if count <= 2 { Some((0.5, -0.5)) } else { None }
            });
        }
        let final_sample = r.pull(|| None);
        assert!((final_sample.0 - 0.5).abs() < 0.01);
        assert!((final_sample.1 - (-0.5)).abs() < 0.01);
    }
}
