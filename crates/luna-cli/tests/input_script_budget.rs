//! Regression tests for `--input` replay budgeting (issue #126).
//!
//! The defect: chasing a checkpoint's frame stepped the emulator
//! *unbounded*, and only then spent `-n`. So `-n 100000 --input
//! "900:0x8000"` ran to frame 910 instead of frame 12 — 75x longer than
//! asked — and a press scheduled far beyond the requested window still
//! reached the ROM. Reported downstream by `OpenSNES` (pinned on v1.9.0).
//!
//! These drive the built `luna` binary the way a user does, on a
//! synthetic `LoROM` image, so they cover the real CLI plumbing (parse →
//! replay -> JSON) rather than a helper in isolation.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the `luna` binary under test (same profile as the test run).
fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A minimal `LoROM` image that boots and keeps running: the reset vector
/// points at an infinite branch-to-self, so frames advance forever
/// without needing a copyrighted ROM.
fn synthetic_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    // $8000: BRA -2 (an infinite loop) — the CPU spins, the PPU runs.
    rom[0x0000] = 0x80;
    rom[0x0001] = 0xFE;
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA INPUT BUDGET    ".as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x07; // size code
    rom[0x7FFC] = 0x00; // reset vector -> $8000
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

/// Run `luna state` and return the reported PPU frame count.
fn frame_count_after(rom: &Path, extra: &[&str]) -> u64 {
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(rom)
        .args(["--force-mapper", "lorom", "--out", "-"])
        .args(extra)
        .output()
        .expect("run luna state");
    assert!(
        out.status.success(),
        "luna state failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("state JSON on stdout");
    json["scheduler"]["frame_count"]
        .as_u64()
        .expect("scheduler.frame_count")
}

#[test]
fn input_checkpoints_do_not_overrun_the_step_budget() {
    let dir = std::env::temp_dir().join("luna-input-budget-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let rom = dir.join("budget.smc");
    synthetic_rom(&rom);

    // Baseline: how far a plain `-n` run gets.
    let plain = frame_count_after(&rom, &["-n", "100000"]);

    // The same budget with a checkpoint scheduled FAR beyond it. The
    // checkpoint must simply never happen — and crucially the run must
    // not grow to reach it (the bug: this returned ~910).
    let scripted = frame_count_after(&rom, &["-n", "100000", "--input", "900:0x8000,903:0"]);

    assert_eq!(
        plain, scripted,
        "`--input` must not extend the run: -n alone reached frame {plain}, \
         with an out-of-budget checkpoint it reached {scripted}"
    );

    let _ = std::fs::remove_file(&rom);
}

#[test]
fn input_checkpoints_still_fire_when_the_budget_reaches_them() {
    let dir = std::env::temp_dir().join("luna-input-budget-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let rom = dir.join("budget2.smc");
    synthetic_rom(&rom);

    // A budget large enough to cross frame 20, with checkpoints inside
    // it: the run proceeds normally (the guard must not truncate a
    // reachable script).
    let frames = frame_count_after(&rom, &["-n", "3000000", "--input", "20:0x8000,23:0"]);
    assert!(
        frames > 23,
        "a reachable checkpoint must not truncate the run (reached frame {frames})"
    );

    let _ = std::fs::remove_file(&rom);
}
