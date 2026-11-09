//! Forcing an NMI or an IRQ before the N-th execution of an address
//! (`--inject-nmi-at`, `--inject-irq-at`, the `inject_*_at` manifest keys).
//!
//! The ROM is a loop `INC $10 ; INC $11 ; BRA`. Its NMI handler copies `$10`
//! to `$12`, copies the stacked return address to `$16/$17` and counts in
//! `$13`; its IRQ handler copies `$10` to `$14` and counts in `$15`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna_inject_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The loop game, written into `dir`. `nmi_on`: reset writes `$4200 = $80`.
/// `cli`: reset clears the I flag (`CLI` at `cli_here`, else a `NOP`).
fn loop_rom(dir: &Path, nmi_on: bool, cli: bool) -> PathBuf {
    let prog = [
        0x78, // 8000 SEI
        0xA2,
        0xFF, // 8001 LDX #$FF
        0x9A, // 8003 TXS
        0xA9,
        if nmi_on { 0x80 } else { 0x00 }, // 8004 LDA #nmi
        0x8D,
        0x00,
        0x42,                          // 8006 STA $4200
        if cli { 0x58 } else { 0xEA }, // 8009 CLI / NOP   cli_here
        0xE6,
        0x10, // 800A INC $10   inc_a
        0xE6,
        0x11, // 800C INC $11   inc_b
        0x80,
        0xFA, // 800E BRA inc_a
    ];
    let nmi = [
        0xA5, 0x10, // 8010 LDA $10
        0x85, 0x12, // 8012 STA $12
        0xAD, 0xFE, 0x01, // 8014 LDA $01FE   stacked PC, low
        0x85, 0x16, // 8017 STA $16
        0xAD, 0xFF, 0x01, // 8019 LDA $01FF   stacked PC, high
        0x85, 0x17, // 801C STA $17
        0xE6, 0x13, // 801E INC $13
        0x40, // 8020 RTI
    ];
    let irq = [
        0xA5, 0x10, // 8030 LDA $10
        0x85, 0x14, // 8032 STA $14
        0xE6, 0x15, // 8034 INC $15
        0x40, // 8036 RTI
    ];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x10..0x10 + nmi.len()].copy_from_slice(&nmi);
    r[0x30..0x30 + irq.len()].copy_from_slice(&irq);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA INJECT          ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA] {
        r[off] = 0x10;
        r[off + 1] = 0x80;
    }
    for off in [0x7FEE, 0x7FFE] {
        r[off] = 0x30;
        r[off + 1] = 0x80;
    }
    let rom = dir.join("game.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    std::fs::write(
        dir.join("game.sym"),
        "[labels]\n00:8000 main\n00:8009 cli_here\n00:800A inc_a\n00:800C inc_b\n\
         7e:0010 ctr\n7e:0012 nmi_copy\n7e:0013 nmi_count\n7e:0014 irq_copy\n\
         7e:0015 irq_count\n7e:0016 ret_pc\n",
    )
    .unwrap();
    rom
}

fn luna(rom: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .arg("state")
        .arg(rom)
        .args(["--force-mapper", "lorom"])
        .args(args)
        .output()
        .expect("run luna")
}

fn text(out: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The `--peek` values of a run, as the hex strings of the output.
fn peeks(out: &Output) -> Vec<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("json: {e}\n{}", text(out)));
    v["peeks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["bytes_hex"].as_str().unwrap().to_string())
        .collect()
}

const WATCH: [&str; 8] = [
    "--peek",
    "ctr:1",
    "--peek",
    "nmi_copy:1",
    "--peek",
    "nmi_count:1",
    "--peek",
    "ret_pc:2",
];

/// (A) The NMI lands before the 5th execution of `inc_a`: the handler sees
/// the counter as four increments left it, the stacked address is that of
/// `inc_a` itself, and the instruction then runs after the `RTI`.
#[test]
fn the_nmi_lands_before_the_instruction() {
    let dir = fresh_dir("before");
    let rom = loop_rom(&dir, true, false);
    // Arrival 6 is the instruction reached again after the `RTI`: the run
    // stops with it still unexecuted.
    let mut args = vec![
        "--inject-nmi-at",
        "inc_a:5",
        "--until-pc",
        "inc_a",
        "--hit",
        "6",
        "--out",
        "-",
    ];
    args.extend(WATCH);
    let out = luna(&rom, &args);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(peeks(&out), ["04", "04", "01", "0a80"], "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("injected NMI before inc_a hit 5 — frame 0, line "),
        "{stderr}"
    );

    // Control: the same stop without the injection ran no handler.
    let mut args = vec!["--until-pc", "inc_a", "--hit", "6", "--out", "-"];
    args.extend(WATCH);
    let out = luna(&rom, &args);
    assert_eq!(peeks(&out)[2], "00", "{}", text(&out));
}

/// (B) With `NMITIMEN.7` clear nothing is injected, and it is said.
#[test]
fn a_disabled_nmi_is_not_injected() {
    let dir = fresh_dir("disabled");
    let rom = loop_rom(&dir, false, false);
    let mut args = vec![
        "--inject-nmi-at",
        "inc_a:5",
        "--until-pc",
        "inc_a",
        "--hit",
        "6",
        "--out",
        "-",
    ];
    args.extend(WATCH);
    let out = luna(&rom, &args);
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NMI not injected before inc_a hit 5"),
        "{stderr}"
    );
    assert!(stderr.contains("NMITIMEN.7 clear"), "{stderr}");
    assert_eq!(peeks(&out)[2], "00");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["poke"]["delivered"], false, "{v:#}");
}

/// (C) The injected NMI is an extra edge: the real `VBlank` NMI still comes.
#[test]
fn the_vblank_nmi_still_comes() {
    let dir = fresh_dir("extra_edge");
    let rom = loop_rom(&dir, true, false);
    let count = |inject: bool| -> u8 {
        let mut args = vec!["--until-frame", "3", "--out", "-"];
        if inject {
            args.extend(["--inject-nmi-at", "inc_a:5"]);
        }
        args.extend(["--peek", "nmi_count:1"]);
        let out = luna(&rom, &args);
        assert!(out.status.success(), "{}", text(&out));
        u8::from_str_radix(&peeks(&out)[0], 16).unwrap()
    };
    let real = count(false);
    assert!(real >= 2, "the control sees the VBlank NMIs: {real}");
    assert_eq!(count(true), real + 1);
}

/// (D) An IRQ is refused while `I` is set, delivered once it is clear.
#[test]
fn an_irq_follows_the_i_flag() {
    let dir = fresh_dir("irq");
    let rom = loop_rom(&dir, false, true);
    let run = |at: &str, hit: &str| {
        let out = luna(
            &rom,
            &[
                "--inject-irq-at",
                at,
                "--until-pc",
                "inc_a",
                "--hit",
                hit,
                "--out",
                "-",
                "--peek",
                "irq_copy:1",
                "--peek",
                "irq_count:1",
            ],
        );
        assert!(out.status.success(), "{}", text(&out));
        out
    };
    // At the `CLI` itself the flag is still set.
    let out = run("cli_here", "2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("IRQ not injected before cli_here"),
        "{stderr}"
    );
    assert!(stderr.contains("I flag set"), "{stderr}");
    assert_eq!(peeks(&out), ["00", "00"]);
    // One instruction later it is clear: taken before the 3rd `inc_a`.
    let out = run("inc_a:3", "4");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("injected IRQ before inc_a hit 3"),
        "{stderr}"
    );
    assert_eq!(peeks(&out), ["02", "01"]);
}

/// (E) A sweep is reproducible: the same injection gives the same machine.
#[test]
fn the_same_injection_gives_the_same_machine() {
    let dir = fresh_dir("determinism");
    let rom = loop_rom(&dir, true, true);
    let run = || {
        let out = luna(
            &rom,
            &[
                "--inject-nmi-at",
                "inc_b:7",
                "--inject-irq-at",
                "inc_a:11",
                "--until-frame",
                "4",
                "--out",
                "-",
                "--peek",
                "ctr:6",
                "--peek",
                "ret_pc:2",
            ],
        );
        assert!(out.status.success(), "{}", text(&out));
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (a, ea) = run();
    let (b, eb) = run();
    // The whole snapshot, framebuffer hash and WRAM peeks included.
    assert_eq!(a, b);
    assert_eq!(ea, eb);
    assert!(ea.contains("injected NMI before inc_b hit 7"), "{ea}");
    assert!(ea.contains("injected IRQ before inc_a hit 11"), "{ea}");
}

/// (F) An arrival the run never reaches is an error.
#[test]
fn an_arrival_never_reached_is_an_error() {
    let dir = fresh_dir("missed");
    let rom = loop_rom(&dir, true, false);
    let out = luna(
        &rom,
        &["--inject-nmi-at", "inc_a:900000000", "--until-frame", "2"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--inject-nmi-at inc_a: hit 900000000 not reached by frame"),
        "{stderr}"
    );
    assert!(stderr.contains("nothing was injected"), "{stderr}");
    // An unknown routine and a zero arrival are refused up front.
    let out = luna(&rom, &["--inject-nmi-at", "nowhere", "--until-frame", "2"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--inject-nmi-at `nowhere`"),
        "{}",
        text(&out)
    );
    let out = luna(&rom, &["--inject-nmi-at", "inc_a:0", "--until-frame", "2"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

/// A bare `BANK:OFFSET` is an address (hit 1); `BANK:OFFSET:N` gives the hit.
#[test]
fn an_address_spec_takes_an_optional_arrival() {
    let dir = fresh_dir("address");
    let rom = loop_rom(&dir, true, false);
    for (spec, hit) in [("00:800A", 1), ("00:800A:3", 3)] {
        let out = luna(&rom, &["--inject-nmi-at", spec, "--until-frame", "1"]);
        assert!(out.status.success(), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("injected NMI before 00:800A hit {hit}")),
            "{stderr}"
        );
    }
}

/// `luna test`: the manifest key injects; a refusal fails the test.
#[test]
fn a_manifest_injects_and_a_refusal_fails_it() {
    let dir = fresh_dir("manifest");
    loop_rom(&dir, true, false);
    let head = "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 1\n";
    let run = |name: &str, body: &str| {
        std::fs::write(dir.join(name), body).unwrap();
        Command::new(luna_bin())
            .arg("test")
            .arg(name)
            .current_dir(&dir)
            .output()
            .expect("run luna test")
    };
    // The handler of the injected NMI ran before the 5th `inc_a`: it saw 4.
    // A checkpoint on the 6th arrival reads the machine before any VBlank
    // NMI overwrites what the handler left.
    let out = run(
        "ok.toml",
        &format!(
            "{head}inject_nmi_at = \"inc_a:5\"\n[[checkpoint]]\nat_symbol = \"inc_a\"\nhit = 6\n\
             [checkpoint.values]\nnmi_copy = 4\nnmi_count = 1\nctr = 4\n"
        ),
    );
    assert!(out.status.success(), "{}", text(&out));
    // The IRQ is refused with I set: the test fails and says why.
    let out = run(
        "irq.toml",
        &format!("{head}inject_irq_at = [\"inc_a:5\"]\n"),
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("IRQ not injected: I flag set"),
        "{}",
        text(&out)
    );
}
