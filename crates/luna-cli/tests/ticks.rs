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
fn a_second_pad_rides_the_same_numbering_on_its_own_port() {
    let dir = fresh_dir("input2");
    std::fs::write(dir.join("pad1.txt"), "2:0x1000\n").unwrap();
    std::fs::write(dir.join("pad2.txt"), "2:0x0080\n3:0\n").unwrap();
    let spec = format!(
        "{BASE}ticks = 4\ninput = \"pad1.txt\"\ninput2 = \"pad2.txt\"\n\
         [[zone]]\nname = \"a\"\nat = \"half_a\"\nlength = 1\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(lines[0]["inputs"]["port"], 1);
    assert_eq!(lines[0]["inputs2"]["port"], 2);
    assert_eq!(lines[0]["inputs2"]["file"], "pad2.txt");
    // Both change at arrival 2; only the second pad at arrival 3.
    assert_eq!(lines[2]["in"]["1"], "0x1000");
    assert_eq!(lines[2]["in"]["2"], "0x0080");
    assert!(lines[3]["in"].get("1").is_none(), "{:?}", lines[3]);
    assert_eq!(lines[3]["in"]["2"], "0x0000");
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

/// `(width, height)` from a PNG's IHDR.
fn png_size(path: &Path) -> (u32, u32) {
    let b = std::fs::read(path).expect("picture written");
    let n = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    (n(16), n(20))
}

#[test]
fn every_frame_writes_one_picture_per_frame_and_the_sound_of_the_same_frames() {
    let dir = fresh_dir("every_frame");
    let wav = dir.join("sound.wav");
    let spec = format!("{BASE}[shots]\nevery_frame = true\nfrom_tick = 2\ncount = 7\n");
    let (out, lines) = ticks(&dir, &spec, &["--audio-out", wav.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    let shots_dir = PathBuf::from(lines[0]["shots_dir"].as_str().unwrap());
    let mut pngs: Vec<_> = std::fs::read_dir(&shots_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    pngs.sort();
    let want: Vec<String> = (1..=7).map(|n| format!("f{n:06}.png")).collect();
    assert_eq!(pngs, want, "one picture per frame, not per tick");
    assert_eq!(png_size(&shots_dir.join("f000001.png")), (256, 224));
    // The run stopped on its last image: the ticks had no limit.
    let end = &lines.last().unwrap()["end"];
    assert_eq!(end["images"]["count"], 7);
    // Image 1 is the frame the anchor (tick 2) falls in, so it ends after
    // the tick line's frame counter.
    let tick2_frame = lines[2]["frame"].as_u64().unwrap();
    assert!(end["images"]["first_frame"].as_u64().unwrap() >= tick2_frame);

    // The WAV: stereo 16-bit PCM whose length is the seven frames' worth.
    let b = std::fs::read(&wav).expect("WAV written");
    assert_eq!(&b[..4], b"RIFF");
    assert_eq!(&b[8..16], b"WAVEfmt ");
    let rate = u32::from_le_bytes(b[24..28].try_into().unwrap());
    let data = u32::from_le_bytes(b[40..44].try_into().unwrap()) as usize;
    assert_eq!(b.len(), 44 + data);
    let seconds = (data / 4) as f64 / f64::from(rate);
    assert!(
        (seconds - 7.0 / 60.0988).abs() < 0.001,
        "{seconds} s for 7 frames"
    );
    assert_eq!(end["images"]["audio"]["hz"], rate);
    assert_eq!(end["images"]["audio"]["samples"], (data / 4) as u64);
    // The sound is the seven frames' own: about 32040 / 60.0988 = 533
    // sample pairs a frame, not the run from power-on.
    let pairs = (data / 4) as f64;
    assert!(
        (pairs / 7.0 - 533.1).abs() < 2.0,
        "{pairs} pairs for 7 frames"
    );
    // 7 frames of the console's sound, roughly 32 kHz.
    assert!((31_000..33_000).contains(&rate), "{rate} Hz");
}

#[test]
fn every_frame_native_is_512_by_448() {
    let dir = fresh_dir("every_frame_native");
    let spec =
        format!("{BASE}[shots]\nevery_frame = true\nnative = true\nfrom_tick = 1\ncount = 2\n");
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let shots_dir = PathBuf::from(lines[0]["shots_dir"].as_str().unwrap());
    for n in 1..=2 {
        assert_eq!(
            png_size(&shots_dir.join(format!("f{n:06}.png"))),
            (512, 448)
        );
    }
}

/// A game that sets BG mode 5 (hi-res, 512 columns) and ticks once a frame.
fn mode5_rom(dir: &Path) -> PathBuf {
    let prog = [
        0x78, // 8000 SEI
        0xA9, 0x05, 0x8D, 0x05, 0x21, // 8001 BGMODE = 5
        0xA9, 0x01, 0x8D, 0x2C, 0x21, // 8006 TM = BG1
        0xA9, 0x0F, 0x8D, 0x00, 0x21, // 800B INIDISP = full brightness
        0xA9, 0x80, 0x8D, 0x00, 0x42, // 8010 NMI on
        0xCB, // 8015 WAI
        0xE6, 0x10, // 8016 tick: INC $10
        0x80, 0xFB, // 8018 BRA $8015
        0x40, // 801A nmi: RTI
    ];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA MODE FIVE       ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA] {
        r[off] = 0x1A;
        r[off + 1] = 0x80;
    }
    let rom = dir.join("mode5.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    std::fs::write(dir.join("mode5.sym"), "[labels]\n00:8016 tick\n").unwrap();
    rom
}

#[test]
fn native_shots_of_a_mode_5_game_are_512_by_448() {
    let dir = fresh_dir("mode5");
    let rom = mode5_rom(&dir);
    let spec = dir.join("spec.toml");
    std::fs::write(
        &spec,
        format!("{BASE}[shots]\nevery_frame = true\nnative = true\nfrom_tick = 2\ncount = 3\n"),
    )
    .unwrap();
    let out_path = dir.join("record.jsonl");
    let out = Command::new(luna_bin())
        .arg("ticks")
        .arg(&rom)
        .args(["--force-mapper", "lorom", "--spec"])
        .arg(&spec)
        .arg("--out")
        .arg(&out_path)
        .output()
        .expect("run luna");
    assert!(out.status.success(), "{}", text(&out));
    let shots = out_path.with_extension("shots");
    for n in 1..=3 {
        assert_eq!(
            png_size(&shots.join(format!("f{n:06}.png"))),
            (512, 448),
            "Mode 5, native"
        );
    }
}

#[test]
fn every_frame_needs_its_anchor_its_count_and_no_ticks() {
    let dir = fresh_dir("every_frame_refused");
    let wav = dir.join("sound.wav");
    let cases = [
        ("every_frame = true\ncount = 3\n", "needs `from_tick`"),
        ("every_frame = true\nfrom_tick = 1\n", "needs `count`"),
        (
            "every_frame = true\nfrom_tick = 0\ncount = 3\n",
            "needs `from_tick`",
        ),
        (
            "every_frame = true\nfrom_tick = 1\ncount = 0\n",
            "needs `count`",
        ),
        (
            "every_frame = true\nfrom_tick = 1\ncount = 3\nticks = [1]\n",
            "exclusive",
        ),
        ("ticks = [1]\ncount = 3\n", "go with `every_frame"),
        ("ticks = [1]\nfrom_tick = 3\n", "go with `every_frame"),
        ("native = true\n", "give `ticks` or `every_frame"),
    ];
    for (shots, why) in cases {
        let spec = format!("{BASE}ticks = 3\n[shots]\n{shots}");
        let (out, lines) = ticks(&dir, &spec, &[]);
        assert_eq!(out.status.code(), Some(2), "{shots}\n{}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(why),
            "{shots}\n{}",
            text(&out)
        );
        assert!(lines.is_empty(), "nothing is written: {lines:?}");
    }
    // The sound goes with the every_frame pictures only.
    let spec = format!("{BASE}ticks = 3\n[shots]\nticks = [1]\n");
    let (out, lines) = ticks(&dir, &spec, &["--audio-out", wav.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("every_frame"));
    assert!(lines.is_empty() && !wav.exists());
    let (out, _) = ticks(
        &dir,
        &format!("{BASE}ticks = 3\n"),
        &["--audio-out", wav.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn every_frame_keeps_running_past_the_tick_limit_and_fails_when_the_run_ends_first() {
    let dir = fresh_dir("every_frame_limit");
    // Two ticks asked for, ten images: the run goes on for the images.
    let spec = format!("{BASE}ticks = 2\n[shots]\nevery_frame = true\nfrom_tick = 1\ncount = 10\n");
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let end = &lines.last().unwrap()["end"];
    assert_eq!(
        (end["ticks"].as_u64(), end["images"]["count"].as_u64()),
        (Some(2), Some(10))
    );
    // The frame bound ends the run first: exit 1, and the end line says how
    // many images there are.
    let spec =
        format!("{BASE}until_frame = 6\n[shots]\nevery_frame = true\nfrom_tick = 1\ncount = 50\n");
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("images asked for"));
    assert!(
        lines.last().unwrap()["end"]["images"]["count"]
            .as_u64()
            .unwrap()
            < 50
    );
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
fn the_hardware_oam_and_the_ppu_registers_are_zones() {
    let dir = fresh_dir("oam_ppu");
    let spec = format!(
        "{BASE}ticks = 2\n\
         [[zone]]\nname = \"oam\"\nat = \"OAM:21C\"\nlength = 4\n\
         [[zone]]\nname = \"regs\"\nat = \"PPU:2100\"\nlength = 2\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(lines[1]["z"]["oam"].as_str().unwrap().len(), 8);
    assert_eq!(lines[1]["z"]["regs"].as_str().unwrap().len(), 4);
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
        (zone("PPU:2104", 1), "not one the PPU keeps"),
        (zone("PPU:2133", 2), "$2134"),
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
        (
            format!(
                "{BASE}input = {{ script = \"1:0\", port = 2 }}\n\
                 input2 = {{ script = \"1:0\" }}\n"
            ),
            "both drive port 2",
        ),
        (
            format!(
                "{BASE}input = {{ script = \"1:0\" }}\n\
                 input2 = {{ script = \"1:0\", at = \"frame\" }}\n"
            ),
            "same routine",
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

#[test]
fn until_ends_the_record_on_another_routine_as_its_last_tick() {
    let dir = fresh_dir("until");
    // `tick_end` is the end of each tick; the second one ends the record.
    let spec = format!(
        "{BASE}ticks = 10\nuntil = {{ symbol = \"tick_end\", hit = 2 }}\n\
         [[zone]]\nname = \"halves\"\nat = \"half_a\"\nlength = 2\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let z: Vec<&str> = lines[1..lines.len() - 1]
        .iter()
        .map(|l| l["z"]["halves"].as_str().unwrap())
        .collect();
    // Two ticks, then the arrival that ends the run, read before it runs.
    assert_eq!(z, ["0000", "0101", "0202"]);
    let end = &lines.last().unwrap()["end"];
    assert_eq!(
        (&end["ticks"], &end["reason"]),
        (&3.into(), &"until".into())
    );
    assert_eq!(lines[0]["tick"]["until"]["symbol"], "tick_end");
    assert_eq!(lines[0]["tick"]["until"]["hit"], 2);

    // An `until` the run never reaches is a shortfall.
    let spec = format!("{BASE}until_frame = 20\nuntil = {{ symbol = \"tick_end\", hit = 99 }}\n");
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("was not reached"),
        "{}",
        text(&out)
    );
    assert_eq!(lines.last().unwrap()["end"]["reason"], "until_frame");

    // Before the start it ends the record with no tick.
    let spec = format!(
        "{BASE}start = {{ symbol = \"tick\", hit = 5 }}\n\
         until = {{ symbol = \"tick_end\" }}\n"
    );
    let (out, lines) = ticks(&dir, &spec, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let end = &lines.last().unwrap()["end"];
    assert_eq!(
        (&end["ticks"], &end["reason"]),
        (&0.into(), &"until".into())
    );
}

/// A `LoROM` with 8 KB of battery RAM whose loop counts up in the first
/// byte of it: the tick is the top of the loop.
fn counter_rom(dir: &Path) -> PathBuf {
    let prog = [
        0xAF, 0x00, 0x00, 0x70, // 8000 LDA $700000    tick:
        0x1A, // 8004 INC A
        0x8F, 0x00, 0x00, 0x70, // 8005 STA $700000
        0x80, 0xF5, // 8009 BRA $8000
    ];
    let mut r = vec![0u8; 0x8000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA SRAM COUNTER    ");
    r[0x7FD5] = 0x20;
    r[0x7FD6] = 0x02; // ROM + RAM + battery
    r[0x7FD7] = 0x05;
    r[0x7FD8] = 0x03; // 8 KB
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    let rom = dir.join("counter.sfc");
    std::fs::write(&rom, &r).unwrap();
    std::fs::write(dir.join("counter.sym"), "[labels]\n00:8000 tick\n").unwrap();
    rom
}

#[test]
fn srm_in_seeds_the_battery_ram_and_srm_out_keeps_it() {
    let dir = fresh_dir("srm");
    let rom = counter_rom(&dir);
    std::fs::write(dir.join("in.srm"), [5u8; 8192]).unwrap();
    std::fs::write(
        dir.join("spec.toml"),
        "format = 1\ntick = { symbol = \"tick\" }\nticks = 3\n\
         srm_in = \"in.srm\"\nsrm_out = \"out.srm\"\n\
         [[zone]]\nname = \"count\"\nat = \"70:0000\"\nlength = 1\n",
    )
    .unwrap();
    let out = Command::new(luna_bin())
        .arg("ticks")
        .arg(&rom)
        .args(["--force-mapper", "lorom", "--spec"])
        .arg(dir.join("spec.toml"))
        .output()
        .expect("run luna");
    assert!(out.status.success(), "{}", text(&out));
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let z: Vec<&str> = lines[1..4]
        .iter()
        .map(|l| l["z"]["count"].as_str().unwrap())
        .collect();
    // Read before each loop runs: the seed, then two increments.
    assert_eq!(z, ["05", "06", "07"]);
    let saved = std::fs::read(dir.join("out.srm")).unwrap();
    assert_eq!((saved.len(), saved[0]), (8192, 7));

    // A cartridge with no battery RAM refuses both keys.
    let (out, _) = ticks(
        &dir,
        &format!("{BASE}ticks = 2\nsrm_in = \"in.srm\"\n"),
        &[],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no battery RAM"),
        "{}",
        text(&out)
    );
}

/// Two records of the slow-tick game, the second with one injected fault.
fn two_records(dir: &Path) -> (PathBuf, PathBuf) {
    let zone = "[[zone]]\nname = \"halves\"\nat = \"half_a\"\nlength = 2\n";
    let record = |name: &str, extra: &str| {
        let spec = format!("{BASE}ticks = 5\n{extra}{zone}");
        let (out, _) = ticks(dir, &spec, &[]);
        assert!(out.status.success(), "{}", text(&out));
        let to = dir.join(name);
        std::fs::rename(dir.join("record.jsonl"), &to).unwrap();
        to
    };
    let a = record("a.jsonl", "");
    let b = record(
        "b.jsonl",
        "[[poke]]\ntick = 3\nat = \"half_a\"\nbytes = \"ff\"\n",
    );
    (a, b)
}

fn compare(dir: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .current_dir(dir)
        .args(["ticks", "--compare"])
        .args(args)
        .output()
        .expect("run luna")
}

#[test]
fn compare_locates_the_first_divergence_and_writes_the_csv() {
    let dir = fresh_dir("compare");
    let (a, b) = two_records(&dir);
    let (sa, sb) = (a.to_str().unwrap(), b.to_str().unwrap());

    // The same record twice: identical, exit 0.
    let out = compare(&dir, &[sa, sa]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("identical: 5 tick(s)"),
        "{}",
        text(&out)
    );

    // One injected fault: exit 1, located at tick 3, byte 0 of the zone.
    let csv = dir.join("diff.csv");
    let out = compare(&dir, &[sa, sb, "--csv", csv.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("first divergence at tick 3, zone `halves`, byte 0: A=02 B=ff"),
        "{}",
        text(&out)
    );
    let rows = std::fs::read_to_string(&csv).unwrap();
    let mut rows = rows.lines();
    assert_eq!(rows.next(), Some("t,zone,first_offset,a,b,differing_bytes"));
    assert!(rows.next().unwrap().starts_with("3,halves,0,02,ff,"));

    // `--csv -` writes the rows to stdout.
    let out = compare(&dir, &[sa, sb, "--csv", "-"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("t,zone,first_offset,a,b,"),
        "{}",
        text(&out)
    );

    // An unknown zone, or a file that is no record: exit 2.
    let out = compare(&dir, &[sa, sb, "--zone", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in both"));
    let junk = dir.join("junk.jsonl");
    std::fs::write(&junk, "hello\n").unwrap();
    let out = compare(&dir, &[sa, junk.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let out = compare(&dir, &[sa, "missing.jsonl"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn compare_excludes_a_rom_and_a_spec() {
    let dir = fresh_dir("compare_flags");
    let out = Command::new(luna_bin())
        .current_dir(&dir)
        .args(["ticks", "--compare", "a", "b", "--spec", "s.toml"])
        .output()
        .expect("run luna");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let out = Command::new(luna_bin())
        .current_dir(&dir)
        .args(["ticks", "--zone", "x"])
        .output()
        .expect("run luna");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

fn intervals(dir: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .current_dir(dir)
        .args(["ticks", "--intervals"])
        .args(args)
        .output()
        .expect("run luna")
}

#[test]
fn intervals_measures_the_ticks_of_a_record() {
    let dir = fresh_dir("intervals");
    let (a, _) = two_records(&dir);
    let sa = a.to_str().unwrap();

    // The slow-tick game takes about three frames a tick: none is above 3.5,
    // all are above 2.5.
    let csv = dir.join("i.csv");
    let out = intervals(&dir, &[sa, "--over", "2.5", "--csv", csv.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("5 tick(s), 4 measured (arrival to arrival, NOT lag frames)"),
        "{stdout}"
    );
    assert!(stdout.contains("4 tick(s) over 2.5 frame(s):"), "{stdout}");
    let rows = std::fs::read_to_string(&csv).unwrap();
    let mut rows = rows.lines();
    assert_eq!(rows.next(), Some("t,frame_delta,cycles,frames,ms,over"));
    assert_eq!(rows.count(), 4);
    let out = intervals(&dir, &[sa, "--over", "3.5"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 tick(s) over 3.5 frame(s)"));

    // A unit the header cannot convert is refused, and --frame-cycles lifts it.
    let foreign = std::fs::read_to_string(&a)
        .unwrap()
        .replace("snes_master", "other_master");
    let f = dir.join("f.jsonl");
    std::fs::write(&f, foreign).unwrap();
    let out = intervals(&dir, &[f.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("give --frame-cycles"));
    let out = intervals(&dir, &[f.to_str().unwrap(), "--frame-cycles", "357366"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    // Not a record, a missing file, a missing flag's partner: exit 2.
    let junk = dir.join("junk.jsonl");
    std::fs::write(&junk, "hello\n").unwrap();
    assert_eq!(
        intervals(&dir, &[junk.to_str().unwrap()]).status.code(),
        Some(2)
    );
    assert_eq!(intervals(&dir, &["missing.jsonl"]).status.code(), Some(2));
    let out = Command::new(luna_bin())
        .current_dir(&dir)
        .args(["ticks", "--over", "2"])
        .output()
        .expect("run luna");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}
