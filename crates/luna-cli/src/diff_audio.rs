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
//! Only windows complete on both sides are compared: the two captures
//! end a few samples apart, and a last window that is partial on one side
//! and empty on the other is not a difference in the sound. A capture a
//! whole window shorter than the other is one (a machine that stopped),
//! and is a DIFF by itself.
//!
//! `--align-onset` handles the larger shift: the code that starts the
//! music gained or lost a frame, so the same sound comes out some hundreds
//! of samples away and the window holding the start compares different
//! amounts of it. The windows then start at each capture's first sample
//! above the silence level, and the verdict names the shift.
//!
//! What it does NOT see: two sounds of equal loudness. It compares an
//! envelope, not a spectrum, so it backs up an audio hash ("something
//! moved") rather than replacing it.

use std::process::ExitCode;

use crate::output::write_json_report;
use crate::parsers::{InputFlags, input_script};
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
    pub align_onset: bool,
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
    /// Whether the windows start at each capture's onset (`--align-onset`).
    align_onset: bool,
    /// `onset_b - onset_a` in samples when the windows were aligned on it
    /// (`null` otherwise, or when both captures are silent).
    onset_shift: Option<i64>,
    /// `true` when one capture is a whole window or more shorter than the
    /// other — a DIFF whatever the windows say.
    length_mismatch: bool,
    windows: Vec<Window>,
    max_delta_pct: f64,
    /// `"match"` or `"diff"`.
    status: &'static str,
}

/// RMS level of a run of stereo samples, both channels together.
pub(crate) fn rms(samples: &[(i16, i16)]) -> f64 {
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

/// Cut both captures into `window` stereo samples and compare the levels
/// of every window complete on both sides. Captures too short to hold one
/// are compared as a single window over their common length.
fn compare(a: &[(i16, i16)], b: &[(i16, i16)], window: usize, silence: u16) -> Vec<Window> {
    let floor = f64::from(silence).max(1.0);
    let common = a.len().min(b.len());
    let (count, len) = if common >= window {
        (common / window, window)
    } else {
        (usize::from(common > 0), common)
    };
    (0..count)
        .map(|i| {
            let at = i * window;
            let (rms_a, rms_b) = (rms(&a[at..at + len]), rms(&b[at..at + len]));
            Window {
                start_ms: at as u64 * 1000 / SAMPLE_RATE,
                rms_a,
                rms_b,
                delta_pct: (rms_a - rms_b).abs() / rms_a.max(rms_b).max(floor) * 100.0,
            }
        })
        .collect()
}

/// A run of stereo samples.
type Capture<'s> = &'s [(i16, i16)];

/// The two captures as they are compared, and the onset shift
/// `onset_b - onset_a` when they were cut at their onsets. With
/// `align`, both start at their first sample above the silence level;
/// two silent captures have no onset and are compared whole. `Err` when
/// only one of them is silent: there is nothing to align, and it is a
/// difference.
fn aligned<'s>(
    a: Capture<'s>,
    b: Capture<'s>,
    onsets: (Option<usize>, Option<usize>),
    align: bool,
) -> Result<(Capture<'s>, Capture<'s>, Option<i64>), ()> {
    if !align {
        return Ok((a, b, None));
    }
    match onsets {
        (Some(oa), Some(ob)) => Ok((&a[oa..], &b[ob..], Some(ob as i64 - oa as i64))),
        (None, None) => Ok((a, b, None)),
        _ => Err(()),
    }
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
    let script = match input_script(&InputFlags::pad1(o.input_script)) {
        Ok(script) => script,
        Err(code) => return ExitCode::from(code),
    };
    let (a, b) = match (capture(rom_a, o, script.clone()), capture(rom_b, o, script)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let (onset_a, onset_b) = (onset(&a, o.silence), onset(&b, o.silence));
    let cut = aligned(&a, &b, (onset_a, onset_b), o.align_onset);
    let (cut_a, cut_b, onset_shift) = cut.unwrap_or((&a, &b, None));
    let windows = compare(cut_a, cut_b, window, o.silence);
    for w in &windows {
        println!(
            "window {:>6} ms: a={:>9.2} b={:>9.2} delta={:.2}%",
            w.start_ms, w.rms_a, w.rms_b, w.delta_pct
        );
    }
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
    // Both ran to the same frame: a capture a window shorter is a machine
    // that stopped, not a late sound.
    let length_mismatch = a.len().abs_diff(b.len()) >= window;
    if length_mismatch {
        println!(
            "the captures differ in length by {} samples, a window or more",
            a.len().abs_diff(b.len())
        );
    }
    if cut.is_err() {
        println!("--align-onset: one capture is silent, the other is not");
    }
    let matched =
        !windows.is_empty() && max_delta_pct <= o.tolerance_pct && !length_mismatch && cut.is_ok();
    let status = if matched { "match" } else { "diff" };
    let shift = onset_shift.map_or_else(String::new, |s| format!(", onset shift {s:+} samples"));
    println!(
        "{} window(s) of {} ms, max delta {:.2}% (tolerance {}%){shift}: {}",
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
            align_onset: o.align_onset,
            onset_shift,
            length_mismatch,
            windows,
            max_delta_pct,
            status,
        };
        if let Err(code) = write_json_report(path, &report) {
            return code;
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
    fn only_windows_complete_on_both_sides_are_compared() {
        // OpenSNES 2026-10-08: 160007 samples against 159945. The eleventh
        // window held 7 samples on one side and none on the other, and
        // read 100 %.
        let w = compare(&square(8000, 160_007), &square(8000, 159_945), 16_000, 64);
        assert_eq!(w.len(), 9);
        assert!(w.iter().all(|w| w.delta_pct < 1e-9), "{w:?}");
        assert_eq!(w[8].start_ms, 4000);
        // Exactly ten windows on both sides: ten are compared.
        let w = compare(&square(8000, 160_007), &square(8000, 160_000), 16_000, 64);
        assert_eq!(w.len(), 10);
    }

    #[test]
    fn captures_shorter_than_a_window_are_one_window_over_their_common_length() {
        let w = compare(&square(8000, 9000), &square(4000, 8000), 16_000, 64);
        assert_eq!(w.len(), 1);
        assert!((w[0].delta_pct - 50.0).abs() < 1e-9, "{w:?}");
        assert!(compare(&square(8000, 9000), &[], 16_000, 64).is_empty());
    }

    #[test]
    fn aligning_on_the_onset_matches_a_start_one_frame_early() {
        // The same sound, 536 samples (one frame) earlier in B. Unaligned,
        // the window holding the start compares different amounts of it.
        let a = [vec![(0, 0); 47_906], square(8000, 112_030)].concat();
        let b = [vec![(0, 0); 47_370], square(8000, 112_566)].concat();
        let unaligned = compare(&a, &b, 16_000, 64);
        assert!(unaligned.iter().any(|w| w.delta_pct > 2.0), "{unaligned:?}");

        let onsets = (onset(&a, 64), onset(&b, 64));
        let (ca, cb, shift) = aligned(&a, &b, onsets, true).unwrap();
        assert_eq!(shift, Some(-536));
        let w = compare(ca, cb, 16_000, 64);
        assert_eq!(w.len(), 7);
        assert!(w.iter().all(|w| w.delta_pct < 1e-9), "{w:?}");
        // Without the option the captures are compared from sample 0.
        assert_eq!(aligned(&a, &b, onsets, false).unwrap().2, None);
    }

    #[test]
    fn aligning_two_silences_compares_them_whole_and_one_silence_is_a_diff() {
        let quiet = vec![(0i16, 0i16); 32_000];
        let loud = square(8000, 32_000);
        let (a, b, shift) = aligned(&quiet, &quiet, (None, None), true).unwrap();
        assert_eq!((a.len(), b.len(), shift), (32_000, 32_000, None));
        assert!(aligned(&quiet, &loud, (None, Some(0)), true).is_err());
    }
}
