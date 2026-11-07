//! `--peek OAM:` and `--peek PPU:` read what the PPU holds, not what the
//! game believes it sent: a game that fills its WRAM OAM mirror and never
//! DMAs it leaves a sane mirror and a wrong screen, and only the PPU's own
//! OAM and registers show it (also through `--peek-at` and `luna ticks`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A `LoROM` that writes `$AB` into the WRAM mirror of sprite 0 (`$0200`)
/// without ever DMA-ing OAM, then sets OBSEL `$63`, TM `$11`, INIDISP `$0F`.
fn synthetic_rom(path: &Path) {
    let prog = [
        0xA9, 0xAB, // LDA #$AB
        0x8D, 0x00, 0x02, // STA $0200       the mirror, never sent
        0xA9, 0x63, // LDA #$63
        0x8D, 0x01, 0x21, // STA $2101       OBSEL
        0xA9, 0x11, // LDA #$11
        0x8D, 0x2C, 0x21, // STA $212C       TM
        0xA9, 0x0F, // LDA #$0F
        0x8D, 0x00, 0x21, // STA $2100       INIDISP
        0x80, 0xFE, // BRA -2
    ];
    let mut rom = vec![0u8; 0x1_0000];
    rom[..prog.len()].copy_from_slice(&prog);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA OAM PPU PEEK    ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

fn state(rom: &Path, peeks: &[&str]) -> (Option<i32>, serde_json::Value, String) {
    let mut cmd = Command::new(luna_bin());
    cmd.arg("state")
        .arg(rom)
        .args(["--force-mapper", "lorom", "-n", "2000", "--out", "-"]);
    for p in peeks {
        cmd.args(["--peek", p]);
    }
    let out = cmd.output().expect("run luna state");
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).expect("state JSON"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_hardware_oam_shows_what_the_mirror_hides() {
    let rom = std::env::temp_dir().join("luna_cli_oam_ppu_peeks.sfc");
    synthetic_rom(&rom);
    let (code, json, err) = state(
        &rom,
        &[
            "7E:0200:4",
            "OAM:0:4",
            "oam:21C:#4",
            "PPU:2101:1",
            "PPU:2100:1",
            "PPU:212C:1",
        ],
    );
    assert_eq!(code, Some(0), "stderr: {err}");
    let p = json["peeks"].as_array().expect("peeks");
    // The mirror is sane, the PPU's OAM is not: the DMA never happened.
    assert_eq!(p[0]["bytes_hex"], "ab000000");
    assert_eq!(p[1]["space"], "oam");
    assert_eq!(p[1]["bytes_hex"], "00000000");
    assert_eq!(p[2]["addr"], 0x21C);
    assert_eq!(p[2]["bytes_hex"].as_str().unwrap().len(), 8);
    // The registers, as the game wrote them.
    assert_eq!(p[3]["space"], "ppu");
    assert_eq!(p[3]["addr"], 0x2101);
    assert_eq!(p[3]["bytes_hex"], "63");
    assert_eq!(p[4]["bytes_hex"], "0f");
    assert_eq!(p[5]["bytes_hex"], "11");
}

#[test]
fn a_register_the_ppu_does_not_keep_is_refused() {
    let rom = std::env::temp_dir().join("luna_cli_oam_ppu_peeks_refused.sfc");
    synthetic_rom(&rom);
    let (code, json, _) = state(
        &rom,
        &[
            "PPU:2104:1",
            "PPU:2101:2",
            "PPU:zz:1",
            "OAM:220:1",
            "PPU:2101",
        ],
    );
    assert_eq!(code, Some(1), "unreadable peeks fail the run");
    let p = json["peeks"].as_array().expect("peeks");
    assert!(
        p[0]["error"].as_str().unwrap().contains("$2104"),
        "{}",
        p[0]
    );
    // $2101 is kept but $2102 (OAMADDL) is not: the read says which.
    assert!(p[1]["error"].as_str().unwrap().contains("$2102"));
    assert!(p[2]["error"].is_string());
    assert!(p[3]["error"].as_str().unwrap().contains("544"));
    assert!(
        p[4]["error"]
            .as_str()
            .unwrap()
            .contains("PPU:REGISTER:COUNT")
    );
}
