//! SNES memory access speed lookup.
//!
//! Each region of the SNES memory map is accessed at one of three speeds.
//! The cost is in master clock cycles per byte access, and is paid via
//! [`crate::Bus::io_cycle`] at every read or write.
//!
//! References: <https://problemkaputt.de/fullsnes.htm> §"SNES Memory Map"
//! and §"SNES Mem Speed (`FastROM` / `SlowROM`)".

use crate::types::{Addr24, MCycles, bank_of, offset_of};

/// SNES memory access speed for a single byte access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySpeed {
    /// 6 master cycles. ROM in banks `$80-$BF` / `$C0-$FF` at `$8000-$FFFF`
    /// if the `FastROM` `MEMSEL` bit is set, and a few other fast regions.
    Fast,
    /// 8 master cycles. Default speed for most of the memory map (WRAM,
    /// MMIO, SRAM, ROM in slow banks).
    Slow,
    /// 12 master cycles. Joypad-controller registers `$4016-$4017`.
    XSlow,
}

impl MemorySpeed {
    /// Master-cycle cost for one byte at this speed.
    #[inline]
    #[must_use]
    pub const fn mcycles(self) -> MCycles {
        match self {
            Self::Fast => 6,
            Self::Slow => 8,
            Self::XSlow => 12,
        }
    }
}

/// Look up the access speed for a given 24-bit address.
///
/// `fast_rom` indicates whether the `FastROM` `MEMSEL` bit is currently set
/// (register `$420D`): when true, ROM accesses in banks `$80-$FF` at
/// `$8000-$FFFF` are FAST (6 mclk) instead of SLOW (8 mclk).
///
/// This function is timing-oriented only — it does **not** decide whether
/// a region is RAM, ROM, MMIO, etc. The bus implementation handles that
/// routing separately.
#[must_use]
pub const fn address_speed(addr: Addr24, fast_rom: bool) -> MemorySpeed {
    let bank = bank_of(addr);
    let offset = offset_of(addr);

    // Joypad registers are XSLOW. They live in banks $00-$3F (mirrored at
    // $80-$BF) at offsets $4016-$4017.
    if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4016..=0x4017) {
        return MemorySpeed::XSlow;
    }

    match bank {
        // System area in banks $00-$3F (and mirror $80-$BF).
        0x00..=0x3F | 0x80..=0xBF => match offset {
            // LowRAM mirror.
            0x0000..=0x1FFF => MemorySpeed::Slow,
            // PPU / APU / CPU / DMA MMIO.
            0x2000..=0x5FFF => MemorySpeed::Slow,
            // Open bus / expansion.
            0x6000..=0x7FFF => MemorySpeed::Slow,
            // ROM cartridge area.
            0x8000..=0xFFFF => {
                if fast_rom && bank >= 0x80 {
                    MemorySpeed::Fast
                } else {
                    MemorySpeed::Slow
                }
            }
        },

        // HiROM / mid-banks: ROM (banks $40-$7D and mirror $C0-$FD).
        0x40..=0x7D => MemorySpeed::Slow,
        0xC0..=0xFD => {
            if fast_rom {
                MemorySpeed::Fast
            } else {
                MemorySpeed::Slow
            }
        }

        // WRAM at banks $7E-$7F: always SLOW.
        0x7E..=0x7F => MemorySpeed::Slow,

        // Banks $FE-$FF: ROM, FastROM-eligible.
        0xFE..=0xFF => {
            if fast_rom {
                MemorySpeed::Fast
            } else {
                MemorySpeed::Slow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    #[test]
    fn fast_rom_only_kicks_in_above_0x80() {
        assert_eq!(
            address_speed(make_addr(0x00, 0x8000), true),
            MemorySpeed::Slow
        );
        assert_eq!(
            address_speed(make_addr(0x80, 0x8000), true),
            MemorySpeed::Fast
        );
        assert_eq!(
            address_speed(make_addr(0x80, 0x8000), false),
            MemorySpeed::Slow
        );
    }

    #[test]
    fn joypad_registers_are_xslow() {
        assert_eq!(
            address_speed(make_addr(0x00, 0x4016), false),
            MemorySpeed::XSlow
        );
        assert_eq!(
            address_speed(make_addr(0x00, 0x4017), false),
            MemorySpeed::XSlow
        );
        // Mirror in $80-$BF banks.
        assert_eq!(
            address_speed(make_addr(0x80, 0x4016), false),
            MemorySpeed::XSlow
        );
        // FastROM bit shouldn't override the XSLOW joypad cost.
        assert_eq!(
            address_speed(make_addr(0x80, 0x4016), true),
            MemorySpeed::XSlow
        );
    }

    #[test]
    fn wram_is_always_slow() {
        assert_eq!(
            address_speed(make_addr(0x7E, 0x0000), true),
            MemorySpeed::Slow
        );
        assert_eq!(
            address_speed(make_addr(0x7F, 0xFFFF), true),
            MemorySpeed::Slow
        );
    }

    #[test]
    fn ppu_registers_are_slow() {
        assert_eq!(
            address_speed(make_addr(0x00, 0x2100), false),
            MemorySpeed::Slow
        );
        assert_eq!(
            address_speed(make_addr(0x00, 0x213F), false),
            MemorySpeed::Slow
        );
    }

    #[test]
    fn mcycles_lookup_matches_spec() {
        assert_eq!(MemorySpeed::Fast.mcycles(), 6);
        assert_eq!(MemorySpeed::Slow.mcycles(), 8);
        assert_eq!(MemorySpeed::XSlow.mcycles(), 12);
    }
}
