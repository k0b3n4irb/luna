//! Reading and asserting state where the program is, not where the frame
//! ends (issue #269), and the profile frame by frame (issue #270).
//!
//! The ROM is a game whose tick spans three frames: it updates half of
//! its state, works for 2.3 frames, updates the other half. A frame
//! boundary almost always falls between the two halves; the end of the
//! tick never does.

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
    let dir = std::env::temp_dir().join(format!("luna_stop_on_symbol_{name}"));
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

fn luna(cmd: &str, rom: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .arg(cmd)
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

#[test]
fn until_pc_stops_on_the_asked_arrival_with_a_consistent_state() {
    let dir = fresh_dir("until_pc");
    let rom = slow_tick_rom(&dir);
    let json = dir.join("state.json");
    let out = luna(
        "state",
        &rom,
        &[
            "--until-pc",
            "tick_end",
            "--hit",
            "3",
            "--peek",
            "half_a:2",
            "--out",
            json.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reached tick_end ($00:8015), hit 3"),
        "{stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    // Both halves of the third tick are in: the state is consistent.
    assert_eq!(v["peeks"][0]["bytes_hex"], "0303", "{v:#}");
    assert_eq!(v["cpu"]["pc"], 0x8015, "stopped before the instruction ran");
    let u = &v["until_pc"];
    assert_eq!(u["reached"], true);
    assert_eq!((&u["hit"], &u["hits_seen"]), (&3.into(), &3.into()));
    assert_eq!(u["frame"], v["scheduler"]["frame_count"]);
    assert_eq!(u["line"], v["scheduler"]["ppu_line"]);

    // The same instant by frame number is in the middle of a tick.
    let frame = u["frame"].as_u64().unwrap().to_string();
    let out = luna(
        "state",
        &rom,
        &["--until-frame", &frame, "--peek", "half_a:2", "--out", "-"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["peeks"][0]["bytes_hex"], "0302", "{v:#}");
    assert!(v.get("until_pc").is_none() && v.get("peek_hits").is_none());

    // The start of the routine works as well as a label inside it, and
    // an address as well as a name.
    for spec in ["tick+14", "00:8015", "80:8015"] {
        let out = luna(
            "state",
            &rom,
            &["--until-pc", spec, "--peek", "half_b", "--out", "-"],
        );
        assert!(out.status.success(), "{spec}: {}", text(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["peeks"][0]["bytes_hex"], "01", "{spec}");
    }
}

#[test]
fn until_pc_not_reached_is_an_error_and_a_bad_spec_a_usage_error() {
    let dir = fresh_dir("until_pc_errors");
    let rom = slow_tick_rom(&dir);
    // Hit 50 is some 150 frames away: frame 20 comes first.
    let out = luna(
        "state",
        &rom,
        &[
            "--until-pc",
            "tick_end",
            "--hit",
            "50",
            "--until-frame",
            "20",
            "--out",
            "-",
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hit 50 not reached by frame 20 (reached 6 time(s))"),
        "{stderr}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["until_pc"]["reached"], false);
    assert_eq!(v["scheduler"]["frame_count"], 20);

    let out = luna("state", &rom, &["--until-pc", "no_such_routine"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    // `--hit` means nothing alone, and counts from 1.
    assert_eq!(luna("state", &rom, &["--hit", "2"]).status.code(), Some(2));
    let out = luna("state", &rom, &["--until-pc", "tick", "--hit", "0"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn peek_at_samples_every_arrival_in_one_run() {
    let dir = fresh_dir("peek_at");
    let rom = slow_tick_rom(&dir);
    let csv = dir.join("ticks.csv");
    let out = luna(
        "state",
        &rom,
        &[
            "--peek-at",
            "tick_end",
            "--peek",
            "half_a",
            "--peek",
            "half_b",
            "--until-frame",
            "14",
            "--peek-at-out",
            csv.to_str().unwrap(),
            "--out",
            "-",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let rows: Vec<Vec<String>> = std::fs::read_to_string(&csv)
        .unwrap()
        .lines()
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect();
    assert_eq!(rows[0], ["hit", "frame", "line", "half_a", "half_b"]);
    assert_eq!(rows.len(), 5, "four ticks end by frame 14: {rows:?}");
    for (i, row) in rows[1..].iter().enumerate() {
        let n = format!("{:02x}", i + 1);
        assert_eq!(row[0], (i + 1).to_string());
        assert_eq!((&row[3], &row[4]), (&n, &n), "tick {} is whole", i + 1);
    }
    let frames: Vec<u64> = rows[1..].iter().map(|r| r[1].parse().unwrap()).collect();
    assert!(frames.windows(2).all(|w| w[1] - w[0] == 3), "{frames:?}");
    // The same rows in the JSON; the end-of-run peeks are still there.
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hits = v["peek_hits"].as_array().unwrap();
    assert_eq!(hits.len(), 4);
    assert_eq!(hits[2]["hit"], 3);
    assert_eq!(hits[2]["frame"], frames[2]);
    assert_eq!(hits[2]["peeks"][1]["bytes_hex"], "03");
    assert_eq!(v["peeks"].as_array().unwrap().len(), 2);
    assert_eq!(v["scheduler"]["frame_count"], 14);

    // Without a file the rows are printed, one line per arrival; the two
    // options combine, on the same routine or on two.
    let out = luna(
        "state",
        &rom,
        &[
            "--peek-at",
            "tick_end",
            "--until-pc",
            "tick_end",
            "--hit",
            "2",
            "--peek",
            "half_a:2",
            "--out",
            "-",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tick_end hit 1 frame "), "{stderr}");
    assert!(stderr.contains(": half_a:2=0202"), "{stderr}");
    assert!(stderr.contains("tick_end: reached 2 time(s)"), "{stderr}");
}

#[test]
fn a_bare_peek_count_that_reads_two_ways_is_said_so() {
    let dir = fresh_dir("peek_count");
    let rom = slow_tick_rom(&dir);
    let peek = |spec: &str| {
        let out = luna("state", &rom, &["-n", "100", "--peek", spec, "--out", "-"]);
        assert!(out.status.success(), "{}", text(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        (
            v["peeks"][0]["bytes_hex"].as_str().unwrap().len() / 2,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    // The bare count is still hex, as it always was — and now says so.
    let (bytes, stderr) = peek("half_a:36");
    assert_eq!(bytes, 0x36);
    assert!(
        stderr.contains("note: --peek half_a:36 reads 54 bytes: a bare count is hex"),
        "{stderr}"
    );
    assert!(stderr.contains("half_a:0x36"), "{stderr}");
    assert!(stderr.contains("half_a:#36 for 36 bytes"), "{stderr}");
    // The explicit spellings are silent.
    let (bytes, stderr) = peek("half_a:#36");
    assert_eq!(bytes, 36);
    assert!(!stderr.contains("note: --peek"), "{stderr}");
    let (bytes, stderr) = peek("half_a:0x36");
    assert_eq!(bytes, 0x36);
    assert!(!stderr.contains("note: --peek"), "{stderr}");
    // A count with one reading needs no note.
    assert!(!peek("half_a:2").1.contains("note: --peek"));
}

fn luna_test(dir: &Path, manifest: &str, body: &str, extra: &[&str]) -> Output {
    std::fs::write(dir.join(manifest), body).unwrap();
    Command::new(luna_bin())
        .arg("test")
        .arg(manifest)
        .args(extra)
        .current_dir(dir)
        .output()
        .expect("run luna test")
}

#[test]
fn a_checkpoint_fires_on_a_symbol() {
    let dir = fresh_dir("at_symbol");
    slow_tick_rom(&dir);
    let head = "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\nframes = 40\n";
    // Arrivals count from power-on, across checkpoints and across the
    // frame checkpoint between them.
    let out = luna_test(
        &dir,
        "ticks.toml",
        &format!(
            r#"{head}
[[checkpoint]]
at_symbol = "tick_end"
[checkpoint.values]
half_a = 1
half_b = 1

[[checkpoint]]
at_frame = 9
[checkpoint.values]
half_a = {{ ge = 2, width = 1 }}

[[checkpoint]]
at_symbol = "tick_end"
hit = 5
[checkpoint.values]
half_a = 5
half_b = 5
[checkpoint.delta]
half_b = {{ dir = "increased", width = 1 }}

[[checkpoint]]
at_symbol = "tick"
hit = 7
[checkpoint.values]
half_a = 6
half_b = 6
"#
        ),
        &["--report", "json"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap();
    let cps = json["tests"][0]["symbol_checkpoints"].as_array().unwrap();
    assert_eq!(cps.len(), 3, "{json:#}");
    assert_eq!(cps[1]["at_symbol"], "tick_end");
    assert_eq!(
        (&cps[1]["hit"], &cps[1]["reached"]),
        (&5.into(), &true.into())
    );
    assert!(cps[1]["frame"].as_u64().unwrap() > cps[0]["frame"].as_u64().unwrap());

    // A wrong value names the routine, the arrival and where it happened.
    let out = luna_test(
        &dir,
        "wrong.toml",
        &format!(
            "{head}[[checkpoint]]\nat_symbol = \"tick_end\"\nhit = 2\n[checkpoint.values]\nhalf_b = 9\n"
        ),
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("checkpoint@tick_end#2 (frame "), "{stdout}");
    assert!(stdout.contains("half_b"), "{stdout}");

    // A routine the run never reaches fails that checkpoint, and says how
    // far it got; its asserts are not evaluated on some other state.
    let out = luna_test(
        &dir,
        "far.toml",
        &format!(
            "{head}[[checkpoint]]\nat_symbol = \"tick_end\"\nhit = 99\n[checkpoint.values]\nhalf_b = 99\n"
        ),
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("checkpoint@tick_end#99: not reached by frame 40 (reached 13 time(s))"),
        "{stdout}"
    );
    assert!(!stdout.contains("values.half_b"), "{stdout}");

    // A frame checkpoint placed before the stop of the previous one.
    let out = luna_test(
        &dir,
        "past.toml",
        &format!(
            "{head}[[checkpoint]]\nat_symbol = \"tick_end\"\nhit = 5\n\n[[checkpoint]]\nat_frame = 3\n"
        ),
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("checkpoint@3: frame 3 was already past"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_malformed_symbol_checkpoint_is_a_usage_error() {
    let dir = fresh_dir("at_symbol_usage");
    slow_tick_rom(&dir);
    let head = "rom = \"game.sfc\"\nforce_mapper = \"lorom\"\n";
    for (name, body, why) in [
        (
            "both",
            "frames = 20\n[[checkpoint]]\nat_frame = 3\nat_symbol = \"tick\"\n",
            "exactly one of `at_frame` and `at_symbol`",
        ),
        (
            "neither",
            "frames = 20\n[[checkpoint]]\n[checkpoint.values]\nhalf_a = 1\n",
            "exactly one of `at_frame` and `at_symbol`",
        ),
        (
            "hit_alone",
            "frames = 20\n[[checkpoint]]\nat_frame = 3\nhit = 2\n",
            "`hit` goes with `at_symbol`",
        ),
        (
            "hit_order",
            "frames = 20\n[[checkpoint]]\nat_symbol = \"tick\"\nhit = 3\n[[checkpoint]]\nat_symbol = \"tick\"\nhit = 3\n",
            "must increase",
        ),
        (
            "no_horizon",
            "[[checkpoint]]\nat_symbol = \"tick\"\n",
            "need `frames`",
        ),
        (
            "unknown",
            "frames = 20\n[[checkpoint]]\nat_symbol = \"nowhere\"\n",
            "at_symbol `nowhere`",
        ),
    ] {
        let out = luna_test(&dir, &format!("{name}.toml"), &format!("{head}{body}"), &[]);
        assert_eq!(out.status.code(), Some(2), "{name}: {}", text(&out));
        assert!(text(&out).contains(why), "{name}: {}", text(&out));
    }
}

#[test]
fn profile_gives_the_cost_of_each_frame_and_gates_on_it() {
    let dir = fresh_dir("profile_frames");
    let rom = slow_tick_rom(&dir);
    let (csv, json) = (dir.join("frames.csv"), dir.join("profile.json"));
    let window = ["--from-frame", "2", "--until-frame", "14"];
    let out = luna(
        "profile",
        &rom,
        &[
            &window[..],
            &[
                "--frames-out",
                csv.to_str().unwrap(),
                "--worst",
                "2",
                "--out",
                json.to_str().unwrap(),
            ],
        ]
        .concat(),
    );
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("frames: 12 completed, active mean "),
        "{stdout}"
    );
    assert!(stdout.contains("longest run 2 (from frame "), "{stdout}");
    assert_eq!(stdout.matches("worst frame ").count(), 2, "{stdout}");

    let rows: Vec<Vec<u64>> = std::fs::read_to_string(&csv)
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| l.split(',').map(|c| c.parse().unwrap()).collect())
        .collect();
    assert_eq!(rows.len(), 12);
    assert_eq!(rows[0][0], 2);
    for r in &rows {
        // frame,active,idle,cpu,dma,hdma,refresh,total,nmi,lag
        assert_eq!(r[1] + r[2] + r[5] + r[6], r[7], "{r:?}");
        assert_eq!(r[1], r[3] + r[4], "{r:?}");
        assert_eq!(r[8], 1, "{r:?}");
        // A frame whose NMI found the CPU working has no idle time
        // before it; one that waited has.
        assert_eq!(r[9] == 0, r[2] > r[7] / 2, "{r:?}");
    }
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["frame_series"].as_array().unwrap().len(), 12);
    assert_eq!(v["frame_series"][0]["active_mclk"], rows[0][1]);
    let s = &v["frame_summary"];
    assert_eq!((&s["frames"], &s["lag_run"]), (&12.into(), &2.into()));
    assert_eq!(s["lag_frames"], 8);
    // Every run of two or more, with its first frame: how many, how regular.
    let runs = s["lag_runs"].as_array().unwrap();
    // (The window opens on the last frame of a tick: a single lag frame.)
    assert_eq!(runs.len(), 3, "{s:#}");
    assert!(runs.iter().all(|r| r["length"] == 2));
    let starts: Vec<u64> = runs.iter().map(|r| r["frame"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[1] - w[0] == 3), "{starts:?}");
    assert!(
        stdout.contains("lag runs of 2 or more: 3 (from frame 4, 7, 10)"),
        "{stdout}"
    );
    let worst = v["worst_frames"].as_array().unwrap();
    assert_eq!(worst.len(), 2);
    assert_eq!(worst[0]["active_mclk"], s["active_max"]);
    assert_eq!(worst[0]["frame"], s["active_max_frame"]);
    assert_eq!(worst[0]["entries"][0]["symbol"], "tick");

    // The gates: this tick spills into a third frame, two lag frames in
    // a row. A two-frame tick would lag once.
    let gate = |args: &[&str]| luna("profile", &rom, &[&window[..], args].concat());
    let out = gate(&["--max-lag-run", "1"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("gate: max-lag-run 2 (frame "),
        "{}",
        text(&out)
    );
    assert!(gate(&["--max-lag-run", "2"]).status.success());
    assert_eq!(gate(&["--max-lag-frames", "7"]).status.code(), Some(1));
    assert!(gate(&["--max-lag-frames", "8"]).status.success());
    let max = s["active_max"].as_u64().unwrap();
    assert_eq!(
        gate(&["--max-frame-mclk", &(max - 1).to_string()])
            .status
            .code(),
        Some(1)
    );
    assert!(
        gate(&["--max-frame-mclk", &max.to_string()])
            .status
            .success()
    );
}
