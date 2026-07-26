//! DSP-1 (uPD7725) port-level differential vs a Mesen2 reference trace.
//!
//! THE method's oracle for the DSP-1 (scorecard open item #5 — the one
//! grade capped by *missing evidence*): Mesen2's Lua `getState()` does
//! not expose the `NecDsp` registers, so instead of per-op state injection
//! (the GSU pattern) this harness compares the DSP-1's **complete
//! observable behaviour** — the DR-port byte stream. Every command byte
//! the game writes and every result byte it reads back crosses `$6xxx`
//! (DR) on the `HiROM` 1K board; if luna's stream is byte-identical to
//! Mesen2's over a long no-input run, the uPD7725 core, the firmware
//! decode and the mapper glue are all validated end-to-end.
//!
//! SR (`$7xxx`) reads are deliberately NOT compared: they are RQM
//! polling, and the *number* of polls is timing-sensitive. The DR
//! sequence is pure protocol data — timing-insensitive.
//!
//! Reference capture (developer-local, like the GSU fixtures):
//!
//! ```text
//! ~/bin/Mesen --testRunner tools/mesen-dsp1-port-trace.lua \
//!     "tests/roms/Super Mario Kart (USA).sfc" -novideo -noaudio
//! LUNA_DSP1_PORT_CSV=/tmp/mesen_dsp1_port.csv \
//!     cargo test -p luna-core --test dsp1_port_differential --release
//! ```
//!
//! Skips silently when the ROM, the firmware or the reference CSV is
//! absent (commercial ROMs are gitignored; CI never runs this).
//!
//! The two streams may differ in LENGTH: SMK reads uninitialized memory
//! (Mesen2 logs it), so the input-less attract PATH eventually diverges
//! between emulators (Mesen2's demo went DSP-quiet at ~48 s of the 60 s
//! window on the 2026-07-26 capture; luna's demo kept racing). That is a
//! game-path artifact, not a DSP divergence — the verdict is the
//! byte-identity of the COMMON prefix (380 783 DR events on that capture).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use luna_cartridge::Cartridge;
use luna_core::{MemEventKind, Snes};

/// Frames to run — must match the Mesen capture window
/// (`DSP1_STOP_FRAME`, default 3600 ≈ 60 s: title + demo race).
const FRAMES: u64 = 3_600;
/// Hard instruction cap so a regression that stalls the frame counter
/// fails instead of hanging.
const STEP_CAP: u64 = 900_000_000;
/// The comparison must cover at least this many DR events to count as
/// evidence — a short prefix match on an idle boot proves nothing.
const MIN_EVENTS: usize = 100_000;

fn roms_root() -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push("tests");
    p.push("roms");
    p.is_dir().then_some(p)
}

/// `true` for the DSP-1 DR window on the `HiROM` 1K board: banks
/// `$00-$1F` / `$80-$9F`, offset `$6000-$6FFF` (bit 12 clear = DR).
fn is_dr(addr: u32) -> bool {
    let bank = (addr >> 16) as u8;
    let off = (addr & 0xFFFF) as u16;
    matches!(bank, 0x00..=0x1F | 0x80..=0x9F)
        && (0x6000..=0x7FFF).contains(&off)
        && off & 0x1000 == 0
}

/// Parse the Mesen CSV (`master_clock,$AAAAAA,K,$VV`) into the DR-only
/// `(is_write, value)` sequence.
fn mesen_dr_sequence(csv: &str) -> Vec<(bool, u8)> {
    let mut out = Vec::new();
    for line in csv.lines().skip(1) {
        let mut cols = line.split(',');
        let (Some(_mclk), Some(addr), Some(kind), Some(value)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let Ok(addr) = u32::from_str_radix(addr.trim_start_matches('$'), 16) else {
            continue;
        };
        if !is_dr(addr) {
            continue;
        }
        let Ok(value) = u8::from_str_radix(value.trim_start_matches('$'), 16) else {
            continue;
        };
        out.push((kind == "W", value));
    }
    out
}

#[test]
fn dsp1_dr_stream_matches_mesen_over_smk_demo() {
    let csv_path =
        std::env::var("LUNA_DSP1_PORT_CSV").unwrap_or_else(|_| "/tmp/mesen_dsp1_port.csv".into());
    let Ok(csv) = std::fs::read_to_string(&csv_path) else {
        eprintln!("[skip] Mesen reference trace absent ({csv_path}) — see the module doc");
        return;
    };
    let Some(root) = roms_root() else {
        eprintln!("[skip] tests/roms/ absent (gitignored — dump your own)");
        return;
    };
    let rom_path = root.join("Super Mario Kart (USA).sfc");
    if !rom_path.is_file() {
        eprintln!("[skip] Super Mario Kart (USA).sfc not present");
        return;
    }

    let reference = mesen_dr_sequence(&csv);
    assert!(
        reference.len() >= MIN_EVENTS,
        "reference trace too short ({} DR events) — re-capture with \
         DSP1_STOP_FRAME=3600",
        reference.len()
    );

    // `Cartridge::load` auto-discovers `dsp1b.rom` next to the ROM —
    // tests/roms/ carries it (gitignored, like the ROMs).
    let cart = Cartridge::load(&rom_path).expect("auto-detect cartridge");
    if cart.needs_coprocessor_firmware() {
        eprintln!("[skip] dsp1b.rom not present next to the ROM — the DSP would run inert");
        return;
    }
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // Trace the whole $6000-$7FFF offset window (all banks); the DR/bank
    // filter is applied per event below (WRAM $7E-$7F also lives there).
    snes.enable_mem_trace(1 << 20, None, Some((0x6000, 0x7FFF)));

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut ours: Vec<(bool, u8)> = Vec::with_capacity(reference.len());
    let mut executed = 0u64;
    let mut last_drained_frame = 0u64;
    while snes.frame_count < FRAMES && executed < STEP_CAP && !snes.cpu.stopped {
        if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
            break;
        }
        executed += 1;
        // Drain the bounded ring once per frame so it never fills.
        if snes.frame_count != last_drained_frame {
            last_drained_frame = snes.frame_count;
            for ev in snes.take_mem_trace_log() {
                if is_dr(ev.addr_full) {
                    ours.push((matches!(ev.kind, MemEventKind::Write), ev.value));
                }
            }
        }
    }
    std::panic::set_hook(prev_hook);
    for ev in snes.take_mem_trace_log() {
        if is_dr(ev.addr_full) {
            ours.push((matches!(ev.kind, MemEventKind::Write), ev.value));
        }
    }

    let common = ours.len().min(reference.len());
    assert!(
        common >= MIN_EVENTS,
        "not enough overlapping DR events to be evidence: luna {} vs mesen {} \
         (frames run: {}, instructions: {executed})",
        ours.len(),
        reference.len(),
        snes.frame_count,
    );
    for i in 0..common {
        assert_eq!(
            ours[i],
            reference[i],
            "first DR divergence at event {i}/{common}: luna {:?} vs mesen {:?} \
             (context luna[{}..{}]: {:?} | mesen: {:?})",
            ours[i],
            reference[i],
            i.saturating_sub(4),
            (i + 4).min(common),
            &ours[i.saturating_sub(4)..(i + 4).min(common)],
            &reference[i.saturating_sub(4)..(i + 4).min(common)],
        );
    }
    eprintln!(
        "dsp1 DR differential: {common} events byte-identical \
         (luna {}, mesen {}) over {} frames",
        ours.len(),
        reference.len(),
        snes.frame_count
    );
}
