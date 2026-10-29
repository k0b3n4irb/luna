//! `luna state --voice-wav-dir`: one WAV per S-DSP voice, lined up with
//! the mixed `--audio-out`.

use std::path::PathBuf;
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

/// A 32 KB `LoROM` that spins forever, so frames (and DSP samples) advance.
fn spin_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[..2].copy_from_slice(&[0x80, 0xFE]);
    rom[0x7FFC..0x7FFE].copy_from_slice(&[0x00, 0x80]);
    rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"LUNA VOICE WAVS      ");
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x05;
    rom
}

fn wav_samples(path: &std::path::Path) -> u32 {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&b[..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    let rate = u32::from_le_bytes(b[24..28].try_into().unwrap());
    let channels = u16::from_le_bytes(b[22..24].try_into().unwrap());
    let bits = u16::from_le_bytes(b[34..36].try_into().unwrap());
    assert_eq!(
        (rate, channels, bits),
        (32_000, 2, 16),
        "{}",
        path.display()
    );
    let data = u32::from_le_bytes(b[40..44].try_into().unwrap());
    assert_eq!(b.len() as u32, 44 + data, "{}", path.display());
    data / 4
}

#[test]
fn one_wav_per_voice_each_as_long_as_the_mix() {
    let dir = std::env::temp_dir().join("luna_voice_wavs_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("spin.sfc");
    std::fs::write(&rom, spin_rom()).unwrap();
    let mix = dir.join("mix.wav");
    let voices = dir.join("voices");

    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom)
        .args([
            "--force-mapper",
            "lorom",
            "--until-frame",
            "30",
            "--audio-out",
        ])
        .arg(&mix)
        .arg("--voice-wav-dir")
        .arg(&voices)
        .args(["--out", "/dev/null"])
        .output()
        .expect("run luna");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mixed = wav_samples(&mix);
    assert!(mixed > 10_000, "half a second of audio or more: {mixed}");
    for v in 0..8 {
        let n = wav_samples(&voices.join(format!("voice{v}.wav")));
        assert_eq!(n, mixed, "voice {v} has the mix's length");
    }
    assert!(!voices.join("voice8.wav").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
