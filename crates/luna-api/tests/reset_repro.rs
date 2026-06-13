//! Regression test for `Emulator::reset` on coprocessor carts.
//!
//! A reset must re-power the cartridge coprocessor (Super FX / SA-1),
//! not just the main CPU. Before the fix, `reset` left the GSU
//! mid-execution, so resetting a Super FX title (Doom) froze instead of
//! rebooting — yet the *frame counter* kept ticking from the main
//! scheduler, so a "frames advanced" check passed while the screen was
//! frozen. The real symptom is "no content is ever drawn again", so
//! that is what this asserts.
//!
//! The ROMs live under `tests/roms/` (gitignored — dump your own); each
//! case skips when its ROM is absent, so CI without ROMs stays green.

use luna_api::Emulator;
use std::path::Path;

/// Boot `rom`, run it to gameplay, reset, then assert it reboots and
/// draws content again within `settle_frames`. Returns silently (skips)
/// if the ROM is not present.
fn assert_reboots(rom: &str, settle_frames: u32) {
    let p = Path::new(rom);
    if !p.exists() {
        eprintln!("[skip] {rom} (absent)");
        return;
    }
    let mut em = Emulator::default();
    em.load_rom(p).expect("load");

    // Run a while so the coprocessor is genuinely mid-execution.
    for _ in 0..400 {
        let _ = em.step_until_frame(400_000);
    }

    em.reset().expect("reset");

    // After reset the game must reboot: advance frames AND draw real
    // (non-forced-blank) content again. A frozen coprocessor leaves the
    // frame counter climbing but no content ever shown.
    let mut drew_content = false;
    let mut advanced = 0u32;
    for _ in 0..settle_frames {
        let before = em.frame_count().unwrap_or(0);
        let _ = em.step_until_frame(400_000);
        if em.frame_count().unwrap_or(0) > before {
            advanced += 1;
        }
        if em.frame_showed_content().unwrap_or(false) {
            drew_content = true;
        }
    }

    let st = em.state();
    assert!(
        advanced > 0,
        "{rom}: no frames advanced after reset (final_frame={}, cpu.stopped={})",
        st.scheduler.frame_count,
        st.cpu.stopped
    );
    assert!(
        drew_content,
        "{rom}: rebooted but never drew content within {settle_frames} frames \
         after reset — coprocessor likely left mid-execution \
         (pb:pc={:02X}:{:04X}, cpu.stopped={})",
        st.cpu.pb, st.cpu.pc, st.cpu.stopped
    );
}

#[test]
fn reset_reboots_superfx_doom() {
    assert_reboots("../../tests/roms/Doom (USA).sfc", 600);
}

#[test]
fn reset_reboots_sa1_smrpg() {
    assert_reboots(
        "../../tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc",
        600,
    );
}

#[test]
fn reset_reboots_lorom_smw() {
    assert_reboots("../../tests/roms/Super Mario World (U) [!].smc", 600);
}
