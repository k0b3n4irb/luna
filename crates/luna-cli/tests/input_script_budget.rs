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

/// A `LoROM` that enables auto-joypad read (`$4200 = $01`) then spins, so
/// `$4218` latches whatever the front-end pushed and `state`'s
/// `cpu_regs.joy1` reports it — the observable an `--input` checkpoint has
/// to move.
fn autojoy_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    // $8000: LDA #$01 ; STA $4200 ; BRA -2
    rom[0x0000..0x0007].copy_from_slice(&[0xA9, 0x01, 0x8D, 0x00, 0x42, 0x80, 0xFE]);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA AUTOJOY         ".as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x07; // size code
    rom[0x7FFC] = 0x00; // reset vector -> $8000
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write autojoy rom");
}

/// Run `luna state` and return the latched joypad-1 word (`$4218/$4219`).
fn joy1_after(rom: &Path, extra: &[&str]) -> u64 {
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
    json["cpu_regs"]["joy1"].as_u64().expect("cpu_regs.joy1")
}

/// `--input` must apply when the run is bounded by `--until-frame`, not
/// only by `-n` (`OpenSNES` report 2026-09-11). The #126 budgeting made the
/// checkpoint chase spend from `-n`, whose `state` default is 1000
/// instructions — exhausted long before the first checkpoint, so every
/// scripted press was silently dropped while the frame-bounded run went
/// on without it.
#[test]
fn input_checkpoints_apply_under_until_frame() {
    let dir = std::env::temp_dir().join("luna-input-budget-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let rom = dir.join("autojoy.smc");
    autojoy_rom(&rom);

    // Held from frame 10, still held at frame 30 (no `-n` given at all).
    assert_eq!(
        joy1_after(&rom, &["--until-frame", "30", "--input", "10:0x8000"]),
        0x8000,
        "the press must reach the ROM under --until-frame"
    );
    // Released at frame 20 → nothing held at frame 30.
    assert_eq!(
        joy1_after(&rom, &["--until-frame", "30", "--input", "10:0x8000,20:0"]),
        0,
        "the release checkpoint must apply too"
    );
    // A checkpoint past the target frame never fires.
    assert_eq!(
        joy1_after(&rom, &["--until-frame", "30", "--input", "90:0x8000"]),
        0,
        "a checkpoint beyond --until-frame must not fire"
    );
    // …and the run still stops exactly on the requested frame.
    assert_eq!(
        frame_count_after(&rom, &["--until-frame", "30", "--input", "10:0x8000"]),
        30
    );

    let _ = std::fs::remove_file(&rom);
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

/// `run --until-frame F` stops at PPU frame F exactly like
/// `state --until-frame F` (issue #222): the displayed-frame hash both
/// print is the same key.
#[test]
fn run_until_frame_matches_state_until_frame() {
    let rom = std::env::temp_dir().join("luna_cli_run_until_frame.sfc");
    synthetic_rom(&rom);
    let fbhash = |sub: &str| -> String {
        let out = Command::new(luna_bin())
            .arg(sub)
            .arg(&rom)
            .args([
                "--force-mapper",
                "lorom",
                "--until-frame",
                "30",
                "--print-fbhash",
            ])
            .output()
            .expect("run luna");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("fbhash="))
            .expect("fbhash line")
            .to_string()
    };
    assert_eq!(fbhash("run"), fbhash("state"));
    assert_eq!(frame_count_after(&rom, &["--until-frame", "30"]), 30);
}

/// Run `luna state` and return the latched joypad-2 word (`$421A/$421B`).
fn joy2_after(rom: &Path, extra: &[&str]) -> u64 {
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
    json["cpu_regs"]["joy2"].as_u64().expect("cpu_regs.joy2")
}

/// `--input2` drives port 2 with the `--input` grammar (`OpenSNES` R-D):
/// its presses land in `$421A`, never in `$4218`, and the two scripts
/// are independent.
#[test]
fn input2_drives_joypad_2_only() {
    let dir = std::env::temp_dir().join("luna-input-budget-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let rom = dir.join("autojoy2.smc");
    autojoy_rom(&rom);

    let both = [
        "--until-frame",
        "30",
        "--input",
        "10:0x0080",
        "--input2",
        "10:0x8000",
    ];
    assert_eq!(
        joy2_after(&rom, &both),
        0x8000,
        "the pad-2 press must reach $421A"
    );
    assert_eq!(
        joy1_after(&rom, &both),
        0x0080,
        "pad 1 keeps its own script"
    );
    let only2 = ["--until-frame", "30", "--input2", "10:0x8000,20:0"];
    assert_eq!(joy1_after(&rom, &only2), 0, "--input2 never touches pad 1");
    assert_eq!(joy2_after(&rom, &only2), 0, "the pad-2 release applies too");

    let _ = std::fs::remove_file(&rom);
}
