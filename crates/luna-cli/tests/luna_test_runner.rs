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
fn thresholds_blocks_traces_and_audio_asserts() {
    let dir = fresh_dir("asserts_v2");
    // $8000: INC $10 ; BRA -4 — a WRAM counter that climbs forever.
    synthetic_rom(&dir.join("game.sfc"), &[0xE6, 0x10, 0x80, 0xFC]);
    std::fs::write(
        dir.join("v2.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 3

[asserts]
audio_rms_min = 0.0            # trivially satisfiable floor (IPL is silent)

[asserts.values]
"7E:0010" = { ge = 1, width = 1 }     # the counter climbed
"7E:0011" = { le = 0 }                # neighbour untouched
"7E:0012" = { ne = 5, lt = 6 }        # multi-op comparator

[asserts.blocks]
"00:8000" = "e610 80fc"               # the program bytes, via the CPU bus
"0100" = { space = "vram", hex = "00000000" }   # VRAM powers up zeroed

[asserts.trace]
spc = { min = 1 }                     # the SPC700 IPL executed
"#,
    )
    .unwrap();
    let out = run(&["v2.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The failing directions: threshold violated, block mismatch,
    // impossible trace count, unreachable RMS.
    std::fs::write(
        dir.join("v2_fail.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 3

[asserts]
audio_rms_min = 1.0

[asserts.values]
"7E:0010" = { le = 0, width = 1 }

[asserts.blocks]
"00:8000" = "ffffffff"

[asserts.trace]
superfx = { min = 1 }
"#,
    )
    .unwrap();
    let out = run(&["v2_fail.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}");
    for needle in [
        "values.7E:0010",
        "blocks.00:8000",
        "trace.superfx: 0 event(s)",
        "audio_rms_min",
    ] {
        assert!(stdout.contains(needle), "missing `{needle}` in: {stdout}");
    }

    // Vocabulary errors are usage errors (exit 2).
    std::fs::write(
        dir.join("v2_bad.toml"),
        "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\n[asserts.trace]\nwarp = { min = 1 }\n",
    )
    .unwrap();
    let out = run(&["v2_bad.toml"], &dir);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn checkpoints_track_deltas_between_legs() {
    let dir = fresh_dir("checkpoints");
    // The same climbing counter: increases between any two checkpoints.
    synthetic_rom(&dir.join("game.sfc"), &[0xE6, 0x10, 0x80, 0xFC]);
    std::fs::write(
        dir.join("delta.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"

[[checkpoint]]
at_frame = 2
[checkpoint.values]
"7E:0010" = { ge = 1, width = 1 }
[checkpoint.delta]
"7E:0010" = { dir = "increased", width = 1 }
"7E:0020" = "unchanged"

[[checkpoint]]
at_frame = 4
[checkpoint.delta]
"7E:0010" = { dir = "increased", width = 1 }
"7E:0020" = "unchanged"
"#,
    )
    .unwrap();
    let out = run(&["delta.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The wrong direction fails, naming the checkpoint.
    std::fs::write(
        dir.join("delta_fail.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"

[[checkpoint]]
at_frame = 2
[checkpoint.delta]
"7E:0010" = { dir = "decreased", width = 1 }
"#,
    )
    .unwrap();
    let out = run(&["delta_fail.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout.contains("checkpoint@2 delta.7E:0010"),
        "stdout: {stdout}"
    );

    // Non-increasing checkpoints and steps+checkpoints are usage errors.
    std::fs::write(
        dir.join("delta_bad.toml"),
        "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nsteps = 100\n[[checkpoint]]\nat_frame = 2\n",
    )
    .unwrap();
    assert_eq!(run(&["delta_bad.toml"], &dir).status.code(), Some(2));
}

#[test]
fn audio_rms_pools_the_whole_stream_not_just_the_ring() {
    // Issue #211: the APU ring holds 512 ms (16384 samples) and drops
    // NEW samples when full, so a single end-of-run drain only ever saw
    // the boot window. The runner now drains during the run — over a
    // ~2 s bound the pooled count must exceed the ring capacity.
    let dir = fresh_dir("audio_pool");
    synthetic_rom(&dir.join("game.sfc"), &[]);
    std::fs::write(
        dir.join("audio.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 120

[asserts]
audio_rms_min = 99999.0     # unreachable — we want the FAIL line's count
"#,
    )
    .unwrap();
    let out = run(&["audio.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1));
    let count: u64 = stdout
        .split("over ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no sample count in: {stdout}"));
    assert!(
        count > 16384,
        "pooled {count} samples — still capped at the ring size"
    );

    // Same manifest under `steps` exercises the chunked path.
    std::fs::write(
        dir.join("audio_steps.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
steps = 2000000

[asserts]
audio_rms_min = 99999.0
"#,
    )
    .unwrap();
    let out = run(&["audio_steps.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let count: u64 = stdout
        .split("over ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no sample count in: {stdout}"));
    assert!(count > 16384, "steps path pooled only {count} samples");
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
