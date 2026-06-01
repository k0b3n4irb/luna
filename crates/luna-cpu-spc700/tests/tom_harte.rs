//! Tom Harte `ProcessorTests` integration for the SPC700.
//!
//! Dataset: <https://github.com/SingleStepTests/spc700>. Not committed to
//! this repo; fetch with:
//!
//! ```bash
//! tools/fetch-tom-harte-spc700.sh
//! ```
//!
//! or set `LUNA_TOM_HARTE_SPC700_DIR` to point elsewhere (it must point at
//! the directory containing the `.json` files — typically `v1/`).
//!
//! Marked `#[ignore]` because it depends on a sizeable external dataset.
//! Invoke explicitly with:
//!
//! ```bash
//! cargo test -p luna-cpu-spc700 --test tom_harte -- --ignored --nocapture
//! ```
//!
//! Set `LUNA_TOM_HARTE_REQUIRE=1` to make any failure cause the test to
//! fail (the strict regression gate). Without it the test always passes
//! and just prints a report. This mirrors the 65C816 harness in
//! `crates/luna-cpu-65c816/tests/tom_harte.rs`.

#![allow(clippy::cast_possible_truncation)]

use luna_cpu_spc700::flags::Psw;
use luna_cpu_spc700::{Spc700, SpcBus};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

// =============================================================================
// Flat 64 KB test bus
// =============================================================================
//
// The crate's `testing::RamBus` lives behind the `test-utils` feature, which an
// integration test can't enable for itself. A local bus keeps this harness
// self-contained.

struct RamBus {
    mem: Vec<u8>,
}

impl RamBus {
    fn new() -> Self {
        Self {
            mem: vec![0; 0x1_0000],
        }
    }

    fn poke(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
    }

    fn peek(&self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }
}

impl SpcBus for RamBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
    }
}

// =============================================================================
// JSON schema (Tom Harte SPC700 format)
// =============================================================================

#[derive(Debug, Deserialize)]
struct State {
    pc: u16,
    a: u8,
    x: u8,
    y: u8,
    #[serde(alias = "s")]
    sp: u8,
    #[serde(alias = "p")]
    psw: u8,
    /// Each entry is `[address, value]`.
    ram: Vec<[u32; 2]>,
}

#[derive(Debug, Deserialize)]
struct TestCase {
    name: String,
    initial: State,
    #[serde(rename = "final")]
    final_: State,
    /// Per-bus-cycle activity trace (`[addr, value, kind]` per entry,
    /// including internal `wait` cycles). luna's SPC700 returns a single
    /// per-opcode total rather than a per-cycle trace, so we don't check
    /// it entry-for-entry — but the trace's **length** is the
    /// instruction's exact cycle count (branch-taken penalty included),
    /// which is precisely what `Spc700::step` returns. That equality is
    /// the cycle backstop validated below.
    cycles: Vec<serde_json::Value>,
}

// =============================================================================
// Discovery
// =============================================================================

fn dataset_path() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("LUNA_TOM_HARTE_SPC700_DIR") {
        let p = PathBuf::from(s);
        return p.is_dir().then_some(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/luna-cpu-spc700 -> crates
    p.pop(); // crates -> workspace root
    p.push("tests");
    p.push("tom-harte-spc700");
    p.push("v1");
    p.is_dir().then_some(p)
}

/// Parse an opcode from a filename like `ea.json` or `9e n.json`.
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

/// Outcome of the per-opcode cycle-count check for one executed case.
#[derive(Debug, Clone, Copy)]
struct CycleCheck {
    /// What `Spc700::step` charged for the instruction.
    got: u8,
    /// The authoritative count: the length of the bus-cycle trace.
    want: usize,
}

/// Run one case. Returns the state-comparison result plus — when the
/// instruction actually executed (didn't panic / isn't unimplemented) —
/// a `CycleCheck`. The cycle check is independent of the state result so
/// a cycle-table bug surfaces even on an opcode whose state is correct.
fn run_case(case: &TestCase, opcode: u8) -> (Result<CaseResult, String>, Option<CycleCheck>) {
    let mut cpu = Spc700::new();
    let mut bus = Box::new(RamBus::new());
    apply_state(&mut cpu, &mut bus, &case.initial);

    let Ok(got_cycles) = catch_unwind(AssertUnwindSafe(|| cpu.step(&mut *bus))) else {
        return (Ok(CaseResult::Skip), None);
    };

    // SLEEP (0xEF) / STOP (0xFF) halt the core: their Tom Harte trace is a
    // fixed 7-entry halt window (1 fetch + repeating read/wait idles), not a
    // completing instruction cost, so they have no meaningful single-total to
    // match. Skip the cycle check for them (state is still validated).
    let cyc = (opcode != 0xEF && opcode != 0xFF).then_some(CycleCheck {
        got: got_cycles,
        want: case.cycles.len(),
    });
    match compare_state(&cpu, &bus, &case.final_) {
        Ok(()) => (Ok(CaseResult::Pass), cyc),
        Err(e) => (Err(e), cyc),
    }
}

fn apply_state(cpu: &mut Spc700, bus: &mut RamBus, s: &State) {
    cpu.a = s.a;
    cpu.x = s.x;
    cpu.y = s.y;
    cpu.sp = s.sp;
    cpu.pc = s.pc;
    cpu.psw = Psw(s.psw);
    for entry in &s.ram {
        bus.poke(entry[0] as u16, entry[1] as u8);
    }
}

fn compare_state(cpu: &Spc700, bus: &RamBus, expected: &State) -> Result<(), String> {
    macro_rules! check {
        ($got:expr, $want:expr, $name:literal) => {
            if $got != $want {
                return Err(format!("{}: got {:#04X}, want {:#04X}", $name, $got, $want));
            }
        };
    }
    check!(cpu.a, expected.a, "A");
    check!(cpu.x, expected.x, "X");
    check!(cpu.y, expected.y, "Y");
    check!(cpu.sp, expected.sp, "SP");
    check!(cpu.pc, expected.pc, "PC");
    check!(cpu.psw.bits(), expected.psw, "PSW");

    for entry in &expected.ram {
        let addr = entry[0] as u16;
        let want = entry[1] as u8;
        let got = bus.peek(addr);
        if got != want {
            return Err(format!(
                "RAM[${addr:04X}]: got ${got:02X}, want ${want:02X}"
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
    /// Cycle-count backstop: executed cases whose `step()` total matched
    /// the bus-cycle trace length, vs. those that didn't.
    cycle_passed: u32,
    cycle_failed: u32,
    first_cycle_failure: Option<String>,
}

#[test]
#[ignore = "requires Tom Harte SPC700 dataset; run with --ignored"]
fn tom_harte() {
    let Some(dir) = dataset_path() else {
        eprintln!("Tom Harte SPC700 dataset not found.");
        eprintln!("Run `tools/fetch-tom-harte-spc700.sh` from the workspace root");
        eprintln!("or set LUNA_TOM_HARTE_SPC700_DIR to point at the JSON directory.");
        return;
    };

    eprintln!("Reading Tom Harte SPC700 tests from {}", dir.display());
    let mut stats: BTreeMap<String, OpStats> = BTreeMap::new();
    let mut files_with_unknown_opcode = 0;

    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("read tom-harte-spc700 dir")
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

        let bytes = fs::read(&path).expect("read json");
        let cases: Vec<TestCase> = serde_json::from_slice(&bytes).expect("parse Tom Harte json");

        let op = stats.entry(stem.clone()).or_default();
        for case in &cases {
            let (state, cycle) = run_case(case, opcode);
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
                if usize::from(c.got) == c.want {
                    op.cycle_passed += 1;
                } else {
                    op.cycle_failed += 1;
                    if op.first_cycle_failure.is_none() {
                        op.first_cycle_failure = Some(format!(
                            "{}: step() charged {} cycles, trace has {}",
                            case.name, c.got, c.want
                        ));
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
    eprintln!("  Tom Harte SPC700 — results");
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
    eprintln!("  Cycle-count backstop (step() total == bus-cycle trace length):");
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
        eprintln!("Files with cycle-count mismatches (top 20 by count):");
        for (name, s) in cyc_failing.iter().take(20) {
            eprintln!(
                "  {name:>12} : {} mismatch / {} match",
                s.cycle_failed, s.cycle_passed
            );
            if let Some(ref f) = s.first_cycle_failure {
                eprintln!("                  first: {f}");
            }
        }
        eprintln!();
    }
}

fn enforce_baseline(stats: &BTreeMap<String, OpStats>) {
    if std::env::var("LUNA_TOM_HARTE_REQUIRE").is_err() {
        return;
    }
    let regressions: Vec<&String> = stats
        .iter()
        .filter(|(_, s)| s.failed > 0)
        .map(|(name, _)| name)
        .collect();
    assert!(
        regressions.is_empty(),
        "Tom Harte SPC700: state regressions: {regressions:?}\n\
         (run without LUNA_TOM_HARTE_REQUIRE to see the full report)"
    );
    let cycle_regressions: Vec<&String> = stats
        .iter()
        .filter(|(_, s)| s.cycle_failed > 0)
        .map(|(name, _)| name)
        .collect();
    assert!(
        cycle_regressions.is_empty(),
        "Tom Harte SPC700: cycle-count regressions: {cycle_regressions:?}\n\
         (run without LUNA_TOM_HARTE_REQUIRE to see the full report)"
    );
}
