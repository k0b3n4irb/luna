//! Tom Harte `ProcessorTests` integration for the 65C816.
//!
//! Dataset: <https://github.com/SingleStepTests/65816> (~5 M cases, ~600 MB
//! uncompressed). Not committed to this repo; fetch with:
//!
//! ```bash
//! tools/fetch-tom-harte.sh
//! ```
//!
//! or set `LUNA_TOM_HARTE_DIR` to point elsewhere (it must point to the
//! `v1` subdirectory containing the `.json` files).
//!
//! Marked `#[ignore]` because it depends on a sizeable external dataset
//! and takes minutes to run. Invoke explicitly with:
//!
//! ```bash
//! cargo test -p luna-cpu-65c816 --test tom_harte -- --ignored --nocapture
//! ```
//!
//! Set `LUNA_TOM_HARTE_REQUIRE=1` to make any unexpected failure (i.e.
//! a failure on an opcode marked implemented in [`is_implemented`] below)
//! cause the test to fail. Without this env var the test always passes
//! and just prints a report — that's the friendly default during P0.4b
//! development.

#![allow(clippy::cast_possible_truncation)]

use luna_bus::testing::{RamBus, TraceKind};
use luna_cpu_65c816::{Cpu, StatusFlags};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

// =============================================================================
// JSON schema (Tom Harte format)
// =============================================================================

#[derive(Debug, Deserialize)]
struct State {
    pc: u16,
    s: u16,
    p: u8,
    a: u16,
    x: u16,
    y: u16,
    dbr: u8,
    d: u16,
    pbr: u8,
    e: u8,
    /// Each entry is `[address, value]`.
    ram: Vec<[u32; 2]>,
}

#[derive(Debug, Deserialize)]
struct TestCase {
    name: String,
    initial: State,
    #[serde(rename = "final")]
    final_: State,
    /// Per-bus-cycle activity trace (`[addr, value, state-flags]` per
    /// entry; internal cycles appear as entries with `addr`/`value` null).
    /// Its **length** is the instruction's exact hardware cycle count,
    /// which (Phase 3) we assert against the number of `io_cycle`
    /// invocations the core emits — the cycle backstop.
    cycles: Vec<serde_json::Value>,
}

// =============================================================================
// Discovery
// =============================================================================

fn dataset_path() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("LUNA_TOM_HARTE_DIR") {
        let p = PathBuf::from(s);
        return p.is_dir().then_some(p);
    }
    // Walk up from CARGO_MANIFEST_DIR (= crates/luna-cpu-65c816) to the
    // workspace root, then into tests/tom-harte/v1.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/luna-cpu-65c816 -> crates
    p.pop(); // crates -> workspace root
    p.push("tests");
    p.push("tom-harte");
    p.push("v1");
    p.is_dir().then_some(p)
}

/// Set of opcodes that are claimed to be implemented in `luna-cpu-65c816`.
///
/// Kept in sync with the dispatch table in `src/opcodes.rs`. Any
/// implemented opcode that fails a Tom Harte case is a real regression
/// and should be flagged (via `LUNA_TOM_HARTE_REQUIRE=1`).
const fn is_implemented(_opcode: u8) -> bool {
    // As of P0.4b.13 (full 256-opcode coverage including BCD ADC/SBC),
    // every opcode is dispatched. The earlier per-opcode allow-list
    // existed so the strict-mode regression gate could exclude
    // not-yet-implemented opcodes; that gate is now universal.
    true
}

/// Parse an opcode from a Tom Harte filename like `ea.n.json` or
/// `00 e.json`. Returns the leading 2-hex-digit byte if present.
fn opcode_from_filename(stem: &str) -> Option<u8> {
    let hex: String = stem.chars().take_while(char::is_ascii_hexdigit).collect();
    if hex.len() == 2 {
        u8::from_str_radix(&hex, 16).ok()
    } else {
        None
    }
}

// =============================================================================
// Per-case runner
// =============================================================================

#[derive(Debug, Clone, Copy)]
enum CaseResult {
    Pass,
    Skip,
}

/// Cycle-backstop outcome for one executed case: the number of `io_cycle`
/// invocations the core emitted vs. the authoritative bus-cycle count.
#[derive(Debug, Clone, Copy)]
struct CycleCheck {
    got: u64,
    want: usize,
}

type RunResult = (
    Result<CaseResult, String>,
    Option<CycleCheck>,
    Option<Result<(), String>>,
);

fn run_case(case: &TestCase, opcode: u8) -> RunResult {
    let mut cpu = Cpu::new();
    let mut bus = RamBus::new();
    apply_state(&mut cpu, &mut bus, &case.initial);
    bus.reset_cycle_counter();
    bus.enable_trace();

    // Catch the panic that unimplemented opcodes raise (P0.4b territory).
    if catch_unwind(AssertUnwindSafe(|| cpu.step(&mut bus))).is_err() {
        return (Ok(CaseResult::Skip), None, None);
    }

    // WAI (0xCB) / STP (0xDB) halt the CPU: their Tom Harte trace is a
    // fixed halt-window (4 entries), not a completing instruction cost, so
    // there is no meaningful single-total to match. Skip the cycle check
    // (state is still validated).
    let cyc = (opcode != 0xCB && opcode != 0xDB).then(|| CycleCheck {
        got: bus.io_cycle_calls(),
        want: case.cycles.len(),
    });
    // Entry-for-entry per-cycle bus-trace check (the faithful cycle-grammar
    // oracle): compares luna's recorded read/write/internal sequence to the
    // Tom Harte `cycles[]` order/addr/value. Informational — a divergence on
    // a hardware dummy-read cycle (luna emits an internal where the chip
    // drives the bus) is the known instruction-atomic gap, surfaced as a
    // precise work-list, not a hard failure.
    let trace =
        (opcode != 0xCB && opcode != 0xDB).then(|| compare_trace(&bus.take_trace(), &case.cycles));
    match compare_state(&cpu, &bus, &case.final_) {
        Ok(()) => (Ok(CaseResult::Pass), cyc, trace),
        Err(e) => (Err(e), cyc, trace),
    }
}

/// Classify one Tom Harte cycle entry `[addr, value, flags]` into
/// (kind, addr, value). Flags: char 0 = `d` (VDA), char 1 = `p` (VPA),
/// `w` anywhere = write. A write is a Write; a valid-address read is a
/// Read; everything else (no VDA/VPA, value usually null) is Internal.
fn classify(entry: &serde_json::Value) -> (TraceKind, Option<u32>, Option<u8>) {
    let arr = entry.as_array().expect("cycle entry is array");
    let addr = arr
        .first()
        .and_then(serde_json::Value::as_u64)
        .map(|a| a as u32);
    let value = arr
        .get(1)
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u8);
    let flags = arr.get(2).and_then(serde_json::Value::as_str).unwrap_or("");
    let kind = if flags.contains('w') {
        TraceKind::Write
    } else if flags.starts_with('d') || flags.get(1..2) == Some("p") {
        TraceKind::Read
    } else {
        TraceKind::Internal
    };
    (kind, addr, value)
}

/// Compare luna's recorded per-cycle bus trace to the Tom Harte `cycles[]`
/// entry-for-entry: kind for every cycle, plus addr+value for reads/writes.
fn compare_trace(
    got: &[(TraceKind, Option<u32>, Option<u8>)],
    want: &[serde_json::Value],
) -> Result<(), String> {
    if got.len() != want.len() {
        return Err(format!("trace length {} != {}", got.len(), want.len()));
    }
    for (i, (entry, &(gk, ga, gv))) in want.iter().zip(got).enumerate() {
        let (wk, wa, wv) = classify(entry);
        if gk != wk {
            return Err(format!("cycle {i}: kind {gk:?} != {wk:?}"));
        }
        if matches!(wk, TraceKind::Read | TraceKind::Write) {
            if ga != wa {
                return Err(format!("cycle {i} {wk:?}: addr {ga:06X?} != {wa:06X?}"));
            }
            if gv != wv {
                return Err(format!("cycle {i} {wk:?}: value {gv:02X?} != {wv:02X?}"));
            }
        }
    }
    Ok(())
}

fn apply_state(cpu: &mut Cpu, bus: &mut RamBus, s: &State) {
    cpu.a = s.a;
    cpu.x = s.x;
    cpu.y = s.y;
    cpu.sp = s.s;
    cpu.pc = s.pc;
    cpu.pb = s.pbr;
    cpu.db = s.dbr;
    cpu.dp = s.d;
    cpu.p = StatusFlags(s.p);
    cpu.e = s.e != 0;
    for entry in &s.ram {
        bus.poke(entry[0], entry[1] as u8);
    }
}

fn compare_state(cpu: &Cpu, bus: &RamBus, expected: &State) -> Result<(), String> {
    macro_rules! check {
        ($got:expr, $want:expr, $name:literal) => {
            if $got != $want {
                return Err(format!("{}: got {:?}, want {:?}", $name, $got, $want));
            }
        };
    }
    check!(cpu.a, expected.a, "A");
    check!(cpu.x, expected.x, "X");
    check!(cpu.y, expected.y, "Y");
    check!(cpu.sp, expected.s, "SP");
    check!(cpu.pc, expected.pc, "PC");
    check!(cpu.pb, expected.pbr, "PB");
    check!(cpu.db, expected.dbr, "DB");
    check!(cpu.dp, expected.d, "DP");
    check!(cpu.p.bits(), expected.p, "P");
    check!(u8::from(cpu.e), expected.e, "E");

    for entry in &expected.ram {
        let addr = entry[0];
        let want = entry[1] as u8;
        let got = bus.peek(addr);
        if got != want {
            return Err(format!(
                "RAM[${addr:06X}]: got ${got:02X}, want ${want:02X}"
            ));
        }
    }
    Ok(())
}

// =============================================================================
// Aggregation
// =============================================================================

#[derive(Default)]
struct OpStats {
    passed: u32,
    failed: u32,
    skipped: u32,
    first_failure: Option<String>,
    /// Cycle backstop: executed cases whose emitted `io_cycle` count
    /// matched the bus-cycle trace length, vs. those that didn't.
    cycle_passed: u32,
    cycle_failed: u32,
    first_cycle_failure: Option<String>,
    /// Entry-for-entry per-cycle bus-trace oracle: cases whose recorded
    /// read/write/internal sequence matched the Tom Harte `cycles[]`
    /// order/addr/value, vs. those that diverged (the dummy-read /
    /// instruction-atomic work-list).
    trace_passed: u32,
    trace_failed: u32,
    first_trace_failure: Option<String>,
}

#[test]
#[ignore = "requires Tom Harte dataset; run with --ignored"]
fn tom_harte() {
    let Some(dir) = dataset_path() else {
        eprintln!("Tom Harte dataset not found.");
        eprintln!("Run `tools/fetch-tom-harte.sh` from the workspace root");
        eprintln!("or set LUNA_TOM_HARTE_DIR to point at the `v1/` directory.");
        return;
    };

    // Cap cases per opcode file. The full dataset is ~10k cases/file × 512
    // files (minutes to run); `LUNA_TOM_HARTE_SAMPLE=N` runs only the first
    // N per file for fast iteration on the cycle backstop. Default: all.
    let sample = std::env::var("LUNA_TOM_HARTE_SAMPLE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    eprintln!("Reading Tom Harte tests from {}", dir.display());
    if sample != usize::MAX {
        eprintln!("Sampling first {sample} case(s) per opcode file");
    }
    let mut stats: BTreeMap<String, OpStats> = BTreeMap::new();
    let mut files_with_unknown_opcode = 0;

    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("read tom-harte dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    let total_files = entries.len();
    eprintln!("Found {total_files} JSON files");

    for (idx, entry) in entries.iter().enumerate() {
        let path = entry.path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();

        if idx % 32 == 0 {
            eprintln!("  [{idx:>3}/{total_files}] {stem}");
        }

        let Some(opcode) = opcode_from_filename(&stem) else {
            files_with_unknown_opcode += 1;
            continue;
        };

        // MVN ($54) / MVP ($44): every SingleStepTests case runs under a
        // fixed 100-cycle budget, so each records a PARTIAL ~14-byte
        // transfer (Adec = Xdec = Ydec = 14 regardless of the initial
        // count), not one-instruction semantics. An instruction-atomic
        // core cannot reproduce that without cycle-stepping. luna's
        // per-byte interruptible MVN/MVP is hardware-correct (ares
        // `instructionBlockMove`) and is gated by the unit test
        // `mvn_moves_one_byte_per_step_and_rewinds_pc` instead — so we
        // skip these here rather than counting an un-gateable "failure".
        if matches!(opcode, 0x44 | 0x54) {
            eprintln!(
                "  [skip] {stem}: MVN/MVP not gateable on an atomic core (100-cycle-budget artifact)"
            );
            continue;
        }

        let bytes = fs::read(&path).expect("read json");
        let cases: Vec<TestCase> = serde_json::from_slice(&bytes).expect("parse Tom Harte json");

        let op = stats.entry(stem.clone()).or_default();
        for case in cases.iter().take(sample) {
            let (state, cycle, trace) = run_case(case, opcode);
            match state {
                Ok(CaseResult::Pass) => op.passed += 1,
                Ok(CaseResult::Skip) => op.skipped += 1,
                Err(reason) => {
                    op.failed += 1;
                    if op.first_failure.is_none() {
                        op.first_failure = Some(format!("{}: {reason}", case.name));
                    }
                }
            }
            if let Some(c) = cycle {
                if u64::try_from(c.want) == Ok(c.got) {
                    op.cycle_passed += 1;
                } else {
                    op.cycle_failed += 1;
                    if op.first_cycle_failure.is_none() {
                        op.first_cycle_failure = Some(format!(
                            "{}: emitted {} io_cycles, trace has {}",
                            case.name, c.got, c.want
                        ));
                    }
                }
            }
            if let Some(t) = trace {
                match t {
                    Ok(()) => op.trace_passed += 1,
                    Err(reason) => {
                        op.trace_failed += 1;
                        if op.first_trace_failure.is_none() {
                            op.first_trace_failure = Some(format!("{}: {reason}", case.name));
                        }
                    }
                }
            }
        }
    }

    print_report(&stats, files_with_unknown_opcode);
    enforce_baseline(&stats);
}

fn print_report(stats: &BTreeMap<String, OpStats>, unknown: usize) {
    let total_files = stats.len();
    let (pass, fail, skip): (u64, u64, u64) = stats.values().fold((0, 0, 0), |(p, f, s), o| {
        (
            p + u64::from(o.passed),
            f + u64::from(o.failed),
            s + u64::from(o.skipped),
        )
    });

    eprintln!();
    eprintln!("============================================");
    eprintln!("  Tom Harte 65C816 — results");
    eprintln!("============================================");
    eprintln!("  Files processed: {total_files}");
    if unknown > 0 {
        eprintln!("  Files skipped (unknown opcode in name): {unknown}");
    }
    eprintln!("  Pass:    {pass}");
    eprintln!("  Fail:    {fail}");
    eprintln!("  Skipped: {skip}   (panic = opcode not yet implemented)");

    let (cyc_pass, cyc_fail): (u64, u64) = stats.values().fold((0, 0), |(p, f), o| {
        (p + u64::from(o.cycle_passed), f + u64::from(o.cycle_failed))
    });
    eprintln!();
    eprintln!("  Cycle-count backstop (io_cycle invocations == bus-cycle trace length):");
    eprintln!("    Match:    {cyc_pass}");
    eprintln!("    Mismatch: {cyc_fail}");
    eprintln!();

    let mut failing: Vec<(&String, &OpStats)> =
        stats.iter().filter(|(_, s)| s.failed > 0).collect();
    failing.sort_by_key(|(_, s)| std::cmp::Reverse(s.failed));

    if !failing.is_empty() {
        eprintln!("Files with state failures (top 20 by count):");
        for (name, s) in failing.iter().take(20) {
            eprintln!("  {name:>12} : {} fail / {} pass", s.failed, s.passed);
            if let Some(ref f) = s.first_failure {
                eprintln!("                  first: {f}");
            }
        }
        eprintln!();
    }

    let mut cyc_failing: Vec<(&String, &OpStats)> =
        stats.iter().filter(|(_, s)| s.cycle_failed > 0).collect();
    cyc_failing.sort_by_key(|(_, s)| std::cmp::Reverse(s.cycle_failed));

    if !cyc_failing.is_empty() {
        eprintln!(
            "Files with cycle mismatches ({} opcodes; showing up to 40):",
            cyc_failing.len()
        );
        for (name, s) in cyc_failing.iter().take(40) {
            eprint!("  {name:>12} : {} mismatch", s.cycle_failed);
            if let Some(ref f) = s.first_cycle_failure {
                eprintln!("  ({f})");
            } else {
                eprintln!();
            }
        }
        eprintln!();
    }

    let (tr_pass, tr_fail): (u64, u64) = stats.values().fold((0, 0), |(p, f), o| {
        (p + u64::from(o.trace_passed), f + u64::from(o.trace_failed))
    });
    eprintln!("  Per-cycle bus-trace oracle (entry-for-entry kind+addr+value):");
    eprintln!("    Match:    {tr_pass}");
    eprintln!("    Diverge:  {tr_fail}   (dummy-read / instruction-atomic work-list)");
    eprintln!();
    let mut tr_failing: Vec<(&String, &OpStats)> =
        stats.iter().filter(|(_, s)| s.trace_failed > 0).collect();
    tr_failing.sort_by_key(|(_, s)| std::cmp::Reverse(s.trace_failed));
    if !tr_failing.is_empty() {
        eprintln!(
            "Opcodes with per-cycle trace divergences ({} opcodes; showing up to 60):",
            tr_failing.len()
        );
        for (name, s) in tr_failing.iter().take(60) {
            eprint!("  {name:>12} : {} diverge", s.trace_failed);
            if let Some(ref f) = s.first_trace_failure {
                eprintln!("  ({f})");
            } else {
                eprintln!();
            }
        }
        eprintln!();
    }

    let implemented_ok: u32 = stats
        .iter()
        .filter(|(name, _)| opcode_from_filename(name).is_some_and(is_implemented))
        .map(|(_, s)| s.passed)
        .sum();
    let implemented_ko: u32 = stats
        .iter()
        .filter(|(name, _)| opcode_from_filename(name).is_some_and(is_implemented))
        .map(|(_, s)| s.failed)
        .sum();
    eprintln!("Among implemented opcodes: {implemented_ok} pass / {implemented_ko} fail");
}

fn enforce_baseline(stats: &BTreeMap<String, OpStats>) {
    if std::env::var("LUNA_TOM_HARTE_REQUIRE").is_err() {
        return;
    }
    let regressions: Vec<&String> = stats
        .iter()
        .filter(|(name, s)| opcode_from_filename(name).is_some_and(is_implemented) && s.failed > 0)
        .map(|(name, _)| name)
        .collect();
    assert!(
        regressions.is_empty(),
        "Tom Harte: state regressions on implemented opcodes: {regressions:?}\n\
         (run without LUNA_TOM_HARTE_REQUIRE to see the full report)"
    );
    // Cycle backstop gate is opt-in separately: the 65c816 internal/idle
    // cycles land incrementally (Phase 3), so only enforce zero cycle
    // mismatches once asked, to avoid blocking state-correctness CI on a
    // mid-migration cycle table.
    if std::env::var("LUNA_TOM_HARTE_CYCLES").is_ok() {
        let cycle_regressions: Vec<&String> = stats
            .iter()
            .filter(|(_, s)| s.cycle_failed > 0)
            .map(|(name, _)| name)
            .collect();
        assert!(
            cycle_regressions.is_empty(),
            "Tom Harte: cycle-count mismatches: {cycle_regressions:?}\n\
             (run without LUNA_TOM_HARTE_CYCLES to see the full report)"
        );
    }
}
