//! SPC700 program status word (PSW).

/// PSW bit masks.
pub mod bit {
    /// Negative.
    pub const N: u8 = 0b1000_0000;
    /// Overflow.
    pub const V: u8 = 0b0100_0000;
    /// Direct page selector: 0 = direct page lives at `$00xx`, 1 = `$01xx`.
    pub const P: u8 = 0b0010_0000;
    /// Break flag (set on BRK, like 6502).
    pub const B: u8 = 0b0001_0000;
    /// Half-carry (for BCD ADC/SBC and DAA/DAS).
    pub const H: u8 = 0b0000_1000;
    /// Interrupt disable.
    pub const I: u8 = 0b0000_0100;
    /// Zero.
    pub const Z: u8 = 0b0000_0010;
    /// Carry.
    pub const C: u8 = 0b0000_0001;
}

/// Wrapper around the 8-bit PSW register with named bit accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Psw(pub u8);

impl Psw {
    /// Raw byte.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` if every bit in `mask` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, mask: u8) -> bool {
        (self.0 & mask) == mask
    }

    /// Set every bit in `mask`.
    #[inline]
    pub fn insert(&mut self, mask: u8) {
        self.0 |= mask;
    }

    /// Clear every bit in `mask`.
    #[inline]
    pub fn remove(&mut self, mask: u8) {
        self.0 &= !mask;
    }

    /// Set every bit in `mask` to `value`.
    #[inline]
    pub fn set(&mut self, mask: u8, value: bool) {
        if value {
            self.insert(mask);
        } else {
            self.remove(mask);
        }
    }

    /// `true` if the direct page lives at `$01xx` (P=1).
    #[inline]
    #[must_use]
    pub const fn direct_page_high(self) -> bool {
        self.contains(bit::P)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p_flag_selects_direct_page() {
        let mut p = Psw::default();
        assert!(!p.direct_page_high());
        p.insert(bit::P);
        assert!(p.direct_page_high());
    }

    #[test]
    fn set_and_remove_round_trip() {
        let mut p = Psw::default();
        p.set(bit::C, true);
        assert!(p.contains(bit::C));
        p.set(bit::C, false);
        assert!(!p.contains(bit::C));
    }
}
