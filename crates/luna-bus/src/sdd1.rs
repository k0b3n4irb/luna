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
//!
//! [`Sdd1Mapper`] is the whole chip: the MMC (ROM banking + `$4800-$4807`
//! control registers) plus — wired in a later stage — the DMA-triggered
//! decompression.

use crate::mapper::{Mapper, MapperKind};
use crate::types::{Addr24, bank_of, offset_of, rom_mirror};

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

/// The S-DD1 cartridge mapper: the MMC (ROM banking + `$4800-$4807` control
/// registers) and SRAM, plus the DMA-triggered graphics decompression
/// (streaming through [`Sdd1Decompressor`]).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sdd1Mapper {
    rom: Vec<u8>,
    sram: Vec<u8>,
    /// `$4800` hard-enable mask (per-channel S-DD1 DMA eligibility).
    r4800: u8,
    /// `$4801` soft-enable mask (per-channel arm; chip clears it at stream end).
    r4801: u8,
    /// `$4804-$4807` MMC bank selects for `$C0-CF/$D0-DF/$E0-EF/$F0-FF`
    /// (bit 7 of `$4805`/`$4807` also flag the `$20-3F`/`$A0-BF` ROM mirror).
    r4804: u8,
    r4805: u8,
    r4806: u8,
    r4807: u8,
    /// The streaming decompressor (one active stream at a time).
    decompressor: Sdd1Decompressor,
    /// Per-channel DMA source address / remaining length, captured by
    /// observing `$43x2-6` writes — so a decompressing DMA read is recognised
    /// (`addr == dma_address[ch]`, fixed-mode) and bounded.
    dma_address: [u32; 8],
    dma_size: [u16; 8],
    /// `true` once the current stream's decompressor is initialised; reset
    /// when a stream's length reaches 0.
    dma_ready: bool,
}

/// Borrowed ROM view implementing the decompressor's input read (`mmcRead` —
/// the `$C0-FF` four-bank MMC decode). Holds `&rom` plus *copies* of the bank
/// selects, so it can be created while the decompressor field is borrowed
/// mutably (disjoint from `rom`).
struct Sdd1RomView<'a> {
    rom: &'a [u8],
    banks: [u8; 4],
}

impl Sdd1RomBus for Sdd1RomView<'_> {
    fn mmc_read(&self, addr: u32) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        let a = addr & 0x00FF_FFFF;
        let sel = self.banks[((a >> 20) & 0x03) as usize];
        let rom_addr = (u32::from(sel & 0x0F) << 20) | (a & 0x000F_FFFF);
        self.rom[rom_mirror(rom_addr as usize, self.rom.len())]
    }
}

impl Sdd1Mapper {
    /// Build an S-DD1 mapper around `rom` with `sram_size` bytes of SRAM.
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_size: usize) -> Self {
        Self {
            rom,
            sram: vec![0; sram_size],
            r4800: 0,
            r4801: 0,
            r4804: 0,
            r4805: 0,
            r4806: 0,
            r4807: 0,
            decompressor: Sdd1Decompressor::new(),
            dma_address: [0; 8],
            dma_size: [0; 8],
            dma_ready: false,
        }
    }

    /// S-DD1 control-register read (`$4800-$480F`); `None` for the non-register
    /// addresses in that window (games never read them → open bus).
    const fn read_reg(&self, offset: u16) -> Option<u8> {
        match offset & 0x000F {
            0x00 => Some(self.r4800),
            0x01 => Some(self.r4801),
            0x04 => Some(self.r4804),
            0x05 => Some(self.r4805),
            0x06 => Some(self.r4806),
            0x07 => Some(self.r4807),
            _ => None,
        }
    }

    /// MMC-decode a 24-bit address to a flat ROM offset — ares `mcuRead`'s
    /// non-decompression path + `mmcRead`. `None` if the address is not ROM.
    /// Two regions:
    /// - `$00-3F/$80-BF : $8000-FFFF` — banked program ROM (`LoROM`-style), with
    ///   the `$20-3F`/`$A0-BF` mirror controlled by `r4805`/`r4807` bit 7.
    /// - bit 22 set (`$40-7F` / `$C0-FF`) — MMC-banked data ROM: address bits
    ///   `[20:21]` pick `r4804-7`, whose low nibble forms ROM bits `[20:23]`.
    fn rom_addr(&self, addr: Addr24) -> Option<u32> {
        let a = addr & 0x00FF_FFFF;
        if a & 0x0040_0000 == 0 {
            if a & 0x0000_8000 == 0 {
                return None; // low half of $00-3F/$80-BF is not ROM
            }
            let mut a = a;
            if a & 0x0080_0000 == 0 && a & 0x0020_0000 != 0 && self.r4805 & 0x80 != 0 {
                a &= !0x0020_0000; // $20-3F mirrors $00-1F
            }
            if a & 0x0080_0000 != 0 && a & 0x0020_0000 != 0 && self.r4807 & 0x80 != 0 {
                a &= !0x0020_0000; // $A0-BF mirrors $80-9F
            }
            Some(((a & 0x003F_0000) >> 1) | (a & 0x0000_7FFF))
        } else {
            let sel = match (a >> 20) & 0x03 {
                0 => self.r4804,
                1 => self.r4805,
                2 => self.r4806,
                _ => self.r4807,
            };
            Some((u32::from(sel & 0x0F) << 20) | (a & 0x000F_FFFF))
        }
    }

    /// SRAM offset (S-DD1 board: `$70-73:$0000-7FFF`, within luna's `LoROM`
    /// `$70-7D/$F0-FD` convention; wraps modulo the actual SRAM size).
    fn sram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if self.sram.is_empty() || !matches!(bank, 0x70..=0x7D | 0xF0..=0xFD) || offset >= 0x8000 {
            return None;
        }
        let normalized_bank = (bank & 0x7F) - 0x70;
        Some((usize::from(normalized_bank) * 0x8000 + usize::from(offset)) % self.sram.len())
    }
}

impl Mapper for Sdd1Mapper {
    fn kind(&self) -> MapperKind {
        MapperKind::Sdd1
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x4800..=0x480F).contains(&offset) {
            if let Some(v) = self.read_reg(offset) {
                return Some(v);
            }
        }
        if let Some(o) = self.sram_offset(bank, offset) {
            return Some(self.sram[o]);
        }
        // DMA-triggered decompression: in the bit-22 region (`$40-7F`/`$C0-FF`),
        // if an armed channel's fixed source address matches this read, stream
        // a decompressed byte (ares `mcuRead`). A normal (non-armed) read falls
        // through to plain MMC ROM below.
        let a = addr & 0x00FF_FFFF;
        if a & 0x0040_0000 != 0 {
            let active = self.r4800 & self.r4801;
            if active != 0 {
                for ch in 0..8 {
                    if active & (1 << ch) != 0 && a == self.dma_address[ch] {
                        // Build the ROM view inline (borrows `&self.rom`, copies
                        // the bank regs) so it is disjoint from the
                        // `&mut self.decompressor` borrow below.
                        let view = Sdd1RomView {
                            rom: &self.rom,
                            banks: [self.r4804, self.r4805, self.r4806, self.r4807],
                        };
                        if !self.dma_ready {
                            self.decompressor.init(&view, a);
                            self.dma_ready = true;
                        }
                        let data = self.decompressor.decompress_byte(&view);
                        self.dma_size[ch] = self.dma_size[ch].wrapping_sub(1);
                        if self.dma_size[ch] == 0 {
                            self.dma_ready = false;
                            self.r4801 &= !(1 << ch);
                        }
                        return Some(data);
                    }
                }
            }
        }
        if let Some(rom_addr) = self.rom_addr(addr) {
            if self.rom.is_empty() {
                return None;
            }
            return Some(self.rom[rom_mirror(rom_addr as usize, self.rom.len())]);
        }
        None
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x4800..=0x480F).contains(&offset) {
            match offset & 0x000F {
                0x00 => self.r4800 = value,
                0x01 => self.r4801 = value,
                0x04 => self.r4804 = value & 0x8F,
                0x05 => self.r4805 = value & 0x8F,
                0x06 => self.r4806 = value & 0x8F,
                0x07 => self.r4807 = value & 0x8F,
                _ => {}
            }
            return true;
        }
        // Observe the SNES DMA controller's `$43x2-6` writes to learn each
        // channel's source address ($43x2-4) and length ($43x5-6) — needed to
        // recognise + bound a decompressing DMA. We only observe; the real DMA
        // controller (luna-core) owns these registers, so don't claim them.
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4300..=0x437F) {
            let ch = usize::from((offset >> 4) & 0x07);
            match offset & 0x000F {
                0x02 => {
                    self.dma_address[ch] = (self.dma_address[ch] & 0x00FF_FF00) | u32::from(value);
                }
                0x03 => {
                    self.dma_address[ch] =
                        (self.dma_address[ch] & 0x00FF_00FF) | (u32::from(value) << 8);
                }
                0x04 => {
                    self.dma_address[ch] =
                        (self.dma_address[ch] & 0x0000_FFFF) | (u32::from(value) << 16);
                }
                0x05 => self.dma_size[ch] = (self.dma_size[ch] & 0xFF00) | u16::from(value),
                0x06 => self.dma_size[ch] = (self.dma_size[ch] & 0x00FF) | (u16::from(value) << 8),
                _ => {}
            }
            return false;
        }
        if let Some(o) = self.sram_offset(bank, offset) {
            self.sram[o] = value;
            return true;
        }
        false
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

    fn reset(&mut self) {
        // Control registers + any in-flight decompression clear on reset;
        // battery SRAM persists.
        self.r4800 = 0;
        self.r4801 = 0;
        self.r4804 = 0;
        self.r4805 = 0;
        self.r4806 = 0;
        self.r4807 = 0;
        self.dma_address = [0; 8];
        self.dma_size = [0; 8];
        self.dma_ready = false;
        self.decompressor = Sdd1Decompressor::new();
    }

    fn save_state(&self) -> Vec<u8> {
        bincode::serialize(&(
            &self.sram,
            self.r4800,
            self.r4801,
            self.r4804,
            self.r4805,
            self.r4806,
            self.r4807,
            self.dma_address,
            self.dma_size,
            self.dma_ready,
            &self.decompressor,
        ))
        .unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        type State = (
            Vec<u8>,
            u8,
            u8,
            u8,
            u8,
            u8,
            u8,
            [u32; 8],
            [u16; 8],
            bool,
            Sdd1Decompressor,
        );
        if let Ok((
            sram,
            r4800,
            r4801,
            r4804,
            r4805,
            r4806,
            r4807,
            dma_addr,
            dma_size,
            ready,
            dec,
        )) = bincode::deserialize::<State>(data)
        {
            self.sram = sram;
            self.r4800 = r4800;
            self.r4801 = r4801;
            self.r4804 = r4804;
            self.r4805 = r4805;
            self.r4806 = r4806;
            self.r4807 = r4807;
            self.dma_address = dma_addr;
            self.dma_size = dma_size;
            self.dma_ready = ready;
            self.decompressor = dec;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

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

    // ---- Sdd1Mapper (MMC) ----

    fn mapper_with_ramp_rom(len: usize) -> Sdd1Mapper {
        // ROM[i] = i & 0xFF, so a read reveals its decoded ROM offset.
        let rom: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        Sdd1Mapper::new(rom, 0x2000)
    }

    #[test]
    fn mapper_program_rom_is_lorom_banked() {
        let mut m = mapper_with_ramp_rom(0x40_0000);
        // $00:8000 → ROM 0; $00:8001 → 1; $01:8000 → 0x8000.
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0x00));
        assert_eq!(m.read(make_addr(0x00, 0x8001)), Some(0x01));
        assert_eq!(
            m.read(make_addr(0x01, 0x8000)),
            Some((0x8000usize & 0xFF) as u8)
        );
        // Low half of $00-3F is not program ROM.
        assert_eq!(m.read(make_addr(0x00, 0x4000)), None);
        // $80-BF mirrors $00-3F (bit 23 ignored in the banked path).
        assert_eq!(
            m.read(make_addr(0x80, 0x8001)),
            m.read(make_addr(0x00, 0x8001))
        );
    }

    #[test]
    fn mapper_mmc_banks_c0_via_4804_7() {
        let mut m = mapper_with_ramp_rom(0x80_0000);
        // r4804 selects the 1 MB bank for $C0-CF: bank 5 → ROM 0x50_0000.
        m.write(make_addr(0x00, 0x4804), 0x05);
        // $C0:1234 → (5 << 20) | 0x1234 = 0x50_1234.
        let want = (0x50_1234usize & 0xFF) as u8;
        assert_eq!(m.read(make_addr(0xC0, 0x1234)), Some(want));
        // r4807 selects $F0-FF: bank 7 → ROM 0x70_0000.
        m.write(make_addr(0x00, 0x4807), 0x07);
        assert_eq!(
            m.read(make_addr(0xF0, 0x0000)),
            Some((0x70_0000usize & 0xFF) as u8)
        );
    }

    #[test]
    fn mapper_control_registers_round_trip_and_mask() {
        let mut m = mapper_with_ramp_rom(0x1_0000);
        m.write(make_addr(0x00, 0x4800), 0xAB);
        m.write(make_addr(0x00, 0x4801), 0xCD);
        m.write(make_addr(0x00, 0x4805), 0xFF); // bank reg masked & 0x8F
        assert_eq!(m.read(make_addr(0x00, 0x4800)), Some(0xAB));
        assert_eq!(m.read(make_addr(0x00, 0x4801)), Some(0xCD));
        assert_eq!(m.read(make_addr(0x00, 0x4805)), Some(0x8F));
        // Reset clears the control registers.
        m.reset();
        assert_eq!(m.read(make_addr(0x00, 0x4800)), Some(0x00));
    }

    #[test]
    fn mapper_dma_decompression_arms_streams_and_disarms() {
        // 8 MB ramp ROM; put a compressed header + payload at a $C0 address.
        let mut rom: Vec<u8> = (0..0x80_0000).map(|i| (i & 0xFF) as u8).collect();
        // r4804 = 0 → $C0:xxxx decodes to ROM 0x00_xxxx. Header 0xC0 = 8 bpp.
        rom[0x0000] = 0xC0;
        let mut m = Sdd1Mapper::new(rom, 0x2000);

        // Game protocol: set DMA channel-0 source ($C0:0000) + length (4),
        // then arm S-DD1 on channel 0 (hard + soft enable).
        m.write(make_addr(0x00, 0x4302), 0x00); // addr byte 0
        m.write(make_addr(0x00, 0x4303), 0x00); // addr byte 1
        m.write(make_addr(0x00, 0x4304), 0xC0); // addr byte 2 → $C0:0000
        m.write(make_addr(0x00, 0x4305), 0x04); // length lo = 4
        m.write(make_addr(0x00, 0x4306), 0x00); // length hi
        m.write(make_addr(0x00, 0x4800), 0x01); // hard-enable ch0
        m.write(make_addr(0x00, 0x4801), 0x01); // soft-enable ch0

        // 4 armed reads at the fixed source → decompressed (not raw ROM=0xC0).
        let out: Vec<u8> = (0..4)
            .map(|_| m.read(make_addr(0xC0, 0x0000)).unwrap())
            .collect();
        // Stream exhausted → S-DD1 clears the soft-enable bit.
        assert_eq!(
            m.read(make_addr(0x00, 0x4801)),
            Some(0x00),
            "r4801 ch0 cleared"
        );

        // Deterministic: re-arm + re-read yields the same bytes.
        m.write(make_addr(0x00, 0x4305), 0x04);
        m.write(make_addr(0x00, 0x4801), 0x01);
        let out2: Vec<u8> = (0..4)
            .map(|_| m.read(make_addr(0xC0, 0x0000)).unwrap())
            .collect();
        assert_eq!(out, out2);

        // A read NOT matching an armed DMA returns plain MMC ROM: after the
        // second stream finished r4801 is clear, so $C0:0010 yields raw 0x10.
        assert_eq!(m.read(make_addr(0xC0, 0x0010)), Some(0x10));
    }

    #[test]
    fn mapper_sram_round_trips_and_save_state() {
        let mut m = mapper_with_ramp_rom(0x1_0000);
        m.write(make_addr(0x70, 0x0010), 0x5A);
        assert_eq!(m.read(make_addr(0x70, 0x0010)), Some(0x5A));
        // Save/restore preserves SRAM + the control registers.
        m.write(make_addr(0x00, 0x4806), 0x03);
        let blob = m.save_state();
        let mut m2 = mapper_with_ramp_rom(0x1_0000);
        m2.load_state(&blob);
        assert_eq!(m2.read(make_addr(0x70, 0x0010)), Some(0x5A));
        assert_eq!(m2.read(make_addr(0x00, 0x4806)), Some(0x03));
    }
}
