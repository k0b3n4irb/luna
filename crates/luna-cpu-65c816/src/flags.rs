//! 65C816 status register (`P`) and the hidden `E` (emulation) flag.

use serde::{Deserialize, Serialize};

/// Status register bit positions.
///
/// `M` and `X` only have meaning when `E = 0` (native mode). In emulation
/// mode (`E = 1`) they are forced to `1` (8-bit accumulator and index
/// registers, 6502-style).
pub mod bit {
    /// Negative.
    pub const N: u8 = 0b1000_0000;
    /// Overflow.
    pub const V: u8 = 0b0100_0000;
    /// Memory / Accumulator width — `1` = 8-bit, `0` = 16-bit. (Native only.)
    pub const M: u8 = 0b0010_0000;
    /// Index width — `1` = 8-bit, `0` = 16-bit. (Native only.)
    pub const X: u8 = 0b0001_0000;
    /// Decimal mode (BCD arithmetic on ADC / SBC).
    pub const D: u8 = 0b0000_1000;
    /// IRQ disable.
    pub const I: u8 = 0b0000_0100;
    /// Zero.
    pub const Z: u8 = 0b0000_0010;
    /// Carry.
    pub const C: u8 = 0b0000_0001;
}

/// Wrapper around the 8-bit `P` register, with named bit accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusFlags(pub u8);

impl StatusFlags {
    /// Power-on / reset value of the status register on the 65C816:
    /// `M = X = I = 1`, all others cleared.
    pub const RESET: Self = Self(bit::M | bit::X | bit::I);

    /// Raw `P` register byte.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` if **all** the bits in `mask` are currently set.
    #[inline]
    #[must_use]
    pub const fn contains(self, mask: u8) -> bool {
        (self.0 & mask) == mask
    }

    /// Set every bit in `mask`.
    #[inline]
    pub const fn insert(&mut self, mask: u8) {
        self.0 |= mask;
    }

    /// Clear every bit in `mask`.
    #[inline]
    pub const fn remove(&mut self, mask: u8) {
        self.0 &= !mask;
    }

    /// Set every bit in `mask` to `value`.
    #[inline]
    pub const fn set(&mut self, mask: u8, value: bool) {
        if value {
            self.insert(mask);
        } else {
            self.remove(mask);
        }
    }

    /// `true` if the accumulator is 8-bit (M flag set).
    #[inline]
    #[must_use]
    pub const fn acc8(self) -> bool {
        self.contains(bit::M)
    }

    /// `true` if the index registers are 8-bit (X flag set).
    #[inline]
    #[must_use]
    pub const fn idx8(self) -> bool {
        self.contains(bit::X)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_value_has_m_x_i_set() {
        let p = StatusFlags::RESET;
        assert!(p.contains(bit::M));
        assert!(p.contains(bit::X));
        assert!(p.contains(bit::I));
        assert!(!p.contains(bit::N));
        assert!(!p.contains(bit::V));
        assert!(!p.contains(bit::D));
        assert!(!p.contains(bit::Z));
        assert!(!p.contains(bit::C));
    }

    #[test]
    fn set_toggles_bits() {
        let mut p = StatusFlags::default();
        p.set(bit::C, true);
        assert!(p.contains(bit::C));
        p.set(bit::C, false);
        assert!(!p.contains(bit::C));
    }

    #[test]
    fn acc8_idx8_helpers() {
        let mut p = StatusFlags::default();
        assert!(!p.acc8());
        assert!(!p.idx8());
        p.insert(bit::M);
        assert!(p.acc8());
        p.insert(bit::X);
        assert!(p.idx8());
    }
}
