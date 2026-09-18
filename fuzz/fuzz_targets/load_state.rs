//! Fuzz `Emulator::load_state` — the OTHER door for outside bytes.
//!
//! A save-state reaches luna from a file the GUI picks, a `--load-state`
//! path, or base64 over MCP. Unlike a ROM it is not "parse then run": it
//! is decoded straight INTO a running machine, so a blob that decodes but
//! is the wrong shape (a coprocessor RAM shorter than the address mask
//! built at construction, a framebuffer of the wrong size) would panic
//! later, in the emulation loop, far from the load.
//!
//! The first input byte picks how the rest is delivered, so the fuzzer can
//! get past the container's version + ROM-hash gate and reach each layer:
//!
//! - `0`: the raw bytes, as the whole state (container decode + gates);
//! - `1`: a genuine container with the bytes as its MAPPER blob;
//! - `2`: a genuine container with the bytes as its CORE blob.
//!
//! Contract: `load_state` returns `Ok` or `ApiError`, never panics; after
//! an `Err` the machine still runs; after an `Ok` it runs without
//! panicking too.
#![no_main]

use libfuzzer_sys::fuzz_target;
use luna_api::Emulator;

/// Mirror of luna-api's private `SaveStateBundle` (bincode is positional,
/// so field order and types are the whole contract).
#[derive(serde::Serialize, serde::Deserialize)]
struct Bundle {
    version: u32,
    rom_hash: u64,
    core: Vec<u8>,
    mapper: Vec<u8>,
}

/// A 32 KB LoROM with 8 KB of SRAM: `SEI ; loop: BRA loop`.
fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[..3].copy_from_slice(&[0x78, 0x80, 0xFE]);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"LUNA FUZZ LOAD STATE ");
    rom[0x7FD5] = 0x20; // LoROM
    rom[0x7FD7] = 0x05; // 32 KB
    rom[0x7FD8] = 0x03; // 8 KB SRAM
    let sum: u32 = rom
        .iter()
        .enumerate()
        .filter(|(i, _)| !(0x7FDC..=0x7FDF).contains(i))
        .map(|(_, b)| u32::from(*b))
        .sum();
    let checksum = (sum & 0xFFFF) as u16;
    rom[0x7FDC..0x7FDE].copy_from_slice(&(!checksum).to_le_bytes());
    rom[0x7FDE..0x7FE0].copy_from_slice(&checksum.to_le_bytes());
    rom
}

thread_local! {
    /// One machine per fuzzing process plus its pristine state: building an
    /// `Emulator` per input costs ~20 ms, reloading the pristine state far
    /// less — and that reload is itself a `load_state` round-trip.
    static MACHINE: std::cell::RefCell<(Emulator, Vec<u8>)> = {
        let mut emu = Emulator::new();
        emu.load_rom_bytes(rom()).expect("the fixed ROM loads");
        let good = emu.save_state().expect("a fresh machine saves");
        std::cell::RefCell::new((emu, good))
    };
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, payload)) = data.split_first() else {
        return;
    };
    MACHINE.with_borrow_mut(|(emu, good)| {
        emu.load_state(good).expect("the pristine state always loads");

        let cfg = bincode::config::standard();
        let state = match mode % 3 {
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
        // readable.
        let _ = emu.load_state(&state);
        let _ = emu.step(64);
        let _ = emu.peek_memory(0x70, 0x0000, 16);
        let _ = emu.peek_memory(0x7E, 0x0000, 16);
    });
});
