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
    assert!(stdout.contains("1 passed, 0 failed, 0 skipped"));
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
    assert!(stdout.contains("1 passed, 0 failed, 0 skipped, 1 total"));
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

[asserts.footprint]
wram = { nonzero_min = 1 }            # the counter made WRAM non-empty
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
fn block_labels_with_explicit_offset_share_a_manifest() {
    // Issue #210: two spaces at the same offset used to collide on the
    // TOML key; an explicit `offset` frees the key into a label.
    let dir = fresh_dir("block_labels");
    synthetic_rom(&dir.join("game.sfc"), &[0xE6, 0x10, 0x80, 0xFC]);
    std::fs::write(
        dir.join("labels.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 2

[asserts.blocks]
vram_zero  = { space = "vram",  offset = "0000", hex = "00000000" }
cgram_zero = { space = "cgram", offset = "0000", hex = "00000000" }
prog       = { space = "wram",  offset = "00:8000", hex = "e610" }
"00:8002"  = "80fc"     # the v1.15.0 key-as-location form still works
"#,
    )
    .unwrap();
    let out = run(&["labels.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A failing labelled block names the label, not the offset.
    std::fs::write(
        dir.join("labels_fail.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
frames = 2

[asserts.blocks]
font = { space = "vram", offset = "0000", hex = "ff" }
"#,
    )
    .unwrap();
    let out = run(&["labels_fail.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout.contains("blocks.font"), "stdout: {stdout}");
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
fn final_capabilities_dsp_footprint_dma_peripherals() {
    let dir = fresh_dir("final_caps");
    // A program that immediately DMAs 16 ROM bytes to VRAM during
    // active display (scanline 0 → unsafe), then spins:
    //   $8000: LDA #$01 ; STA $4300   (mode 1: 2 regs write-twice)
    //          LDA #$18 ; STA $4301   (B-bus $2118 VMDATA)
    //          STZ $4302 ; LDA #$80 ; STA $4303 ; STZ $4304  (A1T=00:8000)
    //          LDA #$10 ; STA $4305 ; STZ $4306              (DAS=16)
    //          LDA #$01 ; STA $420B                          (fire ch0)
    //          BRA *
    synthetic_rom(
        &dir.join("dma.sfc"),
        &[
            // Screen ON first (INIDISP = $0F) — the power-on state is
            // forced blank, under which any DMA is display-safe.
            0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F, STA $2100
            0xA9, 0x01, 0x8D, 0x00, 0x43, // LDA #$01, STA $4300
            0xA9, 0x18, 0x8D, 0x01, 0x43, // LDA #$18, STA $4301
            0x9C, 0x02, 0x43, // STZ $4302
            0xA9, 0x80, 0x8D, 0x03, 0x43, // LDA #$80, STA $4303
            0x9C, 0x04, 0x43, // STZ $4304
            0xA9, 0x10, 0x8D, 0x05, 0x43, // LDA #$10, STA $4305
            0x9C, 0x06, 0x43, // STZ $4306
            0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01, STA $420B
            0x80, 0xFE, // BRA *
        ],
    );
    // DSP regs + footprint + dma ceilings + a mouse/scope script leg.
    std::fs::write(
        dir.join("caps.toml"),
        r#"
rom = "dma.sfc"
force_mapper = "lorom"
frames = 3
mouse = "1:5,5,1"
superscope = "2:100,80,1"

[asserts.dsp]
FLG = { le = 0xFF }           # named register, byte range
"7D" = { ge = 0 }             # raw hex index (EDL)

[asserts.dma]
max_vblank_bytes = 4096
"#,
    )
    .unwrap();
    let out = run(&["caps.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The failing directions: the boot DMA runs during active display,
    // so unsafe_writes = 0 must fail; an absurd footprint floor fails;
    // a wrong DSP exact fails naming the register.
    std::fs::write(
        dir.join("caps_fail.toml"),
        r#"
rom = "dma.sfc"
force_mapper = "lorom"
frames = 3

[asserts.dsp]
FLG = { eq = 0x1FF }

[asserts.footprint]
vram = { nonzero_min = 60000 }

[asserts.dma]
unsafe_writes = 0
"#,
    )
    .unwrap();
    let out = run(&["caps_fail.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}");
    for needle in ["dsp.FLG", "footprint.vram", "dma.unsafe_writes"] {
        assert!(stdout.contains(needle), "missing `{needle}` in: {stdout}");
    }

    // Unknown DSP register / footprint space are usage errors.
    std::fs::write(
        dir.join("caps_bad.toml"),
        "rom = \"dma.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\n[asserts.dsp]\nWARP = 1\n",
    )
    .unwrap();
    assert_eq!(run(&["caps_bad.toml"], &dir).status.code(), Some(2));
}

#[test]
fn dma_classification_matches_the_probe() {
    // Issue #217: [asserts.dma] must bucket exactly like the probes'
    // `--dma-trace` CSV parse — VRAM ports only, forced-blank bytes
    // excluded from the per-frame budget.
    let dir = fresh_dir("dma_probe");

    // 16-byte DMA ch0 ROM→(B-bus port) with the given mode/target; the
    // caller prepends any INIDISP setup.
    let dma_prog = |setup: &[u8], mode: u8, b_port: u8| {
        let mut p = setup.to_vec();
        p.extend_from_slice(&[
            0xA9, mode, 0x8D, 0x00, 0x43, // LDA #mode, STA $4300
            0xA9, b_port, 0x8D, 0x01, 0x43, // LDA #port, STA $4301
            0x9C, 0x02, 0x43, // STZ $4302
            0xA9, 0x80, 0x8D, 0x03, 0x43, // LDA #$80, STA $4303
            0x9C, 0x04, 0x43, // STZ $4304
            0xA9, 0x10, 0x8D, 0x05, 0x43, // LDA #$10, STA $4305
            0x9C, 0x06, 0x43, // STZ $4306
            0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01, STA $420B
            0x80, 0xFE, // BRA *
        ]);
        p
    };

    // 1. Boot upload under power-on forced blank: no VBlank deadline,
    //    so BOTH ceilings hold at 0 (the probe's dynamic_map/tiled case).
    synthetic_rom(&dir.join("fblank.sfc"), &dma_prog(&[], 0x01, 0x18));
    std::fs::write(
        dir.join("a_fblank.toml"),
        "rom = \"fblank.sfc\"\nforce_mapper = \"lorom\"\nframes = 3\n\
         [asserts.dma]\nunsafe_writes = 0\nmax_vblank_bytes = 0\n",
    )
    .unwrap();

    // 2. Screen-on DMA to CGRAM ($2122) during active display: not a
    //    VRAM byte — invisible to both ceilings (the parallax_scroll /
    //    hdma_helpers regression: register (H)DMA is not "unsafe").
    let screen_on: &[u8] = &[0xA9, 0x0F, 0x8D, 0x00, 0x21]; // LDA #$0F, STA $2100
    synthetic_rom(&dir.join("cgram.sfc"), &dma_prog(screen_on, 0x00, 0x22));
    std::fs::write(
        dir.join("b_cgram.toml"),
        "rom = \"cgram.sfc\"\nforce_mapper = \"lorom\"\nframes = 3\n\
         [asserts.dma]\nunsafe_writes = 0\nmax_vblank_bytes = 0\n",
    )
    .unwrap();
    let out = run(&["a_fblank.toml", "b_cgram.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("2 passed, 0 failed"));

    // 3. Screen-on DMA→VRAM still counts against the budget: 16 bytes
    //    with the screen on must break an 8-byte ceiling, and the
    //    unsafe report names the first offending write.
    synthetic_rom(&dir.join("vram.sfc"), &dma_prog(screen_on, 0x01, 0x18));
    std::fs::write(
        dir.join("vram.toml"),
        "rom = \"vram.sfc\"\nforce_mapper = \"lorom\"\nframes = 3\n\
         [asserts.dma]\nunsafe_writes = 0\nmax_vblank_bytes = 8\n",
    )
    .unwrap();
    let out = run(&["vram.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}");
    assert!(stdout.contains("dma.unsafe_writes"), "stdout: {stdout}");
    assert!(stdout.contains("first: frame"), "stdout: {stdout}");
    assert!(stdout.contains("dma.max_vblank_bytes"), "stdout: {stdout}");
}

#[test]
fn oam_asserts_decode_sprites_and_visible_count() {
    // Issue #218: [asserts.oam] — decoded sprite fields + the on-screen
    // count (the oam_struct probe's checks, e.g. simple_sprite's
    // x=112 y=95 tile=16 priority=3 32x32).
    let dir = fresh_dir("oam");
    let mut prog: Vec<u8> = vec![
        // OAMADD = 0, then park all 128 sprites off-screen: 2×256
        // writes of $F0 fill the 512-byte low table (y = 240 >= 224).
        0xA9, 0x00, 0x8D, 0x02, 0x21, // LDA #$00, STA $2102
        0x8D, 0x03, 0x21, // STA $2103
    ];
    for _ in 0..2 {
        prog.extend_from_slice(&[
            0xA2, 0x00, // LDX #$00
            0xA9, 0xF0, // loop: LDA #$F0
            0x8D, 0x04, 0x21, // STA $2104
            0xE8, // INX
            0xD0, 0xF8, // BNE loop (-8)
        ]);
    }
    prog.extend_from_slice(&[
        // Sprite 0: OAMADD = 0; x=112, y=95, tile=16,
        // attr $30 (priority 3, palette 0, no flips, tile.8=0).
        0xA9, 0x00, 0x8D, 0x02, 0x21, // LDA #$00, STA $2102
        0x8D, 0x03, 0x21, // STA $2103
        0xA9, 0x70, 0x8D, 0x04, 0x21, // LDA #112, STA $2104
        0xA9, 0x5F, 0x8D, 0x04, 0x21, // LDA #95,  STA $2104
        0xA9, 0x10, 0x8D, 0x04, 0x21, // LDA #16,  STA $2104
        0xA9, 0x30, 0x8D, 0x04, 0x21, // LDA #$30, STA $2104
        // High table (word $0100): sprite 0 x8=0, size=large; the
        // second byte keeps sprites 4-7 small at x8=0.
        0xA9, 0x00, 0x8D, 0x02, 0x21, // LDA #$00, STA $2102
        0xA9, 0x01, 0x8D, 0x03, 0x21, // LDA #$01, STA $2103
        0xA9, 0x02, 0x8D, 0x04, 0x21, // LDA #$02, STA $2104
        0xA9, 0x00, 0x8D, 0x04, 0x21, // LDA #$00, STA $2104
        // OBSEL size select 1: small 8x8 / large 32x32 → sprite 0 is 32x32.
        0xA9, 0x20, 0x8D, 0x01, 0x21, // LDA #$20, STA $2101
        0x80, 0xFE, // BRA *
    ]);
    synthetic_rom(&dir.join("oam.sfc"), &prog);
    std::fs::write(
        dir.join("oam.toml"),
        r#"
rom = "oam.sfc"
force_mapper = "lorom"
frames = 3

[asserts.oam]
visible = 1

[asserts.oam.sprites.0]
x = 112
y = 95
tile = 16
palette = 0
priority = 3
hflip = false
vflip = false
w = 32
h = 32
"#,
    )
    .unwrap();
    let out = run(&["oam.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Failing direction: wrong tile, a visible floor of 2, a flipped
    // flag — each failure names its field.
    std::fs::write(
        dir.join("oam_fail.toml"),
        r#"
rom = "oam.sfc"
force_mapper = "lorom"
frames = 3

[asserts.oam]
visible = { ge = 2 }

[asserts.oam.sprites.0]
tile = 17
hflip = true
"#,
    )
    .unwrap();
    let out = run(&["oam_fail.toml"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}");
    for needle in ["oam.visible", "oam.sprites.0.tile", "oam.sprites.0.hflip"] {
        assert!(stdout.contains(needle), "missing `{needle}` in: {stdout}");
    }

    // Unknown field / out-of-range index are usage errors (exit 2).
    std::fs::write(
        dir.join("oam_bad.toml"),
        "rom = \"oam.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\n[asserts.oam.sprites.0]\nwarp = 1\n",
    )
    .unwrap();
    assert_eq!(run(&["oam_bad.toml"], &dir).status.code(), Some(2));
    std::fs::write(
        dir.join("oam_idx.toml"),
        "rom = \"oam.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\n[asserts.oam.sprites.128]\nx = 0\n",
    )
    .unwrap();
    assert_eq!(run(&["oam_idx.toml"], &dir).status.code(), Some(2));
}

#[test]
fn sram_round_trip_across_two_manifests() {
    let dir = fresh_dir("sram");
    // LDA #$5A ; STA $70:0000 (long) ; BRA * — writes battery SRAM.
    let mut rom = vec![0u8; 0x1_0000];
    let prog: &[u8] = &[0xA9, 0x5A, 0x8F, 0x00, 0x00, 0x70, 0x80, 0xFE];
    rom[..prog.len()].copy_from_slice(prog);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA SRAM TEST       ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FD8] = 0x03; // 8 KB SRAM
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    std::fs::write(dir.join("game.sfc"), &rom).unwrap();

    // Sorted order runs a_write before b_read (the power-cycle pattern).
    std::fs::write(
        dir.join("a_write.toml"),
        "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nsteps = 100\nsrm_out = \"save.srm\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b_read.toml"),
        r#"
rom = "game.sfc"
force_mapper = "lorom"
steps = 1
srm_in = "save.srm"

[asserts.values]
"70:0000" = 0x5A
"#,
    )
    .unwrap();
    let out = run(&["."], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("PASS a_write") && stdout.contains("PASS b_read"));
    assert_eq!(std::fs::read(dir.join("save.srm")).unwrap()[0], 0x5A);
}

#[test]
fn firmware_gate_skips_when_blob_absent() {
    let dir = fresh_dir("fw_skip");
    synthetic_rom(&dir.join("game.sfc"), &[]);
    std::fs::write(
        dir.join("dsp1.toml"),
        "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\nfirmware = \"definitely_absent_luna_test.rom\"\n",
    )
    .unwrap();
    let out = run(&["dsp1.toml", "--report", "json"], &dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "a skip is not a failure: {stdout}");
    assert!(stdout.contains("SKIP dsp1"), "stdout: {stdout}");
    assert!(stdout.contains("0 passed, 0 failed, 1 skipped, 1 total"));
    let json_start = stdout.find('{').unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    assert_eq!(report["skipped"], 1);
    assert!(report["tests"][0]["skipped"].is_string());
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
