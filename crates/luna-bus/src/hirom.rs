//! HiROM (Mode 21) and ExHiROM (Mode 25) cartridge mapping.
//!
//! # HiROM mapping (Mode 21)
//!
//! HiROM places **full 64 KB pages** in the upper banks of the SNES
//! address space:
//!
//! - `$C0..$FF:$0000-$FFFF` — sequential ROM banks 0..63 (the
//!   primary mapping for `> 32 Mbit` HiROM carts).
//! - `$40..$7D:$0000-$FFFF` — mirror of the same banks (used when
//!   the cart is small enough that the mirror is reachable).
//! - `$00..$3F:$8000-$FFFF` — the **upper half** of bank N at
//!   ROM bytes `N * $10000 + $8000..=N * $10000 + $FFFF`.
//! - `$80..$BF:$8000-$FFFF` — same mirror as `$00..$3F` (FastROM
//!   eligible when `MEMSEL` is set).
//!
//! # ExHiROM mapping (Mode 25)
//!
//! ExHiROM splits the cart into two 4 MB halves. The `$C0..$FF` /
//! `$80..$BF` banks reach the **lower** half (ROM banks 0..63);
//! the `$40..$7D` / `$00..$3F` banks reach the **upper** half
//! (ROM banks 64..125). Concretely:
//!
//! - `$C0..$FF:$0000-$FFFF` → ROM banks 0..63 (lower 4 MB)
//! - `$80..$BF:$8000-$FFFF` → upper half of ROM banks 0..63
//!   (FastROM mirror of `$C0..$FF`)
//! - `$40..$7D:$0000-$FFFF` → ROM banks 64..125 (upper 4 MB)
//! - `$00..$3F:$8000-$FFFF` → upper half of ROM banks 64..127
//!   (mirror of `$40..$7D` — slow)
//!
//! Equivalently: when the mapper is in ExHiROM mode and the high bit
//! of the bank is clear (banks `$00..$7F`), the ROM bank index is
//! offset by `64` to point into the upper 4 MB.
//!
//! # SRAM
//!
//! Banks `$20..$3F` and `$A0..$BF` at offsets `$6000-$7FFF` expose
//! battery-backed SRAM, wrapping at the cart-declared size.

use crate::mapper::{Mapper, MapperKind};
use crate::types::{Addr24, bank_of, offset_of};

/// HiROM / ExHiROM mapper.
pub struct HiRomMapper {
    rom: Vec<u8>,
    sram: Vec<u8>,
    /// `HiRom` or `ExHiRom`. Selects whether banks `$00..$7F` reach
    /// the lower 4 MB (HiROM) or the upper 4 MB (ExHiROM).
    kind: MapperKind,
}

impl HiRomMapper {
    /// Build a HiROM mapper around the given ROM bytes.
    ///
    /// `sram_size` is in bytes (0 / 2K / 8K / 32K / 64K / 128K).
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_size: usize) -> Self {
        Self::with_kind(MapperKind::HiRom, rom, sram_size)
    }

    /// Build an ExHiROM mapper (Mode 25) — same as [`Self::new`] but
    /// routes banks `$00..$7F` to the upper 4 MB of ROM.
    #[must_use]
    pub fn new_exhirom(rom: Vec<u8>, sram_size: usize) -> Self {
        Self::with_kind(MapperKind::ExHiRom, rom, sram_size)
    }

    /// Build a HiROM-family mapper with an explicit kind. Panics if
    /// `kind` is not `HiRom` or `ExHiRom`.
    #[must_use]
    pub fn with_kind(kind: MapperKind, rom: Vec<u8>, sram_size: usize) -> Self {
        assert!(
            matches!(kind, MapperKind::HiRom | MapperKind::ExHiRom),
            "HiRomMapper only supports HiRom and ExHiRom kinds, got {kind:?}"
        );
        Self {
            rom,
            sram: vec![0; sram_size],
            kind,
        }
    }

    /// Translate (bank, offset) → ROM byte offset, or `None` if the
    /// address doesn't fall in a HiROM-mapped region.
    fn rom_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        // In ExHiROM mode, banks $00..$7F reach the upper 4 MB —
        // i.e. ROM banks 64..127.  Banks $80..$FF keep the standard
        // HiROM mapping (lower 4 MB).
        let exhirom_offset: usize = if matches!(self.kind, MapperKind::ExHiRom) && bank & 0x80 == 0
        {
            64
        } else {
            0
        };
        let rom_bank: usize = match bank {
            0xC0..=0xFF => usize::from(bank - 0xC0),
            0x40..=0x7D => usize::from(bank - 0x40) + exhirom_offset,
            0x00..=0x3F if offset >= 0x8000 => usize::from(bank) + exhirom_offset,
            0x80..=0xBF if offset >= 0x8000 => usize::from(bank - 0x80),
            _ => return None,
        };
        let off = rom_bank * 0x1_0000 + usize::from(offset);
        if off < self.rom.len() {
            Some(off)
        } else {
            None
        }
    }

    /// Translate (bank, offset) → SRAM byte offset, or `None`.
    fn sram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        let in_sram_window =
            matches!(bank, 0x20..=0x3F | 0xA0..=0xBF) && matches!(offset, 0x6000..=0x7FFF);
        if !in_sram_window {
            return None;
        }
        // Linear index across the SRAM window, wrapped modulo size.
        let normalized_bank = bank & 0x1F;
        let linear = usize::from(normalized_bank) * 0x2000 + usize::from(offset - 0x6000);
        Some(linear % self.sram.len())
    }
}

impl Mapper for HiRomMapper {
    fn kind(&self) -> MapperKind {
        self.kind
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
        // ROM writes are silently dropped; we still "claim" them so the
        // bus doesn't fall through to WRAM.
        self.rom_offset(bank, offset).is_some()
    }

    fn rom_size(&self) -> usize {
        self.rom.len()
    }

    fn sram_size(&self) -> usize {
        self.sram.len()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    /// Build a deterministic ROM where `rom[i] = (i & 0xFF) as u8` so
    /// every byte's identity is its low byte. Length lets us probe
    /// the edges of the mapping.
    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    #[test]
    fn full_bank_at_c0_maps_to_rom_bank_zero() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        // $C0:0000 → rom[0]
        assert_eq!(m.read(make_addr(0xC0, 0x0000)), Some(0));
        // $C0:1234 → rom[$1234]
        assert_eq!(m.read(make_addr(0xC0, 0x1234)), Some(0x34));
        // $C0:FFFF → rom[$FFFF]
        assert_eq!(m.read(make_addr(0xC0, 0xFFFF)), Some(0xFF));
    }

    #[test]
    fn full_bank_at_c1_maps_to_rom_bank_one() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        // $C1:0000 → rom[$1_0000] (which is 0 modulo 256)
        assert_eq!(m.read(make_addr(0xC1, 0x0000)), Some(0));
        // $C1:0023 → rom[$1_0023] = 0x23
        assert_eq!(m.read(make_addr(0xC1, 0x0023)), Some(0x23));
    }

    #[test]
    fn mirror_at_40_maps_same_as_c0() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        assert_eq!(
            m.read(make_addr(0x40, 0x1234)),
            m.read(make_addr(0xC0, 0x1234))
        );
    }

    #[test]
    fn mirror_at_low_bank_only_covers_8000_to_ffff() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        // Low half of bank 0 (offsets 0..$7FFF) → NOT mapped.
        assert_eq!(m.read(make_addr(0x00, 0x0000)), None);
        assert_eq!(m.read(make_addr(0x00, 0x7FFF)), None);
        // Upper half → bank 0's upper half.
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some((0x8000 & 0xFF) as u8));
    }

    #[test]
    fn mirror_at_80_matches_00() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        let a = m.read(make_addr(0x00, 0xC000));
        let b = m.read(make_addr(0x80, 0xC000));
        assert_eq!(a, b);
    }

    #[test]
    fn read_past_rom_returns_none() {
        let mut m = HiRomMapper::new(ramp_rom(0x1_0000), 0); // 1 bank only
        // $C0:0000 in range
        assert_eq!(m.read(make_addr(0xC0, 0x0000)), Some(0));
        // $C1:0000 past end
        assert_eq!(m.read(make_addr(0xC1, 0x0000)), None);
    }

    #[test]
    fn rom_writes_are_dropped_but_claimed() {
        let mut m = HiRomMapper::new(ramp_rom(2 * 0x1_0000), 0);
        assert!(m.write(make_addr(0xC0, 0x0000), 0xFF));
        assert_eq!(m.read(make_addr(0xC0, 0x0000)), Some(0));
    }

    #[test]
    fn sram_round_trip_in_canonical_window() {
        let mut m = HiRomMapper::new(ramp_rom(0x1_0000), 8 * 1024);
        let addr = make_addr(0x20, 0x6000);
        assert!(m.write(addr, 0x42));
        assert_eq!(m.read(addr), Some(0x42));
    }

    #[test]
    fn sram_wraps_modulo_advertised_size() {
        // 2 KB SRAM. Writing into the "next" sram-window bank should
        // wrap to the same physical SRAM cell.
        let mut m = HiRomMapper::new(ramp_rom(0x1_0000), 2 * 1024);
        m.write(make_addr(0x20, 0x6000), 0xAA);
        // Same physical SRAM cell, addressed via the bank-$A0 mirror.
        assert_eq!(m.read(make_addr(0xA0, 0x6000)), Some(0xAA));
    }

    #[test]
    fn kind_is_hirom() {
        let m = HiRomMapper::new(ramp_rom(0x1_0000), 0);
        assert_eq!(m.kind(), MapperKind::HiRom);
    }

    // ---------- ExHiROM (Mode 25) ----------

    /// In ExHiROM, banks `$40..$7D` reach ROM bank 64+, i.e. the
    /// upper 4 MB. Test against an 8 MB ramp so we can tell the two
    /// halves apart.
    ///
    /// `ramp_rom` makes `rom[i] = (i & 0xFF) as u8`, so a 64 KB-step
    /// crossing always lands on `0x00`. To distinguish bank 0 from
    /// bank 64 we instead seed `rom[bank * 0x10000] = bank as u8`.
    fn marked_rom_8mb() -> Vec<u8> {
        let mut rom = vec![0u8; 128 * 0x1_0000];
        for bank in 0..128 {
            rom[bank * 0x1_0000] = bank as u8;
        }
        rom
    }

    #[test]
    fn exhirom_bank_c0_reaches_lower_half() {
        let mut m = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        // $C0:0000 → ROM bank 0 → marker $00
        assert_eq!(m.read(make_addr(0xC0, 0x0000)), Some(0x00));
        // $C5:0000 → ROM bank 5 → marker $05
        assert_eq!(m.read(make_addr(0xC5, 0x0000)), Some(0x05));
    }

    #[test]
    fn exhirom_bank_40_reaches_upper_half() {
        let mut m = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        // $40:0000 → ROM bank 64 → marker $40
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0x40));
        // $45:0000 → ROM bank 69 → marker $45
        assert_eq!(m.read(make_addr(0x45, 0x0000)), Some(0x45));
    }

    #[test]
    fn exhirom_bank_00_high_half_mirrors_upper() {
        // $00:8000 should read the upper half of ROM bank 64 in
        // ExHiROM, i.e. file offset $40 * $10000 + $8000.
        let mut rom = marked_rom_8mb();
        rom[0x40 * 0x1_0000 + 0x8000] = 0xAB;
        let mut m = HiRomMapper::new_exhirom(rom, 0);
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0xAB));
        // And via the FastROM mirror, $80:$8000 reaches the upper
        // half of ROM bank 0 (lower 4 MB), which we marked with $00.
        assert_eq!(m.read(make_addr(0x80, 0x8000)), Some(0x00));
    }

    #[test]
    fn exhirom_bank_80_high_half_mirrors_lower() {
        let mut m = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        // $80:8000 → upper half of ROM bank 0 (lower 4 MB), marker $00
        assert_eq!(m.read(make_addr(0x80, 0x8000)), Some(0x00));
        // $85:8000 → upper half of ROM bank 5, marker is not at offset 0x8000 but $05
        // is at offset 0x50000, so $85:8000 reads rom[0x58000] which we haven't
        // marked — sanity check it doesn't return the wrong half.
        // The relevant distinction: this must equal $C5:8000.
        assert_eq!(
            m.read(make_addr(0x85, 0x8000)),
            m.read(make_addr(0xC5, 0x8000))
        );
    }

    #[test]
    fn exhirom_hi_and_lo_halves_are_distinct() {
        // Cross-check: bank $40 (upper half) and $C0 (lower half)
        // must NOT read the same ROM byte.
        let mut m = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        assert_ne!(
            m.read(make_addr(0x40, 0x0000)),
            m.read(make_addr(0xC0, 0x0000)),
        );
    }

    #[test]
    fn hirom_and_exhirom_agree_on_lower_4mb() {
        // For banks $C0..$FF the two modes are identical.
        let hi = HiRomMapper::new(marked_rom_8mb(), 0);
        let mut hi = hi;
        let mut ex = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        for b in [0xC0u8, 0xC5, 0xFD] {
            assert_eq!(hi.read(make_addr(b, 0x1234)), ex.read(make_addr(b, 0x1234)));
        }
    }

    #[test]
    fn exhirom_sram_window_is_same_as_hirom() {
        // SRAM placement is independent of the lower/upper-4MB split.
        let mut ex = HiRomMapper::new_exhirom(marked_rom_8mb(), 8 * 1024);
        let addr = make_addr(0x20, 0x6000);
        assert!(ex.write(addr, 0x42));
        assert_eq!(ex.read(addr), Some(0x42));
        // Mirror at $A0.
        assert_eq!(ex.read(make_addr(0xA0, 0x6000)), Some(0x42));
    }

    #[test]
    fn exhirom_kind_is_exhirom() {
        let m = HiRomMapper::new_exhirom(marked_rom_8mb(), 0);
        assert_eq!(m.kind(), MapperKind::ExHiRom);
    }

    #[test]
    #[should_panic(expected = "HiRomMapper only supports")]
    fn with_kind_rejects_non_hirom_family() {
        let _ = HiRomMapper::with_kind(MapperKind::LoRom, vec![0u8; 0x1_0000], 0);
    }
}
