//! `--peek` results are mirrored, machine-readable, into the `--out`
//! JSON (issue #175) — a harness reads them from the same channel as the
//! state instead of regex-parsing the stderr hexdump. The pre-existing
//! top-level keys must stay untouched (the payload flattens
//! `EmulatorState`), which this file pins alongside the new `peeks`
//! array's shape.

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

/// A minimal `LoROM` image that boots and keeps running (see
/// `input_script_budget.rs`).
fn synthetic_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    rom[0x0000] = 0x80; // $8000: BRA -2
    rom[0x0001] = 0xFE;
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA PEEK JSON       ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

#[test]
fn peeks_land_in_the_out_json() {
    let rom = std::env::temp_dir().join("luna_cli_structured_peeks.sfc");
    synthetic_rom(&rom);
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom)
        .args([
            "--force-mapper",
            "lorom",
            "-n",
            "1000",
            "--out",
            "-",
            "--peek",
            "7E:0000:04",
            "--peek",
            "APU:0000:02",
            "--peek",
            "no_such_symbol",
        ])
        .output()
        .expect("run luna state");
    assert!(
        out.status.success(),
        "luna state failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("state JSON");

    // Flatten back-compat: the pre-#175 top-level keys are untouched.
    assert!(json["scheduler"]["frame_count"].is_number());
    assert!(json["cpu"].is_object());

    let peeks = json["peeks"].as_array().expect("peeks array");
    assert_eq!(peeks.len(), 3);

    // CPU-bus peek: resolved address + 4 bytes of hex, no error key.
    assert_eq!(peeks[0]["spec"], "7E:0000:04");
    assert_eq!(peeks[0]["space"], "cpu");
    assert_eq!(peeks[0]["addr"], 0x7E_0000);
    assert_eq!(peeks[0]["bytes_hex"].as_str().unwrap().len(), 8);
    assert!(peeks[0].get("error").is_none());

    // ARAM peek: 16-bit offset space, 2 bytes.
    assert_eq!(peeks[1]["space"], "aram");
    assert_eq!(peeks[1]["addr"], 0);
    assert_eq!(peeks[1]["bytes_hex"].as_str().unwrap().len(), 4);

    // A failed peek is reported, not dropped — harness-friendly.
    assert_eq!(peeks[2]["spec"], "no_such_symbol");
    assert!(peeks[2]["error"].is_string());
    assert_eq!(peeks[2]["bytes_hex"], "");
}

#[test]
fn without_peeks_the_array_is_present_and_empty() {
    let rom = std::env::temp_dir().join("luna_cli_structured_peeks_empty.sfc");
    synthetic_rom(&rom);
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom)
        .args(["--force-mapper", "lorom", "-n", "1000", "--out", "-"])
        .output()
        .expect("run luna state");
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("state JSON");
    assert_eq!(json["peeks"].as_array().map(Vec::len), Some(0));
}
