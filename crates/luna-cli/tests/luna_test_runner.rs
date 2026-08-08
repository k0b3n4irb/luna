//! `luna test` end-to-end (issue #181): manifests against the synthetic
//! ROM, pinning the CI exit-code contract (0 pass / 1 fail / 2 usage),
//! `--only`, `--update` golden regeneration, and `--report json`.

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

/// The minimal spinning `LoROM` (see `input_script_budget.rs`), with an
/// optional program patched over the entry point.
fn synthetic_rom(path: &Path, prog: &[u8]) {
    let mut rom = vec![0u8; 0x1_0000];
    rom[0x0000] = 0x80; // $8000: BRA -2
    rom[0x0001] = 0xFE;
    rom[..prog.len()].copy_from_slice(prog);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA TEST RUNNER     ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

fn run(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(luna_bin())
        .arg("test")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run luna test")
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna_test_runner_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn passing_manifest_exits_zero() {
    let dir = fresh_dir("pass");
    synthetic_rom(&dir.join("game.sfc"), &[]);
    std::fs::write(
        dir.join("boot.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 3

[asserts]
wdm_empty = true

[asserts.values]
"7E:0000" = 0
"#,
    )
    .unwrap();
    let out = run(&["boot.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("PASS boot"));
    assert!(stdout.contains("1 passed, 0 failed"));
}

#[test]
fn sdk_channels_and_failures_exit_one() {
    let dir = fresh_dir("channels");
    // Print "OK" over the nocash TTY, fire SNES_ASSERT, then spin.
    synthetic_rom(
        &dir.join("game.sfc"),
        &[
            0xA9, 0x4F, // LDA #'O'
            0x8D, 0xFC, 0x21, // STA $21FC
            0xA9, 0x4B, // LDA #'K'
            0x8D, 0xFC, 0x21, // STA $21FC
            0x42, 0x00, // WDM #$00
            0x80, 0xFE, // BRA *
        ],
    );
    // nocash_contains passes; wdm_empty fails (the WDM fired).
    std::fs::write(
        dir.join("channels.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
steps = 200

[asserts]
wdm_empty = true
nocash_contains = "OK"
"#,
    )
    .unwrap();
    let out = run(&["channels.toml", "--report", "json"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}");
    assert!(stdout.contains("FAIL channels"));
    assert!(stdout.contains("wdm_empty"), "the WDM assert names itself");
    assert!(
        !stdout.contains("nocash_contains:"),
        "the nocash assert passed"
    );
    // The JSON report is on stdout after the human lines.
    let json_start = stdout.find('{').expect("json report");
    let report: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    assert_eq!(report["failed"], 1);
    assert_eq!(report["tests"][0]["passed"], false);
}

#[test]
fn malformed_manifest_exits_two() {
    let dir = fresh_dir("malformed");
    std::fs::write(dir.join("bad.toml"), "frames = 3\n").unwrap(); // no rom
    let out = run(&["bad.toml"], &dir);
    assert_eq!(out.status.code(), Some(2));

    // Both bounds is also a usage error.
    synthetic_rom(&dir.join("game.sfc"), &[]);
    std::fs::write(
        dir.join("bad2.toml"),
        "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\nsteps = 1\n",
    )
    .unwrap();
    let out = run(&["bad2.toml"], &dir);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn only_filters_manifests() {
    let dir = fresh_dir("only");
    synthetic_rom(&dir.join("game.sfc"), &[]);
    for name in ["alpha", "beta"] {
        std::fs::write(
            dir.join(format!("{name}.toml")),
            "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 2\n",
        )
        .unwrap();
    }
    let out = run(&[".", "--only", "alpha"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("PASS alpha"));
    assert!(!stdout.contains("beta"));
    assert!(stdout.contains("1 passed, 0 failed, 1 total"));
}

#[test]
fn update_regenerates_the_fbhash_golden() {
    let dir = fresh_dir("update");
    synthetic_rom(&dir.join("game.sfc"), &[]);
    let manifest = dir.join("golden.toml");
    std::fs::write(
        &manifest,
        r#"# golden comment survives --update
rom = "game.sfc"
force_mapper = "lorom"
frames = 3

[asserts]
fbhash = "0000000000000000"
"#,
    )
    .unwrap();

    // Wrong golden → exit 1.
    let out = run(&["golden.toml"], &dir);
    assert_eq!(out.status.code(), Some(1));

    // --update rewrites it (comments preserved) and exits 0…
    let out = run(&["golden.toml", "--update"], &dir);
    assert!(out.status.success());
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("# golden comment survives --update"));
    assert!(!text.contains("0000000000000000"));

    // …after which a plain run passes.
    let out = run(&["golden.toml"], &dir);
    assert!(out.status.success());
}
