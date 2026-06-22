//! RFE-3 acceptance (Super Scope): the `OpenSNES` `examples/input/superscope`
//! ROM detects the gun on port 2 (via the auto-joypad-read) and leaves its
//! DETECT state. The ROM is not vendored; the test skips if it is absent.

use std::path::{Path, PathBuf};

use luna_api::{Emulator, PortDevice};

fn scope_rom() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LUNA_SUPERSCOPE_ROM") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let p = PathBuf::from(
        "/home/kobenairb/workspace/opensnes/examples/input/superscope/superscope.sfc",
    );
    p.is_file().then_some(p)
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
        eprintln!("[skip] superscope example ROM absent (set LUNA_SUPERSCOPE_ROM)");
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
