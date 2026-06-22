//! RFE-3 acceptance: the `OpenSNES` `examples/input/mouse` ROM detects the SNES
//! Mouse on port 1 (via the auto-joypad-read signature) and shows its cursor
//! instead of the "No mouse detected" diagnostic. The ROM is not vendored
//! (it lives in the `OpenSNES` tree); the test skips if it is absent.

use std::path::Path;

use luna_api::Emulator;

/// `$LUNA_MOUSE_ROM`, else the default `OpenSNES` example location.
fn mouse_rom() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("LUNA_MOUSE_ROM") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let p = std::path::PathBuf::from(
        "/home/kobenairb/workspace/opensnes/examples/input/mouse/mouse.sfc",
    );
    p.is_file().then_some(p)
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
        eprintln!("[skip] mouse example ROM absent (set LUNA_MOUSE_ROM)");
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
