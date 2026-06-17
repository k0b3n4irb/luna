//! Fundamental time and address aliases used across the Luna emulator.

/// Number of master clock cycles.
///
/// The SNES master clock is 21.477272 MHz (NTSC) / 21.281370 MHz (PAL).
/// One frame ≈ 357 366 master cycles (NTSC).
///
/// A `u64` master-cycle counter overflows after ~27 000 years of emulation,
/// so we don't need wrap-around handling anywhere.
pub type MCycles = u64;

/// A 24-bit SNES bus address `$bb:aaaa`.
///
/// Stored as `u32` for ergonomics; the top 8 bits are always zero in valid
/// values. Helper functions [`bank_of`] and [`offset_of`] decompose it.
pub type Addr24 = u32;

/// Master-clock frequency (NTSC), Hz.
pub const NTSC_MASTER_HZ: u64 = 21_477_272;

/// Master-clock frequency (PAL), Hz.
pub const PAL_MASTER_HZ: u64 = 21_281_370;

/// Number of master cycles per NTSC frame (262 scanlines × 1364 dots).
pub const MCYCLES_PER_NTSC_FRAME: MCycles = 262 * 1364;

/// Number of master cycles per PAL frame (312 scanlines × 1364 dots).
pub const MCYCLES_PER_PAL_FRAME: MCycles = 312 * 1364;

/// Extract the 8-bit bank component of a 24-bit address.
#[inline]
#[must_use]
pub const fn bank_of(addr: Addr24) -> u8 {
    ((addr >> 16) & 0xFF) as u8
}

/// Extract the 16-bit offset component of a 24-bit address.
#[inline]
#[must_use]
pub const fn offset_of(addr: Addr24) -> u16 {
    (addr & 0xFFFF) as u16
}

/// Assemble a 24-bit address from a bank and an offset.
#[inline]
#[must_use]
pub const fn make_addr(bank: u8, offset: u16) -> Addr24 {
    ((bank as u32) << 16) | (offset as u32)
}

/// Fold a linear ROM address into a `size`-byte image, mirroring exactly
/// as SNES hardware does. Faithful port of ares' `Bus::mirror`
/// (`sfc/memory/inline.hpp`). For a power-of-two `size` this is just
/// `address % size`; for a non-power-of-two cart (e.g. 1.5 MB) the region
/// past the end mirrors the trailing largest-power-of-two chunk rather
/// than returning open bus. `size == 0` yields 0 (caller treats an empty
/// ROM as unmapped).
#[must_use]
pub const fn rom_mirror(mut address: usize, mut size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    let mut base = 0usize;
    // SNES ROM space is 24-bit (max 16 MB); start at the top bit.
    let mut mask = 1usize << 23;
    while address >= size {
        while address & mask == 0 {
            mask >>= 1;
        }
        address -= mask;
        if size > mask {
            size -= mask;
            base += mask;
        }
        mask >>= 1;
    }
    base + address
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompose_address() {
        let a: Addr24 = 0x7E_1234;
        assert_eq!(bank_of(a), 0x7E);
        assert_eq!(offset_of(a), 0x1234);
    }

    #[test]
    fn assemble_address() {
        assert_eq!(make_addr(0x7E, 0x1234), 0x7E_1234);
        assert_eq!(make_addr(0, 0), 0);
        assert_eq!(make_addr(0xFF, 0xFFFF), 0x00FF_FFFF);
    }

    #[test]
    fn rom_mirror_matches_ares_bus_mirror() {
        // size 0 → 0 (empty image).
        assert_eq!(rom_mirror(123, 0), 0);
        // In-range addresses pass through untouched.
        assert_eq!(rom_mirror(0, 0x10_0000), 0);
        assert_eq!(rom_mirror(0xF_FFFF, 0x10_0000), 0xF_FFFF);
        // Power-of-two ⇒ plain modulo.
        assert_eq!(rom_mirror(0x10_0000, 0x10_0000), 0);
        assert_eq!(rom_mirror(0x18_0005, 0x10_0000), 0x8_0005);
        // Non-pow2 1.5 MB: the end mirrors the trailing 0.5 MB chunk.
        assert_eq!(rom_mirror(0x18_0000, 0x18_0000), 0x10_0000);
        assert_eq!(rom_mirror(0x1F_FFFF, 0x18_0000), 0x17_FFFF);
        // Non-pow2 6 MB ExHiROM: end mirrors the trailing 2 MB.
        assert_eq!(rom_mirror(0x60_0000, 0x60_0000), 0x40_0000);
    }

    #[test]
    fn ntsc_frame_cycles_match_spec() {
        // 262 × 1364 = 357 368 mclk per frame at the steady-state
        // (non-interlaced) NTSC clock. Real hardware drops one cycle on
        // odd frames when the PPU is in non-interlace; we keep the
        // ideal value here and let the scheduler/PPU model the dropped
        // cycle.
        assert_eq!(MCYCLES_PER_NTSC_FRAME, 357_368);
    }
}
