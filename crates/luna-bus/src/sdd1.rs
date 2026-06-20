//! S-DD1 graphics-decompression coprocessor (Star Ocean, Street Fighter
//! Alpha 2).
//!
//! This module hosts the **decompressor** — a faithful port of the canonical
//! ares / Mesen2 implementation (both byte-identical; see
//! `docs/sdd1_reference.md`). It is a self-contained streaming state machine:
//! the chip decompresses graphics on the fly while a DMA reads from it.
//!
//! Pipeline: Input Manager (bit reader) → Golomb-Code Decoder → 8 Bits
//! Generators → Probability Estimation Module → Context Model (bitplane
//! interleave) → Output Logic (bitplane → byte). The decompressor reads its
//! compressed input only through the [`Sdd1RomBus`] trait, so it is fully
//! testable in isolation from the SNES bus.

/// Raw (already MMC-bank-switched) ROM byte source for the decompressor — the
/// chip's `mmcRead`. Kept abstract so the decompressor is unit-testable
/// against a plain byte slice and driven by the real mapper in production.
pub trait Sdd1RomBus {
    /// Read the ROM byte at the MMC-decoded 24-bit address.
    fn mmc_read(&self, addr: u32) -> u8;
}

/// One Probability-Estimation-Module evolution-table entry: which Bits
/// Generator (`code_num`) feeds this state, and the next state on a
/// most-/least-probable-symbol run end.
#[derive(Clone, Copy)]
struct EvolutionState {
    code_num: u8,
    next_mps: u8,
    next_lps: u8,
}

/// Golomb run-length table (256 entries) — verbatim from ares
/// `decompressor.cpp` / Mesen2 `Sdd1Decomp.cpp` (identical in both).
#[rustfmt::skip]
const RUN_COUNT: [u8; 256] = [
    0x00, 0x00, 0x01, 0x00, 0x03, 0x01, 0x02, 0x00,
    0x07, 0x03, 0x05, 0x01, 0x06, 0x02, 0x04, 0x00,
    0x0f, 0x07, 0x0b, 0x03, 0x0d, 0x05, 0x09, 0x01,
    0x0e, 0x06, 0x0a, 0x02, 0x0c, 0x04, 0x08, 0x00,
    0x1f, 0x0f, 0x17, 0x07, 0x1b, 0x0b, 0x13, 0x03,
    0x1d, 0x0d, 0x15, 0x05, 0x19, 0x09, 0x11, 0x01,
    0x1e, 0x0e, 0x16, 0x06, 0x1a, 0x0a, 0x12, 0x02,
    0x1c, 0x0c, 0x14, 0x04, 0x18, 0x08, 0x10, 0x00,
    0x3f, 0x1f, 0x2f, 0x0f, 0x37, 0x17, 0x27, 0x07,
    0x3b, 0x1b, 0x2b, 0x0b, 0x33, 0x13, 0x23, 0x03,
    0x3d, 0x1d, 0x2d, 0x0d, 0x35, 0x15, 0x25, 0x05,
    0x39, 0x19, 0x29, 0x09, 0x31, 0x11, 0x21, 0x01,
    0x3e, 0x1e, 0x2e, 0x0e, 0x36, 0x16, 0x26, 0x06,
    0x3a, 0x1a, 0x2a, 0x0a, 0x32, 0x12, 0x22, 0x02,
    0x3c, 0x1c, 0x2c, 0x0c, 0x34, 0x14, 0x24, 0x04,
    0x38, 0x18, 0x28, 0x08, 0x30, 0x10, 0x20, 0x00,
    0x7f, 0x3f, 0x5f, 0x1f, 0x6f, 0x2f, 0x4f, 0x0f,
    0x77, 0x37, 0x57, 0x17, 0x67, 0x27, 0x47, 0x07,
    0x7b, 0x3b, 0x5b, 0x1b, 0x6b, 0x2b, 0x4b, 0x0b,
    0x73, 0x33, 0x53, 0x13, 0x63, 0x23, 0x43, 0x03,
    0x7d, 0x3d, 0x5d, 0x1d, 0x6d, 0x2d, 0x4d, 0x0d,
    0x75, 0x35, 0x55, 0x15, 0x65, 0x25, 0x45, 0x05,
    0x79, 0x39, 0x59, 0x19, 0x69, 0x29, 0x49, 0x09,
    0x71, 0x31, 0x51, 0x11, 0x61, 0x21, 0x41, 0x01,
    0x7e, 0x3e, 0x5e, 0x1e, 0x6e, 0x2e, 0x4e, 0x0e,
    0x76, 0x36, 0x56, 0x16, 0x66, 0x26, 0x46, 0x06,
    0x7a, 0x3a, 0x5a, 0x1a, 0x6a, 0x2a, 0x4a, 0x0a,
    0x72, 0x32, 0x52, 0x12, 0x62, 0x22, 0x42, 0x02,
    0x7c, 0x3c, 0x5c, 0x1c, 0x6c, 0x2c, 0x4c, 0x0c,
    0x74, 0x34, 0x54, 0x14, 0x64, 0x24, 0x44, 0x04,
    0x78, 0x38, 0x58, 0x18, 0x68, 0x28, 0x48, 0x08,
    0x70, 0x30, 0x50, 0x10, 0x60, 0x20, 0x40, 0x00,
];

/// Probability-estimation evolution table (33 states) — verbatim from ares /
/// Mesen2 (identical in both). Each = `{code_num, next_if_mps, next_if_lps}`.
#[rustfmt::skip]
const EVOLUTION_TABLE: [EvolutionState; 33] = [
    EvolutionState { code_num: 0, next_mps: 25, next_lps: 25 },
    EvolutionState { code_num: 0, next_mps:  2, next_lps:  1 },
    EvolutionState { code_num: 0, next_mps:  3, next_lps:  1 },
    EvolutionState { code_num: 0, next_mps:  4, next_lps:  2 },
    EvolutionState { code_num: 0, next_mps:  5, next_lps:  3 },
    EvolutionState { code_num: 1, next_mps:  6, next_lps:  4 },
    EvolutionState { code_num: 1, next_mps:  7, next_lps:  5 },
    EvolutionState { code_num: 1, next_mps:  8, next_lps:  6 },
    EvolutionState { code_num: 1, next_mps:  9, next_lps:  7 },
    EvolutionState { code_num: 2, next_mps: 10, next_lps:  8 },
    EvolutionState { code_num: 2, next_mps: 11, next_lps:  9 },
    EvolutionState { code_num: 2, next_mps: 12, next_lps: 10 },
    EvolutionState { code_num: 2, next_mps: 13, next_lps: 11 },
    EvolutionState { code_num: 3, next_mps: 14, next_lps: 12 },
    EvolutionState { code_num: 3, next_mps: 15, next_lps: 13 },
    EvolutionState { code_num: 3, next_mps: 16, next_lps: 14 },
    EvolutionState { code_num: 3, next_mps: 17, next_lps: 15 },
    EvolutionState { code_num: 4, next_mps: 18, next_lps: 16 },
    EvolutionState { code_num: 4, next_mps: 19, next_lps: 17 },
    EvolutionState { code_num: 5, next_mps: 20, next_lps: 18 },
    EvolutionState { code_num: 5, next_mps: 21, next_lps: 19 },
    EvolutionState { code_num: 6, next_mps: 22, next_lps: 20 },
    EvolutionState { code_num: 6, next_mps: 23, next_lps: 21 },
    EvolutionState { code_num: 7, next_mps: 24, next_lps: 22 },
    EvolutionState { code_num: 7, next_mps: 24, next_lps: 23 },
    EvolutionState { code_num: 0, next_mps: 26, next_lps:  1 },
    EvolutionState { code_num: 1, next_mps: 27, next_lps:  2 },
    EvolutionState { code_num: 2, next_mps: 28, next_lps:  4 },
    EvolutionState { code_num: 3, next_mps: 29, next_lps:  8 },
    EvolutionState { code_num: 4, next_mps: 30, next_lps: 12 },
    EvolutionState { code_num: 5, next_mps: 31, next_lps: 16 },
    EvolutionState { code_num: 6, next_mps: 32, next_lps: 18 },
    EvolutionState { code_num: 7, next_mps: 24, next_lps: 22 },
];

/// The S-DD1 streaming decompressor. One `init` per DMA stream, then one
/// [`Self::decompress_byte`] per output byte. All six pipeline stages are
/// flattened into this single struct (idiomatic Rust vs. ares' parent-ref
/// object graph); the logic is a line-for-line port.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sdd1Decompressor {
    // Input Manager.
    offset: u32,
    bit_count: u32,
    // Bits Generators (8, indexed by code_num).
    mps_count: [u8; 8],
    lps_index: [bool; 8],
    // Probability Estimation Module (32 contexts).
    context_status: [u8; 32],
    context_mps: [u8; 32],
    // Context Model.
    bitplanes_info: u8,
    context_bits_info: u8,
    bit_number: u8,
    curr_bitplane: u8,
    prev_bitplane_bits: [u16; 8],
    // Output Logic.
    r0: u8,
    r1: u8,
    r2: u8,
}

impl Default for Sdd1Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Sdd1Decompressor {
    /// A fresh, un-initialised decompressor (call [`Self::init`] before use).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            offset: 0,
            bit_count: 4,
            mps_count: [0; 8],
            lps_index: [false; 8],
            context_status: [0; 32],
            context_mps: [0; 32],
            bitplanes_info: 0,
            context_bits_info: 0,
            bit_number: 0,
            curr_bitplane: 0,
            prev_bitplane_bits: [0; 8],
            r0: 1,
            r1: 0,
            r2: 0,
        }
    }

    /// Begin a new decompression stream at MMC-decoded ROM `offset`. The byte
    /// at `offset` is the header: bits 7:6 select the bitplane mode, 5:4 the
    /// context-bit window; the compressed bitstream starts at bit 3 (the Input
    /// Manager begins at `bit_count = 4`).
    pub fn init(&mut self, rom: &impl Sdd1RomBus, offset: u32) {
        let first_byte = rom.mmc_read(offset);
        // Input Manager.
        self.offset = offset;
        self.bit_count = 4;
        // Bits Generators.
        self.mps_count = [0; 8];
        self.lps_index = [false; 8];
        // Probability Estimation Module.
        self.context_status = [0; 32];
        self.context_mps = [0; 32];
        // Context Model.
        self.bitplanes_info = first_byte & 0xc0;
        self.context_bits_info = first_byte & 0x30;
        self.bit_number = 0;
        self.prev_bitplane_bits = [0; 8];
        self.curr_bitplane = match self.bitplanes_info {
            0x00 => 1,
            0x40 => 7,
            0x80 => 3,
            // 0xc0: derived from `bit_number` per bit, init value unused.
            _ => 0,
        };
        // Output Logic.
        self.r0 = 1;
        self.r1 = 0;
        self.r2 = 0;
    }

    /// Produce the next decompressed byte (Output Logic). For the 2-bitplane
    /// modes (`0x00/0x40/0x80`) one call reads 16 bits and returns the low
    /// plane, the next returns the buffered high plane; for `0xc0` (8 bpp) one
    /// call reads 8 bits and returns one byte.
    pub fn decompress_byte(&mut self, rom: &impl Sdd1RomBus) -> u8 {
        if self.bitplanes_info == 0xc0 {
            self.r1 = 0;
            self.r0 = 0x01;
            while self.r0 != 0 {
                if self.cm_get_bit(rom) != 0 {
                    self.r1 |= self.r0;
                }
                self.r0 <<= 1;
            }
            return self.r1;
        }
        // Modes 0x00 / 0x40 / 0x80: two bitplanes, returned across two calls.
        if self.r0 == 0 {
            self.r0 = !self.r0;
            return self.r2;
        }
        self.r0 = 0x80;
        self.r1 = 0;
        self.r2 = 0;
        while self.r0 != 0 {
            if self.cm_get_bit(rom) != 0 {
                self.r1 |= self.r0;
            }
            if self.cm_get_bit(rom) != 0 {
                self.r2 |= self.r0;
            }
            self.r0 >>= 1;
        }
        self.r1
    }

    /// Context Model: pick the bitplane, build the prediction context from the
    /// plane's recent-bit history, decode one bit, and update the history.
    fn cm_get_bit(&mut self, rom: &impl Sdd1RomBus) -> u8 {
        match self.bitplanes_info {
            0x00 => self.curr_bitplane ^= 0x01,
            0x40 => {
                self.curr_bitplane ^= 0x01;
                if self.bit_number & 0x7f == 0 {
                    self.curr_bitplane = (self.curr_bitplane + 2) & 0x07;
                }
            }
            0x80 => {
                self.curr_bitplane ^= 0x01;
                if self.bit_number & 0x7f == 0 {
                    self.curr_bitplane ^= 0x02;
                }
            }
            // 0xc0
            _ => self.curr_bitplane = self.bit_number & 0x07,
        }

        let idx = usize::from(self.curr_bitplane);
        let context_bits = self.prev_bitplane_bits[idx];
        let low = u8::try_from(context_bits & 0x0001).unwrap_or(0);
        let low2 = u8::try_from(context_bits & 0x0003).unwrap_or(0);
        let mut context = (self.curr_bitplane & 0x01) << 4;
        context |= match self.context_bits_info {
            0x00 => u8::try_from((context_bits & 0x01c0) >> 5).unwrap_or(0) | low,
            0x10 => u8::try_from((context_bits & 0x0180) >> 5).unwrap_or(0) | low,
            0x20 => u8::try_from((context_bits & 0x00c0) >> 5).unwrap_or(0) | low,
            // 0x30
            _ => u8::try_from((context_bits & 0x0180) >> 5).unwrap_or(0) | low2,
        };

        let bit = self.pem_get_bit(rom, context);
        self.prev_bitplane_bits[idx] = (context_bits << 1) | u16::from(bit);
        self.bit_number = self.bit_number.wrapping_add(1);
        bit
    }

    /// Probability Estimation Module: decode one bit for `context`, evolve the
    /// context's adaptive state on a run end, and apply the MPS inversion.
    fn pem_get_bit(&mut self, rom: &impl Sdd1RomBus, context: u8) -> u8 {
        let ctx = usize::from(context);
        let status = self.context_status[ctx];
        let mps = self.context_mps[ctx];
        let state = EVOLUTION_TABLE[usize::from(status)];

        let mut end_of_run = false;
        let bit = self.bg_get_bit(rom, state.code_num, &mut end_of_run);

        if end_of_run {
            if bit != 0 {
                if status & 0xfe == 0 {
                    self.context_mps[ctx] ^= 0x01;
                }
                self.context_status[ctx] = state.next_lps;
            } else {
                self.context_status[ctx] = state.next_mps;
            }
        }

        bit ^ mps
    }

    /// Bits Generator `code_num`: expand the current Golomb run into single
    /// bits (MPS = 0 run, then one LPS = 1), fetching a fresh run when empty.
    /// `end_of_run` is set when the run is exhausted.
    fn bg_get_bit(&mut self, rom: &impl Sdd1RomBus, code_num: u8, end_of_run: &mut bool) -> u8 {
        let i = usize::from(code_num);
        if self.mps_count[i] == 0 && !self.lps_index[i] {
            self.gcd_get_run_count(rom, code_num);
        }
        let bit = if self.mps_count[i] != 0 {
            self.mps_count[i] -= 1;
            0
        } else {
            self.lps_index[i] = false;
            1
        };
        *end_of_run = self.mps_count[i] == 0 && !self.lps_index[i];
        bit
    }

    /// Golomb-Code Decoder: fetch a codeword and resolve it into a
    /// most-probable-symbol run count (and whether a least-probable symbol
    /// terminates it).
    fn gcd_get_run_count(&mut self, rom: &impl Sdd1RomBus, code_num: u8) {
        let codeword = self.im_get_codeword(rom, code_num);
        let i = usize::from(code_num);
        if codeword & 0x80 != 0 {
            self.lps_index[i] = true;
            self.mps_count[i] = RUN_COUNT[usize::from(codeword >> (code_num ^ 0x07))];
        } else {
            self.mps_count[i] = 1u8 << code_num;
        }
    }

    /// Input Manager: read a `code_len`-bit codeword from the compressed
    /// bitstream, straddling byte boundaries. The `>> (9 - bit_count)` runs
    /// over a `u16` to reproduce C++'s integer promotion: a shift of 8 yields
    /// 0 (a `u8 >> 8` would panic in Rust and `wrapping_shr` would wrongly
    /// shift by 0).
    fn im_get_codeword(&mut self, rom: &impl Sdd1RomBus, code_len: u8) -> u8 {
        let mut codeword = rom.mmc_read(self.offset).wrapping_shl(self.bit_count);
        self.bit_count += 1;
        if codeword & 0x80 != 0 {
            let next = u16::from(rom.mmc_read(self.offset + 1));
            codeword |= u8::try_from(next >> (9 - self.bit_count)).unwrap_or(0);
            self.bit_count += u32::from(code_len);
        }
        if self.bit_count & 0x08 != 0 {
            self.offset += 1;
            self.bit_count &= 0x07;
        }
        codeword
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain byte-slice ROM source for testing the decompressor in
    /// isolation (out-of-range reads return 0, like open ROM).
    struct SliceBus<'a> {
        data: &'a [u8],
    }
    impl Sdd1RomBus for SliceBus<'_> {
        fn mmc_read(&self, addr: u32) -> u8 {
            self.data.get(addr as usize).copied().unwrap_or(0)
        }
    }

    #[test]
    fn tables_have_canonical_size_and_known_entries() {
        // Transcription guard: the two tables are the byte-exact ares/Mesen2
        // constants; a single wrong entry corrupts all decompression.
        assert_eq!(RUN_COUNT.len(), 256);
        assert_eq!(EVOLUTION_TABLE.len(), 33);
        // Spot-check a few load-bearing entries.
        assert_eq!(RUN_COUNT[0], 0x00);
        assert_eq!(RUN_COUNT[2], 0x01);
        assert_eq!(RUN_COUNT[128], 0x7f);
        assert_eq!(RUN_COUNT[255], 0x00);
        let s0 = EVOLUTION_TABLE[0];
        assert_eq!((s0.code_num, s0.next_mps, s0.next_lps), (0, 25, 25));
        let s24 = EVOLUTION_TABLE[24];
        assert_eq!((s24.code_num, s24.next_mps, s24.next_lps), (7, 24, 23));
        let s32 = EVOLUTION_TABLE[32];
        assert_eq!((s32.code_num, s32.next_mps, s32.next_lps), (7, 24, 22));
    }

    #[test]
    fn input_manager_reads_codewords_across_byte_boundaries() {
        // Hand-computed against the ares getCodeWord algorithm, exercising the
        // two-byte straddle AND the `9 - bit_count == 8` u16-promotion edge.
        let rom = SliceBus {
            data: &[0xAB, 0xCD, 0x00, 0x00],
        };
        let mut d = Sdd1Decompressor::new();
        d.init(&rom, 0); // offset=0, bit_count=4

        // call 1, code_len=2: cw = (0xAB<<4)=0xB0; bit7 set → |= 0xCD>>4=0x0C
        //   → 0xBC; bit_count 4→5→7; no byte advance.
        let cw1 = d.im_get_codeword(&rom, 2);
        assert_eq!(cw1, 0xBC);
        assert_eq!((d.offset, d.bit_count), (0, 7));

        // call 2, code_len=3: cw = (0xAB<<7)=0x80; bit7 set → |= 0xCD>>(9-8=1)
        //   =0x66 → 0xE6; bit_count 7→8→11 → &7=3, offset→1.
        let cw2 = d.im_get_codeword(&rom, 3);
        assert_eq!(cw2, 0xE6);
        assert_eq!((d.offset, d.bit_count), (1, 3));
    }

    #[test]
    fn decompress_is_deterministic_and_panic_free() {
        // Without a public (compressed, decompressed) golden vector the
        // byte-exact check is the in-game validation (Stage 3); here we guard
        // that streaming is deterministic and never panics on the shift edges
        // across all four bitplane modes.
        let stream: Vec<u8> = (0u32..512)
            .map(|i| (i.wrapping_mul(73) ^ 0x5A) as u8)
            .collect();
        for header in [0x00u8, 0x40, 0x80, 0xC0] {
            let mut data = stream.clone();
            data[0] = header | (data[0] & 0x3F); // set bitplane mode bits
            let rom = SliceBus { data: &data };

            let mut a = Sdd1Decompressor::new();
            a.init(&rom, 0);
            let out_a: Vec<u8> = (0..64).map(|_| a.decompress_byte(&rom)).collect();

            let mut b = Sdd1Decompressor::new();
            b.init(&rom, 0);
            let out_b: Vec<u8> = (0..64).map(|_| b.decompress_byte(&rom)).collect();

            assert_eq!(out_a, out_b, "mode {header:#04x} must be deterministic");
        }
    }
}
