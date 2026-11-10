//! `luna race-sweep`: an interrupt forced before each instruction of a
//! routine, compared with the run that was not interrupted.
//!
//! The ROMs are synthetic. `counter_rom` keeps a 16-bit counter at `$20/$21`
//! that its main loop raises one iteration at a time (40 of them) and its
//! interrupt handler raises as well, counting its own entries in `$23`; the
//! invariant `$20 - $23 = 40` is computed at the end into `$24`, which is
//! what the sweep compares. The main loop's read-modify-write of `$20` is
//! three instructions between a load and a store: an interrupt there loses
//! the handler's increment. `wake_rom` is the other classic: a loop that
//! tests a flag the NMI handler sets, then `WAI`s; an NMI that lands between
//! the test and the `WAI` is never followed by another.

use std::fmt::Write as _;
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
    let dir = std::env::temp_dir().join(format!("luna_race_sweep_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tiny assembler: bytes at `$8000+`, named positions for the `.sym`.
struct Asm {
    base: u16,
    code: Vec<u8>,
    labels: Vec<(String, u16)>,
}

impl Asm {
    const fn new() -> Self {
        Self::at_base(0x8000)
    }
    const fn at_base(base: u16) -> Self {
        Self {
            base,
            code: Vec::new(),
            labels: Vec::new(),
        }
    }
    const fn at(&self) -> u16 {
        self.base + self.code.len() as u16
    }
    fn label(&mut self, name: &str) {
        self.labels.push((name.to_string(), self.at()));
    }
    fn op(&mut self, name: &str, bytes: &[u8]) {
        self.label(name);
        self.code.extend_from_slice(bytes);
    }
    /// A branch to a label already defined (backwards).
    fn back(&mut self, name: &str, opcode: u8, to: &str) {
        let target = self.labels.iter().find(|(n, _)| n == to).unwrap().1;
        let next = self.at() + 2;
        self.op(name, &[opcode, (i32::from(target) - i32::from(next)) as u8]);
    }
}

/// Write `game.sfc` and `game.sym` into `dir`: `main` at `$8000`, the
/// handler (NMI and IRQ vectors) at its own base.
fn write_rom(dir: &Path, main: &Asm, handler: &Asm) -> PathBuf {
    let mut r = vec![0u8; 0x1_0000];
    r[..main.code.len()].copy_from_slice(&main.code);
    let h = usize::from(handler.base - 0x8000);
    assert!(main.code.len() <= h);
    r[h..h + handler.code.len()].copy_from_slice(&handler.code);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA RACE SWEEP      ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA, 0x7FEE, 0x7FFE] {
        r[off..off + 2].copy_from_slice(&handler.base.to_le_bytes());
    }
    let rom = dir.join("game.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    let mut sym = String::from("[labels]\n");
    for (n, a) in main.labels.iter().chain(handler.labels.iter()) {
        let _ = writeln!(sym, "00:{a:04X} {n}");
    }
    sym.push_str("7e:0020 counter\n7e:0023 handled\n7e:0024 invariant\n7e:0030 flag\n");
    std::fs::write(dir.join("game.sym"), sym).unwrap();
    rom
}

/// How the counter's read-modify-write is guarded.
#[derive(Clone, Copy, PartialEq)]
enum Guard {
    /// Load, add, store: three byte-sized steps.
    None,
    /// The same, between `SEI` and `CLI` (keeps IRQs out, not NMIs).
    Sei,
    /// One `INC $20`: a single instruction, nothing can split it.
    Atomic,
}

/// The counter game. `nmi_on`: reset writes `$4200 = $80`.
fn counter_rom(dir: &Path, nmi_on: bool, guard: Guard) -> PathBuf {
    let mut m = Asm::new();
    m.op("reset", &[0x78]); // SEI
    m.op("setup", &[0xA2, 0xFF, 0x9A]); // LDX #$FF ; TXS
    m.op("zero", &[0x64, 0x20, 0x64, 0x21, 0x64, 0x22, 0x64, 0x23]); // STZ $20..$23
    m.op(
        "enable",
        &[0xA9, if nmi_on { 0x80 } else { 0x00 }, 0x8D, 0x00, 0x42],
    );
    m.op("cli", &[0x58]);
    m.label("loop");
    if guard == Guard::Sei {
        m.op("sei", &[0x78]);
    }
    if guard == Guard::Atomic {
        m.op("load", &[0xE6, 0x20]); // INC $20
        m.op("bne", &[0xD0, 0x02]); // BNE +2
    } else {
        m.op("load", &[0xA5, 0x20]); // LDA $20
        m.op("clc", &[0x18]);
        m.op("adc", &[0x69, 0x01]);
        m.op("store", &[0x85, 0x20]); // STA $20
        if guard == Guard::Sei {
            m.op("unguard", &[0x58]); // CLI
        }
        m.op("bcc", &[0x90, 0x02]); // BCC +2
    }
    m.op("carry", &[0xE6, 0x21]); // INC $21
    m.op("count", &[0xE6, 0x22]); // INC $22
    m.op("lda22", &[0xA5, 0x22]);
    m.op("cmp", &[0xC9, 40]);
    m.back("loop_end", 0xD0, "loop"); // BNE loop
    m.op("check", &[0x9C, 0x00, 0x42]); // STZ $4200
    m.op("diff", &[0xA5, 0x20, 0x38, 0xE5, 0x23, 0x85, 0x24]); // $24 = $20 - $23
    m.op("end", &[0x80, 0xFE]); // BRA end
    let mut h = Asm::at_base(0x8100);
    h.op(
        "handler",
        &[0xE6, 0x20, 0xD0, 0x02, 0xE6, 0x21, 0xE6, 0x23, 0x40],
    );
    write_rom(dir, &m, &h)
}

/// The lost wake-up: wait for the flag the NMI sets; the handler also turns
/// the NMI off, so no second one comes.
fn wake_rom(dir: &Path) -> PathBuf {
    let mut m = Asm::new();
    m.op("reset", &[0x78, 0xA2, 0xFF, 0x9A]); // SEI ; LDX #$FF ; TXS
    m.op("zero", &[0x64, 0x30]); // STZ $30
    m.op("enable", &[0xA9, 0x80, 0x8D, 0x00, 0x42]); // NMI on
    m.label("wait");
    m.op("test", &[0xA5, 0x30]); // LDA $30
    m.op("bne", &[0xD0, 0x03]); // BNE done
    m.op("wai", &[0xCB]); // WAI
    m.op("again", &[0x80, 0xF9]); // BRA wait
    m.op("done", &[0xEA]);
    m.op("park", &[0x80, 0xFE]);
    let mut h = Asm::at_base(0x8100);
    h.op(
        "handler",
        &[0xA9, 0x01, 0x85, 0x30, 0x9C, 0x00, 0x42, 0x40], // LDA #1 ; STA $30 ; STZ $4200 ; RTI
    );
    write_rom(dir, &m, &h)
}

fn sweep(rom: &Path, args: &[&str]) -> Output {
    Command::new(luna_bin())
        .arg("race-sweep")
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

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("json: {e}\n{}", text(out)))
}

const COUNTER_ARGS: [&str; 14] = [
    "--warm-until-pc",
    "loop",
    "--from",
    "loop",
    "--to",
    "loop_end",
    "--until-pc",
    "end",
    "--compare",
    "invariant",
    "--json",
    "--kind",
    "nmi",
    "--fail-on-race",
];

/// Trials by symbol, restricted to `verdict`.
fn symbols_with(v: &serde_json::Value, kind: &str, verdict: &str) -> Vec<String> {
    v["trials"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == kind && t["verdict"] == verdict)
        .map(|t| t["symbol"].as_str().unwrap().to_string())
        .collect()
}

/// (A) The unguarded read-modify-write: exactly the instructions between
/// the load and the store are racy, and the safe ones are not.
#[test]
fn the_window_between_the_load_and_the_store_is_the_race() {
    let dir = fresh_dir("window");
    let rom = counter_rom(&dir, true, Guard::None);
    let out = sweep(&rom, &COUNTER_ARGS);
    assert_eq!(out.status.code(), Some(1), "--fail-on-race\n{}", text(&out));
    let v = json(&out);
    assert_eq!(v["schema"], 1);
    assert_eq!(
        symbols_with(&v, "nmi", "differs"),
        ["clc", "adc", "store"],
        "{}",
        text(&out)
    );
    assert_eq!(v["summary"]["racy_addresses"], 3);
    assert_eq!(v["summary"]["not_taken"], 0);
    // 9 instructions run in the loop (the carry's `INC` never does), 3 racy.
    assert_eq!(v["summary"]["addresses_swept"], 9);
    assert_eq!(v["summary"]["identical"], 6);
    // The lost increment: the invariant reads 39, not 40.
    let t = v["trials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["symbol"] == "store")
        .unwrap();
    assert_eq!(t["diffs"][0]["symbol"], "invariant");
    assert_eq!(t["diffs"][0]["baseline"], 40);
    assert_eq!(t["diffs"][0]["injected"], 39);
}

/// (B) A single `INC` cannot be split: nothing is racy, and the exit code
/// under `--fail-on-race` is 0.
#[test]
fn an_atomic_increment_has_no_race() {
    let dir = fresh_dir("atomic");
    let rom = counter_rom(&dir, true, Guard::Atomic);
    let out = sweep(&rom, &COUNTER_ARGS);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let v = json(&out);
    assert_eq!(v["summary"]["racy_addresses"], 0);
    assert_eq!(v["summary"]["differs"], 0);
    assert_eq!(v["summary"]["addresses_swept"], 6);
    assert_eq!(v["summary"]["identical"], 6);
}

/// (C) `SEI`/`CLI` keeps an IRQ out of the window and not an NMI. The
/// refused IRQs are reported as not taken, never as passes; the NMIs still
/// find the race.
#[test]
fn sei_keeps_irqs_out_but_the_refusals_are_reported() {
    let dir = fresh_dir("sei");
    let rom = counter_rom(&dir, true, Guard::Sei);
    let mut args = COUNTER_ARGS.to_vec();
    args.retain(|a| *a != "--fail-on-race");
    let kind = args.iter().position(|a| *a == "nmi").unwrap();
    args[kind] = "both";
    let out = sweep(&rom, &args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let v = json(&out);
    assert_eq!(
        symbols_with(&v, "irq", "not_taken"),
        ["load", "clc", "adc", "store", "unguard"],
        "{}",
        text(&out)
    );
    assert!(symbols_with(&v, "irq", "differs").is_empty());
    // `SEI` does not hold an NMI back: the window is as open as ever.
    assert_eq!(symbols_with(&v, "nmi", "differs"), ["clc", "adc", "store"]);
    let reason = v["trials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["verdict"] == "not_taken")
        .unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains("I flag"), "{reason}");
    // The text says it too.
    let mut text_args = args.clone();
    text_args.retain(|a| *a != "--json");
    let out = sweep(&rom, &text_args);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("NOT TAKEN: not tested"), "{s}");
    assert!(s.contains("DIFFERS"), "{s}");
}

/// (D) Without the NMI enabled, every injection is refused: the sweep tested
/// nothing, and `--fail-on-race` says so.
#[test]
fn a_sweep_where_nothing_was_injected_is_not_green() {
    let dir = fresh_dir("nmi_off");
    let rom = counter_rom(&dir, false, Guard::None);
    let out = sweep(&rom, &COUNTER_ARGS);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let v = json(&out);
    assert_eq!(v["summary"]["not_taken"], v["summary"]["trials"]);
    assert_eq!(v["summary"]["identical"], 0);
    assert_eq!(v["summary"]["racy_addresses"], 0);
}

/// (E) The lost wake-up: an NMI between the flag test and the `WAI` leaves
/// the program waiting for an NMI that already came. The end is never
/// reached, which is a finding.
#[test]
fn a_lost_wake_up_is_a_trial_that_never_ends() {
    let dir = fresh_dir("wake");
    let rom = wake_rom(&dir);
    let out = sweep(
        &rom,
        &[
            "--from",
            "wait",
            "--to",
            "wai",
            "--until-pc",
            "done",
            "--compare",
            "flag",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let v = json(&out);
    assert_eq!(
        symbols_with(&v, "nmi", "end_not_reached"),
        ["bne", "wai"],
        "{}",
        text(&out)
    );
    assert_eq!(symbols_with(&v, "nmi", "identical"), ["test"]);
    assert_eq!(v["summary"]["racy_addresses"], 2);
}

/// (F) The report does not depend on the number of workers, nor on the run.
#[test]
fn the_report_is_the_same_for_any_jobs() {
    let dir = fresh_dir("jobs");
    let rom = counter_rom(&dir, true, Guard::None);
    let mut base = COUNTER_ARGS.to_vec();
    base.retain(|a| *a != "--fail-on-race");
    let one = sweep(&rom, &base);
    let again = sweep(&rom, &base);
    let mut four = base.clone();
    four.extend(["--jobs", "4"]);
    let four = sweep(&rom, &four);
    assert_eq!(one.stdout, again.stdout, "two runs");
    assert_eq!(one.stdout, four.stdout, "jobs 4 vs jobs 1");
    assert!(!one.stdout.is_empty());
}

/// (G) The end by frame, `--hit`, `--out`, and the usage errors (exit 2).
#[test]
fn the_end_by_frame_and_the_usage_errors() {
    let dir = fresh_dir("usage");
    let rom = counter_rom(&dir, true, Guard::None);
    // A frame end: the counter loop is long done by frame 2, the invariant holds.
    let out_file = dir.join("report.txt");
    let out = sweep(
        &rom,
        &[
            "--from",
            "loop",
            "--to",
            "loop_end",
            "--until-frame",
            "2",
            "--compare",
            "invariant",
            "--out",
            out_file.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(out.stdout.is_empty());
    let report = std::fs::read_to_string(&out_file).unwrap();
    assert!(report.contains("9 addresses swept"), "{report}");
    // Usage errors.
    for args in [
        vec!["--from", "loop", "--to", "loop_end"],
        vec!["--from", "loop_end", "--to", "loop", "--until-pc", "end"],
        vec!["--from", "nowhere", "--to", "loop_end", "--until-pc", "end"],
        vec![
            "--from",
            "loop",
            "--to",
            "loop_end",
            "--until-pc",
            "end",
            "--until-frame",
            "9",
        ],
        vec![
            "--from",
            "loop",
            "--to",
            "loop_end",
            "--until-pc",
            "end",
            "--kind",
            "fiq",
        ],
    ] {
        let out = sweep(&rom, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}\n{}", text(&out));
    }
}

/// (H) The `--compare` forms: a symbol, `SYMBOL:COUNT`, `BANK:OFFSET`,
/// `BANK:OFFSET:COUNT`; an unknown one is a usage error.
#[test]
fn the_compare_forms() {
    let dir = fresh_dir("compare");
    let rom = counter_rom(&dir, true, Guard::None);
    let out = sweep(
        &rom,
        &[
            "--from",
            "loop",
            "--to",
            "loop_end",
            "--until-pc",
            "end",
            "--json",
            "--compare",
            "invariant",
            "--compare",
            "counter:2",
            "--compare",
            "7e:0022",
            "--compare",
            "7e:0020:3",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let counts: Vec<(String, u64)> = json(&out)["compare"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["address_hex"].as_str().unwrap().to_string(),
                c["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        counts,
        [
            ("$7E:0024".to_string(), 1),
            ("$7E:0020".to_string(), 2),
            ("$7E:0022".to_string(), 1),
            ("$7E:0020".to_string(), 3),
        ]
    );
    let out = sweep(
        &rom,
        &[
            "--from",
            "loop",
            "--to",
            "loop_end",
            "--until-pc",
            "end",
            "--compare",
            "nothing",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}
