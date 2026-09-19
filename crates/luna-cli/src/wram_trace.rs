//! `luna wram-trace` — vblank-aligned per-frame WRAM page hashes
//! (the Mesen-differential oracle).

use std::process::ExitCode;

use crate::parsers::pad_events;
use crate::rom::load_rom_into;

/// `luna wram-trace` — emit per-frame (vblank-aligned) WRAM page hashes
/// for a confound-free cross-emulator differential (see the subcommand
/// doc). One line per frame: `<ppu_frame> <h0> <h1> ...`.
pub(crate) fn run_wram_trace(
    rom: &std::path::Path,
    steps: u64,
    count: u64,
    page_size: usize,
    out: &std::path::Path,
    dump_frame: Option<u64>,
    dump_out: &std::path::Path,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
    input_script: Option<&str>,
) -> ExitCode {
    use luna_api::FRAME_STEP_BUDGET as FRAME_BUDGET;
    use std::fmt::Write as _;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, force_region, None, None) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    let mut script = luna_api::InputScript::new();
    if let Some(s) = input_script {
        match pad_events(s, 0) {
            Ok(v) => script.extend(v),
            Err(e) => {
                eprintln!("error: --input: {e}");
                return ExitCode::from(2);
            }
        }
    }
    if steps > 0
        && let Err(e) = em.step(steps)
    {
        eprintln!("step warning (warm-up): {e}");
    }
    // Input checkpoints are applied DURING the capture loop, keyed by the
    // current PPU frame — so a scripted joypad pulse can span the frames
    // being hashed (front-loading them would consume the pulse before the
    // capture even starts).
    let mut buf = String::new();
    for _ in 0..count {
        let cur_frame = em.frame_count().unwrap_or(0);
        if let Err(e) = script.apply_due(&mut em, cur_frame) {
            eprintln!("error: scripted input: {e}");
            return ExitCode::from(1);
        }
        let executed = em.step_until_frame(FRAME_BUDGET).unwrap_or(0);
        let frame = em.frame_count().unwrap_or(0);
        let hashes = match em.wram_page_hashes(page_size) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: wram_page_hashes: {e}");
                return ExitCode::from(1);
            }
        };
        let _ = write!(buf, "{frame}");
        for h in &hashes {
            let _ = write!(buf, " {h:016x}");
        }
        buf.push('\n');
        if dump_frame == Some(frame) {
            match em.wram_snapshot() {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(dump_out, &bytes) {
                        eprintln!("error: writing {}: {e}", dump_out.display());
                        return ExitCode::from(1);
                    }
                    println!(
                        "dumped {} WRAM bytes at frame {frame} -> {}",
                        bytes.len(),
                        dump_out.display()
                    );
                }
                Err(e) => {
                    eprintln!("error: wram_snapshot: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        if executed == 0 {
            eprintln!("note: step_until_frame returned 0 (emulator halted?) — stopping early");
            break;
        }
    }
    if let Err(e) = std::fs::write(out, &buf) {
        eprintln!("error: writing {}: {e}", out.display());
        return ExitCode::from(1);
    }
    println!(
        "wrote {count} frames x {} pages of {page_size}-byte WRAM hashes -> {}",
        0x2_0000 / page_size,
        out.display()
    );
    ExitCode::SUCCESS
}
