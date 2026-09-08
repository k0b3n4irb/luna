//! `luna diff a.sfc b.sfc --frames …` (issue #225): per-frame MATCH/DIFF
//! between two ROMs at equal PPU frame, with a tolerance window for the
//! boot-length offset a codegen change introduces.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

/// A `LoROM` whose backdrop colour is a counter bumped every NMI, so every
/// frame looks different and a build whose counter starts `init` frames
/// ahead shows frame `F` of the other build at frame `F - init`.
///
/// ```text
/// SEI ; LDA #$0F ; STA $2100 ; LDA #init ; STA $10 ; LDA #$80 ; STA $4200
/// loop: WAI ; BRA loop
/// nmi:  INC $10 ; STZ $2121 ; LDA $10 ; STA $2122 ; STA $2122 ; RTI
/// ```
fn counter_rom(path: &Path, init: u8) {
    let prog = [
        0x78, 0xA9, 0x0F, 0x8D, 0x00, 0x21, 0xA9, init, 0x85, 0x10, 0xA9, 0x80, 0x8D, 0x00, 0x42,
        0xCB, 0x80, 0xFD, // loop at $800F
        0xE6, 0x10, 0x9C, 0x21, 0x21, 0xA5, 0x10, 0x8D, 0x22, 0x21, 0x8D, 0x22, 0x21, 0x40,
    ];
    let mut rom = vec![0u8; 0x1_0000];
    rom[..prog.len()].copy_from_slice(&prog);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA DIFF FRAMES     ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    // NMI vectors (native + emulation) → the handler at $8012.
    for off in [0x7FEA, 0x7FFA] {
        rom[off] = 0x12;
        rom[off + 1] = 0x80;
    }
    std::fs::write(path, &rom).expect("write rom");
}

fn diff(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(luna_bin())
        .arg("diff")
        .args(args)
        .output()
        .expect("run luna diff");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn identical_roms_match_at_offset_zero_and_report_json() {
    let dir = std::env::temp_dir().join("luna_diff_same");
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("a.sfc");
    counter_rom(&a, 0);
    let report = dir.join("report.json");
    let (code, stdout, stderr) = diff(&[
        a.to_str().unwrap(),
        a.to_str().unwrap(),
        "--force-mapper",
        "lorom",
        "--frames",
        "5,9",
        "--out",
        report.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("frame 5: MATCH (offset +0)"), "{stdout}");
    assert!(stdout.contains("frame 9: MATCH (offset +0)"), "{stdout}");
    assert!(stdout.contains("2 frame(s): 2 match, 0 diff"), "{stdout}");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(json["diff_count"], 0);
    assert_eq!(json["frames"][0]["status"], "match");
    assert_eq!(json["frames"][0]["offset"], 0);
    assert_eq!(json["frames"][1]["frame"], 9);
    assert_eq!(json["frames"][0]["a_hash"], json["frames"][0]["b_hash"]);
}

#[test]
fn shifted_build_diffs_at_zero_tolerance_and_matches_within_one() {
    let dir = std::env::temp_dir().join("luna_diff_shift");
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("a.sfc");
    let b = dir.join("b.sfc");
    counter_rom(&a, 0);
    counter_rom(&b, 1); // one frame "ahead": B's frame F shows A's frame F+1
    let shots = dir.join("shots");
    let _ = std::fs::remove_dir_all(&shots);
    let (code, stdout, _) = diff(&[
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--force-mapper",
        "lorom",
        "--frames",
        "6",
        "--screenshot-dir",
        shots.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "{stdout}");
    assert!(stdout.contains("frame 6: DIFF"), "{stdout}");
    assert!(stdout.contains("1 frame(s): 0 match, 1 diff"), "{stdout}");
    // The DIFF frame's two pictures land on disk.
    assert!(shots.join("frame_6_a.png").is_file());
    assert!(shots.join("frame_6_b.png").is_file());

    let (code, stdout, _) = diff(&[
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--force-mapper",
        "lorom",
        "--frames",
        "6,8",
        "--tolerance",
        "1",
    ]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(stdout.contains("frame 6: MATCH (offset -1)"), "{stdout}");
    assert!(stdout.contains("frame 8: MATCH (offset -1)"), "{stdout}");
}

#[test]
fn missing_frames_is_a_usage_error() {
    let dir = std::env::temp_dir().join("luna_diff_usage");
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("a.sfc");
    counter_rom(&a, 0);
    let (code, _, stderr) = diff(&[a.to_str().unwrap(), a.to_str().unwrap()]);
    assert_eq!(code, Some(2), "{stderr}");
}
