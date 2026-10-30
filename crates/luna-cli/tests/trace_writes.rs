//! `luna state --mem-trace … --trace-writes 2121,2122` (issue #226): the
//! "who wrote this register" hunt — CPU and DMA / HDMA writes in one CSV,
//! each row tagged by `origin`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn luna_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    p.pop();
    p.push("luna");
    p
}

/// One CPU write to `$2122`, then a 4-byte DMA (channel 0) from `$7E:2000`
/// to `$2122`, then `STP`.
fn rom(path: &Path) {
    // Both DAS bytes are written, as a real game must: the channel
    // registers power up at $FF (issue #224), so leaving $4306 alone would
    // ask for $FF04 bytes instead of 4.
    let prog = [
        0xA9, 0x55, 0x8D, 0x22, 0x21, // LDA #$55 : STA $2122 (CPU write)
        0xA9, 0x22, 0x8D, 0x01, 0x43, // BBAD  = $22
        0xA9, 0x00, 0x8D, 0x02, 0x43, // A1TL  = $00
        0xA9, 0x20, 0x8D, 0x03, 0x43, // A1TH  = $20
        0xA9, 0x7E, 0x8D, 0x04, 0x43, // A1B   = $7E
        0xA9, 0x04, 0x8D, 0x05, 0x43, // DAS   low  = $04
        0xA9, 0x00, 0x8D, 0x06, 0x43, // DAS   high = $00
        0xA9, 0x00, 0x8D, 0x00, 0x43, // DMAP  = $00
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // MDMAEN bit 0
        0xDB, // STP
    ];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA TRACE WRITES    ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    std::fs::write(path, &r).expect("write rom");
}

#[test]
fn trace_writes_csv_tags_cpu_and_dma_rows_by_origin() {
    let dir = std::env::temp_dir().join("luna_trace_writes");
    let _ = std::fs::create_dir_all(&dir);
    let rom_path = dir.join("game.sfc");
    rom(&rom_path);
    let csv = dir.join("writes.csv");
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom_path)
        .args(["--force-mapper", "lorom", "-n", "100", "--out", "/dev/null"])
        .arg("--mem-trace")
        .arg(&csv)
        .args(["--trace-writes", "2121,2122"])
        .output()
        .expect("run luna state");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&csv).unwrap();
    let mut lines = text.lines();
    let header = lines.next().unwrap();
    assert!(header.ends_with(",force_blank,origin,symbol"), "{header}");
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').collect()).collect();
    assert_eq!(rows.len(), 5, "{text}");
    // Every row: a write to $00:2122 (the list filter + writes-only).
    assert!(
        rows.iter().all(|r| r[3] == "$00:2122" && r[4] == "W"),
        "{text}"
    );
    assert_eq!(rows[0][5], "$55");
    assert_eq!(rows[0][10], "cpu");
    // No `.sym` beside this ROM: the column is there and empty.
    assert!(
        rows.iter().all(|r| r.len() == 12 && r[11].is_empty()),
        "{text}"
    );
    assert!(rows[1..].iter().all(|r| r[10] == "dma0"), "{text}");
    let dma: Vec<&str> = rows[1..].iter().map(|r| r[5]).collect();
    // WRAM is zero, so the four DMA bytes are $00 (the origin is the point).
    assert_eq!(dma, vec!["$00"; 4]);
}

#[test]
fn trace_writes_requires_mem_trace() {
    let dir = std::env::temp_dir().join("luna_trace_writes_usage");
    let _ = std::fs::create_dir_all(&dir);
    let rom_path = dir.join("game.sfc");
    rom(&rom_path);
    let out = Command::new(luna_bin())
        .arg("state")
        .arg(&rom_path)
        .args(["--force-mapper", "lorom", "--trace-writes", "2122"])
        .output()
        .expect("run luna state");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--mem-trace"), "{err}");
}

#[test]
fn no_render_is_refused_with_an_option_that_reads_the_picture() {
    let dir = std::env::temp_dir().join("luna_no_render");
    let _ = std::fs::create_dir_all(&dir);
    let rom_path = dir.join("game.sfc");
    rom(&rom_path);
    let run = |extra: &[&str]| {
        Command::new(luna_bin())
            .arg("state")
            .arg(&rom_path)
            .args(["--force-mapper", "lorom", "-n", "100", "--out", "-"])
            .args(extra)
            .output()
            .expect("run luna state")
    };
    let png = dir.join("x.png");
    for reader in [
        vec!["--screenshot", png.to_str().unwrap()],
        vec!["--print-fbhash"],
        vec!["--native-res"],
    ] {
        let mut args = vec!["--no-render"];
        args.extend(reader);
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    // Alone it runs, and reports the same machine.
    let (with, without) = (run(&[]), run(&["--no-render"]));
    assert!(without.status.success());
    let json = |o: &std::process::Output| {
        let mut v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("json");
        v["ppu"]["rendering"] = serde_json::Value::Null;
        v
    };
    assert_eq!(json(&with), json(&without));
}

/// Writes `$80` to `$4200` (NMI on), then spins; the NMI handler is an
/// `RTI`.
fn nmi_rom(path: &Path) {
    let prog = [
        0x78, // 8000 SEI
        0xA9, 0x80, 0x8D, 0x00, 0x42, // LDA #$80 : STA $4200
        0x80, 0xFE, // 8006 BRA $8006
        0x40, // 8008 RTI  (nmi)
    ];
    let mut r = vec![0u8; 0x1_0000];
    r[..prog.len()].copy_from_slice(&prog);
    r[0x7FC0..0x7FD5].copy_from_slice(b"LUNA TRACE NMI       ".as_ref());
    r[0x7FD5] = 0x20;
    r[0x7FD7] = 0x07;
    r[0x7FFC] = 0x00;
    r[0x7FFD] = 0x80;
    for off in [0x7FEA, 0x7FFA] {
        r[off] = 0x08;
        r[off + 1] = 0x80;
    }
    std::fs::write(path, &r).expect("write rom");
}

/// A writes-only trace holds writes and nothing else: the NMI/IRQ delivery
/// markers, which skip the address filters, are not kept either (they
/// counted against `max_events` and drowned the writes).
#[test]
fn a_writes_only_trace_keeps_no_interrupt_marker() {
    let dir = std::env::temp_dir().join("luna_trace_writes_nmi");
    let _ = std::fs::create_dir_all(&dir);
    let rom_path = dir.join("game.sfc");
    nmi_rom(&rom_path);
    let kinds = |extra: &[&str]| -> Vec<String> {
        let csv = dir.join("t.csv");
        let _ = std::fs::remove_file(&csv);
        let out = Command::new(luna_bin())
            .arg("state")
            .arg(&rom_path)
            .args([
                "--force-mapper",
                "lorom",
                "--until-frame",
                "6",
                "--out",
                "/dev/null",
            ])
            .arg("--mem-trace")
            .arg(&csv)
            .args(extra)
            .output()
            .expect("run luna state");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(&csv)
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| l.split(',').nth(4).unwrap().to_string())
            .collect()
    };
    // Control: with the address filter alone the markers are there.
    let all = kinds(&["--mem-trace-addr", "4200:4211"]);
    assert!(all.iter().any(|k| k == "N"), "{all:?}");
    // Writes only: the write to $4200, and no marker.
    let writes = kinds(&["--trace-writes", "4200"]);
    assert_eq!(writes, ["W"], "{writes:?}");
}
