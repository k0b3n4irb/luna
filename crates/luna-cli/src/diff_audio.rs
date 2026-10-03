//! `luna diff --audio` — loudness-envelope MATCH/DIFF between two ROMs.
//!
//! A hash of the audio output flips as soon as the code that talks to the
//! SPC700 moves by a few CPU cycles, though nothing audible changed: the
//! same sound comes out a handful of samples earlier or later. This
//! comparison is the one that survives that shift. Both ROMs run to the
//! same PPU frame, their output is cut into fixed windows, and each
//! window's RMS level is compared. MATCH when every window is within the
//! tolerance, DIFF otherwise — exit 0 / 1, 2 for a usage error, the
//! contract of the frame `luna diff`.
//!
//! What it does NOT see: two sounds of equal loudness. It compares an
//! envelope, not a spectrum, so it backs up an audio hash ("something
//! moved") rather than replacing it.

use std::process::ExitCode;

use crate::parsers::pad_events;
use crate::rom::load_rom_into;

/// The S-DSP's output rate, the rate of every sample luna drains.
const SAMPLE_RATE: u64 = 32_000;

/// Options for the audio comparison.
pub(crate) struct AudioDiffOptions<'a> {
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
    pub input_script: Option<&'a str>,
    pub until_frame: u64,
    pub window_ms: u64,
    pub tolerance_pct: f64,
    pub silence: u16,
    pub out: Option<&'a std::path::Path>,
}

/// One window's verdict.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Window {
    /// Where the window starts, in milliseconds of output.
    start_ms: u64,
    rms_a: f64,
    rms_b: f64,
    /// `|rms_a - rms_b|` as a percentage of the louder of the two, with
    /// the silence level as a floor so two near-silent windows don't
    /// produce a huge ratio out of a one-LSB difference.
    delta_pct: f64,
}

/// The `--out` JSON report.
#[derive(serde::Serialize)]
struct Report<'a> {
    a: &'a std::path::Path,
    b: &'a std::path::Path,
    until_frame: u64,
    window_ms: u64,
    tolerance_pct: f64,
    silence: u16,
    samples_a: usize,
    samples_b: usize,
    /// Index of the first stereo sample louder than `silence`, per ROM
    /// (`null` when the whole capture is silent).
    onset_a: Option<usize>,
    onset_b: Option<usize>,
    windows: Vec<Window>,
    max_delta_pct: f64,
    /// `"match"` or `"diff"`.
    status: &'static str,
}

/// RMS level of a run of stereo samples, both channels together.
fn rms(samples: &[(i16, i16)]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&(l, r)| {
            let (l, r) = (f64::from(l), f64::from(r));
            l.mul_add(l, r * r)
        })
        .sum();
    let n = (samples.len() * 2) as f64;
    (sum / n).sqrt()
}

/// Index of the first stereo sample with either channel above `silence`.
fn onset(samples: &[(i16, i16)], silence: u16) -> Option<usize> {
    samples
        .iter()
        .position(|&(l, r)| l.unsigned_abs() > silence || r.unsigned_abs() > silence)
}

/// Cut both captures into `window` stereo samples and compare the levels.
/// A capture shorter than the other reads as silence past its end.
fn compare(a: &[(i16, i16)], b: &[(i16, i16)], window: usize, silence: u16) -> Vec<Window> {
    let floor = f64::from(silence).max(1.0);
    let slice = |s: &'_ [(i16, i16)], at: usize| -> f64 {
        s.get(at..)
            .map_or(0.0, |rest| rms(&rest[..window.min(rest.len())]))
    };
    (0..a.len().max(b.len()))
        .step_by(window)
        .map(|at| {
            let (rms_a, rms_b) = (slice(a, at), slice(b, at));
            Window {
                start_ms: at as u64 * 1000 / SAMPLE_RATE,
                rms_a,
                rms_b,
                delta_pct: (rms_a - rms_b).abs() / rms_a.max(rms_b).max(floor) * 100.0,
            }
        })
        .collect()
}

/// Run `rom` to `o.until_frame`, draining the APU every frame — the same
/// capture as `luna run --until-frame N --audio-out`.
fn capture(
    rom: &std::path::Path,
    o: &AudioDiffOptions<'_>,
    mut script: luna_api::InputScript,
) -> Result<Vec<(i16, i16)>, String> {
    let mut em = luna_api::Emulator::new();
    load_rom_into(
        &mut em,
        rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    )?;
    let mut samples = Vec::new();
    loop {
        let f = em.frame_count().unwrap_or(0);
        if f >= o.until_frame {
            break;
        }
        script.apply_due(&mut em, f).map_err(|e| e.to_string())?;
        match em.step_until_frame(luna_api::FRAME_STEP_BUDGET) {
            Ok(0) => break,
            Ok(_) => {}
            Err(luna_api::ApiError::Panic(msg)) => {
                eprintln!("note: machine panicked at frame {f}: {msg}");
                break;
            }
            Err(e) => return Err(e.to_string()),
        }
        samples.append(&mut em.drain_audio(usize::MAX).map_err(|e| e.to_string())?);
    }
    Ok(samples)
}

/// `luna diff --audio` entry point.
pub(crate) fn run_audio_diff(
    rom_a: &std::path::Path,
    rom_b: &std::path::Path,
    o: &AudioDiffOptions<'_>,
) -> ExitCode {
    let window = usize::try_from(o.window_ms * SAMPLE_RATE / 1000).unwrap_or(0);
    if window == 0 {
        eprintln!("error: --window-ms must be at least 1");
        return ExitCode::from(2);
    }
    if !o.tolerance_pct.is_finite() || o.tolerance_pct < 0.0 {
        eprintln!("error: --tolerance-pct must be a percentage of 0 or more");
        return ExitCode::from(2);
    }
    let mut script = luna_api::InputScript::new();
    match o.input_script.map(|s| pad_events(s, 0)) {
        None => {}
        Some(Ok(v)) => script.extend(v),
        Some(Err(e)) => {
            eprintln!("error: --input: {e}");
            return ExitCode::from(2);
        }
    }
    let (a, b) = match (capture(rom_a, o, script.clone()), capture(rom_b, o, script)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let windows = compare(&a, &b, window, o.silence);
    for w in &windows {
        println!(
            "window {:>6} ms: a={:>9.2} b={:>9.2} delta={:.2}%",
            w.start_ms, w.rms_a, w.rms_b, w.delta_pct
        );
    }
    let (onset_a, onset_b) = (onset(&a, o.silence), onset(&b, o.silence));
    let show = |s: Option<usize>| s.map_or_else(|| "none".to_string(), |i| i.to_string());
    println!(
        "first sample above {}: a={} b={} (of {} / {})",
        o.silence,
        show(onset_a),
        show(onset_b),
        a.len(),
        b.len()
    );
    let max_delta_pct = windows.iter().map(|w| w.delta_pct).fold(0.0, f64::max);
    // No audio at all is not a match: a run that produced nothing proves
    // nothing about the two builds.
    let matched = !windows.is_empty() && max_delta_pct <= o.tolerance_pct;
    let status = if matched { "match" } else { "diff" };
    println!(
        "{} window(s) of {} ms, max delta {:.2}% (tolerance {}%): {}",
        windows.len(),
        o.window_ms,
        max_delta_pct,
        o.tolerance_pct,
        status.to_ascii_uppercase()
    );
    if let Some(path) = o.out {
        let report = Report {
            a: rom_a,
            b: rom_b,
            until_frame: o.until_frame,
            window_ms: o.window_ms,
            tolerance_pct: o.tolerance_pct,
            silence: o.silence,
            samples_a: a.len(),
            samples_b: b.len(),
            onset_a,
            onset_b,
            windows,
            max_delta_pct,
            status,
        };
        let json = serde_json::to_string_pretty(&report).expect("report serialises");
        let res = if path.as_os_str() == "-" {
            println!("{json}");
            Ok(())
        } else {
            std::fs::write(path, json)
        };
        if let Err(e) = res {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }
    if matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square wave of `amp`, `n` stereo samples long.
    fn square(amp: i16, n: usize) -> Vec<(i16, i16)> {
        (0..n)
            .map(|i| {
                if i / 16 % 2 == 0 {
                    (amp, amp)
                } else {
                    (-amp, -amp)
                }
            })
            .collect()
    }

    #[test]
    fn a_few_samples_of_shift_stay_under_a_percent() {
        // The case the command exists for: the same sound, three samples
        // late. Sample-exact comparison differs everywhere; the levels do
        // not.
        let a = [vec![(0, 0); 1000], square(8000, 31_000)].concat();
        let b = [vec![(0, 0); 1003], square(8000, 30_997)].concat();
        assert_ne!(a, b);
        let w = compare(&a, &b, 16_000, 64);
        assert_eq!(w.len(), 2);
        assert!(w.iter().all(|w| w.delta_pct < 1.0), "{w:?}");
        assert_eq!(onset(&a, 64), Some(1000));
        assert_eq!(onset(&b, 64), Some(1003));
    }

    #[test]
    fn half_the_volume_is_fifty_percent() {
        let w = compare(&square(8000, 16_000), &square(4000, 16_000), 16_000, 64);
        assert_eq!(w.len(), 1);
        assert!((w[0].delta_pct - 50.0).abs() < 1e-9, "{w:?}");
    }

    #[test]
    fn near_silence_is_measured_against_the_floor() {
        // 1 LSB against digital silence: 100 % of itself, 1/64 of the
        // floor — not a difference worth a DIFF.
        let w = compare(&square(1, 16_000), &square(0, 16_000), 16_000, 64);
        assert!(w[0].delta_pct < 2.0, "{w:?}");
        assert_eq!(onset(&square(1, 100), 64), None);
    }

    #[test]
    fn a_shorter_capture_reads_as_silence_past_its_end() {
        let w = compare(&square(8000, 32_000), &square(8000, 16_000), 16_000, 64);
        assert_eq!(w.len(), 2);
        assert!(w[0].delta_pct < 1e-9);
        assert!((w[1].delta_pct - 100.0).abs() < 1e-9, "{w:?}");
        assert_eq!(w[1].start_ms, 500);
    }
}
