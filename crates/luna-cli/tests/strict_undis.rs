//! R10 / R11 (the INIDISP and SETINI early latch) and R12 (brightness 0 is
//! not black) against the hardware-glitch
//! test ROMs of undisbeliever/snes-test-roms (Zlib), built by
//! `tools/fetch-undisbeliever-roms.sh` into `.luna_tests_undis/` (override:
//! `LUNA_UNDIS_TEST_DIR`). The ROMs are not in the repository. Absent: the
//! tests skip; `LUNA_UNDIS_TEST_REQUIRE=1` (CI) turns a missing ROM into a
//! failure.
//!
//! The expected verdicts come from the author's own descriptions of what each
//! ROM does, not from a reference emulator (neither ares nor Mesen2 models the
//! early latch). `setini-early-read-obj` is driven by HDMA, whose bus writes
//! are not CPU writes: luna's rules see only CPU writes, so it stays silent
//! (a documented limit, asserted here so a change of it is noticed).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

fn rom_dir() -> PathBuf {
    std::env::var_os("LUNA_UNDIS_TEST_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.luna_tests_undis"),
        PathBuf::from,
    )
}

/// The set of rule codes `luna strict` reports on `name`, or `None` (skip)
/// when the ROM is absent and the run is not required.
fn rules(name: &str) -> Option<BTreeSet<String>> {
    let rom = rom_dir().join(name);
    if !rom.is_file() {
        assert!(
            std::env::var_os("LUNA_UNDIS_TEST_REQUIRE").is_none_or(|v| v != "1"),
            "{} is missing: run tools/fetch-undisbeliever-roms.sh",
            rom.display()
        );
        eprintln!(
            "skip: {} absent (tools/fetch-undisbeliever-roms.sh)",
            rom.display()
        );
        return None;
    }
    let o = Command::new(luna_bin())
        .args([
            "strict",
            "--frames",
            "30",
            "--json",
            "--force-mapper",
            "lorom",
        ])
        .arg(&rom)
        .output()
        .expect("run luna");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("json");
    Some(
        v["faults"]
            .as_array()
            .expect("faults array")
            .iter()
            .map(|f| f["rule"].as_str().expect("rule").to_owned())
            .collect(),
    )
}

fn check(name: &str, expected: &[&str]) {
    if let Some(got) = rules(name) {
        let want: BTreeSet<String> = expected.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(got, want, "{name}");
    }
}

/// `stx.w $20ff`: the bus byte before the write to `$2100` is `$8f`.
#[test]
fn hammer_0f8f_raises_exactly_r10() {
    check("inidisp-hammer-0f8f.sfc", &["R10"]);
}

/// `sta.l $802100`: the bank byte `$80` is the last byte on the bus. Upstream
/// names this one the glitching long write; `$0f2100` is the mitigation.
#[test]
fn hammer_0f_long_bank_80_raises_r10() {
    check("inidisp-hammer-0f-long.sfc", &["R10"]);
}

/// The ROMs whose bus byte before the write has bit 7 clear, or whose value
/// has bit 7 set: nothing to report.
#[test]
fn the_unaffected_hammers_raise_nothing() {
    for name in [
        "inidisp-hammer-0f.sfc",
        "inidisp-hammer-long-0f2100.sfc",
        "inidisp-hammer-0f00.sfc",
        "inidisp-hammer-0f0f.sfc",
        "inidisp-hammer-8f0f.sfc",
    ] {
        check(name, &[]);
    }
}

/// The author's own detector ROM; it also reads the joypad registers while
/// the auto-read runs, hence R9.
#[test]
fn d7_glitch_test_raises_r10() {
    if let Some(got) = rules("inidisp-d7-glitch-test.sfc") {
        assert!(got.contains("R10"), "{got:?}");
    }
}

#[test]
fn setini_mode7ex_raises_exactly_r11() {
    check("setini-early-read-mode7ex.sfc", &["R11"]);
}

/// The SETINI-obj test drives the write through HDMA: not a CPU write, so
/// neither rule sees it. A known limit, not a verdict that the ROM is clean.
#[test]
fn setini_obj_hdma_driven_is_beyond_the_cpu_write_rules() {
    check("setini-early-read-obj.sfc", &[]);
}

/// The author's "brightness 0 is not black on a SNES console": a white
/// backdrop and a checkerboard, display on, `INIDISP = 0`, forever. R12 and
/// nothing else.
#[test]
fn brightness_0_raises_exactly_r12() {
    check("inidisp_brightness_0.sfc", &["R12"]);
}
