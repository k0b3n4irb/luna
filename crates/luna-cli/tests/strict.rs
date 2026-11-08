//! `luna strict` and `luna run --strict-hw` end to end, on synthetic ROMs
//! built here: one injected fault, one healthy ROM, and the proof that the
//! journal does not change the picture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A `LoROM` whose code at `$8000` is `code`.
fn write_rom(path: &Path, code: &[u8]) {
    let mut rom = vec![0u8; 0x1_0000];
    rom[..code.len()].copy_from_slice(code);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA STRICT CLI      ".as_ref());
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write synthetic rom");
}

fn luna(args: &[&str], rom: &Path) -> Output {
    Command::new(luna_bin())
        .args(args)
        .arg(rom)
        .args(["--force-mapper", "lorom"])
        .output()
        .expect("run luna")
}

fn setup(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-strict-cli-{name}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Screen on, then `STA $2118` forever: R1 on every picture line.
const FAULTY: [u8; 10] = [0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x8D, 0x18, 0x21, 0x80, 0xFB];
/// Forced blank, then the same write: nothing to report.
const HEALTHY: [u8; 10] = [0xA9, 0x80, 0x8D, 0x00, 0x21, 0x8D, 0x18, 0x21, 0x80, 0xFB];

#[test]
fn strict_reports_the_injected_fault_and_exits_one() {
    let dir = setup("fault");
    let rom = dir.join("faulty.sfc");
    write_rom(&rom, &FAULTY);
    let o = luna(&["strict", "--frames", "4"], &rom);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("R1 pc=$00:8005 $2118 cpu x"), "{out}");
    assert!(out.contains("strict: 1 fault site(s)"), "{out}");

    let o = luna(&["strict", "--frames", "4", "--json"], &rom);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["faults"][0]["rule"], "R1");
    assert_eq!(v["faults"][0]["pc"], "$00:8005");
}

#[test]
fn strict_is_silent_on_a_healthy_rom_and_exits_zero() {
    let dir = setup("healthy");
    let rom = dir.join("healthy.sfc");
    write_rom(&rom, &HEALTHY);
    let o = luna(&["strict", "--frames", "4"], &rom);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "strict: no hardware fault in 4 frame(s)"
    );
}

#[test]
fn strict_exits_two_when_the_rom_is_unreadable() {
    let o = Command::new(luna_bin())
        .args(["strict", "/nonexistent/luna-strict.sfc"])
        .output()
        .expect("run luna");
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn run_strict_hw_prints_the_journal_without_changing_the_picture() {
    let dir = setup("run");
    let rom = dir.join("faulty.sfc");
    write_rom(&rom, &FAULTY);
    let fbhash = |o: &Output| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.starts_with("fbhash="))
            .map(str::to_owned)
            .expect("fbhash line")
    };
    let common = ["run", "--until-frame", "4", "--print-fbhash"];
    let plain = luna(&common, &rom);
    assert_eq!(plain.status.code(), Some(0));
    let mut args = common.to_vec();
    args.push("--strict-hw");
    let strict = luna(&args, &rom);
    assert_eq!(strict.status.code(), Some(1), "a fault exits 1");
    assert_eq!(fbhash(&plain), fbhash(&strict), "the picture is identical");
    assert!(String::from_utf8_lossy(&strict.stdout).contains("R1 pc=$00:8005"));
}

/// Forced blank, wait for `$10` to turn non-zero, then screen on and
/// `STA $2118` forever: the fault is reached only if something writes `$10`.
const WAITS_FOR_A_POKE: [u8; 19] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // forced blank
    0xA5, 0x10, 0xF0, 0xFC, // LDA $10 ; BEQ back
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // screen on
    0x8D, 0x18, 0x21, 0x80, 0xFB, // STA $2118 forever
];

#[test]
fn strict_plays_a_poke_to_reach_what_the_idle_run_never_does() {
    let dir = setup("poke");
    let rom = dir.join("waits.sfc");
    write_rom(&rom, &WAITS_FOR_A_POKE);
    let o = luna(&["strict", "--frames", "4"], &rom);
    assert_eq!(o.status.code(), Some(0), "idle run is clean");
    let o = luna(
        &[
            "strict",
            "--frames",
            "4",
            "--poke-at",
            "00:8005",
            "--poke",
            "7e:0010=01",
        ],
        &rom,
    );
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("R1 pc=$00:800E $2118"), "{out}");
}

#[test]
fn strict_refuses_a_poke_the_run_never_reaches() {
    let dir = setup("poke-never");
    let rom = dir.join("waits.sfc");
    write_rom(&rom, &WAITS_FOR_A_POKE);
    let o = luna(
        &[
            "strict",
            "--frames",
            "4",
            "--poke-at",
            "00:8005",
            "--poke-hit",
            "999999",
            "--poke",
            "7e:0010=01",
        ],
        &rom,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("never reached arrival"));
}

/// Forced blank; one visible sprite (OAM entry 0 at X = 10, Y = 20, tile 0,
/// the 127 others parked at Y = 240); screen on; wait forever. `fill` is the
/// VRAM word written to tile 0 first (`None`: no VRAM write at all).
fn sprite_rom(fill: Option<u8>) -> Vec<u8> {
    let mut c: Vec<u8> = vec![0xA9, 0x80, 0x8D, 0x00, 0x21]; // forced blank
    c.extend([0x9C, 0x02, 0x21, 0x9C, 0x03, 0x21]); // OAMADD = 0
    c.extend([0xA9, 10, 0x8D, 0x04, 0x21, 0xA9, 20, 0x8D, 0x04, 0x21]); // X, Y
    c.extend([0x9C, 0x04, 0x21, 0x9C, 0x04, 0x21]); // tile 0, attr 0
    c.extend([0xA2, 127]); // LDX #127
    let loop_at = c.len();
    c.extend([0x9C, 0x04, 0x21, 0xA9, 0xF0, 0x8D, 0x04, 0x21]); // X=0, Y=240
    c.extend([0x9C, 0x04, 0x21, 0x9C, 0x04, 0x21, 0xCA]); // tile, attr, DEX
    let back = (loop_at as isize - (c.len() as isize + 2)) as i8;
    c.extend([0xD0, back as u8]);
    if let Some(v) = fill {
        c.extend([
            0xA9, 0x80, 0x8D, 0x15, 0x21, 0x9C, 0x16, 0x21, 0x9C, 0x17, 0x21,
        ]);
        c.extend([
            0xA2, 16, 0xA9, v, 0x8D, 0x18, 0x21, 0x8D, 0x19, 0x21, 0xCA, 0xD0, 0xF5,
        ]);
    }
    c.extend([0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x80, 0xFE]); // screen on; spin
    c
}

#[test]
fn strict_flags_a_drawn_sprite_tile_no_write_filled_even_with_the_zero_power_on() {
    let dir = setup("s1");
    let rom = dir.join("s1.sfc");
    write_rom(&rom, &sprite_rom(None));
    let o = luna(&["strict", "--frames", "5"], &rom);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("S1 tile $0000 sprite #0"), "{out}");
}

#[test]
fn strict_is_quiet_when_the_sprite_tile_was_written_blank_or_not() {
    let dir = setup("s1-quiet");
    let rom = dir.join("s1-quiet.sfc");
    for fill in [0x00u8, 0xFF] {
        write_rom(&rom, &sprite_rom(Some(fill)));
        let o = luna(&["strict", "--frames", "5"], &rom);
        assert_eq!(
            o.status.code(),
            Some(0),
            "fill {fill:#x}: {}",
            String::from_utf8_lossy(&o.stdout)
        );
    }
}
