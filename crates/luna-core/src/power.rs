//! Power-on memory state (issue #224).
//!
//! Real RAM does not come up zeroed. A ROM whose boot code forgets to
//! clear something reads garbage on hardware — and whatever the emulator
//! chose to fill RAM with on an emulator. luna's default is all-zero
//! (deterministic goldens); this module adds the other two states the
//! references offer so a corpus can be run in both:
//!
//! - **ares** (`cpu.cpp:92`, `ppu.cpp:99,123`, `dsp.cpp:200`): WRAM, VRAM,
//!   CGRAM (then `& 0x7FFF`) and APU RAM are randomised on **power**, and
//!   left alone on **reset**. OAM objects are zeroed.
//! - **Mesen2** (`EmuSettings::InitializeRam`): `AllZeros | AllOnes |
//!   Random` (one `mt19937`) over work RAM, VRAM, CGRAM, OAM and SPC RAM;
//!   `Reset()` never re-initialises.
//!
//! luna fills the five RAM arrays (WRAM, VRAM, CGRAM masked to 15 bits,
//! OAM, ARAM) from one seeded generator in a fixed order, so a seed
//! reproduces the exact machine; a soft reset keeps every array, as both
//! references do. OAM follows Mesen2 (random) rather than ares (zero): it
//! is RAM on hardware, and a stale sprite table is exactly the class of
//! boot bug this mode exists to expose.
//!
//! Under [`PowerOnState::Random`] the PPU's registers, latches and both
//! chip MDRs are randomised too, as ares does in `PPU::power` (`ppu.cpp`)
//! — that is the second half of issue #224. Mesen2 randomises only a
//! couple of them (`SnesPpu.cpp:2398`), so this follows ares. `zero` and
//! `ones` leave the registers at their deterministic defaults, which is
//! what every golden and every CI run uses.

use serde::{Deserialize, Serialize};

/// What every RAM array holds when the machine powers on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PowerOnState {
    /// All bytes `$00` (luna's historical default; Mesen2 `AllZeros`).
    #[default]
    Zero,
    /// All bytes `$FF` (Mesen2 `AllOnes`).
    Ones,
    /// Pseudo-random bytes from `seed` (ares / Mesen2 `Random`). The same
    /// seed always yields the same machine.
    Random {
        /// Generator seed.
        seed: u64,
    },
}

impl PowerOnState {
    /// Fill `buf` per the state, drawing from `rng` for [`Self::Random`].
    pub fn fill(self, buf: &mut [u8], rng: &mut PowerOnRng) {
        match self {
            Self::Zero => buf.fill(0x00),
            Self::Ones => buf.fill(0xFF),
            Self::Random { .. } => rng.fill_bytes(buf),
        }
    }

    /// `true` when this state randomises registers and latches, not just
    /// RAM (ares randomises them on power only, never on reset).
    #[must_use]
    pub const fn randomises_registers(self) -> bool {
        matches!(self, Self::Random { .. })
    }

    /// The generator for this state (seeded for [`Self::Random`], inert
    /// otherwise).
    #[must_use]
    pub const fn rng(self) -> PowerOnRng {
        match self {
            Self::Random { seed } => PowerOnRng::new(seed),
            Self::Zero | Self::Ones => PowerOnRng::new(0),
        }
    }
}

/// A small, dependency-free, platform-independent generator
/// (`xoshiro256**`, seeded through `splitmix64`) — reproducibility across
/// builds and architectures is the whole point, so no `rand`.
#[derive(Debug, Clone)]
pub struct PowerOnRng {
    s: [u64; 4],
}

impl PowerOnRng {
    /// Seed the generator.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        let mut x = seed;
        let mut s = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            // splitmix64
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            s[i] = z ^ (z >> 31);
            i += 1;
        }
        Self { s }
    }

    /// Next 64 random bits.
    pub const fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Fill `buf` with random bytes.
    /// One pseudo-random byte.
    pub const fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    /// One pseudo-random 16-bit word.
    pub const fn next_u16(&mut self) -> u16 {
        self.next_u64() as u16
    }

    /// One pseudo-random bit, as ares' `random()` is used for flags.
    pub const fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 != 0
    }

    /// Fill `buf` with pseudo-random bytes.
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_bytes_different_seed_different_bytes() {
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        let mut c = [0u8; 64];
        PowerOnRng::new(42).fill_bytes(&mut a);
        PowerOnRng::new(42).fill_bytes(&mut b);
        PowerOnRng::new(43).fill_bytes(&mut c);
        assert_eq!(a, b);
        assert_ne!(a, c);
        // Not degenerate: a 64-byte draw is neither all-zero nor constant.
        assert!(a.iter().any(|&x| x != a[0]));
    }

    #[test]
    fn fill_follows_the_state() {
        let mut buf = [0xAAu8; 16];
        let mut rng = PowerOnState::Zero.rng();
        PowerOnState::Zero.fill(&mut buf, &mut rng);
        assert_eq!(buf, [0u8; 16]);
        PowerOnState::Ones.fill(&mut buf, &mut rng);
        assert_eq!(buf, [0xFFu8; 16]);
        let st = PowerOnState::Random { seed: 7 };
        let mut rng = st.rng();
        st.fill(&mut buf, &mut rng);
        assert_ne!(buf, [0xFFu8; 16]);
    }
}
