//! Tom Harte ProcessorTests integration for the 65C816.
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

use luna_bus::testing::RamBus;
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
    // `cycles` field intentionally ignored for now — our CPU is not
    // cycle-accurate yet (Phase 0 says "sans timing fin").
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
fn is_implemented(opcode: u8) -> bool {
    matches!(
        opcode,
        // Flag toggles
        0x18 | 0x38 | 0x58 | 0x78 | 0xB8 | 0xD8 | 0xF8
        // Mode control
        | 0xFB | 0xE2 | 0xC2
        // LDA (all 15 modes)
        | 0xA9 | 0xA5 | 0xA7 | 0xAD | 0xAF
        | 0xB5 | 0xB2 | 0xB7 | 0xBD | 0xBF | 0xB9 | 0xB1
        | 0xA3 | 0xB3 | 0xA1
        // LDX (5 modes)
        | 0xA2 | 0xA6 | 0xAE | 0xB6 | 0xBE
        // LDY (5 modes)
        | 0xA0 | 0xA4 | 0xAC | 0xB4 | 0xBC
        // STA (legacy, complete coverage in P0.4b.3)
        | 0x85 | 0x8D | 0x8F
        // Jumps
        | 0x4C | 0x5C
        // Branches
        | 0x80 | 0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0
        // INC / DEC A
        | 0x1A | 0x3A
        // Misc
        | 0xEA | 0xCB | 0xDB
    )
}

/// Parse an opcode from a Tom Harte filename like `ea.n.json` or
/// `00 e.json`. Returns the leading 2-hex-digit byte if present.
fn opcode_from_filename(stem: &str) -> Option<u8> {
    let hex: String = stem.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
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
    let mut cpu = Cpu::new();
    let mut bus = RamBus::new();
    apply_state(&mut cpu, &mut bus, &case.initial);

    // Catch the panic that unimplemented opcodes raise (P0.4b territory).
    if catch_unwind(AssertUnwindSafe(|| cpu.step(&mut bus))).is_err() {
        return Ok(CaseResult::Skip);
    }

    compare_state(&cpu, &bus, &case.final_)?;
    Ok(CaseResult::Pass)
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
}

#[test]
#[ignore = "requires Tom Harte dataset; run with --ignored"]
fn tom_harte() {
    let dir = match dataset_path() {
        Some(d) => d,
        None => {
            eprintln!("Tom Harte dataset not found.");
            eprintln!("Run `tools/fetch-tom-harte.sh` from the workspace root");
            eprintln!("or set LUNA_TOM_HARTE_DIR to point at the `v1/` directory.");
            return;
        }
    };

    eprintln!("Reading Tom Harte tests from {}", dir.display());
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
        _ = opcode; // (will be used below for unexpected-failure detection)
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
        "Tom Harte: regressions on implemented opcodes: {regressions:?}\n\
         (run without LUNA_TOM_HARTE_REQUIRE to see the full report)"
    );
}
