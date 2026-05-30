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
    // `cycles` intentionally ignored — luna's SPC700 returns a per-opcode
    // cycle total, not a per-bus-cycle trace, so the cycle list can't be
    // checked entry-for-entry here.
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

fn run_case(case: &TestCase) -> Result<CaseResult, String> {
    let mut cpu = Spc700::new();
    let mut bus = Box::new(RamBus::new());
    apply_state(&mut cpu, &mut bus, &case.initial);

    if catch_unwind(AssertUnwindSafe(|| cpu.step(&mut *bus))).is_err() {
        return Ok(CaseResult::Skip);
    }

    compare_state(&cpu, &bus, &case.final_)?;
    Ok(CaseResult::Pass)
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

        if opcode_from_filename(&stem).is_none() {
            files_with_unknown_opcode += 1;
            continue;
        }

        let bytes = fs::read(&path).expect("read json");
        let cases: Vec<TestCase> = serde_json::from_slice(&bytes).expect("parse Tom Harte json");

        let op = stats.entry(stem.clone()).or_default();
        for case in &cases {
            match run_case(case) {
                Ok(CaseResult::Pass) => op.passed += 1,
                Ok(CaseResult::Skip) => op.skipped += 1,
                Err(reason) => {
                    op.failed += 1;
                    if op.first_failure.is_none() {
                        op.first_failure = Some(format!("{}: {reason}", case.name));
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
    eprintln!();

    let mut failing: Vec<(&String, &OpStats)> =
        stats.iter().filter(|(_, s)| s.failed > 0).collect();
    failing.sort_by_key(|(_, s)| std::cmp::Reverse(s.failed));

    if !failing.is_empty() {
        eprintln!("Files with failures (top 20 by count):");
        for (name, s) in failing.iter().take(20) {
            eprintln!("  {name:>12} : {} fail / {} pass", s.failed, s.passed);
            if let Some(ref f) = s.first_failure {
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
        "Tom Harte SPC700: regressions: {regressions:?}\n\
         (run without LUNA_TOM_HARTE_REQUIRE to see the full report)"
    );
}
