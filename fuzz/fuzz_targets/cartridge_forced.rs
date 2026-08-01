//! Fuzz the forced-mapper path (`--force-mapper`, the GUI's "load as…"
//! prompt). It deliberately SKIPS the checksum validation that guards
//! the auto-detect path, so it reaches header parsing with far less
//! filtering — the weaker of the two doors.
#![no_main]

use libfuzzer_sys::fuzz_target;
use luna_bus::MapperKind;

const MAPPERS: [MapperKind; 8] = [
    MapperKind::LoRom,
    MapperKind::HiRom,
    MapperKind::ExHiRom,
    MapperKind::Sa1,
    MapperKind::SuperFx,
    MapperKind::Dsp1,
    MapperKind::Sdd1,
    MapperKind::Spc7110,
];

fuzz_target!(|data: &[u8]| {
    if data.len() > 4 * 1024 * 1024 || data.is_empty() {
        return;
    }
    // First byte selects the mapper, the rest is the image — so one
    // corpus entry explores every layout's header offset.
    let mapper = MAPPERS[usize::from(data[0]) % MAPPERS.len()];
    let rom = data[1..].to_vec();
    if let Ok(cart) = luna_cartridge::Cartridge::from_bytes_forced(rom, mapper) {
        let _ = cart.header.rom_size_kb;
        let _ = cart.header.sram_size_kb;
        let _ = cart.header.title.len();
        let _ = cart.rom.len();
    }
});
