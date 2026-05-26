//! Smoke tests: drive luna against a small set of commercial ROMs at
//! fixed instruction counts and compare the produced screenshot +
//! audio against committed goldens.
//!
//! - **Screenshots** (`tests/golden/smoke/<name>.png`): byte-compared
//!   against the freshly captured PNG. Caught regressions on PPU
//!   rendering historically cost hours of audible-fix-test churn
//!   before they were spotted; a byte-exact gate catches them on the
//!   next `cargo test`.
//! - **Audio** (`tests/golden/audio/<name>.json`): compared on summary
//!   statistics (peak / RMS / mean / non-zero ratio) with tolerances
//!   matching `tools/audio_goldens.py`. No FFT comparison — the
//!   simple stats already catch the "echo died" / "voices silent"
//!   regressions we care about, and skipping the FFT keeps the test
//!   under 2 s wall time per case.
//!
//! ROMs aren't redistributable; the test SKIPs gracefully when a ROM
//! file is missing (CI on public repos won't have them; contributors
//! who own the ROMs locally drop them into `tests/roms/`).
//!
//! ## Regenerating goldens
//!
//! ```bash
//! UPDATE_GOLDENS=1 cargo test --test smoke -- --nocapture
//! ```
//!
//! Inspect the diff (`git diff tests/golden/`) and commit if intentional.

use std::path::PathBuf;

use luna_api::Emulator;

/// One ROM × instruction-count combination to exercise.
struct Case {
    /// Short identifier used to name the goldens. Stays the same forever.
    id: &'static str,
    /// Filename inside `tests/roms/`.
    rom_filename: &'static str,
    /// How many main-CPU instructions to run before sampling state.
    insns: u64,
    /// Plain-English description for failure messages.
    note: &'static str,
}

const CASES: &[Case] = &[
    Case {
        id: "smw",
        rom_filename: "Super Mario World (U) [!].smc",
        insns: 10_000_000,
        note: "title screen, ~10 s of audio (no echo on this driver)",
    },
    Case {
        id: "dkc",
        rom_filename: "Donkey Kong Country (U) (V1.2) [!].smc",
        insns: 80_000_000,
        note: "past Rareware logo, ~36 s of audio with echo + voices 4-6",
    },
    Case {
        id: "bomberman",
        rom_filename: "Super Bomberman (USA).sfc",
        insns: 60_000_000,
        note: "title screen, ~63 s of audio",
    },
];

const TOL_AMPLITUDE_PCT: f64 = 0.15; // ±15 % on peak / RMS / mean
const TOL_NONZERO_ABS: f64 = 0.05; // ±5 percentage points on non-zero ratio

/// Locate the workspace root and resolve `tests/roms/<rom>` /
/// `tests/golden/<...>` paths from there. `CARGO_MANIFEST_DIR` is set
/// by cargo to this crate's directory; the workspace root is one level
/// up from `crates/luna-api`.
fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[derive(Debug)]
struct AudioStats {
    peak_pos: i32,
    peak_neg: i32,
    rms: f64,
    mean_abs: f64,
    non_zero: f64,
}

impl AudioStats {
    fn from_samples(samples: &[(i16, i16)]) -> Self {
        let lefts: Vec<i32> = samples.iter().map(|(l, _)| i32::from(*l)).collect();
        let mut peak_pos = 0;
        let mut peak_neg = 0;
        let mut sum_sq: f64 = 0.0;
        let mut sum_abs: f64 = 0.0;
        let mut non_zero = 0usize;
        for &s in &lefts {
            if s > peak_pos {
                peak_pos = s;
            }
            if s < peak_neg {
                peak_neg = s;
            }
            let abs = f64::from(s.abs());
            sum_abs += abs;
            sum_sq += abs * abs;
            if s != 0 {
                non_zero += 1;
            }
        }
        let n = lefts.len().max(1) as f64;
        Self {
            peak_pos,
            peak_neg,
            rms: (sum_sq / n).sqrt(),
            mean_abs: sum_abs / n,
            non_zero: non_zero as f64 / n,
        }
    }
}

fn within_pct(actual: f64, golden: f64, pct: f64) -> bool {
    let tol = golden.abs().max(1.0) * pct;
    (actual - golden).abs() <= tol
}

/// Compare audio stats against the golden JSON. Returns a list of
/// violation strings (empty = pass).
fn check_audio(case: &Case, actual: &AudioStats, duration_s: f64) -> Vec<String> {
    let mut violations = Vec::new();
    let path = workspace_root()
        .join("tests/golden/audio")
        .join(format!("{}.json", case.id));
    let Ok(text) = std::fs::read_to_string(&path) else {
        violations.push(format!(
            "missing audio golden {}: run `UPDATE_GOLDENS=1 cargo test --test smoke`",
            path.display()
        ));
        return violations;
    };
    let Ok(g): Result<serde_json::Value, _> = serde_json::from_str(&text) else {
        violations.push(format!("audio golden {} is not valid JSON", path.display()));
        return violations;
    };
    let stats = &g["stats"];
    let g_peak_pos = stats["peak_pos"].as_f64().unwrap_or_default();
    let g_peak_neg = stats["peak_neg"].as_f64().unwrap_or_default();
    let g_rms = stats["rms"].as_f64().unwrap_or_default();
    let g_mean_abs = stats["mean_abs"].as_f64().unwrap_or_default();
    let g_non_zero = stats["non_zero"].as_f64().unwrap_or_default();
    let g_duration = g["duration_s"].as_f64().unwrap_or_default();

    if !within_pct(f64::from(actual.peak_pos), g_peak_pos, TOL_AMPLITUDE_PCT) {
        violations.push(format!(
            "peak_pos: got {} expected ~{:.0} (±{:.0} %)",
            actual.peak_pos,
            g_peak_pos,
            TOL_AMPLITUDE_PCT * 100.0
        ));
    }
    if !within_pct(f64::from(actual.peak_neg), g_peak_neg, TOL_AMPLITUDE_PCT) {
        violations.push(format!(
            "peak_neg: got {} expected ~{:.0}",
            actual.peak_neg, g_peak_neg
        ));
    }
    if !within_pct(actual.rms, g_rms, TOL_AMPLITUDE_PCT) {
        violations.push(format!("rms: got {:.1} expected ~{:.1}", actual.rms, g_rms));
    }
    if !within_pct(actual.mean_abs, g_mean_abs, TOL_AMPLITUDE_PCT) {
        violations.push(format!(
            "mean_abs: got {:.1} expected ~{:.1}",
            actual.mean_abs, g_mean_abs
        ));
    }
    if (actual.non_zero - g_non_zero).abs() > TOL_NONZERO_ABS {
        violations.push(format!(
            "non_zero: got {:.3} expected ~{:.3} (±{:.3})",
            actual.non_zero, g_non_zero, TOL_NONZERO_ABS
        ));
    }
    if (duration_s - g_duration).abs() > 0.5 {
        violations.push(format!(
            "duration_s: got {:.2} expected ~{:.2}",
            duration_s, g_duration
        ));
    }
    violations
}

/// Byte-compare the captured PNG against the golden. On mismatch the
/// actual PNG is dumped to `target/smoke-test/<id>-actual.png` so the
/// developer can eyeball the regression with their preferred image
/// viewer.
fn check_screenshot(case: &Case, png: &[u8]) -> Result<(), String> {
    let golden_path = workspace_root()
        .join("tests/golden/smoke")
        .join(format!("{}.png", case.id));
    let Ok(golden) = std::fs::read(&golden_path) else {
        return Err(format!(
            "missing screenshot golden {}: run `UPDATE_GOLDENS=1 cargo test --test smoke`",
            golden_path.display()
        ));
    };
    if png == golden.as_slice() {
        return Ok(());
    }
    // Dump for inspection.
    let dump_dir = workspace_root().join("target/smoke-test");
    std::fs::create_dir_all(&dump_dir).ok();
    let dump = dump_dir.join(format!("{}-actual.png", case.id));
    let _ = std::fs::write(&dump, png);
    Err(format!(
        "screenshot {} differs from golden {} (got {} bytes, expected {}); \
         actual dumped to {}",
        case.id,
        golden_path.display(),
        png.len(),
        golden.len(),
        dump.display()
    ))
}

fn write_audio_golden(case: &Case, stats: &AudioStats, duration_s: f64) {
    let path = workspace_root()
        .join("tests/golden/audio")
        .join(format!("{}.json", case.id));
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    // Preserve any spectrum_peaks from a previous golden (the Python
    // tool populates these; we don't recompute them here).
    let prev_peaks = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("spectrum_peaks").cloned())
        .unwrap_or(serde_json::Value::Array(vec![]));
    let golden = serde_json::json!({
        "rom": case.rom_filename,
        "insns": case.insns,
        "note": case.note,
        "duration_s": (duration_s * 1000.0).round() / 1000.0,
        "stats": {
            "peak_pos": stats.peak_pos,
            "peak_neg": stats.peak_neg,
            "rms": (stats.rms * 100.0).round() / 100.0,
            "mean_abs": (stats.mean_abs * 100.0).round() / 100.0,
            "non_zero": (stats.non_zero * 10000.0).round() / 10000.0,
        },
        "spectrum_peaks": prev_peaks,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&golden).unwrap() + "\n")
        .unwrap_or_else(|e| panic!("write {}: {}", path.display(), e));
}

fn write_screenshot_golden(case: &Case, png: &[u8]) {
    let path = workspace_root()
        .join("tests/golden/smoke")
        .join(format!("{}.png", case.id));
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    std::fs::write(&path, png).unwrap_or_else(|e| panic!("write {}: {}", path.display(), e));
}

#[test]
fn smoke() {
    let update = std::env::var("UPDATE_GOLDENS").is_ok();
    let mut violations: Vec<String> = Vec::new();
    let mut ran = 0;
    let mut skipped = 0;

    for case in CASES {
        let rom_path = workspace_root().join("tests/roms").join(case.rom_filename);
        if !rom_path.exists() {
            eprintln!("SKIP {}: ROM not at {}", case.id, rom_path.display());
            skipped += 1;
            continue;
        }
        eprintln!("RUN  {} ({}M insns)", case.id, case.insns / 1_000_000);

        let mut emu = Emulator::new();
        if let Err(e) = emu.load_rom(&rom_path) {
            violations.push(format!("{}: load_rom failed: {e}", case.id));
            continue;
        }
        // The APU's audio queue caps at ~16 k samples (~ 512 ms); a
        // long run with a single `step()` overflows and drops most of
        // the audio. Chunk the step + drain incrementally, same trick
        // luna-cli uses when emitting `--audio-out`.
        const AUDIO_CHUNK: u64 = 100_000;
        let mut audio: Vec<(i16, i16)> = Vec::new();
        let mut left = case.insns;
        let mut step_err: Option<String> = None;
        while left > 0 {
            let take = left.min(AUDIO_CHUNK);
            if let Err(e) = emu.step(take) {
                step_err = Some(format!("{}: step failed: {e}", case.id));
                break;
            }
            left -= take;
            if let Ok(mut chunk) = emu.drain_audio(usize::MAX) {
                audio.append(&mut chunk);
            }
        }
        if let Some(e) = step_err {
            violations.push(e);
            continue;
        }
        let png = match emu.render_frame_png(false) {
            Ok(p) => p,
            Err(e) => {
                violations.push(format!("{}: render_frame_png failed: {e}", case.id));
                continue;
            }
        };
        let stats = AudioStats::from_samples(&audio);
        let duration_s = audio.len() as f64 / 32_000.0;

        if update {
            write_audio_golden(case, &stats, duration_s);
            write_screenshot_golden(case, &png);
            eprintln!(
                "     wrote goldens (peak ±{}/{}, rms {:.0}, duration {:.2}s, png {} bytes)",
                stats.peak_pos,
                stats.peak_neg,
                stats.rms,
                duration_s,
                png.len()
            );
        } else {
            let mut case_failed = false;
            for v in check_audio(case, &stats, duration_s) {
                violations.push(format!("{}/audio: {v}", case.id));
                case_failed = true;
            }
            if let Err(v) = check_screenshot(case, &png) {
                violations.push(format!("{}/screenshot: {v}", case.id));
                case_failed = true;
            }
            if !case_failed {
                eprintln!("OK   {}", case.id);
            }
        }
        ran += 1;
    }

    eprintln!(
        "smoke: {} case(s) ran, {} skipped, {} violation(s)",
        ran,
        skipped,
        violations.len()
    );
    if ran == 0 {
        eprintln!(
            "no ROMs found in tests/roms/. This is expected on public CI \
             (commercial ROMs aren't redistributable). Drop your ROMs in \
             tests/roms/ to run smoke tests locally."
        );
        return;
    }
    if !violations.is_empty() {
        panic!(
            "smoke test failed ({} violation(s)):\n  {}",
            violations.len(),
            violations.join("\n  ")
        );
    }
}
