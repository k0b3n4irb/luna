//! RFE-3 acceptance: the `OpenSNES` `examples/input/mouse` ROM detects the SNES
//! Mouse on port 1 (via the auto-joypad-read signature) and shows its cursor
//! instead of the "No mouse detected" diagnostic. The ROM is not vendored
//! (it lives in the `OpenSNES` tree): point `LUNA_MOUSE_ROM` at
//! `<opensnes>/examples/input/mouse/mouse.sfc`. Without it the test skips —
//! or fails, when `LUNA_GAME_TEST_REQUIRE` is set.

use std::path::{Path, PathBuf};

use luna_api::Emulator;

/// Report a test that cannot run: a skip notice, or a failure when
/// `LUNA_GAME_TEST_REQUIRE` is set — the switch of the commercial goldens in
/// `luna-core/tests/snes_test_roms.rs`, so one variable covers every test
/// that needs a file the repository cannot ship.
fn skip(why: &str) {
    assert!(
        std::env::var_os("LUNA_GAME_TEST_REQUIRE").is_none(),
        "{why} — and LUNA_GAME_TEST_REQUIRE is set, so a skip is a failure"
    );
    eprintln!("[skip] {why}");
}

/// `$LUNA_MOUSE_ROM`. There is no default: the ROM lives in another
/// checkout, whose location is the developer's business.
fn mouse_rom() -> Option<PathBuf> {
    let Some(p) = std::env::var_os("LUNA_MOUSE_ROM").map(PathBuf::from) else {
        skip("LUNA_MOUSE_ROM is not set (the OpenSNES examples/input/mouse/mouse.sfc)");
        return None;
    };
    if !p.is_file() {
        skip(&format!("LUNA_MOUSE_ROM: {} is not a file", p.display()));
        return None;
    }
    Some(p)
}

fn settle_hash(rom: &Path, mouse_on_port1: bool) -> u64 {
    let mut em = Emulator::new();
    em.load_rom(rom).expect("load mouse rom");
    if mouse_on_port1 {
        em.set_port_mouse(0, true).expect("select port-1 mouse");
    }
    // Run well past startup detection (mouseInit runs in the first frames).
    em.step(2_000_000).expect("step");
    em.frame_hash(true).expect("frame hash")
}

#[test]
fn mouse_is_detected_on_port1() {
    let Some(rom) = mouse_rom() else {
        return;
    };
    let pad = settle_hash(&rom, false);
    let mouse = settle_hash(&rom, true);
    assert_ne!(
        pad, mouse,
        "with a port-1 Mouse the ROM must detect it (cursor) rather than show \
         'No mouse detected' — the two framebuffers must differ"
    );
}
