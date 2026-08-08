//! `luna test` — the manifest-driven homebrew test runner (issue #181).
//!
//! One TOML manifest per test describes a ROM, an optional input script,
//! a run bound, and a set of asserts; `luna test` runs each in-process
//! against `luna_api::Emulator` (API-first — no shelling out, no output
//! parsing) and reports PASS/FAIL with the CI exit-code contract:
//! `0` all tests passed, `1` at least one assert failed, `2` manifest /
//! usage errors.
//!
//! ```toml
//! # tests/attract.toml
//! rom = "../game.sfc"            # relative to this manifest
//! frames = 600                   # run bound: `frames` or `steps`
//! input = "300:0x1000,310:0"     # optional --input script (or "@file")
//!
//! [asserts]
//! wdm_empty = true               # SNES_ASSERT never fired
//! nocash_contains = "BOOT OK"    # the $21FC TTY printed this
//! fbhash = "7429bf441a1c7d6c"    # displayed-frame hash (--update refreshes)
//!
//! [asserts.values]               # symbol (or "BANK:OFFSET") = expected
//! r_done = 0xBEEF                # ≤0xFF checks 1 byte, else 2 (LE)
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

use crate::parsers::{parse_input_script, step_to_frame_bounded};
use crate::rom::load_rom_into;

/// Instruction budget granted per frame of `frames` bound (matches the
/// `--input` replay budget) and per whole `steps` run's frame chase.
const FRAME_BUDGET: u64 = 200_000;

/// One parsed manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    /// ROM path, relative to the manifest file.
    rom: PathBuf,
    /// Optional explicit `.sym` (beside-ROM auto-detection applies anyway).
    sym: Option<PathBuf>,
    /// Optional forced mapper (the `--force-mapper` vocabulary).
    force_mapper: Option<String>,
    /// Optional forced region (`ntsc` / `pal`).
    force_region: Option<String>,
    /// Run bound: whole frames…
    frames: Option<u64>,
    /// …or raw instructions. Exactly one must be present.
    steps: Option<u64>,
    /// Optional joypad script (`frame:mask` entries or `@file`).
    input: Option<String>,
    /// Optional screenshot artifact path (relative to the manifest),
    /// written after the run completes.
    screenshot: Option<PathBuf>,
    #[serde(default)]
    asserts: Asserts,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asserts {
    /// The WDM assert channel must have stayed silent.
    #[serde(default)]
    wdm_empty: Option<bool>,
    /// The nocash TTY output must contain this text.
    #[serde(default)]
    nocash_contains: Option<String>,
    /// The displayed-frame hash (16 hex chars — `--update` refreshes it).
    #[serde(default)]
    fbhash: Option<String>,
    /// `symbol` (or `"BANK:OFFSET"`) → expected value. Values ≤ 0xFF
    /// check one byte; larger values check a little-endian u16.
    #[serde(default)]
    values: std::collections::BTreeMap<String, i64>,
}

/// Outcome of one test.
struct TestOutcome {
    name: String,
    path: PathBuf,
    /// Empty = pass. Manifest-level errors surface separately as exit 2.
    failures: Vec<String>,
    /// The measured fbhash (for `--update` and the JSON report).
    fbhash: Option<String>,
}

/// `luna test` entry point.
pub(crate) fn run_tests(
    paths: &[PathBuf],
    update: bool,
    only: Option<&str>,
    report_json: bool,
) -> ExitCode {
    // Collect manifests: explicit files verbatim, directories scanned
    // recursively for `*.toml`.
    let mut manifests: Vec<PathBuf> = Vec::new();
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from("tests")]
    } else {
        paths.to_vec()
    };
    for root in &roots {
        if root.is_file() {
            manifests.push(root.clone());
        } else if root.is_dir() {
            collect_tomls(root, &mut manifests);
        } else {
            eprintln!("error: no such file or directory: {}", root.display());
            return ExitCode::from(2);
        }
    }
    manifests.sort();
    if let Some(filter) = only {
        manifests.retain(|p| p.to_string_lossy().contains(filter));
    }
    if manifests.is_empty() {
        eprintln!(
            "error: no test manifests found under {:?} (looked for *.toml)",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
        );
        return ExitCode::from(2);
    }

    let mut outcomes: Vec<TestOutcome> = Vec::new();
    for path in &manifests {
        match run_one(path) {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                // A malformed manifest is a usage error, not a test failure.
                eprintln!("error: {}: {e}", path.display());
                return ExitCode::from(2);
            }
        }
    }

    // --update: rewrite each manifest's asserts.fbhash with the measured
    // value, preserving formatting and comments (toml_edit).
    if update {
        for o in &outcomes {
            let Some(hash) = &o.fbhash else { continue };
            if let Err(e) = update_fbhash(&o.path, hash) {
                eprintln!("error: updating {}: {e}", o.path.display());
                return ExitCode::from(2);
            }
        }
        eprintln!("updated fbhash in {} manifest(s)", outcomes.len());
    }

    let failed: Vec<&TestOutcome> = outcomes.iter().filter(|o| !o.failures.is_empty()).collect();
    for o in &outcomes {
        if o.failures.is_empty() {
            println!("PASS {}", o.name);
        } else {
            println!("FAIL {}", o.name);
            for f in &o.failures {
                println!("     {f}");
            }
        }
    }
    println!(
        "{} passed, {} failed, {} total",
        outcomes.len() - failed.len(),
        failed.len(),
        outcomes.len()
    );

    if report_json {
        let report = serde_json::json!({
            "passed": outcomes.len() - failed.len(),
            "failed": failed.len(),
            "total": outcomes.len(),
            "tests": outcomes.iter().map(|o| serde_json::json!({
                "name": o.name,
                "manifest": o.path.display().to_string(),
                "passed": o.failures.is_empty(),
                "failures": o.failures,
                "fbhash": o.fbhash,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    }

    if update || failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn collect_tomls(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_tomls(&p, out);
        } else if p.extension().is_some_and(|e| e == "toml") {
            out.push(p);
        }
    }
}

/// Run one manifest. `Err` = manifest/setup problem (exit 2 at the top
/// level); assert failures land in the returned outcome.
fn run_one(path: &Path) -> Result<TestOutcome, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("reading manifest: {e}"))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("parsing manifest: {e}"))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_stem().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    let bound = match (m.frames, m.steps) {
        (Some(f), None) => Bound::Frames(f),
        (None, Some(s)) => Bound::Steps(s),
        _ => return Err("exactly one of `frames` or `steps` is required".into()),
    };

    // Input scripts resolve `@file` relative to the manifest directory.
    let input_entries = match &m.input {
        Some(spec) => {
            let spec = match spec.strip_prefix('@') {
                Some(rel) => format!("@{}", dir.join(rel).display()),
                None => spec.clone(),
            };
            parse_input_script(&spec).map_err(|e| format!("input script: {e}"))?
        }
        None => Vec::new(),
    };

    let mut em = luna_api::Emulator::new();
    load_rom_into(
        &mut em,
        &dir.join(&m.rom),
        m.force_mapper.as_deref(),
        m.force_region.as_deref(),
        None,
    )?;
    if let Some(sym) = &m.sym {
        em.load_symbols(&dir.join(sym))
            .map_err(|e| format!("loading symbols: {e}"))?;
    }
    // The SDK assert/log channels back the wdm/nocash asserts.
    let _ = em.enable_wdm_log();
    let _ = em.enable_nocash_log();

    // Drive: input checkpoints spend from the same budget as the run
    // (the issue #126 semantics), then the remaining bound.
    let total_budget = match bound {
        Bound::Frames(f) => f.saturating_mul(FRAME_BUDGET),
        Bound::Steps(s) => s,
    };
    let mut spent = 0u64;
    for (frame, mask) in &input_entries {
        spent += step_to_frame_bounded(&mut em, *frame, total_budget.saturating_sub(spent));
        em.set_joypad(0, *mask).map_err(|e| e.to_string())?;
    }
    match bound {
        Bound::Frames(f) => {
            spent += step_to_frame_bounded(&mut em, f, total_budget.saturating_sub(spent));
            let _ = spent;
        }
        Bound::Steps(s) => {
            em.step(s.saturating_sub(spent))
                .map_err(|e| e.to_string())?;
        }
    }

    // Evaluate the asserts.
    let mut failures = Vec::new();
    if m.asserts.wdm_empty == Some(true) {
        let hits = em.take_wdm_log().map_err(|e| e.to_string())?;
        if !hits.is_empty() {
            let (pc, operand) = hits[0];
            failures.push(format!(
                "wdm_empty: {} assertion(s) fired — first at PC=${pc:06X} operand=${operand:02X}",
                hits.len()
            ));
        }
    }
    if let Some(needle) = &m.asserts.nocash_contains {
        let text =
            String::from_utf8_lossy(&em.take_nocash_log().map_err(|e| e.to_string())?).into_owned();
        if !text.contains(needle) {
            failures.push(format!(
                "nocash_contains: `{needle}` not found in TTY output ({} bytes captured)",
                text.len()
            ));
        }
    }
    let measured_fbhash = em.frame_hash(false).map(|h| format!("{h:016x}")).ok();
    if let Some(want) = &m.asserts.fbhash {
        match &measured_fbhash {
            Some(got) if got.eq_ignore_ascii_case(want) => {}
            Some(got) => failures.push(format!(
                "fbhash: expected {want}, got {got} (run `luna test --update` after an intended render change)"
            )),
            None => failures.push("fbhash: no frame rendered".into()),
        }
    }
    for (key, &expected) in &m.asserts.values {
        match check_value(&mut em, key, expected) {
            Ok(None) => {}
            Ok(Some(msg)) => failures.push(msg),
            Err(e) => failures.push(format!("values.{key}: {e}")),
        }
    }

    if let Some(shot) = &m.screenshot {
        let dest = dir.join(shot);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match em.render_frame_png(false) {
            Ok(png) => {
                if let Err(e) = std::fs::write(&dest, png) {
                    eprintln!("warning: writing {}: {e}", dest.display());
                }
            }
            Err(e) => eprintln!("warning: rendering screenshot: {e}"),
        }
    }

    Ok(TestOutcome {
        name,
        path: path.to_path_buf(),
        failures,
        fbhash: measured_fbhash,
    })
}

enum Bound {
    Frames(u64),
    Steps(u64),
}

/// Check one `[asserts.values]` entry: `key` is a loaded symbol or a
/// `BANK:OFFSET` hex pair; `expected` ≤ 0xFF checks one byte, larger
/// values check a little-endian u16. `Ok(Some(msg))` = failed assert.
fn check_value(
    em: &mut luna_api::Emulator,
    key: &str,
    expected: i64,
) -> Result<Option<String>, String> {
    if !(0..=0xFFFF).contains(&expected) {
        return Err("expected value must fit in 16 bits".into());
    }
    let addr = if let Some(a) = em.resolve_symbol(key) {
        a
    } else {
        let (bank_s, off_s) = key
            .split_once(':')
            .ok_or_else(|| "not a loaded symbol and not BANK:OFFSET".to_string())?;
        let bank = u8::from_str_radix(bank_s, 16).map_err(|e| format!("bad bank: {e}"))?;
        let off = u16::from_str_radix(off_s, 16).map_err(|e| format!("bad offset: {e}"))?;
        (u32::from(bank) << 16) | u32::from(off)
    };
    let width = if expected <= 0xFF { 1 } else { 2 };
    let bytes = em
        .peek_memory((addr >> 16) as u8, addr as u16, width)
        .map_err(|e| e.to_string())?;
    let got = match width {
        1 => i64::from(bytes[0]),
        _ => i64::from(u16::from_le_bytes([bytes[0], bytes[1]])),
    };
    if got == expected {
        Ok(None)
    } else {
        Ok(Some(format!(
            "values.{key}: expected {expected:#X}, got {got:#X} at {addr:#08X}"
        )))
    }
}

/// Rewrite `asserts.fbhash` in place, preserving the manifest's
/// formatting and comments.
fn update_fbhash(path: &Path, hash: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    // Only touch manifests that assert an fbhash at all.
    let has = doc.get("asserts").and_then(|a| a.get("fbhash")).is_some();
    if !has {
        return Ok(());
    }
    doc["asserts"]["fbhash"] = toml_edit::value(hash);
    std::fs::write(path, doc.to_string()).map_err(|e| e.to_string())
}
