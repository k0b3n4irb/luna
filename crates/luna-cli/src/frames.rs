//! `luna frames` — per-frame framebuffer-hash report.

use std::process::ExitCode;

use crate::parsers::parse_input_script;
use crate::rom::load_rom_into;

/// `luna frames` — capture `count` exactly-consecutive PPU frames as
/// PNGs via the same `luna-api` render path the GUI uses, tagging each
/// with its frame number and forced-blank flag. Lets us reproduce the
/// temporal artefacts (flicker / page-flip desync) that a single
/// `state --screenshot` is structurally blind to.
pub(crate) fn run_frames(
    rom: &std::path::Path,
    steps: u64,
    count: u64,
    out_dir: &std::path::Path,
    force_mapper: Option<&str>,
    input_script: Option<&str>,
) -> ExitCode {
    const FRAME_BUDGET: u64 = 200_000;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, None) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("error: creating {}: {e}", out_dir.display());
        return ExitCode::from(1);
    }
    // Scripted input during warm-up (same semantics as `state --input`),
    // so the capture can land in gameplay rather than at a title screen.
    let checkpoints: Vec<(u64, u16)> = match input_script {
        None => Vec::new(),
        Some(script) => match parse_input_script(script) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --input: {e}");
                return ExitCode::from(1);
            }
        },
    };
    for (frame, mask) in &checkpoints {
        while em.state().scheduler.frame_count < *frame {
            if em.step_until_frame(FRAME_BUDGET).unwrap_or(0) == 0 {
                break;
            }
        }
        if let Err(e) = em.set_joypad(0, *mask) {
            eprintln!("error: set_joypad: {e}");
            return ExitCode::from(1);
        }
    }
    if let Err(e) = em.step(steps) {
        eprintln!("step warning (warm-up): {e}");
    }
    // Capture loop: one PNG per consecutive frame, tagged frame# + blank.
    for i in 0..count {
        let executed = em.step_until_frame(FRAME_BUDGET).unwrap_or(0);
        let frame = em.frame_count().unwrap_or(0);
        let blanked = em.forced_blank().unwrap_or(false);
        let showed = em.frame_showed_content().unwrap_or(true);
        let png = match em.render_frame_png(false) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: render_frame_png: {e}");
                return ExitCode::from(1);
            }
        };
        // Tag on the per-frame "showed visible content" latch (what the GUI
        // publishes), not the instantaneous forced-blank bit — the latter
        // mislabels Super FX frames that re-blank at VBlank as "blank".
        let tag = if showed { "live" } else { "blank" };
        let path = out_dir.join(format!("frame_{i:03}_f{frame}_{tag}.png"));
        if let Err(e) = std::fs::write(&path, &png) {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
        println!(
            "frame {i:>3}: ppu_frame={frame} showed_content={showed} forced_blank={blanked} (+{executed} instr) -> {}",
            path.display()
        );
        if executed == 0 {
            eprintln!("note: step_until_frame returned 0 (emulator halted?) — stopping early");
            break;
        }
    }
    ExitCode::SUCCESS
}
