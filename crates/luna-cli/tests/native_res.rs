//! `--native-res` must reach a subcommand's screenshot, not only its
//! hash (`OpenSNES` report, 2026-09-17).
//!
//! `luna state` and `luna diff` already wrote the captured 512×448 frame;
//! `luna run` hashed it natively but saved the averaged 256×224 view, so
//! a human diffing the PNG saw a different frame from the one the
//! harness gated on.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A `LoROM` that turns the screen on and spins, so there is a real
/// picture to capture.
fn synthetic_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    // LDA #$0F ; STA $2100 ; BRA *
    rom[0x0000..0x0007].copy_from_slice(&[0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x80, 0xFE]);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA NATIVE RES      ".as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x07; // size code
    rom[0x7FFC] = 0x00; // reset vector -> $8000
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

/// `(width, height)` from a PNG's IHDR.
fn png_size(path: &Path) -> (u32, u32) {
    let b = std::fs::read(path).expect("screenshot written");
    let n = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    (n(16), n(20))
}

/// Run a subcommand that ends in a screenshot and report its size.
fn shot(cmd: &str, rom: &Path, out: &Path, extra: &[&str]) -> (u32, u32) {
    let mut c = Command::new(luna_bin());
    c.arg(cmd)
        .arg(rom)
        .args(["--force-mapper", "lorom", "--until-frame", "4"])
        .args(extra)
        .arg("--screenshot")
        .arg(out);
    if cmd == "state" {
        c.args(["--out", "-"]);
    }
    let o = c.output().expect("run luna");
    assert!(
        o.status.success(),
        "luna {cmd} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    png_size(out)
}

#[test]
fn native_res_reaches_the_screenshot_of_run_and_state() {
    let dir = std::env::temp_dir().join("luna-native-res-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);

    // The displayed frame, 256×224 — both subcommands, unchanged.
    assert_eq!(shot("run", &rom, &dir.join("run.png"), &[]), (256, 224));
    assert_eq!(shot("state", &rom, &dir.join("state.png"), &[]), (256, 224));

    // With `--native-res`, the captured 512×448 frame — the one
    // `--print-fbhash` hashes. `run` used to write 256×224 here.
    let native = ["--native-res"];
    assert_eq!(
        shot("run", &rom, &dir.join("run_native.png"), &native),
        (512, 448)
    );
    assert_eq!(
        shot("state", &rom, &dir.join("state_native.png"), &native),
        (512, 448)
    );

    // The single-BG debug render has no native form, so `--bg` keeps
    // its 256-wide output rather than failing the run.
    assert_eq!(
        shot(
            "run",
            &rom,
            &dir.join("run_bg.png"),
            &["--native-res", "--bg", "1"]
        ),
        (256, 224)
    );
}
