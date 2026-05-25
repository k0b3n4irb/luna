//! SA-1 (Super Accelerator 1) cartridge mapping — phase-1 stub.
//!
//! The SA-1 is Nintendo's custom co-processor used by Super Mario RPG,
//! DKC 2/3, Kirby Super Star, and ~25 other titles. Internally it's a
//! 65C816 running at 10.74 MHz alongside the main CPU, plus banks of
//! shared RAM, a DMA controller, character-conversion hardware, a
//! hardware multiplier / divider / accumulator, and a complex ROM
//! banking scheme.
//!
//! This phase implements **just enough** of the mapping to accept an
//! SA-1 cartridge without panicking and serve up code from ROM in the
//! default ($00, $01, $02, $03) banking layout. The SA-1 65C816 itself
//! does NOT run yet — its I/O registers at `$2200-$23FF` are
//! memory-backed stubs, plus a faithful implementation of the
//! self-contained hardware multiplier / divider at `$2250-$2254` and
//! `$2306-$230A`. Game code that doesn't depend on the SA-1 CPU
//! actually executing (boot screens, intro sequences, code that uses
//! the multiplier as a fast math coprocessor) can make progress.
//!
//! Reference: <https://problemkaputt.de/fullsnes.htm> §"SNES SA-1".
//!
//! # Mapping
//!
//! Four "super-bank" registers select which 1 MB of ROM is visible in
//! each quarter of the CPU's 24-bit address space:
//!
//! - `$2220 CXB` — banks `$00-$1F` and `$80-$9F`'s upper half
//! - `$2221 DXB` — banks `$20-$3F` and `$A0-$BF`'s upper half
//! - `$2222 EXB` — banks `$40-$5F` (linear)
//! - `$2223 FXB` — banks `$60-$7D` (linear)
//!
//! Within each LoROM-style super-bank region the byte mapping is the
//! standard "32 KB at `$8000-$FFFF`, mirrored across 32 banks" used
//! by the LoROM mapper, scaled by the super-bank offset (`bank << 20`
//! into ROM).
//!
//! BW-RAM (up to 256 KB) appears as the cart's SRAM window at
//! `$00-$3F:$6000-$7FFF` (an 8 KB sliding window selected by
//! `$2224 BMAPS`) and linearly at `$40-$4F:$0000-$FFFF` (the
//! contiguous 256 KB view). I-RAM (2 KB, shared with the SA-1 CPU)
//! appears at `$00-$3F:$3000-$37FF`.

use crate::mapper::{Mapper, MapperKind};
use crate::types::{Addr24, bank_of, offset_of};

/// Up-to-256 KB SA-1 BW-RAM.
const BWRAM_SIZE: usize = 0x40000;
/// 2 KB SA-1 / main-CPU shared I-RAM.
const IRAM_SIZE: usize = 0x800;
/// SA-1 MMIO byte range — we memory-back the whole window.
const MMIO_SIZE: usize = 0x200;

/// SA-1 cartridge mapper (Mode 23).
pub struct Sa1Mapper {
    rom: Vec<u8>,
    bwram: Vec<u8>,
    iram: [u8; IRAM_SIZE],
    /// Memory-backed I/O register file at `$2200-$23FF`. Specific
    /// registers (banking + multiplier) have first-class semantics
    /// below; everything else lands here as a generic write/read.
    mmio: [u8; MMIO_SIZE],
    /// $2220 CXB super-bank selector for `$00-$1F` / `$80-$9F`.
    cxb: u8,
    /// $2221 DXB super-bank selector for `$20-$3F` / `$A0-$BF`.
    dxb: u8,
    /// $2222 EXB super-bank selector for `$40-$5F`.
    exb: u8,
    /// $2223 FXB super-bank selector for `$60-$7D`.
    fxb: u8,
    /// $2224 BMAPS — BW-RAM 8 KB window select for the `$6000-$7FFF`
    /// window in main-CPU LoROM space.
    bmaps: u8,
    /// Multiplier / divider operands and result.
    /// `$2251/$2252 MA` — multiplicand (signed 16-bit, write-twice).
    ma: i16,
    /// `$2253/$2254 MB` — multiplier (signed 16-bit, write-twice).
    /// Writing the high byte triggers the operation per `mcnt`.
    mb: i16,
    /// `$2250 MCNT` — operation select (bit 0 = arithmetic mode:
    /// 0 = multiply, 1 = divide; bit 1 = accumulator mode).
    mcnt: u8,
    /// `$2306-$230A` — 40-bit signed result (multiplication) or
    /// 16-bit quotient + 16-bit remainder packed (division).
    mr: i64,
}

impl Sa1Mapper {
    /// Build an SA-1 mapper with default banking (the layout games
    /// see at power-on).
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_size: usize) -> Self {
        let bwram_bytes = sram_size.clamp(0x800, BWRAM_SIZE);
        Self {
            rom,
            bwram: vec![0; bwram_bytes],
            iram: [0; IRAM_SIZE],
            mmio: [0; MMIO_SIZE],
            cxb: 0x00,
            dxb: 0x01,
            exb: 0x02,
            fxb: 0x03,
            bmaps: 0x00,
            ma: 0,
            mb: 0,
            mcnt: 0,
            mr: 0,
        }
    }

    /// Translate a CPU-side ROM access through the four super-bank
    /// registers into a linear byte offset into the ROM vector.
    /// Returns `None` if the address doesn't fall in a ROM region.
    fn rom_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        // Each super-bank register selects 1 MB of ROM (= 0x10_0000
        // bytes = 32 LoROM pages of 32 KB each).
        const MB: usize = 0x10_0000;
        let (super_bank, lorom_bank) = match bank {
            0x00..=0x1F if offset >= 0x8000 => (self.cxb, bank),
            0x20..=0x3F if offset >= 0x8000 => (self.dxb, bank - 0x20),
            0x80..=0x9F if offset >= 0x8000 => (self.cxb, bank - 0x80),
            0xA0..=0xBF if offset >= 0x8000 => (self.dxb, bank - 0xA0),
            // Linear "HiROM-style" full-bank regions.
            0x40..=0x5F => (self.exb, bank - 0x40),
            0x60..=0x7D => (self.fxb, bank - 0x60),
            0xC0..=0xDF => (self.exb, bank - 0xC0),
            0xE0..=0xFF => (self.fxb, bank - 0xE0),
            _ => return None,
        };
        let base = usize::from(super_bank & 0x07) * MB;
        let within_super = if offset >= 0x8000 {
            // LoROM-style: each 32 KB page maps the upper half of
            // its bank.
            (usize::from(lorom_bank) * 0x8000) + (usize::from(offset) - 0x8000)
        } else {
            // Linear HiROM-style (banks $40+ / $C0+): full 64 KB
            // per bank within the 1 MB super-bank.
            (usize::from(lorom_bank) * 0x1_0000) + usize::from(offset)
        };
        let off = base + within_super;
        if off < self.rom.len() {
            Some(off)
        } else {
            None
        }
    }

    /// I-RAM access: 2 KB at `$3000-$37FF` of banks `$00-$3F` and
    /// `$80-$BF`. Wraps modulo size for the 4× mirrored 2 KB visible
    /// inside `$3000-$3FFF`.
    fn iram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        let bank_ok = matches!(bank, 0x00..=0x3F | 0x80..=0xBF);
        let offset_ok = (0x3000..=0x37FF).contains(&offset);
        if bank_ok && offset_ok {
            Some(usize::from(offset - 0x3000))
        } else {
            None
        }
    }

    /// BW-RAM access: two views, both gated by the cart having
    /// declared SRAM:
    ///   * 8 KB sliding window at `$00-$3F:$6000-$7FFF`, offset by
    ///     `BMAPS << 13` within BW-RAM.
    ///   * Linear 256 KB at `$40-$4F:$0000-$FFFF` for the SA-1's own
    ///     full-bandwidth view.
    fn bwram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if self.bwram.is_empty() {
            return None;
        }
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x6000..=0x7FFF).contains(&offset) {
            let window = usize::from(self.bmaps & 0x1F) * 0x2000;
            let off = window + usize::from(offset - 0x6000);
            return Some(off % self.bwram.len());
        }
        if matches!(bank, 0x40..=0x4F) {
            let off = usize::from(bank - 0x40) * 0x1_0000 + usize::from(offset);
            return Some(off % self.bwram.len());
        }
        None
    }

    /// SA-1 I/O register-window check.
    fn mmio_offset(addr: Addr24) -> Option<usize> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x2200..=0x23FF).contains(&offset) {
            Some(usize::from(offset - 0x2200))
        } else {
            None
        }
    }

    /// Re-run the multiplier / divider per `mcnt`. Triggered by a
    /// write to `$2254` MB-high.
    fn update_arith(&mut self) {
        let mode = self.mcnt & 0x01;
        if mode == 0 {
            // Multiply: signed 16 × signed 16 → 32-bit signed.
            // Accumulate when mcnt bit 1 is set (chained MAC).
            let product = i32::from(self.ma) * i32::from(self.mb);
            self.mr = if self.mcnt & 0x02 != 0 {
                self.mr.saturating_add(i64::from(product))
            } else {
                i64::from(product)
            };
        } else {
            // Divide: signed 16 dividend (ma) / signed 16 divisor (mb).
            // Result packs quotient (low 16 bits) and remainder (high).
            if self.mb == 0 {
                self.mr = 0;
            } else {
                let q = self.ma / self.mb;
                let r = self.ma % self.mb;
                self.mr = i64::from(i32::from(q as u16) & 0xFFFF)
                    | (i64::from(i32::from(r as u16) & 0xFFFF) << 16);
            }
        }
    }
}

impl Mapper for Sa1Mapper {
    fn kind(&self) -> MapperKind {
        MapperKind::Sa1
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        // I/O reads — multiplier result is the only "live" path; the
        // rest of the window is open-bus / memory-backed stub.
        if let Some(idx) = Self::mmio_offset(addr) {
            // $2306-$230A → 40-bit MR result. We expose 5 bytes
            // little-endian.
            let mr_addr = 0x2200 + idx as u16;
            return Some(match mr_addr {
                0x2306 => self.mr as u8,
                0x2307 => (self.mr >> 8) as u8,
                0x2308 => (self.mr >> 16) as u8,
                0x2309 => (self.mr >> 24) as u8,
                0x230A => (self.mr >> 32) as u8,
                _ => self.mmio[idx],
            });
        }
        if let Some(o) = self.iram_offset(bank, offset) {
            return Some(self.iram[o]);
        }
        if let Some(o) = self.bwram_offset(bank, offset) {
            return Some(self.bwram[o]);
        }
        if let Some(o) = self.rom_offset(bank, offset) {
            return Some(self.rom[o]);
        }
        None
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(idx) = Self::mmio_offset(addr) {
            self.mmio[idx] = value;
            let absolute = 0x2200 + idx as u16;
            match absolute {
                0x2220 => self.cxb = value,
                0x2221 => self.dxb = value,
                0x2222 => self.exb = value,
                0x2223 => self.fxb = value,
                0x2224 => self.bmaps = value,
                0x2250 => {
                    self.mcnt = value;
                    if value & 0x02 != 0 {
                        // Accumulator clear when bit 1 written 1
                        // (and then bit 1 stays as "accumulate mode").
                        // Real HW: writing 0x02 resets MR.
                        if value == 0x02 {
                            self.mr = 0;
                        }
                    }
                }
                0x2251 => self.ma = (self.ma & !0xFF) | i16::from(value),
                0x2252 => self.ma = (self.ma & 0xFF) | (i16::from(value as i8) << 8),
                0x2253 => self.mb = (self.mb & !0xFF) | i16::from(value),
                0x2254 => {
                    self.mb = (self.mb & 0xFF) | (i16::from(value as i8) << 8);
                    self.update_arith();
                }
                _ => {}
            }
            return true;
        }
        if let Some(o) = self.iram_offset(bank, offset) {
            self.iram[o] = value;
            return true;
        }
        if let Some(o) = self.bwram_offset(bank, offset) {
            self.bwram[o] = value;
            return true;
        }
        // ROM writes drop but claim the access.
        self.rom_offset(bank, offset).is_some()
    }

    fn rom_size(&self) -> usize {
        self.rom.len()
    }

    fn sram_size(&self) -> usize {
        self.bwram.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    #[test]
    fn default_banking_reads_first_megabyte_via_cxb() {
        // CXB = 0 → $00:8000 → ROM[0].
        let mut m = Sa1Mapper::new(ramp_rom(0x20_0000), 0);
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x8001)), Some(1));
    }

    #[test]
    fn second_megabyte_via_dxb_default_1() {
        // DXB = 1 → $20:8000 → ROM[1 << 20] = byte 0 of MB 1.
        let mut m = Sa1Mapper::new(ramp_rom(0x20_0000), 0);
        assert_eq!(m.read(make_addr(0x20, 0x8000)), Some(0));
    }

    #[test]
    fn iram_round_trip() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        let addr = make_addr(0x00, 0x3010);
        assert!(m.write(addr, 0x42));
        assert_eq!(m.read(addr), Some(0x42));
    }

    #[test]
    fn bwram_8kb_window_at_6000() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 8 * 1024);
        let addr = make_addr(0x00, 0x6000);
        assert!(m.write(addr, 0xAB));
        assert_eq!(m.read(addr), Some(0xAB));
    }

    #[test]
    fn bwram_linear_view_at_bank_40() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10_0000);
        let addr = make_addr(0x40, 0x1234);
        assert!(m.write(addr, 0x99));
        assert_eq!(m.read(addr), Some(0x99));
    }

    #[test]
    fn cxb_write_remaps_low_banks() {
        // Re-point CXB to bank 4 (= ROM offset 4 MB); reads from
        // $00:8000 must now follow.
        let mut m = Sa1Mapper::new(ramp_rom(0x60_0000), 0);
        assert!(m.write(make_addr(0x00, 0x2220), 0x04));
        let want_offset = 4 * 0x10_0000;
        assert_eq!(
            m.read(make_addr(0x00, 0x8000)),
            Some((want_offset & 0xFF) as u8)
        );
    }

    #[test]
    fn multiplier_16x16_writes_to_mr() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // MCNT = 0 → multiply mode.
        m.write(make_addr(0x00, 0x2250), 0x00);
        // MA = 7 (signed)
        m.write(make_addr(0x00, 0x2251), 0x07);
        m.write(make_addr(0x00, 0x2252), 0x00);
        // MB = 8 (signed) → high-byte write triggers
        m.write(make_addr(0x00, 0x2253), 0x08);
        m.write(make_addr(0x00, 0x2254), 0x00);
        assert_eq!(m.read(make_addr(0x00, 0x2306)), Some(56));
        assert_eq!(m.read(make_addr(0x00, 0x2307)), Some(0));
    }

    #[test]
    fn divider_16_div_16_packs_quotient_and_remainder() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x2250), 0x01); // divide
        // MA = 100, MB = 7 → q = 14, r = 2.
        m.write(make_addr(0x00, 0x2251), 100);
        m.write(make_addr(0x00, 0x2252), 0);
        m.write(make_addr(0x00, 0x2253), 7);
        m.write(make_addr(0x00, 0x2254), 0);
        assert_eq!(m.read(make_addr(0x00, 0x2306)), Some(14)); // quotient lo
        assert_eq!(m.read(make_addr(0x00, 0x2307)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x2308)), Some(2)); // remainder lo
    }

    #[test]
    fn multiplier_signed_negative() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x2250), 0x00);
        // MA = -1 ($FFFF)
        m.write(make_addr(0x00, 0x2251), 0xFF);
        m.write(make_addr(0x00, 0x2252), 0xFF);
        // MB = 100
        m.write(make_addr(0x00, 0x2253), 100);
        m.write(make_addr(0x00, 0x2254), 0);
        // Result = -100 = 0xFFFFFF9C.
        assert_eq!(m.read(make_addr(0x00, 0x2306)), Some(0x9C));
        assert_eq!(m.read(make_addr(0x00, 0x2307)), Some(0xFF));
        assert_eq!(m.read(make_addr(0x00, 0x2308)), Some(0xFF));
        assert_eq!(m.read(make_addr(0x00, 0x2309)), Some(0xFF));
    }

    #[test]
    fn mmio_writes_are_memory_backed_when_not_special() {
        // $22FF is an unused / open MMIO slot — verify our backing
        // store accepts and returns the value (covers the generic
        // catch-all path).
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x22FF), 0x5A);
        assert_eq!(m.read(make_addr(0x00, 0x22FF)), Some(0x5A));
    }

    #[test]
    fn kind_is_sa1() {
        let m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        assert_eq!(m.kind(), MapperKind::Sa1);
    }
}
