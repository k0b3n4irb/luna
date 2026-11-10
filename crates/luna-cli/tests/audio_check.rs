//! `luna audio-check`: exit codes (0 clean, 1 faults, 2 usage/IO) and the
//! JSON contract, on WAVs built here with injected faults.

// Test signals are written as plain sums of sines, as the maths reads.
#![allow(clippy::suboptimal_flops)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

fn write_wav(path: &Path, frames: &[(i16, i16)]) {
    let rate = luna_api::SAMPLE_RATE_HZ;
    let data = frames.len() as u32 * 4;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for (l, r) in frames {
        b.extend_from_slice(&l.to_le_bytes());
        b.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

fn tone(n: usize) -> Vec<(i16, i16)> {
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(luna_api::SAMPLE_RATE_HZ);
            let v = 4000.0 * (std::f64::consts::TAU * 262.0 * t).sin()
                + 2000.0 * (std::f64::consts::TAU * 392.0 * t).sin();
            (v as i16, v as i16)
        })
        .collect()
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(luna_bin()).args(args).output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn exit_codes_and_json() {
    let dir = std::env::temp_dir().join("luna_audio_check_cli");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clean = dir.join("clean.wav");
    let holed = dir.join("holed.wav");
    let mut v = tone(32_040 * 2);
    write_wav(&clean, &v);
    for s in &mut v[30_000..30_700] {
        *s = (0, 0);
    }
    write_wav(&holed, &v);
    let (c, c_s) = (clean.to_str().unwrap(), holed.to_str().unwrap());

    let (code, out) = run(&["audio-check", c]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("ok:"), "{out}");

    let (code, out) = run(&["audio-check", c_s, "--json", "--region", "pal"]);
    assert_eq!(code, 1, "{out}");
    let j: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(j["schema"], 1);
    assert_eq!(j["verdict"], "faults");
    assert_eq!(j["region"], "pal");
    assert_eq!(j["summary"]["gap"], 1, "{out}");
    let faults = j["faults"].as_array().unwrap();
    assert!(faults.iter().any(|f| f["kind"] == "gap"), "{out}");

    // A kind left out of --fail-on is listed but does not fail.
    let (code, _) = run(&["audio-check", c_s, "--fail-on", "saturation,voice-cut"]);
    assert_eq!(code, 0);
    // A gap threshold turned up past the hole.
    let (code, _) = run(&[
        "audio-check",
        c_s,
        "--fail-on",
        "gap",
        "--gap-min-frames",
        "5",
    ]);
    assert_eq!(code, 0);

    // Usage and I/O errors are 2.
    assert_eq!(run(&["audio-check", "/nonexistent/x.wav"]).0, 2);
    assert_eq!(run(&["audio-check", c, "--region", "secam"]).0, 2);
    assert_eq!(run(&["audio-check", c, "--fail-on", "bogus"]).0, 2);
    assert_eq!(run(&["audio-check", c, "--gap-floor", "abc"]).0, 2);
    let _ = std::fs::remove_dir_all(&dir);
}
