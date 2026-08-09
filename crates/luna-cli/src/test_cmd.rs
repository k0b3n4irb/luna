//! `luna test` — the manifest-driven homebrew test runner (issue #181;
//! asserts v2 in issue #205).
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
//! audio_rms_min = 100.0          # captured audio is non-silent (#205)
//!
//! [asserts.values]               # symbol (or "BANK:OFFSET") = expected
//! r_done = 0xBEEF                # bare int = eq; ≤0xFF is 1 byte, else 2 (LE)
//! r_score = { ge = 0x1000 }      # eq/ne/ge/gt/le/lt (+ optional width = 1|2)
//!
//! [asserts.blocks]               # arbitrary-length byte-range equality
//! "00:00AC" = "000001fe40"       # cpu space: symbol or BANK:OFFSET = hex
//! "0000" = { space = "vram", hex = "ffff" }   # wram|vram|cgram|oam|aram
//!
//! [asserts.trace]                # the trace recorded ≥ min events
//! superfx = { min = 1 }          # dma|dsp|mailbox|sa1|superfx|dsp1|spc
//!
//! [[checkpoint]]                 # before/after checks along the run (#205)
//! at_frame = 60
//! input = "60:0x0100,63:0"       # this leg's presses (absolute frames)
//! [checkpoint.values]            # evaluated when frame 60 is reached
//! r_state = 2
//! [checkpoint.delta]             # vs the previous checkpoint (or the start)
//! xloc = "increased"             # increased|decreased|changed|unchanged
//! r_mode = { dir = "unchanged", width = 1 }
//! ```

use std::collections::BTreeMap;
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
    /// …or raw instructions. One is required unless `[[checkpoint]]`s
    /// define the run (then `frames` may still extend past the last one).
    steps: Option<u64>,
    /// Optional joypad script (`frame:mask` entries or `@file`).
    input: Option<String>,
    /// Optional screenshot artifact path (relative to the manifest),
    /// written after the run completes.
    screenshot: Option<PathBuf>,
    /// Ordered mid-run checkpoints (issue #205): each runs to
    /// `at_frame`, then evaluates its `values` and `delta` asserts.
    #[serde(default)]
    checkpoint: Vec<Checkpoint>,
    #[serde(default)]
    asserts: Asserts,
}

/// One `[[checkpoint]]` (issue #205): a mid-run measurement point.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    /// Absolute frame this checkpoint fires at (must be increasing).
    at_frame: u64,
    /// This leg's joypad entries (absolute frames, same grammar as the
    /// top-level `input`).
    input: Option<String>,
    /// Point-in-time value asserts, same grammar as `[asserts.values]`.
    #[serde(default)]
    values: BTreeMap<String, ValueAssert>,
    /// Directional asserts vs the previous checkpoint (or the run start
    /// for the first one): `increased | decreased | changed | unchanged`.
    #[serde(default)]
    delta: BTreeMap<String, DeltaAssert>,
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
    /// Captured audio RMS (over the drained sample ring) must be ≥ this
    /// — the "music is actually playing" oracle (issue #205).
    #[serde(default)]
    audio_rms_min: Option<f64>,
    /// `symbol` (or `"BANK:OFFSET"`) → expected value: a bare integer
    /// (`eq`) or a `{eq|ne|ge|gt|le|lt, width?}` comparator table.
    #[serde(default)]
    values: BTreeMap<String, ValueAssert>,
    /// Arbitrary-length byte-range equality, any memory space
    /// (issue #205).
    #[serde(default)]
    blocks: BTreeMap<String, BlockAssert>,
    /// Per-trace minimum event counts — proves a coprocessor/driver
    /// actually executed (issue #205).
    #[serde(default)]
    trace: BTreeMap<String, TraceAssert>,
}

/// A `[asserts.values]` entry: bare integer = exact match, or a
/// comparator table.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ValueAssert {
    Exact(i64),
    Cmp(CmpSpec),
}

/// The comparator-table form of a value assert.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CmpSpec {
    eq: Option<i64>,
    ne: Option<i64>,
    ge: Option<i64>,
    gt: Option<i64>,
    le: Option<i64>,
    lt: Option<i64>,
    /// Read width in bytes (1 or 2). Default: 1 if every bound fits in
    /// a byte, else 2 (little-endian).
    width: Option<u8>,
}

/// A `[checkpoint.delta]` entry: a bare direction string, or a table
/// with an explicit read width.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeltaAssert {
    Dir(String),
    Spec {
        dir: String,
        #[serde(default)]
        width: Option<u8>,
    },
}

impl DeltaAssert {
    fn dir(&self) -> &str {
        match self {
            Self::Dir(d) => d,
            Self::Spec { dir, .. } => dir,
        }
    }
    fn width(&self) -> u8 {
        match self {
            Self::Dir(_) => 2,
            Self::Spec { width, .. } => width.unwrap_or(2),
        }
    }
}

/// A `[asserts.blocks]` entry: a bare hex string (CPU space), or a
/// table selecting another memory space.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BlockAssert {
    Hex(String),
    Spec {
        /// `wram` (alias of cpu addressing) | `vram` | `cgram` | `oam`
        /// | `aram`.
        space: String,
        hex: String,
    },
}

/// A `[asserts.trace]` entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceAssert {
    /// The trace must have recorded at least this many events.
    min: u64,
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

    // Run bound: `frames` XOR `steps`; checkpoints alone are enough (the
    // last `at_frame` bounds the run), and `frames` may extend past them.
    let bound = match (m.frames, m.steps) {
        (Some(_), Some(_)) => return Err("`frames` and `steps` are mutually exclusive".into()),
        (Some(f), None) => Some(Bound::Frames(f)),
        (None, Some(s)) => Some(Bound::Steps(s)),
        (None, None) if !m.checkpoint.is_empty() => None,
        (None, None) => {
            return Err("one of `frames`, `steps` or `[[checkpoint]]`s is required".into());
        }
    };
    if m.steps.is_some() && !m.checkpoint.is_empty() {
        return Err("`steps` cannot be combined with `[[checkpoint]]`s (use `frames`)".into());
    }
    let mut prev_frame = 0u64;
    for (i, cp) in m.checkpoint.iter().enumerate() {
        if i > 0 && cp.at_frame <= prev_frame {
            return Err(format!(
                "checkpoint at_frame values must increase (checkpoint {} is at {} after {})",
                i + 1,
                cp.at_frame,
                prev_frame
            ));
        }
        prev_frame = cp.at_frame;
        for d in cp.delta.values() {
            let dir = d.dir();
            if !matches!(dir, "increased" | "decreased" | "changed" | "unchanged") {
                return Err(format!(
                    "unknown delta direction `{dir}` (increased, decreased, changed, unchanged)"
                ));
            }
        }
    }

    // Input scripts resolve `@file` relative to the manifest directory.
    let parse_input = |spec: &str| -> Result<Vec<(u64, u16)>, String> {
        let spec = match spec.strip_prefix('@') {
            Some(rel) => format!("@{}", dir.join(rel).display()),
            None => spec.to_string(),
        };
        parse_input_script(&spec).map_err(|e| format!("input script: {e}"))
    };
    let mut input_entries: Vec<(u64, u16)> = match &m.input {
        Some(spec) => parse_input(spec)?,
        None => Vec::new(),
    };
    for cp in &m.checkpoint {
        if let Some(spec) = &cp.input {
            input_entries.extend(parse_input(spec)?);
        }
    }
    input_entries.sort_by_key(|&(frame, _)| frame);

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
    // Requested trace counters (issue #205): enable before the run with a
    // cap comfortably above the asserted minimum.
    for (trace, spec) in &m.asserts.trace {
        let cap = usize::try_from(spec.min.saturating_add(10_000)).unwrap_or(usize::MAX);
        enable_trace(&mut em, trace, cap).map_err(|e| format!("asserts.trace.{trace}: {e}"))?;
    }

    // The whole run's frame horizon: the later of the top-level `frames`
    // bound and the last checkpoint.
    let last_cp_frame = m.checkpoint.last().map_or(0, |c| c.at_frame);
    let final_frame = match bound {
        Some(Bound::Frames(f)) => f.max(last_cp_frame),
        _ => last_cp_frame,
    };
    let total_budget = match bound {
        Some(Bound::Steps(s)) => s,
        _ => final_frame.saturating_mul(FRAME_BUDGET),
    };

    let mut failures = Vec::new();
    let mut spent = 0u64;
    let mut input_iter = input_entries.iter().peekable();
    // Baseline snapshot for the first checkpoint's deltas.
    let mut delta_prev: BTreeMap<String, i64> = BTreeMap::new();
    snapshot_deltas(&mut em, &m.checkpoint, 0, &mut delta_prev);

    let drive_to = |em: &mut luna_api::Emulator,
                    target_frame: u64,
                    spent: &mut u64,
                    input_iter: &mut std::iter::Peekable<std::slice::Iter<(u64, u16)>>|
     -> Result<(), String> {
        while let Some(&&(frame, mask)) = input_iter.peek() {
            if frame > target_frame {
                break;
            }
            *spent += step_to_frame_bounded(em, frame, total_budget.saturating_sub(*spent));
            em.set_joypad(0, mask).map_err(|e| e.to_string())?;
            input_iter.next();
        }
        *spent += step_to_frame_bounded(em, target_frame, total_budget.saturating_sub(*spent));
        Ok(())
    };

    // Checkpoints in order, then the final bound.
    for (i, cp) in m.checkpoint.iter().enumerate() {
        drive_to(&mut em, cp.at_frame, &mut spent, &mut input_iter)?;
        let label = format!("checkpoint@{}", cp.at_frame);
        for (key, assert) in &cp.values {
            match check_value(&mut em, key, assert) {
                Ok(None) => {}
                Ok(Some(msg)) => failures.push(format!("{label} {msg}")),
                Err(e) => failures.push(format!("{label} values.{key}: {e}")),
            }
        }
        for (key, d) in &cp.delta {
            let prev = delta_prev.get(key).copied();
            match read_value(&mut em, key, d.width()) {
                Ok(cur) => {
                    let Some(prev) = prev else { continue };
                    let ok = match d.dir() {
                        "increased" => cur > prev,
                        "decreased" => cur < prev,
                        "changed" => cur != prev,
                        _ => cur == prev, // "unchanged" (validated above)
                    };
                    if !ok {
                        failures.push(format!(
                            "{label} delta.{key}: expected {}, was {prev:#X} -> {cur:#X}",
                            d.dir()
                        ));
                    }
                }
                Err(e) => failures.push(format!("{label} delta.{key}: {e}")),
            }
        }
        // This checkpoint becomes the next one's baseline.
        snapshot_deltas(&mut em, &m.checkpoint, i + 1, &mut delta_prev);
    }
    match bound {
        Some(Bound::Frames(_)) | None => {
            drive_to(&mut em, final_frame, &mut spent, &mut input_iter)?;
        }
        Some(Bound::Steps(s)) => {
            // No checkpoints (validated above): input entries then the rest.
            for &(frame, mask) in &input_entries {
                spent += step_to_frame_bounded(&mut em, frame, total_budget.saturating_sub(spent));
                em.set_joypad(0, mask).map_err(|e| e.to_string())?;
            }
            em.step(s.saturating_sub(spent))
                .map_err(|e| e.to_string())?;
        }
    }

    // ---- Final asserts ----
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
    if let Some(min) = m.asserts.audio_rms_min {
        let samples = em.drain_audio(usize::MAX).map_err(|e| e.to_string())?;
        let rms = audio_rms(&samples);
        if rms < min {
            failures.push(format!(
                "audio_rms_min: RMS {rms:.1} < {min} over {} samples",
                samples.len()
            ));
        }
    }
    for (key, assert) in &m.asserts.values {
        match check_value(&mut em, key, assert) {
            Ok(None) => {}
            Ok(Some(msg)) => failures.push(msg),
            Err(e) => failures.push(format!("values.{key}: {e}")),
        }
    }
    for (key, block) in &m.asserts.blocks {
        match check_block(&mut em, key, block) {
            Ok(None) => {}
            Ok(Some(msg)) => failures.push(msg),
            Err(e) => failures.push(format!("blocks.{key}: {e}")),
        }
    }
    for (trace, spec) in &m.asserts.trace {
        match take_trace_count(&mut em, trace) {
            Ok(n) if n >= spec.min => {}
            Ok(n) => failures.push(format!(
                "trace.{trace}: {n} event(s) recorded, expected >= {}",
                spec.min
            )),
            Err(e) => failures.push(format!("trace.{trace}: {e}")),
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

/// Record the current values of every delta key of checkpoint `idx`
/// onward (a key's baseline is wherever it was last snapshotted).
fn snapshot_deltas(
    em: &mut luna_api::Emulator,
    checkpoints: &[Checkpoint],
    idx: usize,
    out: &mut BTreeMap<String, i64>,
) {
    for cp in checkpoints.iter().skip(idx) {
        for (key, d) in &cp.delta {
            if let Ok(v) = read_value(em, key, d.width()) {
                out.insert(key.clone(), v);
            }
        }
    }
}

/// Resolve `key` (loaded symbol or `BANK:OFFSET` hex) to a CPU address.
fn resolve_key(em: &luna_api::Emulator, key: &str) -> Result<u32, String> {
    if let Some(a) = em.resolve_symbol(key) {
        return Ok(a);
    }
    let (bank_s, off_s) = key
        .split_once(':')
        .ok_or_else(|| "not a loaded symbol and not BANK:OFFSET".to_string())?;
    let bank = u8::from_str_radix(bank_s, 16).map_err(|e| format!("bad bank: {e}"))?;
    let off = u16::from_str_radix(off_s, 16).map_err(|e| format!("bad offset: {e}"))?;
    Ok((u32::from(bank) << 16) | u32::from(off))
}

/// Read a 1- or 2-byte little-endian value at `key`.
fn read_value(em: &mut luna_api::Emulator, key: &str, width: u8) -> Result<i64, String> {
    let addr = resolve_key(em, key)?;
    let bytes = em
        .peek_memory((addr >> 16) as u8, addr as u16, u16::from(width.max(1)))
        .map_err(|e| e.to_string())?;
    Ok(match width {
        1 => i64::from(bytes[0]),
        _ => i64::from(u16::from_le_bytes([bytes[0], bytes[1]])),
    })
}

/// Check one value assert. `Ok(Some(msg))` = failed assert.
fn check_value(
    em: &mut luna_api::Emulator,
    key: &str,
    assert: &ValueAssert,
) -> Result<Option<String>, String> {
    // Normalise the bare-integer form to `{eq = N}`.
    let owned;
    let cmp: &CmpSpec = match assert {
        ValueAssert::Exact(v) => {
            owned = CmpSpec {
                eq: Some(*v),
                ..CmpSpec::default()
            };
            &owned
        }
        ValueAssert::Cmp(c) => c,
    };
    let bounds: Vec<i64> = [cmp.eq, cmp.ne, cmp.ge, cmp.gt, cmp.le, cmp.lt]
        .into_iter()
        .flatten()
        .collect();
    if bounds.is_empty() {
        return Err("comparator table needs at least one of eq/ne/ge/gt/le/lt".into());
    }
    if bounds.iter().any(|&b| !(0..=0xFFFF).contains(&b)) {
        return Err("bounds must fit in 16 bits".into());
    }
    let width = match cmp.width {
        Some(w @ (1 | 2)) => w,
        Some(w) => return Err(format!("width must be 1 or 2, got {w}")),
        None => {
            if bounds.iter().all(|&b| b <= 0xFF) {
                1
            } else {
                2
            }
        }
    };
    let got = read_value(em, key, width)?;
    let check = |ok: bool, op: &str, bound: i64| {
        if ok {
            None
        } else {
            Some(format!("values.{key}: {got:#X} violates `{op} {bound:#X}`"))
        }
    };
    let fail = cmp
        .eq
        .and_then(|b| check(got == b, "eq", b))
        .or_else(|| cmp.ne.and_then(|b| check(got != b, "ne", b)))
        .or_else(|| cmp.ge.and_then(|b| check(got >= b, "ge", b)))
        .or_else(|| cmp.gt.and_then(|b| check(got > b, "gt", b)))
        .or_else(|| cmp.le.and_then(|b| check(got <= b, "le", b)))
        .or_else(|| cmp.lt.and_then(|b| check(got < b, "lt", b)));
    Ok(fail)
}

/// Check one `[asserts.blocks]` entry. `Ok(Some(msg))` = mismatch.
fn check_block(
    em: &mut luna_api::Emulator,
    key: &str,
    block: &BlockAssert,
) -> Result<Option<String>, String> {
    let (space, hex) = match block {
        BlockAssert::Hex(hex) => ("cpu", hex.as_str()),
        BlockAssert::Spec { space, hex } => (space.as_str(), hex.as_str()),
    };
    let want = parse_hex_bytes(hex)?;
    if want.is_empty() {
        return Err("empty hex block".into());
    }
    let got: Vec<u8> = match space {
        "cpu" | "wram" => {
            let addr = resolve_key(em, key)?;
            let len = u16::try_from(want.len()).map_err(|_| "block longer than 64 KB")?;
            em.peek_memory((addr >> 16) as u8, addr as u16, len)
                .map_err(|e| e.to_string())?
        }
        "vram" | "aram" => {
            let off =
                u16::from_str_radix(key, 16).map_err(|e| format!("bad {space} offset: {e}"))?;
            let len = u32::try_from(want.len()).unwrap_or(u32::MAX);
            if space == "vram" {
                em.peek_vram(off, len).map_err(|e| e.to_string())?
            } else {
                em.peek_aram(off, len).map_err(|e| e.to_string())?
            }
        }
        "cgram" => {
            let off = usize::from_str_radix(key, 16).map_err(|e| format!("bad offset: {e}"))?;
            let all: Vec<u8> = em
                .peek_cgram()
                .map_err(|e| e.to_string())?
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect();
            slice_at(&all, off, want.len(), "cgram")?
        }
        "oam" => {
            let off = usize::from_str_radix(key, 16).map_err(|e| format!("bad offset: {e}"))?;
            let all = em.peek_oam().map_err(|e| e.to_string())?;
            slice_at(&all, off, want.len(), "oam")?
        }
        other => {
            return Err(format!(
                "unknown space `{other}` (wram, vram, cgram, oam, aram)"
            ));
        }
    };
    if got == want {
        return Ok(None);
    }
    let first = got
        .iter()
        .zip(&want)
        .position(|(g, w)| g != w)
        .unwrap_or_else(|| got.len().min(want.len()));
    Ok(Some(format!(
        "blocks.{key} ({space}): first mismatch at +{first:#X} (expected {:02x}, got {:02x})",
        want.get(first).copied().unwrap_or_default(),
        got.get(first).copied().unwrap_or_default(),
    )))
}

fn slice_at(all: &[u8], off: usize, len: usize, space: &str) -> Result<Vec<u8>, String> {
    all.get(off..off.saturating_add(len))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            format!(
                "range {off:#X}+{len:#X} exceeds {space} ({:#X} bytes)",
                all.len()
            )
        })
}

/// Parse an even-length hex string into bytes.
fn parse_hex_bytes(hex: &str) -> Result<Vec<u8>, String> {
    let hex: String = hex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err("hex block must have an even number of digits".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("bad hex: {e}")))
        .collect()
}

/// Enable the named trace with `cap` events (issue #205).
fn enable_trace(em: &mut luna_api::Emulator, name: &str, cap: usize) -> Result<(), String> {
    match name {
        "dma" => em.enable_dma_trace(cap),
        "dsp" => em.enable_dsp_trace(cap),
        "mailbox" => em.enable_mailbox_log(),
        "sa1" => em.enable_sa1_trace(cap),
        "superfx" => em.enable_superfx_trace(cap),
        "dsp1" => em.enable_dsp1_trace(cap, false),
        "spc" => em.enable_spc_trace(cap),
        other => {
            return Err(format!(
                "unknown trace `{other}` (dma, dsp, mailbox, sa1, superfx, dsp1, spc)"
            ));
        }
    }
    .map_err(|e| e.to_string())
}

/// Drain the named trace and return its event count.
fn take_trace_count(em: &mut luna_api::Emulator, name: &str) -> Result<u64, String> {
    let n = match name {
        "dma" => em.take_dma_trace().map_err(|e| e.to_string())?.len(),
        "dsp" => em.take_dsp_trace().map_err(|e| e.to_string())?.len(),
        "mailbox" => em.take_mailbox_log().map_err(|e| e.to_string())?.len(),
        "sa1" => em.take_sa1_trace().map_err(|e| e.to_string())?.len(),
        "superfx" => em.take_superfx_trace().map_err(|e| e.to_string())?.len(),
        "dsp1" => em.take_dsp1_trace().map_err(|e| e.to_string())?.len(),
        "spc" => em.take_spc_trace().map_err(|e| e.to_string())?.len(),
        other => return Err(format!("unknown trace `{other}`")),
    };
    Ok(n as u64)
}

/// RMS over interleaved stereo samples (both channels pooled).
fn audio_rms(samples: &[(i16, i16)]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .flat_map(|&(l, r)| [f64::from(l), f64::from(r)])
        .map(|s| s * s)
        .sum();
    (sum_sq / (samples.len() as f64 * 2.0)).sqrt()
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
