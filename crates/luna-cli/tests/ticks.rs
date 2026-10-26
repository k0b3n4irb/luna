//! `luna ticks`: the per-tick record in the `tick-record` format.
//!
//! The ROM is a game whose tick spans three frames (the one of
//! `stop_on_symbol.rs`): `half_a` goes up at the start of each tick,
//! `half_b` 2.3 frames later.

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
    let dir = std::env::temp_dir().join(format!("luna_ticks_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The slow-tick game and its `.sym`, written into `dir`.
fn slow_tick_rom(dir: &Path) -> PathBuf {
    let prog = [
        0x78, // 8000 SEI
        0xA9, 0x80, // 8001 LDA #$80
        0x8D, 0x00, 0x42, // 8003 STA $4200      NMI on
        0xCB, // 8006 WAI               wait:
        0xE6, 0x10, // 8007 INC $10         tick: first half of the state
        0xA0, 0x50, // 8009 LDY #$50
        0xA2, 0x00, // 800B LDX #$00
        0xCA, // 800D DEX
        0xD0, 0xFD, // 800E BNE $800D
        0x88, // 8010 DEY
        0xD0, 0xF8, // 8011 BNE $800B
        0xE6, 0x11, // 8013 INC $11         second half, 2.3 frames later
        0x80, 0xEF, // 8015 BRA $8006       tick_end:
        0x40, // 8017 RTI               nmi:
    ];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA SLOW TICK       ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA] {
        r[off] = 0x17;
        r[off + 1] = 0x80;
    }
    let rom = dir.join("game.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    std::fs::write(
        dir.join("game.sym"),
        "[labels]\n00:8000 main\n00:8006 wait\n00:8007 tick\n00:8015 tick_end\n00:8017 nmi\n\
         7e:0010 half_a\n7e:0011 half_b\n",
    )
    .unwrap();
    rom
}

/// Run `luna ticks` with `spec` written next to the ROM; the record is
/// returned as parsed lines.
fn ticks(dir: &Path, spec: &str, extra: &[&str]) -> (Output, Vec<serde_json::Value>) {
    let rom = slow_tick_rom(dir);
    let spec_path = dir.join("spec.toml");
    std::fs::write(&spec_path, spec).unwrap();
    let out_path = dir.join("record.jsonl");
    let _ = std::fs::remove_file(&out_path);
    let out = Command::new(luna_bin())
        .arg("ticks")
        .arg(&rom)
        .args(["--force-mapper", "lorom", "--spec"])
        .arg(&spec_path)
        .arg("--out")
        .arg(&out_path)
        .args(extra)
        .output()
        .expect("run luna");
    let lines = std::fs::read_to_string(&out_path)
        .map(|text| {
            text.lines()
                .map(|l| serde_json::from_str(l).expect("a JSON line"))
                .collect()
        })
        .unwrap_or_default();
    (out, lines)
}

fn text(out: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const BASE: &str = "format = 1\ntick = { symbol = \"tick\" }\n";

#[test]
fn a_record_is_a_header_one_line_per_tick_and_an_end() {
    let dir = fresh_dir("basic");
    let spec = format!(
        "{BASE}ticks = 4\n\
         [[zone]]\nname = \"halves\"\nat = \"half_a\"\nlength = 2\n\
         [[zone]]\nname = \"pal\"\nat = \"CGRAM:0\"\nlength = 2\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(lines.len(), 6, "{lines:#?}");

    let h = &lines[0];
    assert_eq!(
        (&h["format"], &h["version"]),
        (&"tick-record".into(), &1.into())
    );
    assert_eq!(h["producer"]["name"], "luna");
    assert_eq!(h["subject"]["machine"], "snes");
    assert_eq!(h["subject"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(h["endian"], "little");
    assert_eq!(h["tick"]["symbol"], "tick");
    assert_eq!(h["clock"]["frame"], "frame_end");
    assert_eq!(h["clock"]["hz"], 21_477_272);
    assert_eq!(h["zones"][0]["name"], "halves");
    assert_eq!(h["zones"][1]["at"], "CGRAM:0");

    // Each tick is read before its first instruction: both halves of the
    // previous tick are in, none of this one.
    for (i, line) in lines[1..5].iter().enumerate() {
        let t = i as u64 + 1;
        assert_eq!(line["t"], t);
        assert_eq!(line["z"]["halves"], format!("{0:02x}{0:02x}", t - 1));
        assert_eq!(line["beam"].as_array().unwrap().len(), 2);
    }
    // Frames and cycles only move forward.
    let frames: Vec<u64> = lines[1..5]
        .iter()
        .map(|l| l["frame"].as_u64().unwrap())
        .collect();
    assert!(frames.windows(2).all(|w| w[0] < w[1]), "{frames:?}");
    // Zones keep the spec's order.
    let keys: Vec<&String> = lines[1]["z"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["halves", "pal"]);
    assert_eq!(lines[5]["end"]["ticks"], 4);
    assert_eq!(lines[5]["end"]["reason"], "limit");
}

#[test]
fn start_and_if_choose_which_arrivals_are_ticks() {
    let dir = fresh_dir("start_if");
    let spec = "format = 1\n\
        tick = { symbol = \"tick\", if = \"([half_a] & 1) == 0\" }\n\
        start = { symbol = \"tick_end\", hit = 2 }\n\
        ticks = 3\n\
        [[zone]]\nname = \"a\"\nat = \"half_a\"\nlength = 1\n";
    let (out, lines) = ticks(&dir, spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let a: Vec<&str> = lines[1..4]
        .iter()
        .map(|l| l["z"]["a"].as_str().unwrap())
        .collect();
    // After the second tick ends, the even arrivals only.
    assert_eq!(a, ["02", "04", "06"]);
    assert_eq!(lines[0]["tick"]["start"]["hit"], 2);
    assert_eq!(lines[0]["tick"]["if"], "([half_a] & 1) == 0");
}

#[test]
fn a_poke_lands_before_its_tick_is_read() {
    let dir = fresh_dir("poke");
    let spec = format!(
        "{BASE}ticks = 4\n\
         [[poke]]\ntick = 3\nat = \"half_b\"\nbytes = \"7f\"\n\
         [[zone]]\nname = \"b\"\nat = \"half_b\"\nlength = 1\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let b: Vec<&str> = lines[1..5]
        .iter()
        .map(|l| l["z"]["b"].as_str().unwrap())
        .collect();
    assert_eq!(b, ["00", "01", "7f", "80"]);
}

#[test]
fn a_poke_on_an_arrival_lands_before_any_tick() {
    let dir = fresh_dir("poke_on");
    // `main` runs once, at power-on, long before the first tick; the
    // second arrival on `tick` is the tick-2 read.
    let spec = format!(
        "{BASE}ticks = 3\n\
         [[poke]]\non = {{ symbol = \"main\" }}\nat = \"half_a\"\nbytes = \"40\"\n\
         [[poke]]\non = {{ symbol = \"tick\", hit = 2 }}\nat = \"half_b\"\nbytes = \"20\"\n\
         [[zone]]\nname = \"halves\"\nat = \"half_a\"\nlength = 2\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let h: Vec<&str> = lines[1..4]
        .iter()
        .map(|l| l["z"]["halves"].as_str().unwrap())
        .collect();
    assert_eq!(h, ["4000", "4120", "4221"]);

    // An arrival the run never makes is an error, after the record ends.
    let spec = format!(
        "{BASE}ticks = 2\n\
         [[poke]]\non = {{ symbol = \"tick\", hit = 9 }}\nat = \"half_a\"\nbytes = \"00\"\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("arrival 9"),
        "{}",
        text(&out)
    );
    assert_eq!(lines.last().unwrap()["end"]["ticks"], 2);
}

#[test]
fn the_input_script_counts_arrivals_and_the_line_says_what_changed() {
    let dir = fresh_dir("input");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pad.txt"), "2:0x1000\n4:0\n").unwrap();
    let spec = format!(
        "{BASE}ticks = 5\ninput = \"pad.txt\"\n\
         [[zone]]\nname = \"a\"\nat = \"half_a\"\nlength = 1\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let input: Vec<&serde_json::Value> = lines[1..6].iter().map(|l| &l["in"]).collect();
    assert!(input[0].is_null(), "{input:?}");
    assert_eq!(input[1]["1"], "0x1000");
    assert!(input[2].is_null(), "{input:?}");
    assert_eq!(input[3]["1"], "0x0000");
    assert_eq!(lines[0]["inputs"]["at"], "tick");
}

#[test]
fn shots_are_files_beside_the_record() {
    let dir = fresh_dir("shots");
    let spec = format!(
        "{BASE}ticks = 3\n[shots]\nticks = [3]\n\
         [[zone]]\nname = \"a\"\nat = \"half_a\"\nlength = 1\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(lines[1].get("screen").is_none());
    let screen = &lines[3]["screen"];
    assert_eq!(screen["file"], "t000003.png");
    assert_eq!(screen["fnv1a64"].as_str().unwrap().len(), 16);
    let shots_dir = PathBuf::from(lines[0]["shots_dir"].as_str().unwrap());
    assert!(shots_dir.join("t000003.png").is_file());
}

#[test]
fn a_record_that_falls_short_still_ends_and_exits_1() {
    let dir = fresh_dir("short");
    let spec = format!("{BASE}ticks = 100\nuntil_frame = 20\n");
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let end = &lines.last().unwrap()["end"];
    assert_eq!(end["reason"], "until_frame");
    assert_eq!(end["ticks"], lines.len() as u64 - 2);
}

#[test]
fn a_bad_spec_is_refused_before_anything_runs() {
    let dir = fresh_dir("strict");
    let zone = |at: &str, len: u32| {
        format!("{BASE}ticks = 2\n[[zone]]\nname = \"z\"\nat = \"{at}\"\nlength = {len}\n")
    };
    let cases = [
        (zone("nosuch", 1), "not a loaded symbol"),
        (zone("00:2100", 1), "I/O registers"),
        (zone("40:0000", 4), "unmapped"),
        (zone("half_a", 0), "length is 0"),
        (zone("CGRAM:FF", 4), "run past the end"),
        (
            "format = 1\ntick = { symbol = \"tick\", if = \"[half_a] & 1 == 0\" }\n".to_string(),
            "expected",
        ),
        (
            "format = 1\ntick = { symbol = \"tick\", if = \"[nosuch] == 0\" }\n".to_string(),
            "nosuch",
        ),
        (
            "format = 2\ntick = { symbol = \"tick\" }\n".to_string(),
            "format 2",
        ),
        (format!("{BASE}colour = 3\n"), "unknown field"),
        (
            format!(
                "{BASE}[[zone]]\nname = \"z\"\nat = \"half_a\"\nlength = 1\n\
                 [[zone]]\nname = \"z\"\nat = \"half_b\"\nlength = 1\n"
            ),
            "given twice",
        ),
        (
            format!("{BASE}[[poke]]\ntick = 1\nat = \"00:8000\"\nbytes = \"00\"\n"),
            "not all WRAM",
        ),
        (
            format!("{BASE}[[poke]]\nat = \"half_a\"\nbytes = \"00\"\n"),
            "either `tick` or `on`",
        ),
        (
            format!(
                "{BASE}[[poke]]\ntick = 1\non = {{ symbol = \"main\" }}\nat = \"half_a\"\nbytes = \"00\"\n"
            ),
            "either `tick` or `on`",
        ),
    ];
    for (spec, why) in cases {
        let (out, lines) = ticks(&dir, &spec, &[]);
        assert_eq!(out.status.code(), Some(2), "{spec}\n{}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(why),
            "{spec}\n{}",
            text(&out)
        );
        assert!(lines.is_empty(), "nothing is written: {lines:?}");
    }
}

#[test]
fn the_schema_needs_no_rom() {
    let out = Command::new(luna_bin())
        .args(["ticks", "--schema"])
        .output()
        .expect("run luna");
    assert!(out.status.success(), "{}", text(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for kind in ["header", "tick", "end"] {
        assert!(v[kind].is_object(), "{kind}");
    }
}
