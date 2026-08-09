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

/// Event cap for the `[asserts.dma]` trace (issue #212) — hitting it is
/// reported as a failure rather than silently under-counting.
const DMA_TRACE_CAP: usize = 1_000_000;

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
    /// Optional SNES Mouse script (`frame:dx,dy,buttons`, `;`-separated
    /// — the `--mouse` grammar). Plugs a mouse into port 1 (issue #212).
    mouse: Option<String>,
    /// Optional Super Scope script (`frame:x,y,buttons` — the
    /// `--superscope` grammar). Plugs a scope into port 2 (issue #212).
    superscope: Option<String>,
    /// Seed battery SRAM from this `.srm` before the run (issue #212;
    /// relative to the manifest) — the read half of a power-cycle test.
    srm_in: Option<PathBuf>,
    /// Write battery SRAM to this `.srm` after the run (issue #212) —
    /// the write half; a later manifest reloads it via `srm_in`.
    srm_out: Option<PathBuf>,
    /// SKIP (not fail) this test when the named coprocessor firmware
    /// (e.g. `dsp1b.rom`) is absent from luna's firmware folder — CI
    /// machines can't ship Sony blobs (issue #212).
    firmware: Option<String>,
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
    /// This leg's SNES Mouse entries (issue #212).
    mouse: Option<String>,
    /// This leg's Super Scope entries (issue #212).
    superscope: Option<String>,
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
    /// S-DSP register asserts (issue #212): register name (`FLG`,
    /// `EDL`, `V0_VOLL`, …) or raw hex index → the `[asserts.values]`
    /// comparator grammar (registers are bytes; `width` is ignored).
    #[serde(default)]
    dsp: BTreeMap<String, ValueAssert>,
    /// Non-zero-byte floors per space (issue #212): proof an upload
    /// happened without pinning exact bytes.
    #[serde(default)]
    footprint: BTreeMap<String, FootprintAssert>,
    /// DMA-discipline ceilings (issue #212), classified from the DMA
    /// trace luna already records.
    #[serde(default)]
    dma: Option<DmaAssert>,
}

/// A `[asserts.footprint]` entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FootprintAssert {
    /// The space must contain at least this many non-zero bytes.
    nonzero_min: u64,
}

/// The `[asserts.dma]` ceilings — both are maxima, both count only the
/// VRAM data ports (`$2118`/`$2119`), exactly like the `--dma-trace`
/// CSV the probes bucket (issue #217): the trace also records OAM /
/// CGRAM / scroll-register DMA-and-HDMA writes, which are not VRAM
/// bytes and never race the VRAM deadline.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DmaAssert {
    /// Max DMA→VRAM bytes written during active display — outside both
    /// `VBlank` and forced blank (`0` = every VRAM write was
    /// display-safe).
    unsafe_writes: Option<u64>,
    /// Max DMA→VRAM bytes in any single frame's burst window,
    /// **excluding forced-blank bytes**: the ~4 KB budget exists
    /// because of the `VBlank` deadline with the screen on, and forced
    /// blank has no deadline (that is exactly why big boot uploads use
    /// it) — issue #217.
    max_vblank_bytes: Option<u64>,
}

/// One scripted peripheral event (issue #212).
enum InEv {
    /// Joypad-1 mask.
    Pad(u16),
    /// SNES Mouse `dx, dy, buttons` (port 1).
    Mouse(i32, i32, u8),
    /// Super Scope `x, y, buttons` (port 2).
    Scope(i32, i32, u8),
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
        /// Explicit location (issue #210): with it, the TOML key becomes
        /// a free label — so two spaces at the same offset can share a
        /// manifest. Same grammar as a key: symbol / `BANK:OFFSET` for
        /// `wram`, a hex offset for the PPU/APU spaces. Without it, the
        /// key is the location (the v1.15.0 form).
        #[serde(default)]
        offset: Option<String>,
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
    /// `Some(reason)` when the test was skipped (firmware gate, issue
    /// #212) — neither passed nor failed.
    skipped: Option<String>,
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
    let skipped = outcomes.iter().filter(|o| o.skipped.is_some()).count();
    for o in &outcomes {
        if let Some(reason) = &o.skipped {
            println!("SKIP {} ({reason})", o.name);
        } else if o.failures.is_empty() {
            println!("PASS {}", o.name);
        } else {
            println!("FAIL {}", o.name);
            for f in &o.failures {
                println!("     {f}");
            }
        }
    }
    println!(
        "{} passed, {} failed, {skipped} skipped, {} total",
        outcomes.len() - failed.len() - skipped,
        failed.len(),
        outcomes.len()
    );

    if report_json {
        let report = serde_json::json!({
            "passed": outcomes.len() - failed.len() - skipped,
            "failed": failed.len(),
            "skipped": skipped,
            "total": outcomes.len(),
            "tests": outcomes.iter().map(|o| serde_json::json!({
                "name": o.name,
                "manifest": o.path.display().to_string(),
                "passed": o.failures.is_empty() && o.skipped.is_none(),
                "skipped": o.skipped,
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

    // Firmware gate (issue #212): a manifest that needs a coprocessor
    // blob luna's firmware folder doesn't have SKIPs instead of failing
    // — CI machines can't ship Sony firmware.
    if let Some(fw) = &m.firmware {
        let present = luna_api::Emulator::firmware_dir().is_some_and(|d| d.join(fw).is_file());
        if !present {
            return Ok(TestOutcome {
                name,
                path: path.to_path_buf(),
                failures: Vec::new(),
                fbhash: None,
                skipped: Some(format!("firmware `{fw}` not installed")),
            });
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
    // The unified event stream (issue #212): joypad + mouse + scope
    // entries from the top level and every checkpoint, sorted by frame.
    let mut input_entries: Vec<(u64, InEv)> = Vec::new();
    let mut mouse_used = false;
    let mut scope_used = false;
    {
        let mut add_leg = |input: &Option<String>,
                           mouse: &Option<String>,
                           scope: &Option<String>|
         -> Result<(), String> {
            if let Some(spec) = input {
                input_entries.extend(
                    parse_input(spec)?
                        .into_iter()
                        .map(|(f, mask)| (f, InEv::Pad(mask))),
                );
            }
            if let Some(spec) = mouse {
                mouse_used = true;
                input_entries.extend(
                    crate::parsers::parse_mouse_script(spec)
                        .map_err(|e| format!("mouse script: {e}"))?
                        .into_iter()
                        .map(|(f, (dx, dy, b))| (f, InEv::Mouse(dx, dy, b))),
                );
            }
            if let Some(spec) = scope {
                scope_used = true;
                input_entries.extend(
                    crate::parsers::parse_mouse_script(spec)
                        .map_err(|e| format!("superscope script: {e}"))?
                        .into_iter()
                        .map(|(f, (x, y, b))| (f, InEv::Scope(x, y, b))),
                );
            }
            Ok(())
        };
        add_leg(&m.input, &m.mouse, &m.superscope)?;
        for cp in &m.checkpoint {
            add_leg(&cp.input, &cp.mouse, &cp.superscope)?;
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
    // Peripheral devices (issue #212): mouse rides port 1, scope port 2
    // — the CLI --port1/--port2 convention.
    if mouse_used {
        em.set_port_mouse(0, true).map_err(|e| e.to_string())?;
    }
    if scope_used {
        em.set_port_device(1, luna_api::PortDevice::SuperScope)
            .map_err(|e| e.to_string())?;
    }
    // Battery SRAM seed (issue #212) — before any stepping, like --srm-in.
    if let Some(srm) = &m.srm_in {
        let data =
            std::fs::read(dir.join(srm)).map_err(|e| format!("srm_in {}: {e}", srm.display()))?;
        em.load_sram(&data).map_err(|e| e.to_string())?;
    }
    // DMA-discipline ceilings need the DMA trace (issue #212).
    if m.asserts.dma.is_some() {
        em.enable_dma_trace(DMA_TRACE_CAP)
            .map_err(|e| e.to_string())?;
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

    // audio_rms_min needs the WHOLE sample stream: the APU ring holds
    // 512 ms and drops NEW samples when full, so a single end-of-run
    // drain only ever sees the boot silence (issue #211). Drain after
    // every stepping leg instead and pool the stream.
    let want_audio = m.asserts.audio_rms_min.is_some();
    let mut audio_acc: Vec<(i16, i16)> = Vec::new();
    // Advance to `frame`, draining the audio ring often enough that it
    // can never overflow between drains (one frame ≈ 533 samples vs the
    // 16384-sample ring): frame-at-a-time when pooling, one bounded call
    // otherwise (zero overhead for manifests without the assert).
    let advance = |em: &mut luna_api::Emulator,
                   frame: u64,
                   spent: &mut u64,
                   audio_acc: &mut Vec<(i16, i16)>|
     -> Result<(), String> {
        if !want_audio {
            *spent += step_to_frame_bounded(em, frame, total_budget.saturating_sub(*spent));
            return Ok(());
        }
        loop {
            let cur = em.state().scheduler.frame_count;
            audio_acc.extend(em.drain_audio(usize::MAX).map_err(|e| e.to_string())?);
            if cur >= frame || total_budget.saturating_sub(*spent) == 0 {
                return Ok(());
            }
            let stepped = step_to_frame_bounded(em, cur + 1, total_budget.saturating_sub(*spent));
            *spent += stepped;
            if stepped == 0 {
                return Ok(());
            }
        }
    };
    // Apply one scripted event to its device (issue #212).
    let apply_ev = |em: &mut luna_api::Emulator, ev: &InEv| -> Result<(), String> {
        match *ev {
            InEv::Pad(mask) => em.set_joypad(0, mask),
            InEv::Mouse(dx, dy, b) => em.set_mouse(dx, dy, b),
            InEv::Scope(x, y, b) => em.set_superscope(x, y, b),
        }
        .map_err(|e| e.to_string())
    };
    let drive_to = |em: &mut luna_api::Emulator,
                    target_frame: u64,
                    spent: &mut u64,
                    input_iter: &mut std::iter::Peekable<std::slice::Iter<(u64, InEv)>>,
                    audio_acc: &mut Vec<(i16, i16)>|
     -> Result<(), String> {
        while let Some(frame) = input_iter.peek().map(|e| e.0) {
            if frame > target_frame {
                break;
            }
            advance(em, frame, spent, audio_acc)?;
            let (_, ev) = input_iter.next().expect("peeked entry exists");
            apply_ev(em, ev)?;
        }
        advance(em, target_frame, spent, audio_acc)
    };

    // Checkpoints in order, then the final bound.
    for (i, cp) in m.checkpoint.iter().enumerate() {
        drive_to(
            &mut em,
            cp.at_frame,
            &mut spent,
            &mut input_iter,
            &mut audio_acc,
        )?;
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
            drive_to(
                &mut em,
                final_frame,
                &mut spent,
                &mut input_iter,
                &mut audio_acc,
            )?;
        }
        Some(Bound::Steps(s)) => {
            // No checkpoints (validated above): input entries then the rest.
            for (frame, ev) in &input_entries {
                advance(&mut em, *frame, &mut spent, &mut audio_acc)?;
                apply_ev(&mut em, ev)?;
            }
            // Chunked so the 512 ms audio ring can never overflow
            // between drains (issue #211).
            let mut left = s.saturating_sub(spent);
            while left > 0 {
                let chunk = left.min(100_000);
                em.step(chunk).map_err(|e| e.to_string())?;
                left -= chunk;
                if want_audio {
                    audio_acc.extend(em.drain_audio(usize::MAX).map_err(|e| e.to_string())?);
                }
            }
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
        // The stream pooled across the whole run (issue #211) plus
        // whatever the ring still holds.
        audio_acc.extend(em.drain_audio(usize::MAX).map_err(|e| e.to_string())?);
        let rms = audio_rms(&audio_acc);
        if rms < min {
            failures.push(format!(
                "audio_rms_min: RMS {rms:.1} < {min} over {} samples",
                audio_acc.len()
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
    // [asserts.dsp] — the S-DSP register file (issue #212).
    if !m.asserts.dsp.is_empty() {
        let regs = em.dsp_registers().map_err(|e| e.to_string())?;
        for (key, assert) in &m.asserts.dsp {
            let Some(idx) = dsp_register_index(key) else {
                return Err(format!(
                    "asserts.dsp.{key}: unknown S-DSP register (name like FLG/EDL/V0_VOLL, or a hex index < 80)"
                ));
            };
            match normalize_assert(assert) {
                Ok((cmp, _)) => {
                    // Registers are bytes; any explicit width is moot.
                    if let Some(msg) = eval_cmp(
                        &format!("dsp.{key}"),
                        i64::from(regs[usize::from(idx)]),
                        &cmp,
                    ) {
                        failures.push(msg);
                    }
                }
                Err(e) => return Err(format!("asserts.dsp.{key}: {e}")),
            }
        }
    }
    // [asserts.footprint] — non-zero-byte floors per space (issue #212).
    for (space, spec) in &m.asserts.footprint {
        let bytes: Vec<u8> = match space.as_str() {
            "wram" => em.wram_snapshot().map_err(|e| e.to_string())?,
            "vram" => em.peek_vram(0, 0x1_0000).map_err(|e| e.to_string())?,
            "aram" => em.peek_aram(0, 0x1_0000).map_err(|e| e.to_string())?,
            "cgram" => em
                .peek_cgram()
                .map_err(|e| e.to_string())?
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect(),
            "oam" => em.peek_oam().map_err(|e| e.to_string())?,
            other => {
                return Err(format!(
                    "asserts.footprint.{other}: unknown space (wram, vram, cgram, oam, aram)"
                ));
            }
        };
        let nonzero = bytes.iter().filter(|&&b| b != 0).count() as u64;
        if nonzero < spec.nonzero_min {
            failures.push(format!(
                "footprint.{space}: {nonzero} non-zero byte(s), expected >= {}",
                spec.nonzero_min
            ));
        }
    }
    // [asserts.dma] — DMA-discipline ceilings from the trace (issue #212).
    if let Some(dma) = &m.asserts.dma {
        let events = em.take_dma_trace().map_err(|e| e.to_string())?;
        if events.len() >= DMA_TRACE_CAP {
            failures.push(format!(
                "dma: trace hit its {DMA_TRACE_CAP}-event cap — counts would under-report; shorten the run"
            ));
        }
        // Only the VRAM data ports count (issue #217) — the trace also
        // records OAM/CGRAM/register (H)DMA writes, which the probes'
        // CSV bucketing never saw and which don't race the VRAM
        // deadline. A VRAM write is display-safe iff it happened in
        // VBlank OR under forced blank (the DmaTraceEvent
        // classification the Event Viewer already uses).
        let vram_events = || events.iter().filter(|e| matches!(e.b_offset, 0x18 | 0x19));
        let unsafe_events: Vec<_> = vram_events()
            .filter(|e| !(e.blank || e.force_blank))
            .collect();
        let unsafe_writes = unsafe_events.len() as u64;
        if let Some(max) = dma.unsafe_writes
            && unsafe_writes > max
        {
            // Name the first offender so a disagreement with an external
            // bucketing is diagnosable at a glance (issue #217).
            let first = unsafe_events[0];
            failures.push(format!(
                "dma.unsafe_writes: {unsafe_writes} VRAM byte(s) written during active \
                 display, expected <= {max} (first: frame {} line {} ch{} \
                 vram_word ${:04X} src ${:06X})",
                first.frame, first.line, first.channel, first.vram_word, first.src_full
            ));
        }
        if let Some(max) = dma.max_vblank_bytes {
            // Forced-blank uploads have no VBlank deadline — exclude
            // them from the per-frame budget (issue #217).
            let mut per_frame: BTreeMap<u64, u64> = BTreeMap::new();
            for e in vram_events().filter(|e| !e.force_blank) {
                *per_frame.entry(e.frame).or_default() += 1;
            }
            if let Some((frame, &n)) = per_frame.iter().max_by_key(|&(_, n)| *n)
                && n > max
            {
                failures.push(format!(
                    "dma.max_vblank_bytes: frame {frame} transferred {n} \
                     screen-on VRAM byte(s), expected <= {max}"
                ));
            }
        }
    }
    // srm_out — the write half of a power-cycle test (issue #212).
    if let Some(srm) = &m.srm_out {
        let dest = dir.join(srm);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&dest, em.sram()).map_err(|e| format!("srm_out {}: {e}", dest.display()))?;
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
        skipped: None,
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
    Ok(eval_cmp(&format!("values.{key}"), got, cmp))
}

/// Run one comparator table against an already-read value. `Some(msg)`
/// = the first violated bound (shared by `[asserts.values]`,
/// checkpoint values and `[asserts.dsp]` — issue #212).
fn eval_cmp(label: &str, got: i64, cmp: &CmpSpec) -> Option<String> {
    let check = |ok: bool, op: &str, bound: i64| {
        if ok {
            None
        } else {
            Some(format!("{label}: {got:#X} violates `{op} {bound:#X}`"))
        }
    };
    cmp.eq
        .and_then(|b| check(got == b, "eq", b))
        .or_else(|| cmp.ne.and_then(|b| check(got != b, "ne", b)))
        .or_else(|| cmp.ge.and_then(|b| check(got >= b, "ge", b)))
        .or_else(|| cmp.gt.and_then(|b| check(got > b, "gt", b)))
        .or_else(|| cmp.le.and_then(|b| check(got <= b, "le", b)))
        .or_else(|| cmp.lt.and_then(|b| check(got < b, "lt", b)))
}

/// Normalise a [`ValueAssert`] to its comparator table and validate the
/// bounds; returns the effective read width too.
fn normalize_assert(assert: &ValueAssert) -> Result<(CmpSpec, u8), String> {
    let cmp = match assert {
        ValueAssert::Exact(v) => CmpSpec {
            eq: Some(*v),
            ..CmpSpec::default()
        },
        ValueAssert::Cmp(c) => CmpSpec {
            eq: c.eq,
            ne: c.ne,
            ge: c.ge,
            gt: c.gt,
            le: c.le,
            lt: c.lt,
            width: c.width,
        },
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
    Ok((cmp, width))
}

/// The S-DSP register-name vocabulary for `[asserts.dsp]` (issue #212).
/// Raw hex indices (`"7D"`) are also accepted.
fn dsp_register_index(name: &str) -> Option<u8> {
    let global = match name.to_ascii_uppercase().as_str() {
        "MVOL_L" | "MVOLL" => 0x0C,
        "MVOL_R" | "MVOLR" => 0x1C,
        "EVOL_L" | "EVOLL" => 0x2C,
        "EVOL_R" | "EVOLR" => 0x3C,
        "KON" => 0x4C,
        "KOF" | "KOFF" => 0x5C,
        "FLG" => 0x6C,
        "ENDX" => 0x7C,
        "EFB" => 0x0D,
        "PMON" => 0x2D,
        "NON" => 0x3D,
        "EON" => 0x4D,
        "DIR" => 0x5D,
        "ESA" => 0x6D,
        "EDL" => 0x7D,
        _ => 0xFF,
    };
    if global != 0xFF {
        return Some(global);
    }
    let upper = name.to_ascii_uppercase();
    // FIR0..FIR7 at $x0F.
    if let Some(n) = upper.strip_prefix("FIR")
        && let Ok(i) = n.parse::<u8>()
        && i < 8
    {
        return Some((i << 4) | 0x0F);
    }
    // Per-voice: V<n>_<VOLL|VOLR|PITCHL|PITCHH|SRCN|ADSR1|ADSR2|GAIN>.
    if let Some(rest) = upper.strip_prefix('V')
        && let Some((v, reg)) = rest.split_once('_')
        && let Ok(v) = v.parse::<u8>()
        && v < 8
    {
        let lo = match reg {
            "VOLL" => 0x0,
            "VOLR" => 0x1,
            "PITCHL" => 0x2,
            "PITCHH" => 0x3,
            "SRCN" => 0x4,
            "ADSR1" => 0x5,
            "ADSR2" => 0x6,
            "GAIN" => 0x7,
            _ => return None,
        };
        return Some((v << 4) | lo);
    }
    // Raw hex index.
    u8::from_str_radix(name, 16).ok().filter(|&i| i < 0x80)
}

/// Check one `[asserts.blocks]` entry. `Ok(Some(msg))` = mismatch.
fn check_block(
    em: &mut luna_api::Emulator,
    key: &str,
    block: &BlockAssert,
) -> Result<Option<String>, String> {
    // With an explicit `offset` the TOML key is a free label (issue
    // #210); otherwise the key itself is the location (v1.15.0 form).
    let (space, hex, at) = match block {
        BlockAssert::Hex(hex) => ("cpu", hex.as_str(), key),
        BlockAssert::Spec { space, offset, hex } => (
            space.as_str(),
            hex.as_str(),
            offset.as_deref().unwrap_or(key),
        ),
    };
    let want = parse_hex_bytes(hex)?;
    if want.is_empty() {
        return Err("empty hex block".into());
    }
    let got: Vec<u8> = match space {
        "cpu" | "wram" => {
            let addr = resolve_key(em, at)?;
            let len = u16::try_from(want.len()).map_err(|_| "block longer than 64 KB")?;
            em.peek_memory((addr >> 16) as u8, addr as u16, len)
                .map_err(|e| e.to_string())?
        }
        "vram" | "aram" => {
            let off =
                u16::from_str_radix(at, 16).map_err(|e| format!("bad {space} offset: {e}"))?;
            let len = u32::try_from(want.len()).unwrap_or(u32::MAX);
            if space == "vram" {
                em.peek_vram(off, len).map_err(|e| e.to_string())?
            } else {
                em.peek_aram(off, len).map_err(|e| e.to_string())?
            }
        }
        "cgram" => {
            let off = usize::from_str_radix(at, 16).map_err(|e| format!("bad offset: {e}"))?;
            let all: Vec<u8> = em
                .peek_cgram()
                .map_err(|e| e.to_string())?
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect();
            slice_at(&all, off, want.len(), "cgram")?
        }
        "oam" => {
            let off = usize::from_str_radix(at, 16).map_err(|e| format!("bad offset: {e}"))?;
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
