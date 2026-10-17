//! `luna state --layer-shots`: each layer alone, at the instant of the
//! picture, and where two of them both show a pixel.
//!
//! The ROM draws, in Mode 1, an 8×8 square on BG1 and a 16×8 bar on BG2,
//! both from (0, 0): they overlap on the square.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna_layer_shots_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The two-layer picture, written into `dir`.
fn two_layers_rom(dir: &Path) -> PathBuf {
    let mut code: Vec<u8> = vec![
        0xA9, 0x80, 0x8D, 0x00, 0x21, // forced blank
        0x8D, 0x15, 0x21, // VMAIN: increment after $2119
        0xA9, 0x10, 0x8D, 0x16, 0x21, 0x9C, 0x17, 0x21, // VMADD $0010: tile 1
        0xA2, 0x08, // 8 rows of plane 0 = $FF: colour 1
        0xA9, 0xFF, 0x8D, 0x18, 0x21, 0x9C, 0x19, 0x21, 0xCA, 0xD0, 0xF5,
    ];
    let entry = |hi: u8, count: usize| {
        let mut v = vec![0x9C, 0x16, 0x21, 0xA9, hi, 0x8D, 0x17, 0x21];
        for _ in 0..count {
            v.extend([0xA9, 0x01, 0x8D, 0x18, 0x21, 0x9C, 0x19, 0x21]);
        }
        v
    };
    code.extend(entry(0x04, 1)); // BG1 map at $0400: tile 1
    code.extend(entry(0x08, 2)); // BG2 map at $0800: tiles 1, 1
    code.extend([
        0xA9, 0x04, 0x8D, 0x07, 0x21, // BG1SC
        0xA9, 0x08, 0x8D, 0x08, 0x21, // BG2SC
        0x9C, 0x0B, 0x21, // BG12NBA = 0
        0xA9, 0x01, 0x8D, 0x05, 0x21, // BGMODE 1
        0xA9, 0x01, 0x8D, 0x21, 0x21, 0xA9, 0xFF, 0x8D, 0x22, 0x21, 0xA9, 0x7F, 0x8D, 0x22,
        0x21, // CGRAM 1 = white
        0xA9, 0x03, 0x8D, 0x2C, 0x21, // TM: BG1, BG2
        0xA9, 0x0F, 0x8D, 0x00, 0x21, // display on
        0x80, 0xFE,
    ]);
    let mut r = vec![0u8; 0x8000];
    r[..code.len()].copy_from_slice(&code);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA TWO LAYERS      ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    let rom = dir.join("layers.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    rom
}

#[test]
fn layer_shots_write_each_layer_and_report_overlaps() {
    let dir = fresh_dir("overlaps");
    let rom = two_layers_rom(&dir);
    let shots = dir.join("shots");
    let out = Command::new(luna_bin())
        .args(["state"])
        .arg(&rom)
        .args([
            "--force-mapper",
            "lorom",
            "--until-frame",
            "3",
            "--layers",
            "bg1,bg2,obj",
        ])
        .arg("--layer-shots")
        .arg(&shots)
        .args(["--out", "-"])
        .output()
        .expect("run luna");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("bg1 and bg2: 56 pixel(s) in x 0..=7"),
        "{stderr}"
    );
    for name in ["bg1", "bg2", "obj"] {
        assert!(shots.join(format!("{name}.png")).is_file(), "{name}.png");
    }
    assert!(!shots.join("bg3.png").exists(), "only the layers asked for");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap();
    assert_eq!(
        v["layers"]["written"],
        serde_json::json!(["bg1", "bg2", "obj"])
    );
    assert_eq!(v["layers"]["overlaps"][0]["pixels"], 56);

    let bad = Command::new(luna_bin())
        .args(["state"])
        .arg(&rom)
        .args(["--force-mapper", "lorom", "--layers", "bg5"])
        .arg("--layer-shots")
        .arg(&shots)
        .output()
        .expect("run luna");
    assert_eq!(bad.status.code(), Some(2));
}
