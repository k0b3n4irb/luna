//! `luna record`: a run as a video, a text timeline and contact sheets.
//!
//! The video needs an `ffmpeg` no test machine is promised to have, so
//! what is proven here is everything around it: the timeline, the sheets,
//! and what the command says when there is no encoder to run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A `LoROM` whose backdrop takes a new colour at every pass of its loop
/// until pad 1 holds B, then keeps the one it has.
fn synthetic_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    let code: &[u8] = &[
        0xA9, 0x0F, 0x8D, 0x00, 0x21, // 8000 LDA #$0F : STA $2100  screen on
        0xA9, 0x01, 0x8D, 0x00, 0x42, // 8005 LDA #$01 : STA $4200  pad auto-read
        // $4219 is a partial word while the auto-read runs (it starts a few
        // dots after V-blank): read it only between two checks that busy
        // is clear.
        0xAD, 0x12, 0x42, // 800A LDA $4212
        0x4A, // 800D LSR A
        0xB0, 0xFA, // 800E BCS $800A             busy: wait
        0xAD, 0x19, 0x42, // 8010 LDA $4219
        0x85, 0x11, // 8013 STA $11
        0xAD, 0x12, 0x42, // 8015 LDA $4212
        0x4A, // 8018 LSR A
        0xB0, 0xEF, // 8019 BCS $800A             it started meanwhile
        0xA5, 0x11, // 801B LDA $11
        0x30, 0xEB, // 801D BMI $800A               B held: wait
        0x9C, 0x21, 0x21, // 801F STZ $2121
        0xA5, 0x10, // 8022 LDA $10
        0x8D, 0x22, 0x21, // 8024 STA $2122
        0x9C, 0x22, 0x21, // 8027 STZ $2122
        0xE6, 0x10, // 802A INC $10
        0x80, 0xDC, // 802C BRA $800A
    ];
    rom[..code.len()].copy_from_slice(code);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA RECORD          ".as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x07; // size code
    rom[0x7FFC] = 0x00; // reset vector -> $8000
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

fn record(rom: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .arg("record")
        .arg(rom)
        .args(["--force-mapper", "lorom"])
        .args(args)
        .output()
        .expect("run luna")
}

/// `(width, height)` from a PNG's IHDR.
fn png_size(path: &Path) -> (u32, u32) {
    let b = std::fs::read(path).expect("sheet written");
    let n = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    (n(16), n(20))
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-record-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn the_timeline_and_the_sheets_need_no_encoder() {
    let dir = fresh_dir("no-video");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let base = dir.join("demo");
    let out = record(
        &rom,
        &[
            "--no-video",
            "--from-frame",
            "10",
            "--count",
            "100",
            "--sheet-every",
            "10",
            "--input",
            "40:0x8000",
            "-o",
            base.to_str().unwrap(),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("recorded frames 10..109 (100 frames"),
        "{stdout}"
    );
    assert!(!dir.join("demo.mp4").exists());

    // Ten pictures, six per row: two rows of 256x(224 + 10).
    assert_eq!(png_size(&dir.join("demo-1.png")), (1536, 468));
    assert!(!dir.join("demo-2.png").exists());

    let md = std::fs::read_to_string(dir.join("demo.md")).unwrap();
    assert!(md.starts_with("# game.sfc — frames 10 to 109\n"), "{md}");
    assert!(md.contains("Input script, pad 1: `40:0x8000`"), "{md}");
    assert!(
        md.contains("| `demo-1.png` | one picture every 10 frames"),
        "{md}"
    );
    // The backdrop moves until B is held from the script's frame 40, the
    // first picture drawn with it is 41, and the picture then holds.
    assert!(md.contains("| 10 | 0.00 | moving, "), "{md}");
    assert!(md.contains("| 41 | 0.52 | pad 1: B held |"), "{md}");
    assert!(md.contains(" | still picture, "), "{md}");
    assert!(md.contains("| 0 | 10 | silent | — |"), "{md}");
}

#[test]
fn a_missing_encoder_is_said_before_the_run() {
    let dir = fresh_dir("no-ffmpeg");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let base = dir.join("demo");
    let out = record(
        &rom,
        &[
            "--ffmpeg",
            "/nonexistent/ffmpeg",
            "-o",
            base.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("needs ffmpeg to write the video"),
        "{stderr}"
    );
    assert!(stderr.contains("--no-video"), "{stderr}");
    assert!(!dir.join("demo.md").exists());
}

#[test]
fn a_count_of_zero_is_a_usage_error() {
    let dir = fresh_dir("zero");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let base = dir.join("demo");
    let out = record(
        &rom,
        &["--no-video", "--count", "0", "-o", base.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(2));
}

/// The piloting options of `state`: a second pad, bytes written at an
/// arrival on a routine, and scripts numbered by arrivals.
#[test]
fn a_record_is_driven_like_a_state_run() {
    let dir = fresh_dir("driven");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let base = dir.join("demo");
    let out = record(
        &rom,
        &[
            "--no-video",
            "--count",
            "60",
            "--input2",
            "20:0x0080",
            "--poke-at",
            "00:800A",
            "--poke-hit",
            "3",
            "--poke",
            "7E:0010=40",
            "-o",
            base.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("poked 1 value(s) at 00:800A hit 3"),
        "{stderr}"
    );
    let md = std::fs::read_to_string(dir.join("demo.md")).unwrap();
    assert!(md.contains("Input script, pad 2: `20:0x0080`"), "{md}");
    assert!(
        md.contains("Written at arrival 3 on `00:800A`: `7E:0010=40`"),
        "{md}"
    );
    assert!(md.contains("| 21 | 0.33 | pad 2: A held |"), "{md}");

    // Numbered by arrivals on the loop, which turns many times a frame,
    // the fiftieth comes within the first frames; by frame it would be 51.
    let out = record(
        &rom,
        &[
            "--no-video",
            "--count",
            "60",
            "--input",
            "50:0x8000",
            "--input-at",
            "00:800A",
            "-o",
            base.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = std::fs::read_to_string(dir.join("demo.md")).unwrap();
    assert!(md.contains("numbered by arrivals on `00:800A`"), "{md}");
    let row = md
        .lines()
        .find(|l| l.ends_with("| pad 1: B held |"))
        .expect(&md);
    let frame: u64 = row.split('|').nth(1).unwrap().trim().parse().unwrap();
    assert!(frame <= 3, "{row}");
}

#[test]
fn a_poke_the_run_never_reaches_fails_the_record() {
    let dir = fresh_dir("missed");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let base = dir.join("demo");
    let out = record(
        &rom,
        &[
            "--no-video",
            "--count",
            "10",
            "--poke-at",
            "00:9000",
            "--poke",
            "7E:0010=40",
            "-o",
            base.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--poke-at 00:9000: hit 1 not reached"),
        "{stderr}"
    );
}

#[test]
fn raw_writes_the_bare_picture_and_the_sound_apart() {
    let dir = fresh_dir("raw");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let video = dir.join("frames.rgb");
    let sound = dir.join("sound.pcm");
    let out = record(
        &rom,
        &[
            "--raw",
            video.to_str().unwrap(),
            "--raw-audio",
            sound.to_str().unwrap(),
            "--count",
            "5",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    let head = err.lines().next().expect("a header line");
    assert!(head.contains("\"width\":512"), "{head}");
    assert!(head.contains("\"pixel_format\":\"rgb24\""), "{head}");
    let height: usize = head
        .split("\"height\":")
        .nth(1)
        .and_then(|s| s.split([',', '}']).next())
        .and_then(|s| s.parse().ok())
        .expect("a height");
    let bytes = std::fs::metadata(&video).expect("stream written").len() as usize;
    assert_eq!(
        bytes,
        5 * 512 * height * 3,
        "five whole pictures, nothing else"
    );
    assert!(err.contains("\"frames\":5"), "{err}");
    assert!(std::fs::metadata(&sound).expect("pcm written").len() > 0);
    // Nothing but the stream and the sound: no strip, timeline or sheets.
    let others: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(others.len(), 3, "{others:?}");
}

#[test]
fn raw_to_stdout_keeps_the_stream_clean() {
    let dir = fresh_dir("raw-stdout");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let out = record(&rom, &["--raw", "-", "--count", "3"]);
    assert!(out.status.success());
    assert!(!out.stdout.is_empty() && out.stdout.len().is_multiple_of(512 * 3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("\"sound_file\":null"));
}

#[test]
fn raw_refuses_the_options_of_the_video() {
    let dir = fresh_dir("raw-conflict");
    let rom = dir.join("game.sfc");
    synthetic_rom(&rom);
    let out = record(&rom, &["--raw", "-", "--no-video"]);
    assert_eq!(out.status.code(), Some(2));
}
