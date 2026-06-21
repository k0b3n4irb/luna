//! `LoROM` (Mode 20) cartridge mapping.
//!
//! # Mapping
//!
//! ROM is split into 32 KB pages. Each bank `$00-$7D` exposes one such
//! page at `$8000-$FFFF`. Banks `$80-$FD` are mirrors. The bottom half of
//! each bank (`$0000-$7FFF`) is **not** ROM — it routes to `LowRAM` / MMIO /
//! SRAM depending on the bank.
//!
//! SRAM, when present, lives in banks `$70-$7D` (mirror `$F0-$FD`) at
//! offsets `$0000-$7FFF`.

use crate::mapper::{Mapper, MapperKind};
use crate::types::{Addr24, bank_of, offset_of, rom_mirror};

/// `LoROM` mapper.
pub struct LoRomMapper {
    rom: Vec<u8>,
    sram: Vec<u8>,
}

impl LoRomMapper {
    /// Build a new `LoROM` mapper around the given ROM bytes.
    ///
    /// `sram_size` is the SRAM size in bytes (typically 0 / 2 KB / 8 KB
    /// / 32 KB / 64 KB / 128 KB).
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_size: usize) -> Self {
        Self {
            rom,
            sram: vec![0; sram_size],
        }
    }

    /// Translate a (bank, offset) into a ROM offset, if the address maps
    /// to ROM.
    fn rom_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if offset < 0x8000 {
            return None;
        }
        // LoROM: each bank contributes 32 KB at $8000-$FFFF. Banks $80-$FF
        // mirror $00-$7F (with FastROM speed if MEMSEL is set, but speed
        // is the bus's concern).
        let normalized_bank = bank & 0x7F;
        let rom_offset = (usize::from(normalized_bank) * 0x8000) + (usize::from(offset) - 0x8000);
        if self.rom.is_empty() {
            return None;
        }
        // A ROM smaller than the mapped LoROM space mirrors (hardware
        // wraps the image), via ares' `Bus::mirror`. Power-of-two images
        // reduce to `% len`; non-pow2 carts mirror the trailing chunk.
        Some(rom_mirror(rom_offset, self.rom.len()))
    }

    /// Translate a (bank, offset) into an SRAM offset, if the address
    /// maps to SRAM.
    fn sram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        // SRAM at banks $70-$7D / $F0-$FD, offsets $0000-$7FFF.
        let is_sram_bank = matches!(bank, 0x70..=0x7D | 0xF0..=0xFD);
        if !is_sram_bank || offset >= 0x8000 {
            return None;
        }
        let normalized_bank = (bank & 0x7F) - 0x70;
        let sram_offset = usize::from(normalized_bank) * 0x8000 + usize::from(offset);
        // SRAM wraps modulo its actual size.
        Some(sram_offset % self.sram.len())
    }
}

impl Mapper for LoRomMapper {
    fn kind(&self) -> MapperKind {
        MapperKind::LoRom
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(o) = self.rom_offset(bank, offset) {
            return Some(self.rom[o]);
        }
        if let Some(o) = self.sram_offset(bank, offset) {
            return Some(self.sram[o]);
        }
        None
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(o) = self.sram_offset(bank, offset) {
            self.sram[o] = value;
            return true;
        }
        // ROM writes are silently dropped (open-bus on real hardware).
        // We still claim the access if it maps into the ROM area, to
        // prevent the bus from falling through to WRAM.
        self.rom_offset(bank, offset).is_some()
    }

    fn rom_size(&self) -> usize {
        self.rom.len()
    }

    fn sram_size(&self) -> usize {
        self.sram.len()
    }

    fn sram(&self) -> &[u8] {
        &self.sram
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&data[..n]);
    }

    fn save_state(&self) -> Vec<u8> {
        bincode::serialize(&self.sram).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Ok(sram) = bincode::deserialize::<Vec<u8>>(data) {
            self.sram = sram;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    /// Build a deterministic ROM where `rom[i] == (i & 0xFF) as u8`.
    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    #[test]
    fn bank0_reads_rom_from_offset_zero() {
        let mut m = LoRomMapper::new(ramp_rom(64 * 1024), 0);
        // Bank $00, offset $8000 → rom[0]
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
        // Bank $00, offset $8001 → rom[1]
        assert_eq!(m.read(make_addr(0x00, 0x8001)), Some(1));
        // Bank $00, offset $FFFF → rom[0x7FFF]
        assert_eq!(m.read(make_addr(0x00, 0xFFFF)), Some(0xFF));
    }

    #[test]
    fn bank1_reads_rom_from_offset_0x8000() {
        let mut m = LoRomMapper::new(ramp_rom(64 * 1024), 0);
        // Bank $01, offset $8000 → rom[0x8000] (mod 256 = 0)
        assert_eq!(m.read(make_addr(0x01, 0x8000)), Some(0));
        // Bank $01, offset $8001 → rom[0x8001] (mod 256 = 1)
        assert_eq!(m.read(make_addr(0x01, 0x8001)), Some(1));
    }

    #[test]
    fn high_banks_mirror_low_banks() {
        let mut m = LoRomMapper::new(ramp_rom(64 * 1024), 0);
        // Bank $80 mirrors bank $00.
        assert_eq!(m.read(make_addr(0x80, 0x8000)), Some(0));
        assert_eq!(m.read(make_addr(0x80, 0xFFFF)), Some(0xFF));
        // Bank $81 mirrors bank $01.
        assert_eq!(m.read(make_addr(0x81, 0x8000)), Some(0));
    }

    #[test]
    fn offsets_below_0x8000_do_not_map_to_rom() {
        let mut m = LoRomMapper::new(ramp_rom(64 * 1024), 0);
        assert_eq!(m.read(make_addr(0x00, 0x0000)), None);
        assert_eq!(m.read(make_addr(0x00, 0x7FFF)), None);
        // Even in mirror banks.
        assert_eq!(m.read(make_addr(0x80, 0x0000)), None);
    }

    #[test]
    fn reads_past_rom_end_mirror_power_of_two() {
        let mut m = LoRomMapper::new(ramp_rom(32 * 1024), 0); // one 32 KB page
        // Bank $00, offset $8000 → rom[0] (in range).
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
        // Bank $01, offset $8000 → linear $8000; a 32 KB ROM mirrors
        // every 32 KB, so this wraps to rom[0], NOT open bus.
        assert_eq!(m.read(make_addr(0x01, 0x8000)), Some(0));
        // Bank $01, offset $8001 → wraps to rom[1].
        assert_eq!(m.read(make_addr(0x01, 0x8001)), Some(1));
    }

    #[test]
    fn non_pow2_rom_mirrors_trailing_chunk() {
        // 1.5 MB = 1 MB + 0.5 MB. Linear $180000 (bank $30, off $8000) is
        // exactly at the end; ares `mirror` folds it to $100000 — i.e. the
        // trailing 0.5 MB chunk repeats, not open bus. ramp_rom stores
        // `i & 0xFF`, so rom[$100000] == 0x00.
        let mut m = LoRomMapper::new(ramp_rom(0x18_0000), 0);
        // In-range sanity: bank $20 off $8000 → linear $100000.
        assert_eq!(m.read(make_addr(0x20, 0x8000)), Some(0x00));
        // Past-end mirror: bank $30 off $8000 → linear $180000 → $100000.
        assert_eq!(m.read(make_addr(0x30, 0x8000)), Some(0x00));
        // …and bank $30 off $8005 → $180005 → $100005 → rom byte 0x05.
        assert_eq!(m.read(make_addr(0x30, 0x8005)), Some(0x05));
    }

    #[test]
    fn rom_writes_are_dropped_but_claimed() {
        let mut m = LoRomMapper::new(ramp_rom(64 * 1024), 0);
        // Writing to a ROM address returns `true` (the mapper claims the
        // access) but the underlying byte is unchanged.
        assert!(m.write(make_addr(0x00, 0x8000), 0xFF));
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
    }

    #[test]
    fn sram_round_trip() {
        let mut m = LoRomMapper::new(ramp_rom(32 * 1024), 8 * 1024);
        // SRAM lives in banks $70-$7D at offsets $0000-$7FFF.
        let sram_addr = make_addr(0x70, 0x0000);
        assert_eq!(m.read(sram_addr), Some(0));
        assert!(m.write(sram_addr, 0x42));
        assert_eq!(m.read(sram_addr), Some(0x42));
    }

    #[test]
    fn sram_addresses_outside_window_dont_claim() {
        let m = LoRomMapper::new(ramp_rom(32 * 1024), 8 * 1024);
        // Bank $70 offset $8000 is ROM territory, not SRAM.
        assert!(m.sram_offset(0x70, 0x8000).is_none());
        // Bank $00 offset $0000 is LowRAM territory, not SRAM.
        assert!(m.sram_offset(0x00, 0x0000).is_none());
    }

    #[test]
    fn kind_is_lorom() {
        let m = LoRomMapper::new(ramp_rom(32 * 1024), 0);
        assert_eq!(m.kind(), MapperKind::LoRom);
        assert_eq!(m.rom_size(), 32 * 1024);
        assert_eq!(m.sram_size(), 0);
    }
}
