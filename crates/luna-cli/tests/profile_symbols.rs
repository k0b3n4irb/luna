//! `luna profile` (issue #227): real master cycles per symbol.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

/// The idle ROM (`SEI ; LDA #$80 ; STA $4200 ; loop: WAI ; BRA loop ;
/// nmi: RTI`) with a `.sym` beside it naming its three parts.
fn rom_with_sym(dir: &Path) -> PathBuf {
    let prog = [0x78, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xCB, 0x80, 0xFD, 0x40];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA PROFILE         ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA] {
        r[off] = 0x09;
        r[off + 1] = 0x80;
    }
    let rom = dir.join("game.sfc");
    std::fs::write(&rom, &r).expect("write rom");
    std::fs::write(
        dir.join("game.sym"),
        "[labels]\n00:8000 main\n00:8006 wait_vblank\n00:8009 nmi_handler\n",
    )
    .unwrap();
    rom
}

#[test]
fn profile_reports_symbols_heaviest_first_with_json() {
    let dir = std::env::temp_dir().join("luna_profile_symbols");
    let _ = std::fs::create_dir_all(&dir);
    let rom = rom_with_sym(&dir);
    let json = dir.join("profile.json");
    let out = Command::new(luna_bin())
        .arg("profile")
        .arg(&rom)
        .args([
            "--force-mapper",
            "lorom",
            "--from-frame",
            "2",
            "--until-frame",
            "6",
            "--top",
            "1",
        ])
        .arg("--out")
        .arg(&json)
        .output()
        .expect("run luna profile");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("profile: frames 2..6"), "{stdout}");
    // One row printed (the heaviest), the rest referred to the JSON.
    let rows: Vec<&str> = stdout.lines().filter(|l| l.contains('%')).skip(1).collect();
    assert_eq!(rows.len(), 1, "{stdout}");
    assert!(rows[0].contains("wait_vblank"), "{stdout}");
    assert!(
        stdout.contains("1 more (raise --top or read --out)"),
        "{stdout}"
    );

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["from_frame"], 2);
    assert_eq!(v["end_frame"], 6);
    let entries = v["entries"].as_array().unwrap();
    // `main` ran before frame 2, so it is not in a profile that starts
    // there: only the wait loop and the handler executed.
    assert_eq!(entries.len(), 2, "{v}");
    assert_eq!(entries[0]["symbol"], "wait_vblank");
    assert!(entries[0]["pct"].as_f64().unwrap() > 90.0);
    assert!(entries[0]["idle_mclk"].as_u64().unwrap() > 0);
    assert_eq!(entries[0]["pcs"], 2, "WAI + BRA");
    assert_eq!(entries[1]["symbol"], "nmi_handler");
    assert_eq!(
        entries[1]["instructions"], 4,
        "one RTI per frame, frames 2..6"
    );
    let total: u64 = entries.iter().map(|e| e["mclk"].as_u64().unwrap()).sum();
    assert_eq!(total, v["total_mclk"].as_u64().unwrap());
}

/// `--pc-set` (`OpenSNES` R-C) writes every executed 24-bit PC once,
/// sorted, as little-endian `u32`s — the raw input of a coverage tool.
#[test]
fn profile_pc_set_lists_executed_pcs_sorted() {
    let dir = std::env::temp_dir().join("luna_profile_pc_set");
    let _ = std::fs::create_dir_all(&dir);
    let rom = rom_with_sym(&dir);
    let set = dir.join("pcs.bin");
    let out = Command::new(luna_bin())
        .arg("profile")
        .arg(&rom)
        .args([
            "--force-mapper",
            "lorom",
            "--from-frame",
            "2",
            "--until-frame",
            "6",
        ])
        .arg("--pc-set")
        .arg(&set)
        .output()
        .expect("run luna profile");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&set).expect("pc set written");
    let pcs: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // Frames 2..6 only run the wait loop (WAI at $8006, BRA at $8007) and
    // the handler's RTI at $8009; `main` finished before the window.
    assert_eq!(pcs, vec![0x8006, 0x8007, 0x8009], "{pcs:x?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pc-set: 3 distinct PCs"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--budget SYMBOL=MCLK` (`OpenSNES` R-B) gates on the symbol's worst
/// completed frame: exit 1 on overrun with the frame named, exit 0 under
/// it, exit 2 for a symbol nobody knows; the JSON carries `per_frame`
/// per row and one verdict per gate.
#[test]
fn profile_budget_gates_on_the_worst_frame() {
    let dir = std::env::temp_dir().join("luna_profile_budget");
    let _ = std::fs::create_dir_all(&dir);
    let rom = rom_with_sym(&dir);
    let json = dir.join("budget.json");
    let run = |budgets: &[&str], out: Option<&std::path::Path>| {
        let mut cmd = Command::new(luna_bin());
        cmd.arg("profile").arg(&rom).args([
            "--force-mapper",
            "lorom",
            "--from-frame",
            "2",
            "--until-frame",
            "6",
        ]);
        for b in budgets {
            cmd.args(["--budget", b]);
        }
        if let Some(o) = out {
            cmd.arg("--out").arg(o);
        }
        cmd.output().expect("run luna profile")
    };

    // Under budget: the handler is one RTI per frame.
    let out = run(&["nmi_handler=100000", "main=1"], Some(&json));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("budget: nmi_handler max "), "{stdout}");
    assert!(stdout.contains("<= 100000 — ok"), "{stdout}");
    // `main` is a known label that finished before the window: passes.
    assert!(
        stdout.contains("budget: main never ran in a completed frame — ok"),
        "{stdout}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["frames"], 4, "{v}");
    let nmi = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["symbol"] == "nmi_handler")
        .unwrap();
    assert_eq!(nmi["per_frame"]["frames"], 4, "{v}");
    assert!(nmi["per_frame"]["max"].as_u64().unwrap() > 0, "{v}");
    assert!(nmi["per_frame"]["max_frame"].as_u64().unwrap() >= 2, "{v}");
    let budgets = v["budgets"].as_array().unwrap();
    assert_eq!(budgets.len(), 2);
    assert_eq!(budgets[0]["symbol"], "nmi_handler");
    assert_eq!(budgets[0]["ok"], true);
    assert_eq!(budgets[1]["symbol"], "main");
    assert!(budgets[1]["max"].is_null());
    assert_eq!(budgets[1]["ok"], true);

    // Over budget: exit 1, the frame named, the table still printed.
    let out = run(&["nmi_handler=1"], None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("> 1 — OVER"), "{stdout}");
    assert!(stdout.contains("(frame "), "{stdout}");
    assert!(stdout.contains("max/frame"), "{stdout}");

    // A typo is a usage error, not a pass.
    let out = run(&["NmiHandler=6000"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown symbol"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&["nmi_handler"], None);
    assert_eq!(out.status.code(), Some(2));
}
