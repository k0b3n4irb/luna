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
