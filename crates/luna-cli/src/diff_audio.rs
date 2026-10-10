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
//! `--align-onset` handles the shift a window's edge turns into a level
//! difference: a sound that starts a few samples later puts less of its
//! attack in the window that holds it. Each window of A is then compared
//! with the window of B that fits it best within `--max-shift` samples
//! (least sum of absolute differences), and every line names the shift it
//! kept. The search is per window because sounds move independently: the
//! first can stay put while the second comes two samples late (`OpenSNES`,
//! 2026-10-10), which one shift for the whole capture cannot follow.
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
    /// `--max-shift`: how far a window of B may be moved, in samples.
    pub max_shift: usize,
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
    /// `--align-onset`: how many samples later (`+`) or earlier (`-`) B's
    /// window was taken to fit A's best (`null` without the option).
    shift: Option<i64>,
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
    /// Whether each window of B was fitted to A's (`--align-onset`).
    align_onset: bool,
    /// `--max-shift`: the widest shift searched (`null` without
    /// `--align-onset`).
    shift_limit: Option<usize>,
    /// The shift of largest magnitude a window kept (`null` without
    /// `--align-onset`).
    largest_shift: Option<i64>,
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

/// A run of stereo samples.
type Capture<'s> = &'s [(i16, i16)];

/// Sum of absolute differences between two runs of equal length.
fn distance(a: Capture<'_>, b: Capture<'_>) -> u64 {
    a.iter()
        .zip(b)
        .map(|(&(al, ar), &(bl, br))| {
            u64::from((i32::from(al) - i32::from(bl)).unsigned_abs())
                + u64::from((i32::from(ar) - i32::from(br)).unsigned_abs())
        })
        .sum()
}

/// `len` samples of `b` from `at + shift`. Before sample 0 the machine was
/// not running: that is silence. Past the end nothing is known: `None`.
fn shifted(b: Capture<'_>, at: usize, shift: i64, len: usize) -> Option<Vec<(i16, i16)>> {
    let start = at as i64 + shift;
    let lead = usize::try_from(-start).unwrap_or(0).min(len);
    let from = usize::try_from(start).unwrap_or(0);
    let rest = b.get(from..from + (len - lead))?;
    Some([&vec![(0, 0); lead][..], rest].concat())
}

/// B's window that is closest to `a[at..at + len]` within `±limit`
/// samples, and its shift. Of several equally close ones the smallest
/// shift wins (0, then +1, -1…), so a silent or periodic window keeps the
/// shift nearest to none.
fn best_fit(
    a: Capture<'_>,
    b: Capture<'_>,
    at: usize,
    len: usize,
    limit: usize,
) -> (i64, Vec<(i16, i16)>) {
    let reference = &a[at..at + len];
    let mut best = (u64::MAX, 0i64, b[at..at + len].to_vec());
    for magnitude in 0..=limit as i64 {
        for shift in [magnitude, -magnitude] {
            let Some(candidate) = shifted(b, at, shift, len) else {
                continue;
            };
            let d = distance(reference, &candidate);
            if d < best.0 {
                best = (d, shift, candidate);
            }
        }
    }
    (best.1, best.2)
}

/// Cut both captures into `window` stereo samples and compare the levels
/// of every window complete on both sides. Captures too short to hold one
/// are compared as a single window over their common length. With
/// `max_shift`, B's window is the one that fits A's best within that many
/// samples.
fn compare(
    a: Capture<'_>,
    b: Capture<'_>,
    window: usize,
    silence: u16,
    max_shift: Option<usize>,
) -> Vec<Window> {
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
            let fit = max_shift.map(|limit| best_fit(a, b, at, len, limit));
            let rms_a = rms(&a[at..at + len]);
            let rms_b = fit
                .as_ref()
                .map_or_else(|| rms(&b[at..at + len]), |(_, fitted)| rms(fitted));
            let shift = fit.map(|(shift, _)| shift);
            Window {
                start_ms: at as u64 * 1000 / SAMPLE_RATE,
                rms_a,
                rms_b,
                delta_pct: (rms_a - rms_b).abs() / rms_a.max(rms_b).max(floor) * 100.0,
                shift,
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
    if o.align_onset && o.max_shift >= window {
        eprintln!(
            "error: --max-shift must be shorter than a window ({window} samples at --window-ms {})",
            o.window_ms
        );
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
    let shift_limit = o.align_onset.then_some(o.max_shift);
    let windows = compare(&a, &b, window, o.silence, shift_limit);
    for w in &windows {
        let shift = w
            .shift
            .map_or_else(String::new, |s| format!(" shift={s:+}"));
        println!(
            "window {:>6} ms: a={:>9.2} b={:>9.2} delta={:.2}%{shift}",
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
    let matched = !windows.is_empty() && max_delta_pct <= o.tolerance_pct && !length_mismatch;
    let status = if matched { "match" } else { "diff" };
    let largest_shift = windows
        .iter()
        .filter_map(|w| w.shift)
        .max_by_key(|s| s.unsigned_abs());
    // The method is on the verdict line: a line quoted from before this
    // search existed ("onset shift") cannot be taken for one from after.
    let shift = largest_shift.map_or_else(String::new, |s| {
        format!(
            ", per-window shift, max {} samples (searched ±{})",
            s.unsigned_abs(),
            o.max_shift
        )
    });
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
            shift_limit,
            largest_shift,
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
        let w = compare(&a, &b, 16_000, 64, None);
        assert_eq!(w.len(), 2);
        assert!(w.iter().all(|w| w.delta_pct < 1.0), "{w:?}");
        assert_eq!(onset(&a, 64), Some(1000));
        assert_eq!(onset(&b, 64), Some(1003));
    }

    #[test]
    fn half_the_volume_is_fifty_percent() {
        let w = compare(
            &square(8000, 16_000),
            &square(4000, 16_000),
            16_000,
            64,
            None,
        );
        assert_eq!(w.len(), 1);
        assert!((w[0].delta_pct - 50.0).abs() < 1e-9, "{w:?}");
    }

    #[test]
    fn near_silence_is_measured_against_the_floor() {
        // 1 LSB against digital silence: 100 % of itself, 1/64 of the
        // floor — not a difference worth a DIFF.
        let w = compare(&square(1, 16_000), &square(0, 16_000), 16_000, 64, None);
        assert!(w[0].delta_pct < 2.0, "{w:?}");
        assert_eq!(onset(&square(1, 100), 64), None);
    }

    #[test]
    fn only_windows_complete_on_both_sides_are_compared() {
        // OpenSNES 2026-10-08: 160007 samples against 159945. The eleventh
        // window held 7 samples on one side and none on the other, and
        // read 100 %.
        let w = compare(
            &square(8000, 160_007),
            &square(8000, 159_945),
            16_000,
            64,
            None,
        );
        assert_eq!(w.len(), 9);
        assert!(w.iter().all(|w| w.delta_pct < 1e-9), "{w:?}");
        assert_eq!(w[8].start_ms, 4000);
        // Exactly ten windows on both sides: ten are compared.
        let w = compare(
            &square(8000, 160_007),
            &square(8000, 160_000),
            16_000,
            64,
            None,
        );
        assert_eq!(w.len(), 10);
    }

    #[test]
    fn captures_shorter_than_a_window_are_one_window_over_their_common_length() {
        let w = compare(&square(8000, 9000), &square(4000, 8000), 16_000, 64, None);
        assert_eq!(w.len(), 1);
        assert!((w[0].delta_pct - 50.0).abs() < 1e-9, "{w:?}");
        assert!(compare(&square(8000, 9000), &[], 16_000, 64, None).is_empty());
    }

    /// Two bursts of a decaying tone after `lead` and `gap` samples of
    /// silence — no two shifts of it look alike.
    fn two_bursts(lead: usize, gap: usize, total: usize) -> Vec<(i16, i16)> {
        let burst = |n: usize| -> Vec<(i16, i16)> {
            (0..n)
                .map(|i| {
                    let v = (((i * 37) % 251) as i32 - 125) * 60 * (n - i) as i32 / n as i32;
                    (v as i16, (v / 2) as i16)
                })
                .collect()
        };
        let mut out = [
            vec![(0, 0); lead],
            burst(12_000),
            vec![(0, 0); gap],
            burst(12_000),
        ]
        .concat();
        out.resize(total, (0, 0));
        out
    }

    #[test]
    fn each_window_is_fitted_by_its_own_shift() {
        // OpenSNES 2026-10-10: the first sound has not moved, the second
        // comes two samples late, and a window edge falls in its attack.
        let a = two_bursts(1000, 9000, 48_100);
        let b = two_bursts(1000, 9002, 48_100);
        let plain = compare(&a, &b, 16_000, 64, None);
        assert!(
            plain[1].delta_pct > 0.0,
            "the edge cuts the attack: {plain:?}"
        );
        let w = compare(&a, &b, 16_000, 64, Some(64));
        assert_eq!(
            w.iter().map(|w| w.shift).collect::<Vec<_>>(),
            [Some(0), Some(2), Some(2)]
        );
        assert!(w.iter().all(|w| w.delta_pct < 0.01), "{w:?}");
    }

    #[test]
    fn a_shift_beyond_the_limit_is_not_found_and_silence_keeps_zero() {
        // One frame (536 samples) early: out of reach at 64, a fact to
        // explain; within reach when asked for.
        let a = two_bursts(8000, 2000, 64_000);
        let b = two_bursts(8000 - 536, 2000, 64_000);
        let near = compare(&a, &b, 16_000, 64, Some(64));
        assert!(near.iter().any(|w| w.delta_pct > 2.0), "{near:?}");
        let far = compare(&a, &b, 16_000, 64, Some(536));
        assert_eq!(far[0].shift, Some(-536));
        assert!(far[0].delta_pct < 0.01, "{far:?}");
        // The last window is silent on both sides: every shift fits, the
        // one nearest to none is kept, and it is still printed. B's first
        // window was read from before its sample 0: silence.
        assert_eq!(
            far.iter().map(|w| w.shift).collect::<Vec<_>>(),
            [Some(-536), Some(-536), Some(-536), Some(0)]
        );
        // The other way round the shift is forward.
        let late = compare(&b, &a, 16_000, 64, Some(600));
        assert_eq!(late[0].shift, Some(536));
    }
}
