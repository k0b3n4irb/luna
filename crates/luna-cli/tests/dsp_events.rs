//! `luna state --dsp-events notes.jsonl`: one JSON line per voice keyed on,
//! with the PPU frame number, read from the voice registers at the `KON`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

/// The SPC700 program the ROM uploads through the IPL protocol: voice 3
/// gets SRCN, PITCH, volumes, then a `KON` raises its bit.
const SPC: [u8; 38] = [
    0x8F, 0x34, 0xF2, 0x8F, 0xA5, 0xF3, // SRCN = $A5
    0x8F, 0x32, 0xF2, 0x8F, 0x21, 0xF3, // PITCHL = $21
    0x8F, 0x33, 0xF2, 0x8F, 0xFF, 0xF3, // PITCHH = $FF (14 bits kept)
    0x8F, 0x30, 0xF2, 0x8F, 0x7F, 0xF3, // VOLL = 127
    0x8F, 0x31, 0xF2, 0x8F, 0x80, 0xF3, // VOLR = -128
    0x8F, 0x4C, 0xF2, 0x8F, 0x08, 0xF3, // KON voice 3
    0x2F, 0xFE, // BRA self
];

/// A tiny 65C816 uploader: waits for the IPL, sends `SPC` to `$0200` byte
/// by byte, then asks it to execute.
fn rom(path: &Path) -> u16 {
    let mut prog = vec![
        0xAD,
        0x40,
        0x21,
        0xC9,
        0xAA,
        0xD0,
        0xF9, // wait for $AA
        0xA9,
        0x00,
        0x8D,
        0x42,
        0x21, // dest lo
        0xA9,
        0x02,
        0x8D,
        0x43,
        0x21, // dest hi
        0xA9,
        0x01,
        0x8D,
        0x41,
        0x21, // non-zero = upload
        0xA9,
        0xCC,
        0x8D,
        0x40,
        0x21, // $CC
        0xAD,
        0x40,
        0x21,
        0xC9,
        0xCC,
        0xD0,
        0xF9, // wait for the echo
        0xA2,
        0x00, // LDX #0
        0xBD,
        0x00,
        0x81,
        0x8D,
        0x41,
        0x21, // loop: LDA $8100,X ; STA $2141
        0x8A,
        0x8D,
        0x40,
        0x21, // TXA ; STA $2140
        0xCD,
        0x40,
        0x21,
        0xD0,
        0xFB, // wait for the echo
        0xE8,
        0xE0,
        SPC.len() as u8,
        0xD0,
        0xEC, // INX ; CPX #N ; BNE loop
        0xA9,
        0x00,
        0x8D,
        0x42,
        0x21, // entry lo
        0xA9,
        0x02,
        0x8D,
        0x43,
        0x21, // entry hi
        0x9C,
        0x41,
        0x21, // STZ $2141 : execute
        0x8A,
        0x1A,
        0x8D,
        0x40,
        0x21, // TXA ; INC A ; STA $2140
        0x80,
        0xFE, // BRA self
    ];
    assert!(prog.len() < 0x100);
    let spin = 0x8000 + u16::try_from(prog.len() - 2).unwrap(); // the final BRA self
    prog.resize(0x100, 0);
    prog.extend_from_slice(&SPC);
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA DSP EVENTS      ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    std::fs::write(path, &r).expect("write rom");
    spin
}

#[test]
fn dsp_events_prints_one_json_line_per_key_on() {
    let dir = std::env::temp_dir().join("luna_dsp_events");
    let _ = std::fs::create_dir_all(&dir);
    let rom_path = dir.join("game.sfc");
    let _ = rom(&rom_path);
    let jsonl = dir.join("notes.jsonl");
    let _ = std::fs::remove_file(&jsonl);
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom_path)
        .args([
            "--force-mapper",
            "lorom",
            "--until-frame",
            "10",
            "--out",
            "/dev/null",
        ])
        .arg("--dsp-events")
        .arg(&jsonl)
        .output()
        .expect("run luna state");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&jsonl).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let l = lines[0];
    for field in [
        "\"voice\":3,",
        "\"srcn\":165,",
        "\"pitch\":16161,", // $3F21
        "\"vol_l\":127,",
        "\"vol_r\":-128}",
    ] {
        assert!(l.contains(field), "{field} in {l}");
    }
    assert!(l.starts_with("{\"frame\":"), "{l}");
    assert!(l.contains("\"spc_cycles\":"), "{l}");
}

fn luna_test(dir: &Path, manifest: &str) -> std::process::Output {
    std::fs::write(dir.join("m.toml"), manifest).unwrap();
    Command::new(luna_bin())
        .arg("test")
        .arg("m.toml")
        .current_dir(dir)
        .output()
        .expect("run luna test")
}

/// `[checkpoint.dsp]`: the S-DSP register file at a frame and at a routine,
/// with `[asserts.dsp]`'s names and comparators; a miss is labelled like
/// the other checkpoint tables.
#[test]
fn checkpoint_dsp_reads_registers_at_a_frame_and_at_a_symbol() {
    let dir = std::env::temp_dir().join("luna_dsp_checkpoint");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let spin = rom(&dir.join("game.sfc"));
    std::fs::write(
        dir.join("game.sym"),
        format!("[labels]\n00:{spin:04X} spin\n"),
    )
    .unwrap();
    let head = "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 12\n";

    let ok = luna_test(
        &dir,
        &format!(
            r#"{head}
[[checkpoint]]
at_symbol = "spin"
hit = 100000                      # the spin loop: well after the upload
[checkpoint.dsp]
"31" = 0x80

[[checkpoint]]
at_frame = 8
[checkpoint.dsp]
V3_SRCN = 0xA5
V3_VOLL = {{ ge = 100 }}
V0_SRCN = 0
"#
        ),
    );
    let out = String::from_utf8_lossy(&ok.stdout);
    assert!(
        ok.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let bad = luna_test(
        &dir,
        &format!(
            r#"{head}
[[checkpoint]]
at_symbol = "spin"
hit = 100000
[checkpoint.dsp]
V3_VOLL = {{ le = 5 }}

[[checkpoint]]
at_frame = 8
[checkpoint.dsp]
V3_SRCN = 0x11
"#
        ),
    );
    let out = String::from_utf8_lossy(&bad.stdout);
    assert_eq!(bad.status.code(), Some(1), "{out}");
    assert!(
        out.contains("checkpoint@8 dsp.V3_SRCN: 0xA5 violates `eq 0x11`"),
        "{out}"
    );
    assert!(
        out.contains("checkpoint@spin#100000") && out.contains("dsp.V3_VOLL"),
        "{out}"
    );

    let unknown = luna_test(
        &dir,
        &format!("{head}\n[[checkpoint]]\nat_frame = 8\n[checkpoint.dsp]\nNOPE = 1\n"),
    );
    let out = String::from_utf8_lossy(&unknown.stdout);
    assert_eq!(unknown.status.code(), Some(1), "{out}");
    assert!(out.contains("unknown S-DSP register"), "{out}");
}
