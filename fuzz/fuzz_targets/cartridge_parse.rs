//! Fuzz the auto-detecting ROM parser — luna's single largest untrusted
//! input. `Cartridge::from_bytes` runs header scoring across candidate
//! offsets, decodes size exponents into allocations, and strips SMC /
//! DSP-1 firmware tails; every one of those is arbitrary-byte-driven.
//!
//! Contract: any input either parses or returns `CartError`. It must
//! never panic (index out of bounds, slice overflow, capacity
//! overflow) and never allocate unboundedly.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Keep inputs in a realistic band: below 32 KiB the parser
    // short-circuits on TooSmall, and multi-megabyte inputs only slow
    // the fuzzer down without reaching new code.
    if data.len() > 4 * 1024 * 1024 {
        return;
    }
    if let Ok(cart) = luna_cartridge::Cartridge::from_bytes(data.to_vec()) {
        // Accessors must hold on whatever the parser accepted.
        let _ = cart.header.mapper_kind;
        let _ = cart.header.rom_size_kb;
        let _ = cart.header.sram_size_kb;
        let _ = cart.header.expansion_ram_kb;
        let _ = cart.header.checksum_valid();
        let _ = cart.needs_coprocessor_firmware();
        let _ = cart.header.title.len();
        let _ = cart.rom.len();
    }
});
