//! RFE-3 acceptance (Super Scope): the `OpenSNES` `examples/input/superscope`
//! ROM detects the gun on port 2 (via the auto-joypad-read) and leaves its
//! DETECT state. The ROM is not vendored: point `LUNA_SUPERSCOPE_ROM` at
//! `<opensnes>/examples/input/superscope/superscope.sfc`. Without it the test
//! skips — or fails, when `LUNA_GAME_TEST_REQUIRE` is set.

use std::path::{Path, PathBuf};

use luna_api::{Emulator, PortDevice};

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

/// `$LUNA_SUPERSCOPE_ROM`. There is no default: the ROM lives in another
/// checkout, whose location is the developer's business.
fn scope_rom() -> Option<PathBuf> {
    let Some(p) = std::env::var_os("LUNA_SUPERSCOPE_ROM").map(PathBuf::from) else {
        skip(
            "LUNA_SUPERSCOPE_ROM is not set (the OpenSNES \
             examples/input/superscope/superscope.sfc)",
        );
        return None;
    };
    if !p.is_file() {
        skip(&format!(
            "LUNA_SUPERSCOPE_ROM: {} is not a file",
            p.display()
        ));
        return None;
    }
    Some(p)
}

fn settle_hash(rom: &Path, scope: bool) -> u64 {
    let mut em = Emulator::new();
    em.load_rom(rom).expect("load superscope rom");
    if scope {
        em.set_port_device(1, PortDevice::SuperScope)
            .expect("select port-2 super scope");
        em.set_superscope(128, 112, 0)
            .expect("aim at screen centre");
    }
    em.step(2_000_000).expect("step");
    em.frame_hash(true).expect("frame hash")
}

#[test]
fn super_scope_is_detected_on_port2() {
    let Some(rom) = scope_rom() else {
        return;
    };
    let pad = settle_hash(&rom, false);
    let scope = settle_hash(&rom, true);
    assert_ne!(
        pad, scope,
        "with a port-2 Super Scope the ROM must detect it and leave DETECT — \
         the framebuffer must differ from the pad case"
    );
}
