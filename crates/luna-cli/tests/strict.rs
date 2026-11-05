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
