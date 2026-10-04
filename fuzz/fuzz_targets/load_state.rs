//! Fuzz `Emulator::load_state` — the OTHER door for outside bytes.
//!
//! A save-state reaches luna from a file the GUI picks, a `--load-state`
//! path, or base64 over MCP. Unlike a ROM it is not "parse then run": it
//! is decoded straight INTO a running machine, so a blob that decodes but
//! is the wrong shape (a coprocessor RAM shorter than the address mask
//! built at construction, a framebuffer of the wrong size, a register
//! selector past the register file) would panic later, in the emulation
//! loop, far from the load.
//!
//! The first input byte picks how the rest is delivered, so the fuzzer can
//! get past the container's version + ROM-hash gate and reach each layer:
//!
//! - `0`: the raw bytes, as the whole state (container decode + gates);
//! - `1`: a genuine container with the bytes as its MAPPER blob;
//! - `2`: a genuine container with the bytes as its CORE blob.
//!
//! (modulo 3), on a plain `LoROM` machine. Every mapper kind decodes its own
//! blob, so an input that starts with the escape byte [`COPROCESSOR`]
//! carries a second byte choosing one of the coprocessor machines of
//! [`machines`] (`% 4`: Super FX, S-DD1, SA-1, DSP-1) and the layer
//! (`/ 4 % 3`, as above). The escape leaves every other first byte meaning
//! what it always has, which is what keeps the committed seeds valid.
//!
//! Contract: `load_state` returns `Ok` or `ApiError`, never panics; after
//! an `Err` the machine still runs; after an `Ok` it runs without
//! panicking too.
#![no_main]

use libfuzzer_sys::fuzz_target;
use luna_api::Emulator;

#[path = "load_state_machines.rs"]
mod machines;

/// First byte of an input aimed at a coprocessor machine. Chosen because
/// no committed seed starts with it.
const COPROCESSOR: u8 = 0xF5;

/// Mirror of luna-api's private `SaveStateBundle` (bincode is positional,
/// so field order and types are the whole contract).
#[derive(serde::Serialize, serde::Deserialize)]
struct Bundle {
    version: u32,
    rom_hash: u64,
    core: Vec<u8>,
    mapper: Vec<u8>,
}

thread_local! {
    /// One machine per kind per fuzzing process, each with its pristine
    /// state: building an `Emulator` per input costs ~20 ms, reloading the
    /// pristine state far less — and that reload is itself a `load_state`
    /// round-trip.
    static MACHINES: std::cell::RefCell<Vec<(Emulator, Vec<u8>)>> =
        std::cell::RefCell::new(machines::build());
}

fuzz_target!(|data: &[u8]| {
    let (kind, layer, payload) = match data {
        [COPROCESSOR, select, payload @ ..] => {
            (1 + usize::from(select % 4), (select / 4) % 3, payload)
        }
        [mode, payload @ ..] => (machines::LOROM, mode % 3, payload),
        [] => return,
    };
    MACHINES.with_borrow_mut(|all| {
        let (emu, good) = &mut all[kind];
        emu.load_state(good).expect("the pristine state always loads");

        let cfg = bincode::config::standard();
        let state = match layer {
            0 => payload.to_vec(),
            m => {
                let (mut bundle, _): (Bundle, usize) =
                    bincode::serde::decode_from_slice(good, cfg).expect("own state decodes");
                if m == 1 {
                    bundle.mapper = payload.to_vec();
                } else {
                    bundle.core = payload.to_vec();
                }
                bincode::serde::encode_to_vec(&bundle, cfg).expect("bundle encodes")
            }
        };

        // Whatever the verdict, the machine must keep running and stay
        // readable. A coprocessor gets a longer burst: its restored state
        // is consumed by the chip's own loop, a few instructions in.
        let _ = emu.load_state(&state);
        let _ = emu.step(if kind == machines::LOROM { 64 } else { 256 });
        let _ = emu.peek_memory(0x70, 0x0000, 16);
        let _ = emu.peek_memory(0x7E, 0x0000, 16);
        let _ = emu.peek_memory(0xC0, 0x0000, 16);
    });
});
