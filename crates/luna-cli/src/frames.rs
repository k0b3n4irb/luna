//! `luna frames` — per-frame framebuffer-hash report.

use std::process::ExitCode;

use crate::parsers::pad_events;
use crate::rom::load_rom_into;

/// `luna frames` — capture `count` exactly-consecutive PPU frames as
/// PNGs via the same `luna-api` render path the GUI uses, tagging each
/// with its frame number and forced-blank flag. Lets us reproduce the
/// temporal artefacts (flicker / page-flip desync) that a single
/// `state --screenshot` is structurally blind to.
pub(crate) fn run_frames(
    rom: &std::path::Path,
    steps: u64,
    from_frame: Option<u64>,
    count: u64,
    out_dir: &std::path::Path,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
    input_script: Option<&str>,
    power_on: Option<&str>,
) -> ExitCode {
    use luna_api::FRAME_STEP_BUDGET as FRAME_BUDGET;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, force_region, None, power_on) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("error: creating {}: {e}", out_dir.display());
        return ExitCode::from(1);
    }
    // Scripted input, so the capture can land in gameplay rather than at a
    // title screen. The luna-api rule, as in `state`: during the `-n`
    // warm-up the checkpoints spend from `-n` (issue #126), under
    // `--from-frame` they are chased frame by frame; a checkpoint later than
    // the warm-up fires during the capture, on its own frame.
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
    // `--from-frame N` (issue #222): the first capture is PPU frame N. The
    // capture loop steps one frame per PNG, so the warm-up stops one short.
    let bound = match from_frame {
        Some(target) => luna_api::ScriptBound::Frame(target.saturating_sub(1)),
        None => luna_api::ScriptBound::Steps(steps),
    };
    let spent = match em.run_input_script(&mut script, bound) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: scripted input: {e}");
            return ExitCode::from(1);
        }
    };
    if let Some(target) = from_frame {
        while em.frame_count().unwrap_or(0) + 1 < target {
            if em.step_until_frame(FRAME_BUDGET).unwrap_or(0) == 0 {
                eprintln!("note: emulator halted before frame {target} — capturing from here");
                break;
            }
        }
    } else if let Err(e) = em.step(steps.saturating_sub(spent)) {
        eprintln!("step warning (warm-up): {e}");
    }
    // Capture loop: one PNG per consecutive frame, tagged frame# + blank.
    for i in 0..count {
        let due = em.frame_count().unwrap_or(0);
        if let Err(e) = script.apply_due(&mut em, due) {
            eprintln!("error: scripted input: {e}");
            return ExitCode::from(1);
        }
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
