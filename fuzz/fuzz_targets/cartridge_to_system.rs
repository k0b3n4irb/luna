//! Fuzz the whole load chain: parse → build the system (mapper shims,
//! RAM sizing, coprocessor wiring) → reset → step a few instructions.
//!
//! `Cartridge::from_bytes` succeeding is not the end of the untrusted
//! path: the header's size/RAM fields then drive allocations and
//! address masks inside the mapper shims, and a malformed-but-accepted
//! cart could still take the system down. luna-api wraps construction
//! in `catch_unwind` precisely because this is reachable; this target
//! looks for what that net is catching.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }
    let Ok(cart) = luna_cartridge::Cartridge::from_bytes(data.to_vec()) else {
        return;
    };
    let mut snes = luna_core::Snes::from_cartridge(cart);
    snes.reset();
    // A short burst: enough to fault on a bad reset vector / mapper
    // mask, short enough to keep the fuzzer's throughput up.
    for _ in 0..256 {
        if snes.cpu.stopped {
            break;
        }
        snes.step();
    }
});
