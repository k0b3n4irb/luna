//! Regression tests for `--*-trace-from` under `--until-frame`.
//!
//! The defect (reported by `OpenSNES`, 2026-09-29): the bridge that steps to
//! a trace's start instruction spent from `-n`, which keeps its unused
//! default of 1000 when the run is bounded by `--until-frame`. Every trace
//! starting after instruction ~1000 never switched on (CPU, memory), or
//! switched on at ~1000 instead of where it was asked (DMA, Super FX) — and
//! the documented recipe (take `stats.instructions_executed` at frame N,
//! pass it as `-from`) led straight to it.
//!
//! These drive the built `luna` binary on a synthetic `LoROM` image: a
//! trace window must be the same whether the run is bounded by `-n` or by
//! `--until-frame`, and the run must still end on the frame asked for.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the `luna` binary under test (same profile as the test run).
fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop(); // deps/
    p.pop(); // <profile>/
    p.push("luna");
    p
}

/// A `LoROM` that counts in WRAM forever, so both traces have something to
/// record at any instruction: `$8000: INC $10 ; BRA -4`.
fn counting_rom(path: &Path) {
    let mut rom = vec![0u8; 0x1_0000];
    rom[0x0000..0x0004].copy_from_slice(&[0xE6, 0x10, 0x80, 0xFC]);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA TRACE FROM      ".as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x07; // size code
    rom[0x7FFC] = 0x00; // reset vector -> $8000
    rom[0x7FFD] = 0x80;
    std::fs::write(path, &rom).expect("write counting rom");
}

/// Run `luna state`, return the reported frame count.
fn run(rom: &Path, extra: &[&str]) -> u64 {
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(rom)
        .args(["--force-mapper", "lorom", "--out", "-"])
        .args(extra)
        .output()
        .expect("run luna state");
    assert!(
        out.status.success(),
        "luna state failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("state JSON on stdout");
    json["scheduler"]["frame_count"]
        .as_u64()
        .expect("scheduler.frame_count")
}

/// The data rows of a trace CSV (header dropped).
fn rows(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("read trace")
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_trace_window_is_the_same_under_until_frame_and_under_n() {
    let dir = std::env::temp_dir().join(format!("luna-trace-from-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("count.sfc");
    counting_rom(&rom);
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();

    // Well past the old 1000-instruction ceiling, well before frame 12.
    let from = "50000";
    let traces = |cpu: &str, mem: &str| {
        vec![
            "--cpu-trace".to_owned(),
            cpu.to_owned(),
            "--cpu-trace-from".to_owned(),
            from.to_owned(),
            "--cpu-trace-max".to_owned(),
            "200".to_owned(),
            "--mem-trace".to_owned(),
            mem.to_owned(),
            "--mem-trace-from".to_owned(),
            from.to_owned(),
            "--mem-trace-max".to_owned(),
            "200".to_owned(),
        ]
    };

    let by_frame = traces(&p("cpu_f.csv"), &p("mem_f.csv"));
    let mut args: Vec<&str> = vec!["--until-frame", "12"];
    args.extend(by_frame.iter().map(String::as_str));
    assert_eq!(
        run(&rom, &args),
        12,
        "the run still ends on the frame asked for"
    );

    let by_steps = traces(&p("cpu_n.csv"), &p("mem_n.csv"));
    let mut args: Vec<&str> = vec!["-n", "300000"];
    args.extend(by_steps.iter().map(String::as_str));
    run(&rom, &args);

    for (kind, f, n) in [
        ("cpu", "cpu_f.csv", "cpu_n.csv"),
        ("mem", "mem_f.csv", "mem_n.csv"),
    ] {
        let (f, n) = (rows(&dir.join(f)), rows(&dir.join(n)));
        assert_eq!(f.len(), 200, "{kind}: the --until-frame trace switched on");
        assert_eq!(f, n, "{kind}: same window as with -n");
    }

    // A start beyond the frame records nothing and does not stretch the run.
    let late = p("cpu_late.csv");
    let args = [
        "--until-frame",
        "12",
        "--cpu-trace",
        late.as_str(),
        "--cpu-trace-from",
        "100000000",
    ];
    assert_eq!(run(&rom, &args), 12, "a late start does not extend the run");
    assert!(rows(Path::new(&late)).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
