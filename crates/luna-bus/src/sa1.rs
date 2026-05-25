//! SA-1 (Super Accelerator 1) cartridge mapping.
//!
//! The SA-1 is Nintendo's custom co-processor used by Super Mario RPG,
//! DKC 2/3, Kirby Super Star, and ~25 other titles. Internally it's a
//! 65C816 running at 10.74 MHz alongside the main CPU, plus banks of
//! shared RAM, a DMA controller, character-conversion hardware, a
//! hardware multiplier / divider / accumulator, and a complex ROM
//! banking scheme.
//!
//! This module owns the shared cartridge memory + MMIO register file
//! seen by both CPUs: ROM banking, I-RAM, BW-RAM, hardware
//! multiplier / divider / accumulator, IRQ message latches, timer,
//! normal-mode DMA, CC1 + CC2 character-conversion DMA, VLBP
//! bit-stream reader, and the per-side I-RAM / BW-RAM
//! write-protection masks. The SA-1's own 65C816 instance is
//! layered on top in [`luna_coproc::Sa1Chip`].
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

    // ---- Phase-3 IRQ message system ----
    /// `$2201 SIE` — main-CPU IRQ enable mask for incoming SA-1 →
    /// S-CPU interrupts. bit 7 = SA-1-IRQ enable, bit 5 = CC1-DMA-IRQ
    /// enable.
    sie: u8,
    /// `$220A CIE` — SA-1 IRQ enable mask for incoming S-CPU → SA-1
    /// interrupts. bit 7 = S-CPU-IRQ, bit 6 = S-CPU-NMI, bit 5 = timer
    /// IRQ, bit 4 = DMA IRQ.
    cie: u8,
    /// SA-1 → S-CPU IRQ latch (raised on `$2209` bit-7 0 → 1 edge,
    /// cleared by `$2202` bit-7 write).
    s_irq_to_main: bool,
    /// SA-1 → S-CPU NMI latch (raised on `$2209` bit-6 0 → 1 edge,
    /// cleared by `$2202` bit-6 write).
    s_nmi_to_main: bool,
    /// CC1-DMA completion → S-CPU IRQ latch (raised by CC1 engine,
    /// cleared by `$2202` bit-5 write).
    cc1_irq_to_main: bool,
    /// S-CPU → SA-1 IRQ latch (raised on `$2200` bit-4 0 → 1 edge,
    /// cleared by `$220B` bit-7 write).
    main_irq_to_sa1: bool,
    /// S-CPU → SA-1 NMI latch (raised on `$2200` bit-6 0 → 1 edge,
    /// cleared by `$220B` bit-6 write).
    main_nmi_to_sa1: bool,
    /// Timer → SA-1 IRQ latch (cleared by `$220B` bit-5 write).
    timer_irq_to_sa1: bool,
    /// DMA → SA-1 IRQ latch (cleared by `$220B` bit-4 write).
    dma_irq_to_sa1: bool,
    /// Last value written to `$2200` CCNT (low nibble = message to
    /// SA-1, visible in `$2301` CFR low nibble).
    ccnt_msg: u8,
    /// Last value written to `$2209` SCNT (bits 4-0 carry IVSW /
    /// NMIVW vector-override flags + message to S-CPU, visible in
    /// `$2300` SFR).
    scnt: u8,
    /// `$2207/$2208` CIV — SA-1's IRQ vector (set by S-CPU, used when
    /// the SA-1 CPU fetches its IRQ vector at `$00:FFEE/FFEF` or
    /// `$00:FFFE/FFFF`).
    civ_lo: u8,
    civ_hi: u8,
    /// `$2205/$2206` CNV — SA-1's NMI vector.
    cnv_lo: u8,
    cnv_hi: u8,
    /// `$220E/$220F` SIV — S-CPU's IRQ vector when the SA-1 fires
    /// its IRQ line (only used when SCNT bit-5 IVSW = 1).
    siv_lo: u8,
    siv_hi: u8,
    /// `$220C/$220D` SNV — S-CPU's NMI vector when the SA-1 fires its
    /// NMI line (only used when SCNT bit-4 NMIVW = 1).
    snv_lo: u8,
    snv_hi: u8,

    // ---- Phase-3 timer ($2210-$2215) ----
    /// `$2210 TMC` — timer control. bit 7 = mode (0 = HV timer, 1 =
    /// linear timer), bit 1 = V enable, bit 0 = H enable. In linear
    /// mode an 18-bit counter wraps every 2^18 SA-1 clocks; the
    /// `HCNT:VCNT.lo[1:0]` compare raises a timer IRQ.
    tmc: u8,
    /// `$2212/$2213 HCNT` — H compare (write) / counter low (read).
    hcnt_lo: u8,
    hcnt_hi: u8,
    /// `$2214/$2215 VCNT` — V compare (write) / counter high (read).
    vcnt_lo: u8,
    vcnt_hi: u8,
    /// Free-running 18-bit linear-mode counter. Wraps modulo 2^18.
    linear_counter: u32,
    /// True between a compare-match and the next reset / clear, so we
    /// only raise one IRQ edge per match.
    timer_match_armed: bool,

    // ---- Phase-3 DMA ($2230-$2239) ----
    /// `$2230 DCNT` — DMA control byte (bit 7 = enable, bit 5 = char
    /// conversion, bit 4 = CC type, bit 3..2 = source, bit 1..0 =
    /// destination).
    dcnt: u8,
    /// `$2231 CDMA` — character-conversion parameters (colour depth +
    /// tile width). Stored for the Type-1 path; the normal-DMA fast
    /// path ignores it.
    cdma: u8,
    /// `$2232-$2234` SDA — 24-bit source address.
    sda: u32,
    /// `$2235-$2237` DDA — 24-bit destination address.
    dda: u32,
    /// `$2238/$2239` DTC — 16-bit transfer byte counter.
    dtc: u16,
    /// `true` while a Type-2 character-conversion DMA is armed and
    /// the SA-1 is streaming pixel bytes through BBF.
    cc2_active: bool,
    /// Resolved bpp at the time CC2 was armed — captured here so a
    /// mid-stream CDMA write can't change it under the staging buffer.
    cc2_bpp: usize,
    /// Source pixel bytes accumulated for the current in-flight tile.
    /// Filled `bpp` bytes per row; on the last row the staged 8×bpp
    /// bytes are planarized and emitted to DDA.
    cc2_buf: [u8; 64],
    /// Number of bytes staged into `cc2_buf` for the current tile.
    cc2_byte_idx: usize,

    // ---- Phase-5 VLBP (Variable-Length Bit Processor) ----
    /// `$2258 VBD` — variable-length bit-data control. bit 7 = mode
    /// (0 = fixed, advances on `$230C` read; 1 = variable, advances
    /// on `$230D` read); bits 3..0 = vlen (1..15 with 0 meaning 16).
    vbd: u8,
    /// `$2259-$225B VDA` — 24-bit source address into the bit-packed
    /// data stream. Writing VDA-high (`$225B`) zeroes `vbit_offset`.
    vda_base: u32,
    /// Bit cursor into the stream, counted from `vda_base`.
    vbit_offset: u32,

    // ---- Phase-5 memory write protection ----
    /// `$2226 SBWE` — main-CPU BW-RAM write-enable (bit 7 = 1 lets
    /// the S-CPU write to BW-RAM at all).
    sbwe: u8,
    /// `$2227 CBWE` — SA-1 BW-RAM write-enable (bit 7 = 1 lets the
    /// SA-1 CPU write to BW-RAM at all).
    cbwe: u8,
    /// `$2228 BWPA` — BW-RAM write-protected-area size. Protects the
    /// first `256 << (bwpa & 0x0F)` bytes of BW-RAM from main-CPU
    /// writes (the SA-1 side ignores BWPA). A value of `0` disables.
    bwpa: u8,
    /// `$222A SIWP` — main-CPU I-RAM page write-enable mask. Each of
    /// the 8 bits gates one 256-byte page of the 2 KB I-RAM; bit
    /// set = writable, clear = protected.
    siwp: u8,
    /// `$222B CIWP` — SA-1 I-RAM page write-enable mask (same shape
    /// as SIWP).
    ciwp: u8,
}

/// Which CPU side is performing a write — only relevant for the
/// `Sa1Mapper`'s memory-protection registers.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum WriteSide {
    /// Write originated from the main 65C816 (S-CPU) via the SNES bus.
    Main,
    /// Write originated from the SA-1's own 65C816 via [`super::Sa1Bus`].
    Sa1,
}

/// Decode `bpp` source bytes (one bitmap pixel-row's worth) into SNES
/// planar format inside `tile_buf` at the slot for `row`.
///
/// `tile_buf` is a 64-byte working buffer covering up to 8bpp; for
/// shallower colour depths only the relevant prefix is touched.
/// Plane interleaving matches the SNES tile layout — bp0/bp1 in
/// bytes 0..16, bp2/bp3 in 16..32, bp4/bp5 in 32..48, bp6/bp7 in
/// 48..64.
///
/// Shared by Type-1 ([`Sa1Mapper::run_cc1_dma`]) and Type-2
/// ([`Sa1Mapper::cc2_consume_byte`]) character-conversion DMA.
fn planarize_row(bpp: usize, src: &[u8], row: usize, tile_buf: &mut [u8; 64]) {
    let mut pixels = [0u8; 8];
    match bpp {
        2 => {
            for byte_idx in 0..2 {
                let b = src[byte_idx];
                pixels[byte_idx * 4] = (b >> 6) & 0x03;
                pixels[byte_idx * 4 + 1] = (b >> 4) & 0x03;
                pixels[byte_idx * 4 + 2] = (b >> 2) & 0x03;
                pixels[byte_idx * 4 + 3] = b & 0x03;
            }
        }
        4 => {
            for byte_idx in 0..4 {
                let b = src[byte_idx];
                pixels[byte_idx * 2] = b >> 4;
                pixels[byte_idx * 2 + 1] = b & 0x0F;
            }
        }
        _ => {
            for (i, b) in src.iter().enumerate().take(8) {
                pixels[i] = *b;
            }
        }
    }
    for plane in 0..bpp {
        let mut planar = 0u8;
        for (col, p) in pixels.iter().enumerate() {
            let bit = (p >> plane) & 1;
            planar |= bit << (7 - col);
        }
        let plane_group = plane / 2;
        let plane_inside = plane % 2;
        let byte_off = plane_group * 16 + row * 2 + plane_inside;
        tile_buf[byte_off] = planar;
    }
}

impl Sa1Mapper {
    /// Build an SA-1 mapper with default banking (the layout games
    /// see at power-on).
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_size: usize) -> Self {
        let bwram_bytes = sram_size.clamp(0x800, BWRAM_SIZE);
        // CCNT bit 5 (SA-1 reset, per ares + Mesen2) is set at
        // power-on so the SA-1 boots into reset. The main CPU
        // releases it by writing CCNT with bit 5 clear.
        let mut mmio = [0u8; MMIO_SIZE];
        mmio[0] = 0x20; // CCNT slot at $2200, bit 5 = reset
        Self {
            rom,
            bwram: vec![0; bwram_bytes],
            iram: [0; IRAM_SIZE],
            mmio,
            cxb: 0x00,
            dxb: 0x01,
            exb: 0x02,
            fxb: 0x03,
            bmaps: 0x00,
            ma: 0,
            mb: 0,
            mcnt: 0,
            mr: 0,
            sie: 0,
            cie: 0,
            s_irq_to_main: false,
            s_nmi_to_main: false,
            cc1_irq_to_main: false,
            main_irq_to_sa1: false,
            main_nmi_to_sa1: false,
            timer_irq_to_sa1: false,
            dma_irq_to_sa1: false,
            ccnt_msg: 0,
            scnt: 0,
            civ_lo: 0,
            civ_hi: 0,
            cnv_lo: 0,
            cnv_hi: 0,
            siv_lo: 0,
            siv_hi: 0,
            snv_lo: 0,
            snv_hi: 0,
            tmc: 0,
            hcnt_lo: 0,
            hcnt_hi: 0,
            vcnt_lo: 0,
            vcnt_hi: 0,
            linear_counter: 0,
            timer_match_armed: true,
            dcnt: 0,
            cdma: 0,
            sda: 0,
            dda: 0,
            dtc: 0,
            cc2_active: false,
            cc2_bpp: 0,
            cc2_buf: [0; 64],
            cc2_byte_idx: 0,
            vbd: 0,
            vda_base: 0,
            vbit_offset: 0,
            // Default protections to "allow all" — real hardware boots
            // with these enables clear and games immediately
            // re-configure them, but starting from "allow" keeps the
            // luna integration tests + naive carts working out-of-box.
            sbwe: 0x80,
            cbwe: 0x80,
            bwpa: 0x00,
            siwp: 0xFF,
            ciwp: 0xFF,
        }
    }

    fn iram_writable_for(&self, byte_off: usize, side: WriteSide) -> bool {
        let mask = match side {
            WriteSide::Main => self.siwp,
            WriteSide::Sa1 => self.ciwp,
        };
        let page = (byte_off / 256) & 7;
        (mask >> page) & 1 != 0
    }

    fn bwram_writable_for(&self, byte_off: usize, side: WriteSide) -> bool {
        match side {
            WriteSide::Main => {
                if (self.sbwe & 0x80) == 0 {
                    return false;
                }
                // BWPA is linear-by-256: $00 = no protection, $0F =
                // 3840 bytes protected at the start of BW-RAM. Per
                // fullsnes "SA-1 SBWE/CBWE/BWPA registers".
                let prot_bytes = usize::from(self.bwpa & 0x0F) * 256;
                byte_off >= prot_bytes
            }
            WriteSide::Sa1 => (self.cbwe & 0x80) != 0,
        }
    }

    /// Effective bit-length for VLBP reads. VBD bits 0..3 carry the
    /// length 1..15; an encoded value of 0 means 16.
    fn vlbp_vlen(&self) -> u32 {
        let n = u32::from(self.vbd & 0x0F);
        if n == 0 { 16 } else { n }
    }

    /// Pull the current `vlen` bits from the source stream as a 16-bit
    /// value. Reads three source bytes so a `vbit_offset` of up to 7
    /// can still slide a 16-bit window cleanly.
    fn vlbp_data(&mut self) -> u16 {
        let byte_off = self.vbit_offset / 8;
        let bit_off = self.vbit_offset & 7;
        let base = self.vda_base;
        let a0 = base.wrapping_add(byte_off) & 0x00FF_FFFF;
        let a1 = base.wrapping_add(byte_off + 1) & 0x00FF_FFFF;
        let a2 = base.wrapping_add(byte_off + 2) & 0x00FF_FFFF;
        let b0 = u32::from(self.read(a0).unwrap_or(0));
        let b1 = u32::from(self.read(a1).unwrap_or(0));
        let b2 = u32::from(self.read(a2).unwrap_or(0));
        let raw = b0 | (b1 << 8) | (b2 << 16);
        let shifted = raw >> bit_off;
        let vlen = self.vlbp_vlen();
        let mask = if vlen >= 16 {
            0xFFFFu32
        } else {
            (1u32 << vlen) - 1
        };
        (shifted & mask) as u16
    }

    /// Advance the VLBP bit cursor by `vlen` bits.
    fn vlbp_advance(&mut self) {
        self.vbit_offset = self.vbit_offset.wrapping_add(self.vlbp_vlen());
    }

    /// Run a normal (non-character-conversion) SA-1 DMA. Copies `dtc`
    /// bytes from `sda` to `dda` through the regular bus dispatch so
    /// ROM / BW-RAM / I-RAM source-destination combinations all work.
    ///
    /// At completion clears the DMA-enable bit + raises the DMA-IRQ
    /// latch (gated by `CIE.4`).
    fn run_normal_dma(&mut self) {
        let n = self.dtc as usize;
        for i in 0..n {
            let s = (self.sda.wrapping_add(i as u32)) & 0x00FF_FFFF;
            let d = (self.dda.wrapping_add(i as u32)) & 0x00FF_FFFF;
            let byte = self.read(s).unwrap_or(0xFF);
            // Use the raw byte path so MMIO doesn't try to interpret
            // our DMA bursts as register writes.
            self.write_raw_for_dma(d, byte);
        }
        self.dcnt &= 0x7F;
        // Mirror into the memory-backed copy at $2230.
        self.mmio[0x2230 - 0x2200] = self.dcnt;
        self.dma_irq_to_sa1 = true;
    }

    /// Bypass the MMIO write path during DMA — we never want a DMA
    /// stream into the `$2200-$23FF` window to start re-triggering DMA
    /// or rebanking ROM mid-burst. The destination is restricted by
    /// `DCNT.1-0` to BW-RAM / I-RAM, so a direct write is correct.
    fn write_raw_for_dma(&mut self, addr: u32, value: u8) {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(o) = self.iram_offset(bank, offset) {
            self.iram[o] = value;
            return;
        }
        if let Some(o) = self.bwram_offset(bank, offset) {
            self.bwram[o] = value;
        }
    }

    /// Arm a Type-2 character-conversion stream. Captures the current
    /// `cdma` colour mode and resets the staging buffer; subsequent
    /// writes to BBF (`$223F`) feed pixel bytes in scanline order.
    fn cc2_arm(&mut self) {
        self.cc2_bpp = match (self.cdma >> 2) & 0x07 {
            0 => 8,
            1 => 4,
            2 => 2,
            _ => 8,
        };
        self.cc2_buf = [0; 64];
        self.cc2_byte_idx = 0;
        self.cc2_active = true;
    }

    /// End an in-flight CC2 stream. Triggered by `CDMA.7` (CDEND) or
    /// a DCNT write that drops bit 5.
    fn cc2_end(&mut self) {
        self.cc2_active = false;
        self.dcnt &= 0x7F;
        self.mmio[0x2230 - 0x2200] = self.dcnt;
    }

    /// Consume one source pixel byte streamed by the SA-1 CPU into
    /// BBF. When `bpp × 8` bytes have been staged we have a complete
    /// 8×8 tile worth of source data — planarize it, write it to
    /// `DDA`, auto-increment DDA, raise the CC IRQ latch, and reset
    /// the staging buffer for the next tile.
    fn cc2_consume_byte(&mut self, byte: u8) {
        if !self.cc2_active {
            return;
        }
        let bytes_per_tile = self.cc2_bpp * 8;
        self.cc2_buf[self.cc2_byte_idx] = byte;
        self.cc2_byte_idx += 1;
        if self.cc2_byte_idx < bytes_per_tile {
            return;
        }
        // Tile complete — planarize row-by-row.
        let bpp = self.cc2_bpp;
        let buf = self.cc2_buf;
        let mut planar = [0u8; 64];
        for row in 0..8 {
            let src = &buf[row * bpp..(row + 1) * bpp];
            planarize_row(bpp, src, row, &mut planar);
        }
        // Write planar tile to DDA, advance DDA.
        let dst_base = self.dda & 0x00FF_FFFF;
        for (k, b) in planar.iter().enumerate().take(bytes_per_tile) {
            self.write_raw_for_dma(dst_base.wrapping_add(k as u32) & 0x00FF_FFFF, *b);
        }
        self.dda = self.dda.wrapping_add(bytes_per_tile as u32) & 0x00FF_FFFF;
        self.cc2_byte_idx = 0;
        self.cc1_irq_to_main = true;
    }

    /// Run a Type-1 Character-Conversion DMA — convert a linear
    /// `bpp`-packed bitmap at SDA into SNES planar tile data at DDA.
    /// Tile data is laid out in raster (left-to-right, top-to-bottom)
    /// order so destination consumers can stream it straight into a
    /// VRAM-bound DMA channel.
    ///
    /// CDMA layout:
    ///   * bits 4..2 = colour mode (`0`=8bpp, `1`=4bpp, `2`=2bpp)
    ///   * bits 1..0 = virtual bitmap width
    ///     (`0`=8 / `1`=16 / `2`=32 / `3`=64 tiles wide)
    ///
    /// Source pixel-byte packing (per Anomie's SA-1 reference):
    ///   * 8bpp — 1 byte/pixel
    ///   * 4bpp — 2 pixels/byte, leftmost pixel in high nibble
    ///   * 2bpp — 4 pixels/byte, leftmost pixel in bits 7..6
    ///
    /// DTC defines the number of *output* bytes to produce; we floor
    /// that to a whole-tile multiple.
    ///
    /// On completion: clears DCNT.7, raises `cc1_irq_to_main`.
    fn run_cc1_dma(&mut self) {
        let bpp = match (self.cdma >> 2) & 0x07 {
            0 => 8,
            1 => 4,
            2 => 2,
            _ => 8, // reserved → keep going at 8bpp rather than panic
        };
        let tile_width_tiles: u32 = 8u32 << (self.cdma & 0x03);
        let bytes_per_tile: u32 = (bpp as u32) * 8;
        let total_out = u32::from(self.dtc);
        let num_tiles = total_out / bytes_per_tile;
        // Pixel-row stride within the source bitmap (bytes per row).
        let row_stride = tile_width_tiles * (bpp as u32);

        for tile_idx in 0..num_tiles {
            let tile_col = tile_idx % tile_width_tiles;
            let tile_row = tile_idx / tile_width_tiles;
            let mut tile_buf = [0u8; 64];

            for row in 0..8 {
                let pixel_row = (tile_row as usize) * 8 + row;
                let src_row_off = pixel_row * (row_stride as usize) + (tile_col as usize) * bpp;

                // Pull `bpp` source bytes for one pixel-row of one tile.
                let mut src_bytes = [0u8; 8];
                for (k, slot) in src_bytes.iter_mut().enumerate().take(bpp) {
                    let a = self
                        .sda
                        .wrapping_add(u32::try_from(src_row_off + k).unwrap_or(0))
                        & 0x00FF_FFFF;
                    *slot = self.read(a).unwrap_or(0);
                }
                planarize_row(bpp, &src_bytes[..bpp], row, &mut tile_buf);
            }

            // Spill the converted tile to the destination buffer.
            let tile_dst_base =
                self.dda.wrapping_add(tile_idx.wrapping_mul(bytes_per_tile)) & 0x00FF_FFFF;
            for (k, byte) in tile_buf.iter().enumerate().take(bytes_per_tile as usize) {
                self.write_raw_for_dma(tile_dst_base.wrapping_add(k as u32) & 0x00FF_FFFF, *byte);
            }
        }

        self.dcnt &= 0x7F;
        self.mmio[0x2230 - 0x2200] = self.dcnt;
        self.cc1_irq_to_main = true;
    }

    /// Compose the 18-bit linear-mode compare value from HCNT lo/hi
    /// + VCNT lo's low 2 bits (per Anomie's SA-1 doc).
    fn linear_compare(&self) -> u32 {
        u32::from(self.hcnt_lo)
            | (u32::from(self.hcnt_hi) << 8)
            | (u32::from(self.vcnt_lo & 0x03) << 16)
    }

    /// Advance the SA-1 timer by `ticks` SA-1-clock cycles.
    ///
    /// In linear mode (TMC bit 7 = 1), the 18-bit counter increments
    /// and a compare match against `linear_compare()` raises the
    /// timer IRQ latch (gated by CIE.5 in the IRQ-line query).
    ///
    /// HV-mode timing isn't wired yet — the SA-1 has no direct view of
    /// the PPU's dot counter here, so the H/V counter readbacks just
    /// return the latched compare values until that hookup lands.
    pub fn tick_timer(&mut self, ticks: u32) {
        if (self.tmc & 0x80) == 0 || (self.tmc & 0x03) == 0 {
            // HV mode or both H/V disabled → no IRQ progress here.
            self.linear_counter = self.linear_counter.wrapping_add(ticks) & 0x3FFFF;
            return;
        }
        let compare = self.linear_compare();
        let before = self.linear_counter;
        let after = (self.linear_counter.wrapping_add(ticks)) & 0x3FFFF;
        // Detect a forward crossing through `compare` modulo 2^18.
        let crossed = if before <= after {
            before < compare && compare <= after
        } else {
            // Wraparound — crossed if compare in (before, 2^18) or [0, after].
            before < compare || compare <= after
        };
        if crossed && self.timer_match_armed {
            self.timer_irq_to_sa1 = true;
            self.timer_match_armed = false;
        }
        self.linear_counter = after;
    }

    /// `true` while the SA-1 is asserting an IRQ line onto the main
    /// CPU. The bus ORs this into the main CPU's `irq_pending` so the
    /// CPU services it through its normal IRQ path.
    #[must_use]
    pub fn main_irq_line(&self) -> bool {
        // SIE layout (per ares + Mesen2):
        //   bit 7 = SA-1 → S-CPU IRQ enable
        //   bit 5 = CC IRQ (Type-1 char-conv) enable
        (self.s_irq_to_main && (self.sie & 0x80) != 0)
            || (self.cc1_irq_to_main && (self.sie & 0x20) != 0)
    }

    /// `true` while the SA-1 is taking an IRQ from any of the three
    /// enabled sources (S-CPU IRQ, timer, DMA).
    #[must_use]
    pub fn sa1_irq_line(&self) -> bool {
        // CIE layout (per ares + Mesen2):
        //   bit 7 = S-CPU → SA-1 IRQ enable
        //   bit 6 = timer IRQ enable
        //   bit 5 = DMA IRQ enable
        //   bit 4 = S-CPU → SA-1 NMI enable
        (self.main_irq_to_sa1 && (self.cie & 0x80) != 0)
            || (self.timer_irq_to_sa1 && (self.cie & 0x40) != 0)
            || (self.dma_irq_to_sa1 && (self.cie & 0x20) != 0)
    }

    /// `true` while the S-CPU has raised an NMI to the SA-1 and the
    /// SA-1's enable mask permits it.
    #[must_use]
    pub fn sa1_nmi_line(&self) -> bool {
        self.main_nmi_to_sa1 && (self.cie & 0x10) != 0
    }

    /// Returns the override byte for a main-CPU vector fetch from
    /// bank 0 at `$FFE0-$FFFF`, or `None` if the SA-1 doesn't override
    /// that vector right now.
    ///
    /// Per ares + Mesen2: the override is level-triggered on the
    /// IVSW / NMIVW bits of SCNT — *not* gated by whether the SA-1
    /// is currently asserting its IRQ line. While the bit is set,
    /// the matching vector reads return SIV / SNV.
    fn main_vector_override(&self, bank: u8, offset: u16) -> Option<u8> {
        if bank != 0 {
            return None;
        }
        let ivsw = (self.scnt & 0x40) != 0;
        let nmivw = (self.scnt & 0x10) != 0;
        match offset {
            0xFFEE | 0xFFFE if ivsw => Some(self.siv_lo),
            0xFFEF | 0xFFFF if ivsw => Some(self.siv_hi),
            0xFFEA | 0xFFFA if nmivw => Some(self.snv_lo),
            0xFFEB | 0xFFFB if nmivw => Some(self.snv_hi),
            _ => None,
        }
    }

    /// Returns the SA-1-side override byte for an SA-1-CPU vector
    /// fetch from bank 0 at `$FFE0-$FFFF`. The SA-1 always overrides
    /// reset / NMI / IRQ vectors through CRV / CNV / CIV — there is
    /// no enable bit (the SA-1 *has* no on-board ROM vector table).
    pub fn sa1_vector_override(&self, bank: u8, offset: u16) -> Option<u8> {
        if bank != 0 {
            return None;
        }
        match offset {
            0xFFFC => Some(self.mmio[0x2203 - 0x2200]),
            0xFFFD => Some(self.mmio[0x2204 - 0x2200]),
            0xFFEE | 0xFFFE => Some(self.civ_lo),
            0xFFEF | 0xFFFF => Some(self.civ_hi),
            0xFFEA | 0xFFFA => Some(self.cnv_lo),
            0xFFEB | 0xFFFB => Some(self.cnv_hi),
            _ => None,
        }
    }

    /// Compose `$2300` SFR (S-CPU flag read). Layout (per ares + Mesen2):
    ///   bit 7 = SA-1 → S-CPU IRQ latched
    ///   bit 6 = IVSW mirror (vector override active for IRQ)
    ///   bit 5 = CC IRQ latched
    ///   bit 4 = NMIVW mirror (vector override active for NMI)
    ///   bits 3..0 = message nibble from SA-1 (low nibble of SCNT)
    fn read_sfr(&self) -> u8 {
        let mut b = 0u8;
        if self.s_irq_to_main {
            b |= 0x80;
        }
        if (self.scnt & 0x40) != 0 {
            b |= 0x40;
        }
        if self.cc1_irq_to_main {
            b |= 0x20;
        }
        if (self.scnt & 0x10) != 0 {
            b |= 0x10;
        }
        b |= self.scnt & 0x0F;
        b
    }

    /// Compose `$2301` CFR (SA-1 flag read). Layout (per ares + Mesen2):
    ///   bit 7 = S-CPU → SA-1 IRQ latched
    ///   bit 6 = timer → SA-1 IRQ latched
    ///   bit 5 = DMA → SA-1 IRQ latched
    ///   bit 4 = S-CPU → SA-1 NMI latched
    ///   bits 3..0 = message nibble from S-CPU (low nibble of CCNT)
    fn read_cfr(&self) -> u8 {
        let mut b = 0u8;
        if self.main_irq_to_sa1 {
            b |= 0x80;
        }
        if self.timer_irq_to_sa1 {
            b |= 0x40;
        }
        if self.dma_irq_to_sa1 {
            b |= 0x20;
        }
        if self.main_nmi_to_sa1 {
            b |= 0x10;
        }
        b |= self.ccnt_msg & 0x0F;
        b
    }

    /// Translate a CPU-side ROM access through the four super-bank
    /// registers into a linear byte offset into the ROM vector.
    /// Returns `None` if the address doesn't fall in a ROM region.
    ///
    /// SA-1 banking (per ares' `rom.cpp` + Mesen2's `UpdatePrgRomMappings`):
    ///
    /// * `$00-1F:8000-FFFF` + `$C0-CF:0000-FFFF` → bank C (CXB / `Banks[0]`)
    /// * `$20-3F:8000-FFFF` + `$D0-DF:0000-FFFF` → bank D (DXB / `Banks[1]`)
    /// * `$80-9F:8000-FFFF` + `$E0-EF:0000-FFFF` → bank E (EXB / `Banks[2]`)
    /// * `$A0-BF:8000-FFFF` + `$F0-FF:0000-FFFF` → bank F (FXB / `Banks[3]`)
    ///
    /// Each super-bank register has bit 7 = "remap mode" flag:
    /// * Cleared → LoROM half-bank windows use the *default* MB
    ///   slot for that bank (Banks[0]→MB0, [1]→MB1, [2]→MB2, [3]→MB3).
    /// * Set → LoROM half-bank windows use the MB selected by bits
    ///   0..2 of the bank register.
    ///
    /// The HiROM full-bank windows (`$C0+`) always use the banked MB
    /// regardless of the mode flag.
    fn rom_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        const MB: usize = 0x10_0000;
        // Identify which super-bank region this address belongs to, the
        // local bank index within that region (0..31 for LoROM windows,
        // 0..15 for HiROM windows), and whether it's the LoROM half-bank
        // ($8000-$FFFF) or the HiROM full-bank view.
        let (reg, default_mb, local_bank, is_lo) = match bank {
            0x00..=0x1F if offset >= 0x8000 => (self.cxb, 0u8, bank, true),
            0x20..=0x3F if offset >= 0x8000 => (self.dxb, 1u8, bank - 0x20, true),
            0x80..=0x9F if offset >= 0x8000 => (self.exb, 2u8, bank - 0x80, true),
            0xA0..=0xBF if offset >= 0x8000 => (self.fxb, 3u8, bank - 0xA0, true),
            0xC0..=0xCF => (self.cxb, 0u8, bank - 0xC0, false),
            0xD0..=0xDF => (self.dxb, 1u8, bank - 0xD0, false),
            0xE0..=0xEF => (self.exb, 2u8, bank - 0xE0, false),
            0xF0..=0xFF => (self.fxb, 3u8, bank - 0xF0, false),
            _ => return None,
        };
        // Pick the MB slot. LoROM half-banks defer to `default_mb`
        // unless the mode flag is set; HiROM full-banks always use
        // the banked MB.
        let mb = if !is_lo || (reg & 0x80) != 0 {
            usize::from(reg & 0x07)
        } else {
            usize::from(default_mb)
        };
        let base = mb * MB;
        let within_mb = if is_lo {
            usize::from(local_bank) * 0x8000 + (usize::from(offset) - 0x8000)
        } else {
            usize::from(local_bank) * 0x1_0000 + usize::from(offset)
        };
        let off = base + within_mb;
        if off < self.rom.len() {
            Some(off)
        } else {
            None
        }
    }

    /// I-RAM access from the **main** CPU's view: 2 KB at
    /// `$3000-$37FF` of banks `$00-$3F` and `$80-$BF`.
    fn iram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        let bank_ok = matches!(bank, 0x00..=0x3F | 0x80..=0xBF);
        let offset_ok = (0x3000..=0x37FF).contains(&offset);
        if bank_ok && offset_ok {
            Some(usize::from(offset - 0x3000))
        } else {
            None
        }
    }

    /// I-RAM access from the **SA-1** CPU's view. The SA-1 sees the
    /// shared 2 KB I-RAM at both `$3000-$37FF` and `$0000-$07FF` of
    /// banks `$00-$3F` / `$80-$BF` (per ares' `memory.cpp` and
    /// Mesen2's `RegisterHandler(0x00, 0x3F, 0x0000, 0x0FFF, ...)`).
    /// The `$0000-$07FF` mirror is what the SA-1's direct-page mode
    /// reaches when DP is in low memory.
    fn iram_offset_sa1(&self, bank: u8, offset: u16) -> Option<usize> {
        let bank_ok = matches!(bank, 0x00..=0x3F | 0x80..=0xBF);
        if !bank_ok {
            return None;
        }
        if (0x3000..=0x37FF).contains(&offset) {
            return Some(usize::from(offset - 0x3000));
        }
        if offset < 0x0800 {
            return Some(usize::from(offset));
        }
        None
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
            let mr_addr = 0x2200 + idx as u16;
            // VDP reads are side-effecting (auto-advance the bit
            // cursor on the matching trigger half), so they're not in
            // the pure-`match` arm below.
            match mr_addr {
                0x230C => {
                    let data = self.vlbp_data();
                    let lo = data as u8;
                    if (self.vbd & 0x80) == 0 {
                        self.vlbp_advance();
                    }
                    return Some(lo);
                }
                0x230D => {
                    let data = self.vlbp_data();
                    let hi = (data >> 8) as u8;
                    if (self.vbd & 0x80) != 0 {
                        self.vlbp_advance();
                    }
                    return Some(hi);
                }
                _ => {}
            }
            // $2306-$230A → 40-bit MR result. We expose 5 bytes
            // little-endian.
            return Some(match mr_addr {
                0x2300 => self.read_sfr(),
                0x2301 => self.read_cfr(),
                // HCR / VCR — the SA-1's free-running H/V counters.
                // In linear mode we expose the 18-bit `linear_counter`
                // split into lo (HCR), hi (VCR), and the top 2 bits in
                // the next register.
                0x2302 => self.linear_counter as u8,
                0x2303 => (self.linear_counter >> 8) as u8,
                0x2304 => (self.linear_counter >> 16) as u8 & 0x03,
                0x2305 => 0,
                0x2306 => self.mr as u8,
                0x2307 => (self.mr >> 8) as u8,
                0x2308 => (self.mr >> 16) as u8,
                0x2309 => (self.mr >> 24) as u8,
                0x230A => (self.mr >> 32) as u8,
                _ => self.mmio[idx],
            });
        }
        // Main-CPU vector override — when the SA-1 is currently
        // asserting an IRQ/NMI to the S-CPU and the matching IVSW /
        // NMIVW bit is set, the bank-0 vector slots read back as the
        // SA-1's SIV / SNV instead of the real ROM bytes.
        if let Some(v) = self.main_vector_override(bank, offset) {
            return Some(v);
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
        // The trait entry point is the main-CPU view. `Sa1Bus` calls
        // `write_from_sa1` directly so I-RAM / BW-RAM write protection
        // can distinguish the two sides.
        self.write_with_side(addr, value, WriteSide::Main)
    }

    fn rom_size(&self) -> usize {
        self.rom.len()
    }

    fn sram_size(&self) -> usize {
        self.bwram.len()
    }
}

impl Sa1Mapper {
    /// Side-aware read entry for the SA-1's own bus. Uses
    /// [`Sa1Mapper::iram_offset_sa1`] which also exposes the I-RAM
    /// mirror at `$0000-$07FF`, and skips the main-CPU vector
    /// override (the SA-1 has its own override via
    /// [`Sa1Mapper::sa1_vector_override`], applied by [`super::Sa1Bus`]).
    /// MMIO reads route identically to the main-CPU path.
    pub fn read_from_sa1(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(idx) = Self::mmio_offset(addr) {
            // Same MMIO handling as the trait's `read`. We re-dispatch
            // through it but mask out the main-CPU `main_vector_override`
            // by intercepting it ourselves first (MMIO addresses live
            // outside that range so this is a no-op early-out).
            let _ = idx;
            return self.read(addr);
        }
        if let Some(o) = self.iram_offset_sa1(bank, offset) {
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

    /// Side-aware write entry for the SA-1's own bus. Drives I-RAM /
    /// BW-RAM through the CIWP / CBWE protection masks instead of
    /// the S-CPU's SIWP / SBWE / BWPA. MMIO writes are routed
    /// identically.
    pub fn write_from_sa1(&mut self, addr: Addr24, value: u8) -> bool {
        self.write_with_side(addr, value, WriteSide::Sa1)
    }

    fn write_with_side(&mut self, addr: Addr24, value: u8, side: WriteSide) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(idx) = Self::mmio_offset(addr) {
            let absolute = 0x2200 + idx as u16;
            let prev = self.mmio[idx];
            self.mmio[idx] = value;
            match absolute {
                // -------- S-CPU → SA-1 control --------
                0x2200 => {
                    // CCNT bit layout (per ares + Mesen2):
                    //   bit 7 = SA-1 IRQ request (0 → 1 edge latches)
                    //   bit 6 = SA-1 wait (not modelled)
                    //   bit 5 = SA-1 reset (handled in Sa1Chip::write)
                    //   bit 4 = SA-1 NMI request (0 → 1 edge latches)
                    //   bits 3..0 = message to SA-1
                    self.ccnt_msg = value & 0x0F;
                    if (prev & 0x80) == 0 && (value & 0x80) != 0 {
                        self.main_irq_to_sa1 = true;
                    }
                    if (prev & 0x10) == 0 && (value & 0x10) != 0 {
                        self.main_nmi_to_sa1 = true;
                    }
                }
                0x2201 => self.sie = value,
                0x2202 => {
                    if (value & 0x80) != 0 {
                        self.s_irq_to_main = false;
                    }
                    if (value & 0x40) != 0 {
                        self.s_nmi_to_main = false;
                    }
                    if (value & 0x20) != 0 {
                        self.cc1_irq_to_main = false;
                    }
                }
                0x2203 | 0x2204 => {}
                0x2205 => self.cnv_lo = value,
                0x2206 => self.cnv_hi = value,
                0x2207 => self.civ_lo = value,
                0x2208 => self.civ_hi = value,

                // -------- SA-1 → S-CPU control --------
                0x2209 => {
                    // SCNT bit layout (per ares + Mesen2):
                    //   bit 7 = SA-1 → S-CPU IRQ request (0 → 1 edge)
                    //   bit 6 = IVSW (vector override for S-CPU IRQ)
                    //   bit 4 = NMIVW (vector override for S-CPU NMI)
                    //   bits 3..0 = message to S-CPU
                    // Real hardware has no SA-1 → S-CPU NMI trigger
                    // path; only the NMIVW override re-routes the
                    // S-CPU's own NMI vector to SNV.
                    self.scnt = value;
                    if (prev & 0x80) == 0 && (value & 0x80) != 0 {
                        self.s_irq_to_main = true;
                    }
                }
                // CIE bit layout (per ares + Mesen2):
                //   bit 7 = S-CPU → SA-1 IRQ enable
                //   bit 6 = timer IRQ enable
                //   bit 5 = DMA IRQ enable
                //   bit 4 = S-CPU → SA-1 NMI enable
                0x220A => self.cie = value,
                // CIC mirror: each bit clears its CFR latch.
                0x220B => {
                    if (value & 0x80) != 0 {
                        self.main_irq_to_sa1 = false;
                    }
                    if (value & 0x40) != 0 {
                        self.timer_irq_to_sa1 = false;
                    }
                    if (value & 0x20) != 0 {
                        self.dma_irq_to_sa1 = false;
                    }
                    if (value & 0x10) != 0 {
                        self.main_nmi_to_sa1 = false;
                    }
                }
                0x220C => self.snv_lo = value,
                0x220D => self.snv_hi = value,
                0x220E => self.siv_lo = value,
                0x220F => self.siv_hi = value,

                // -------- SA-1 timer --------
                0x2210 => {
                    self.tmc = value;
                    self.timer_match_armed = true;
                }
                0x2211 => {
                    self.linear_counter = 0;
                    self.timer_match_armed = true;
                }
                0x2212 => self.hcnt_lo = value,
                0x2213 => self.hcnt_hi = value,
                0x2214 => self.vcnt_lo = value,
                0x2215 => self.vcnt_hi = value,

                // -------- SA-1 DMA --------
                0x2230 => {
                    self.dcnt = value;
                    if (value & 0x80) != 0 {
                        match (value & 0x30) >> 4 {
                            0 | 1 => self.run_normal_dma(), // bit 5 = 0 → normal
                            2 => self.run_cc1_dma(),        // CC enable + Type-1
                            3 => self.cc2_arm(),            // CC enable + Type-2
                            _ => unreachable!(),
                        }
                    } else if self.cc2_active {
                        // Game cleared DCNT.7 → terminate any in-flight
                        // CC2 stream.
                        self.cc2_end();
                    }
                }
                0x2231 => {
                    self.cdma = value;
                    // CDMA bit 7 = CDEND. Writing 1 terminates a CC2
                    // stream immediately.
                    if (value & 0x80) != 0 && self.cc2_active {
                        self.cc2_end();
                    }
                }
                0x2232 => self.sda = (self.sda & !0x0000FF) | u32::from(value),
                0x2233 => self.sda = (self.sda & !0x00FF00) | (u32::from(value) << 8),
                0x2234 => self.sda = (self.sda & !0xFF0000) | (u32::from(value) << 16),
                0x2235 => self.dda = (self.dda & !0x0000FF) | u32::from(value),
                0x2236 => self.dda = (self.dda & !0x00FF00) | (u32::from(value) << 8),
                0x2237 => self.dda = (self.dda & !0xFF0000) | (u32::from(value) << 16),
                0x2238 => self.dtc = (self.dtc & 0xFF00) | u16::from(value),
                0x2239 => self.dtc = (self.dtc & 0x00FF) | (u16::from(value) << 8),
                0x223F => self.cc2_consume_byte(value),

                0x2220 => self.cxb = value,
                0x2221 => self.dxb = value,
                0x2222 => self.exb = value,
                0x2223 => self.fxb = value,
                0x2224 => self.bmaps = value,
                0x2226 => self.sbwe = value,
                0x2227 => self.cbwe = value,
                0x2228 => self.bwpa = value,
                // Per ares + Mesen2: SIWP at $2229, CIWP at $222A.
                // (Older docs had these one address higher.)
                0x2229 => self.siwp = value,
                0x222A => self.ciwp = value,
                0x2250 => {
                    self.mcnt = value;
                    if value & 0x02 != 0 && value == 0x02 {
                        self.mr = 0;
                    }
                }
                0x2251 => self.ma = (self.ma & !0xFF) | i16::from(value),
                0x2252 => self.ma = (self.ma & 0xFF) | (i16::from(value as i8) << 8),
                0x2253 => self.mb = (self.mb & !0xFF) | i16::from(value),
                0x2254 => {
                    self.mb = (self.mb & 0xFF) | (i16::from(value as i8) << 8);
                    self.update_arith();
                }
                // -------- VLBP (Variable-Length Bit Processor) --------
                0x2258 => self.vbd = value,
                0x2259 => {
                    self.vda_base = (self.vda_base & !0x0000FF) | u32::from(value);
                }
                0x225A => {
                    self.vda_base = (self.vda_base & !0x00FF00) | (u32::from(value) << 8);
                }
                0x225B => {
                    self.vda_base = (self.vda_base & !0xFF0000) | (u32::from(value) << 16);
                    self.vbit_offset = 0;
                }
                _ => {}
            }
            return true;
        }
        // SA-1 writes can also reach the I-RAM mirror at $0000-$07FF;
        // the main CPU only sees I-RAM at $3000-$37FF (so its writes
        // into $0000-$07FF should fall through to the bus's WRAM).
        let iram = match side {
            WriteSide::Main => self.iram_offset(bank, offset),
            WriteSide::Sa1 => self.iram_offset_sa1(bank, offset),
        };
        if let Some(o) = iram {
            if self.iram_writable_for(o, side) {
                self.iram[o] = value;
            }
            // Always claim the access — the bus would otherwise fall
            // through to WRAM, which isn't what protection means.
            return true;
        }
        if let Some(o) = self.bwram_offset(bank, offset) {
            if self.bwram_writable_for(o, side) {
                self.bwram[o] = value;
            }
            return true;
        }
        self.rom_offset(bank, offset).is_some()
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

    // ------------- Phase-3 IRQ message tests -------------

    #[test]
    fn main_to_sa1_irq_edge_latches_and_gates_through_cie() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // Enable S-CPU → SA-1 IRQ on the SA-1 side first.
        m.write(make_addr(0x00, 0x220A), 0x80);
        assert!(!m.sa1_irq_line(), "no IRQ until the S-CPU triggers it");
        // CCNT bit 7 0→1 latches the IRQ (per ares + Mesen2).
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(m.sa1_irq_line(), "edge should latch + gate through CIE");
        // CIC bit 7 clears the latch.
        m.write(make_addr(0x00, 0x220B), 0x80);
        assert!(!m.sa1_irq_line());
    }

    #[test]
    fn main_to_sa1_irq_requires_a_clean_0_to_1_edge() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x220A), 0x80);
        // Pre-set bit 7 → first 0→1 edge latches.
        m.write(make_addr(0x00, 0x2200), 0x80);
        m.write(make_addr(0x00, 0x220B), 0x80); // CIC bit 7 = clear IRQ
        assert!(!m.sa1_irq_line());
        // Writing bit 7 again with no intervening clear is a 1→1: no edge.
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(
            !m.sa1_irq_line(),
            "1→1 same-bit retain should not re-trigger"
        );
        // Clear bit 7, then set it again: 0→1 edge.
        // (Mind the bit-5 reset bit — leave the default $20 alone.)
        m.write(make_addr(0x00, 0x2200), 0x20);
        m.write(make_addr(0x00, 0x2200), 0xA0);
        assert!(m.sa1_irq_line());
    }

    #[test]
    fn cie_mask_zero_blocks_main_to_sa1_irq() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // CIE = 0 → all incoming sources disabled.
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(!m.sa1_irq_line(), "CIE disabled blocks the IRQ line");
    }

    #[test]
    fn sa1_to_main_irq_edge_latches_and_gates_through_sie() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // Enable the SA-1 → S-CPU IRQ on the main side.
        m.write(make_addr(0x00, 0x2201), 0x80);
        // SCNT bit 7 0→1 → latch.
        m.write(make_addr(0x00, 0x2209), 0x80);
        assert!(m.main_irq_line());
        // SIC clears it.
        m.write(make_addr(0x00, 0x2202), 0x80);
        assert!(!m.main_irq_line());
    }

    #[test]
    fn sfr_reflects_sa1_to_main_irq_latch_and_message_nibble() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x2201), 0x80);
        // SCNT: bit 7 = IRQ, bit 4 = NMIVW (mirror is bit 4? no — bit 4
        // = IVSW-bit 5 of SFR; we only check IRQ bit + message here).
        m.write(make_addr(0x00, 0x2209), 0x80 | 0x05);
        let sfr = m.read(make_addr(0x00, 0x2300)).unwrap();
        assert_eq!(sfr & 0x80, 0x80, "bit 7 = SA-1 IRQ");
        assert_eq!(sfr & 0x0F, 0x05, "low nibble = message");
    }

    #[test]
    fn cfr_reflects_main_to_sa1_irq_latch_and_message_nibble() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x220A), 0x80);
        // CCNT: bit 7 = IRQ trigger, bits 0..3 = message.
        m.write(make_addr(0x00, 0x2200), 0x80 | 0x0A);
        let cfr = m.read(make_addr(0x00, 0x2301)).unwrap();
        assert_eq!(cfr & 0x80, 0x80);
        assert_eq!(cfr & 0x0F, 0x0A);
    }

    #[test]
    fn main_irq_vector_overrides_to_siv_when_ivsw_and_latched() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x220E), 0x34); // SIV lo
        m.write(make_addr(0x00, 0x220F), 0x12); // SIV hi
        m.write(make_addr(0x00, 0x2201), 0x80); // SIE.7 enable
        // SCNT: IVSW (bit 5… err, in our impl we use $40) + IRQ trigger.
        // IVSW = bit 5 of SCNT per Anomie; our scheme uses $40.
        m.write(make_addr(0x00, 0x2209), 0x80 | 0x40);
        // Now main reads $00:FFEE/FFEF — they should reflect SIV.
        assert_eq!(m.read(make_addr(0x00, 0xFFEE)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0xFFEF)), Some(0x12));
        assert_eq!(m.read(make_addr(0x00, 0xFFFE)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0xFFFF)), Some(0x12));
    }

    #[test]
    fn main_irq_vector_falls_back_to_rom_when_ivsw_clear() {
        let mut m = Sa1Mapper::new(ramp_rom(0x10_0000), 0);
        m.write(make_addr(0x00, 0x220E), 0x34);
        m.write(make_addr(0x00, 0x220F), 0x12);
        m.write(make_addr(0x00, 0x2201), 0x80);
        // IVSW (SCNT bit 6) clear → no override. ares + Mesen2
        // explicitly treat the vector override as level-only on the
        // IVSW bit, *not* gated by whether the IRQ line is pending.
        m.write(make_addr(0x00, 0x2209), 0x00);
        let v = m.read(make_addr(0x00, 0xFFEE)).unwrap();
        assert_ne!(v, 0x34, "no override without IVSW");
    }

    // ------------- Phase-3 timer tests -------------

    #[test]
    fn timer_linear_mode_fires_irq_on_compare_match() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // Enable SA-1-side timer IRQ (CIE bit 6 per ares + Mesen2).
        m.write(make_addr(0x00, 0x220A), 0x40);
        // Set TMC linear mode + H enable.
        m.write(make_addr(0x00, 0x2210), 0x81);
        // Compare = 100.
        m.write(make_addr(0x00, 0x2212), 100);
        m.write(make_addr(0x00, 0x2213), 0);
        m.write(make_addr(0x00, 0x2214), 0);
        // Tick 50 ticks — not enough, no IRQ.
        m.tick_timer(50);
        assert!(!m.sa1_irq_line());
        // Tick another 60 — crosses 100, fires.
        m.tick_timer(60);
        assert!(m.sa1_irq_line(), "timer should have fired at 100");
    }

    #[test]
    fn timer_reset_via_ctr_clears_the_counter() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x2210), 0x81);
        m.tick_timer(123);
        assert_eq!(m.read(make_addr(0x00, 0x2302)), Some(123));
        m.write(make_addr(0x00, 0x2211), 0x00); // CTR
        assert_eq!(m.read(make_addr(0x00, 0x2302)), Some(0));
    }

    #[test]
    fn timer_hv_mode_does_not_fire_irq() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x220A), 0x40); // CIE bit 6 = timer
        // TMC = $01 (H enable, HV mode) — no linear progress + no IRQ.
        m.write(make_addr(0x00, 0x2210), 0x01);
        m.tick_timer(10_000);
        assert!(!m.sa1_irq_line());
    }

    #[test]
    fn timer_compare_match_re_arms_after_clear() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x220A), 0x40); // CIE bit 6 = timer
        m.write(make_addr(0x00, 0x2210), 0x81);
        m.write(make_addr(0x00, 0x2212), 50);
        m.tick_timer(60);
        assert!(m.sa1_irq_line());
        // Clear via CIC bit 6.
        m.write(make_addr(0x00, 0x220B), 0x40);
        assert!(!m.sa1_irq_line());
        // CTR write to re-arm.
        m.write(make_addr(0x00, 0x2211), 0x00);
        // Need another full pass to re-trigger.
        m.tick_timer(60);
        assert!(m.sa1_irq_line(), "re-armed timer fires on next match");
    }

    // ------------- Phase-3 normal DMA tests -------------

    #[test]
    fn normal_dma_copies_iram_to_bwram_and_raises_irq() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Seed I-RAM with a tiny pattern.
        for i in 0..16 {
            m.write(make_addr(0x00, 0x3000 + i), 0xA0 + i as u8);
        }
        m.write(make_addr(0x00, 0x220A), 0x20); // CIE bit 5 = DMA IRQ
        // SDA = $00:3000 (I-RAM)
        m.write(make_addr(0x00, 0x2232), 0x00);
        m.write(make_addr(0x00, 0x2233), 0x30);
        m.write(make_addr(0x00, 0x2234), 0x00);
        // DDA = $40:0000 (linear BW-RAM view)
        m.write(make_addr(0x00, 0x2235), 0x00);
        m.write(make_addr(0x00, 0x2236), 0x00);
        m.write(make_addr(0x00, 0x2237), 0x40);
        // DTC = 16
        m.write(make_addr(0x00, 0x2238), 16);
        m.write(make_addr(0x00, 0x2239), 0);
        // DCNT = bit 7 enable, no CC.
        m.write(make_addr(0x00, 0x2230), 0x80);
        // Check destination got the bytes + DMA enable cleared + IRQ
        // line asserted.
        for i in 0..16 {
            assert_eq!(m.read(make_addr(0x40, i as u16)), Some(0xA0 + i as u8));
        }
        let dcnt = m.read(make_addr(0x00, 0x2230)).unwrap();
        assert_eq!(dcnt & 0x80, 0, "DMA enable should auto-clear");
        assert!(m.sa1_irq_line(), "DMA IRQ should be asserted");
    }

    #[test]
    fn normal_dma_from_rom_to_bwram() {
        let rom = (0..0x1_0000).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let mut m = Sa1Mapper::new(rom, 0x10000);
        // SDA = $00:8000 (= ROM[0]).
        m.write(make_addr(0x00, 0x2232), 0x00);
        m.write(make_addr(0x00, 0x2233), 0x80);
        m.write(make_addr(0x00, 0x2234), 0x00);
        // DDA = $40:0000.
        m.write(make_addr(0x00, 0x2235), 0x00);
        m.write(make_addr(0x00, 0x2236), 0x00);
        m.write(make_addr(0x00, 0x2237), 0x40);
        m.write(make_addr(0x00, 0x2238), 4);
        m.write(make_addr(0x00, 0x2239), 0);
        m.write(make_addr(0x00, 0x2230), 0x80);
        assert_eq!(m.read(make_addr(0x40, 0)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 1)), Some(0x01));
        assert_eq!(m.read(make_addr(0x40, 2)), Some(0x02));
        assert_eq!(m.read(make_addr(0x40, 3)), Some(0x03));
    }

    // ------------- Phase-4 CC1 DMA tests -------------

    /// Helper — set up a CC1 DMA so the caller can pre-fill the
    /// source and read the converted tile back.
    fn cc1_setup(m: &mut Sa1Mapper, cdma: u8, dtc: u16) {
        // Enable CC1 IRQ on the S-CPU side.
        m.write(make_addr(0x00, 0x2201), 0x20);
        // SDA = $40:0000 (linear BW-RAM start).
        m.write(make_addr(0x00, 0x2232), 0x00);
        m.write(make_addr(0x00, 0x2233), 0x00);
        m.write(make_addr(0x00, 0x2234), 0x40);
        // DDA = $00:3000 (I-RAM start).
        m.write(make_addr(0x00, 0x2235), 0x00);
        m.write(make_addr(0x00, 0x2236), 0x30);
        m.write(make_addr(0x00, 0x2237), 0x00);
        m.write(make_addr(0x00, 0x2231), cdma);
        m.write(make_addr(0x00, 0x2238), (dtc & 0xFF) as u8);
        m.write(make_addr(0x00, 0x2239), (dtc >> 8) as u8);
    }

    #[test]
    fn cc1_4bpp_solid_color_5_produces_expected_planar_bytes() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // 4bpp, tile_width=8 → bitmap row stride = 8 tiles × 4 B = 32
        // bytes. Tile 0 occupies bytes [row*32 + 0..=3]. Each pixel =
        // 5 → packed-byte value 0x55.
        for row in 0..8u32 {
            for c in 0..4u32 {
                m.write(make_addr(0x40, (row * 32 + c) as u16), 0x55);
            }
        }
        cc1_setup(&mut m, 0b0000_0100, 32);
        // Fire CC1.
        m.write(make_addr(0x00, 0x2230), 0xA0);
        // Read converted tile from $00:3000.
        let mut read = |off: u16| m.read(make_addr(0x00, 0x3000 + off)).unwrap();
        // bp0 / bp2 → all 0xFF (bits 0 and 2 of 5 = 1).
        // bp1 / bp3 → all 0x00 (bits 1 and 3 of 5 = 0).
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(read(b), 0xFF, "bp0 row {row}");
            assert_eq!(read(b + 1), 0x00, "bp1 row {row}");
            assert_eq!(read(b + 16), 0xFF, "bp2 row {row}");
            assert_eq!(read(b + 17), 0x00, "bp3 row {row}");
        }
        // CC1 IRQ + DCNT auto-cleared.
        assert!(m.main_irq_line(), "CC1 IRQ must reach the main CPU");
        let dcnt = m.read(make_addr(0x00, 0x2230)).unwrap();
        assert_eq!(dcnt & 0x80, 0);
    }

    #[test]
    fn cc1_4bpp_first_row_gradient_matches_anomie_layout() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // 4bpp, tile_width=8 → row stride 32 bytes. Tile-0 row-0
        // occupies bytes [0..=3] and represents pixels [1..=8].
        m.write(make_addr(0x40, 0), 0x12);
        m.write(make_addr(0x40, 1), 0x34);
        m.write(make_addr(0x40, 2), 0x56);
        m.write(make_addr(0x40, 3), 0x78);
        cc1_setup(&mut m, 0b0000_0100, 32);
        m.write(make_addr(0x00, 0x2230), 0xA0);
        let mut read = |off: u16| m.read(make_addr(0x00, 0x3000 + off)).unwrap();
        // Anomie-correct planar values: bp0=0xAA, bp1=0x66, bp2=0x1E,
        // bp3=0x01 (computed offline).
        assert_eq!(read(0), 0xAA);
        assert_eq!(read(1), 0x66);
        assert_eq!(read(16), 0x1E);
        assert_eq!(read(17), 0x01);
    }

    #[test]
    fn cc1_2bpp_one_tile_solid_color_3() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // 2bpp, tile_width=8 → row stride = 8 tiles × 2 B = 16 bytes.
        // Tile 0 occupies bytes [row*16 + 0..=1]. All pixels = 3 →
        // packed-byte = 0xFF.
        for row in 0..8u32 {
            for c in 0..2u32 {
                m.write(make_addr(0x40, (row * 16 + c) as u16), 0xFF);
            }
        }
        cc1_setup(&mut m, 0b0000_1000, 16);
        m.write(make_addr(0x00, 0x2230), 0xA0);
        let mut read = |off: u16| m.read(make_addr(0x00, 0x3000 + off)).unwrap();
        // bp0 / bp1 both all 0xFF for 8 rows.
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(read(b), 0xFF);
            assert_eq!(read(b + 1), 0xFF);
        }
    }

    #[test]
    fn cc1_8bpp_one_tile_color_1_only_bp0_lights_up() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // 8bpp, tile_width=8 → row stride = 64. Tile 0 occupies bytes
        // [row*64 + 0..=7]. All pixels = 1.
        for row in 0..8u32 {
            for c in 0..8u32 {
                m.write(make_addr(0x40, (row * 64 + c) as u16), 0x01);
            }
        }
        cc1_setup(&mut m, 0b0000_0000, 64);
        m.write(make_addr(0x00, 0x2230), 0xA0);
        let mut read = |off: u16| m.read(make_addr(0x00, 0x3000 + off)).unwrap();
        // Only bp0 should be 0xFF; bp1..bp7 should be 0.
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(read(b), 0xFF, "bp0 row {row}");
            assert_eq!(read(b + 1), 0x00, "bp1 row {row}");
        }
        // Higher plane groups (bp2..bp7) all-zero.
        for off in 16..64u16 {
            assert_eq!(read(off), 0x00, "high plane at {off:#x}");
        }
    }

    #[test]
    fn cc1_two_tiles_wide_16_layout_reads_tile1_from_correct_offset() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // 4bpp, tile_width = 16 (bits 1..0 = 1). Each pixel-row of the
        // bitmap spans 16 tiles × 4 bytes = 64 bytes. Tile 1 starts at
        // byte 4 of each pixel-row.
        // Fill tile 0 (cols 0..3 of each row) with 0x00; tile 1 (cols
        // 4..7) with 0xFF (= pixel value 15 everywhere).
        for row in 0..8 {
            let row_off = row * 64;
            for c in 0..4 {
                m.write(make_addr(0x40, (row_off + c) as u16), 0x00);
            }
            for c in 4..8 {
                m.write(make_addr(0x40, (row_off + c) as u16), 0xFF);
            }
        }
        cc1_setup(&mut m, 0b0000_0101, 64);
        m.write(make_addr(0x00, 0x2230), 0xA0);
        // Tile 0 → all 0x00 in I-RAM at $3000..$3020.
        for off in 0u16..32 {
            assert_eq!(
                m.read(make_addr(0x00, 0x3000 + off)),
                Some(0x00),
                "tile 0 should be empty at {off:#x}"
            );
        }
        // Tile 1 → all 0xFF (pixel 15 = 0b1111 sets all four planes).
        for off in 0u16..32 {
            assert_eq!(
                m.read(make_addr(0x00, 0x3020 + off)),
                Some(0xFF),
                "tile 1 should be all-set at {off:#x}"
            );
        }
    }

    // ------------- Phase-6 CC2 tests -------------

    /// Helper — arm a CC2 stream targeting I-RAM `$00:3000` with the
    /// given colour mode in CDMA (bits 4..2).
    fn cc2_setup(m: &mut Sa1Mapper, cdma: u8) {
        m.write(make_addr(0x00, 0x2201), 0x20); // SIE — enable CC-IRQ
        m.write(make_addr(0x00, 0x2235), 0x00); // DDA lo
        m.write(make_addr(0x00, 0x2236), 0x30);
        m.write(make_addr(0x00, 0x2237), 0x00);
        m.write(make_addr(0x00, 0x2231), cdma);
        // DCNT bit 7 + bit 5 + bit 4 = arm CC2.
        m.write(make_addr(0x00, 0x2230), 0xB0);
    }

    #[test]
    fn cc2_4bpp_solid_color_5_produces_expected_planar_bytes() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0100); // CDMA = 4bpp
        // 4bpp tile = 32 source bytes (4 per row × 8 rows). Each byte
        // packs two 4bpp pixels of value 5 → 0x55.
        for _ in 0..32 {
            m.write(make_addr(0x00, 0x223F), 0x55);
        }
        let mut read = |off: u16| m.read(make_addr(0x00, 0x3000 + off)).unwrap();
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(read(b), 0xFF, "bp0 row {row}");
            assert_eq!(read(b + 1), 0x00, "bp1 row {row}");
            assert_eq!(read(b + 16), 0xFF, "bp2 row {row}");
            assert_eq!(read(b + 17), 0x00, "bp3 row {row}");
        }
        assert!(m.main_irq_line(), "CC2 must raise the CC IRQ");
    }

    #[test]
    fn cc2_advances_dda_per_tile() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0100);
        // Tile 0 — pixels all = 5.
        for _ in 0..32 {
            m.write(make_addr(0x00, 0x223F), 0x55);
        }
        // Tile 1 — pixels all = 10 (`0xAA`).
        for _ in 0..32 {
            m.write(make_addr(0x00, 0x223F), 0xAA);
        }
        // Tile 0 at $3000..=$301F: bp0=FF, bp1=00, bp2=FF, bp3=00.
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0xFF));
        // Tile 1 at $3020..=$303F: pixel 10 = 0b1010, so bp0=00, bp1=FF,
        // bp2=00, bp3=FF.
        assert_eq!(m.read(make_addr(0x00, 0x3020)), Some(0x00));
        assert_eq!(m.read(make_addr(0x00, 0x3021)), Some(0xFF));
        assert_eq!(m.read(make_addr(0x00, 0x3030)), Some(0x00));
        assert_eq!(m.read(make_addr(0x00, 0x3031)), Some(0xFF));
    }

    #[test]
    fn cc2_2bpp_stream_one_tile() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_1000); // 2bpp
        // 2bpp: 2 source bytes per row × 8 rows = 16 bytes. All
        // pixels = 3 → packed byte 0xFF.
        for _ in 0..16 {
            m.write(make_addr(0x00, 0x223F), 0xFF);
        }
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(m.read(make_addr(0x00, 0x3000 + b)), Some(0xFF));
            assert_eq!(m.read(make_addr(0x00, 0x3000 + b + 1)), Some(0xFF));
        }
    }

    #[test]
    fn cc2_8bpp_stream_one_tile() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0000); // 8bpp
        // 8bpp: 8 source bytes per row × 8 rows = 64 bytes. Pixel = 1
        // → only bp0 lights.
        for _ in 0..64 {
            m.write(make_addr(0x00, 0x223F), 0x01);
        }
        for row in 0..8 {
            let b = row as u16 * 2;
            assert_eq!(m.read(make_addr(0x00, 0x3000 + b)), Some(0xFF));
            assert_eq!(m.read(make_addr(0x00, 0x3000 + b + 1)), Some(0x00));
        }
        // bp2..bp7 quiet.
        for off in 16u16..64 {
            assert_eq!(m.read(make_addr(0x00, 0x3000 + off)), Some(0x00));
        }
    }

    #[test]
    fn cc2_partial_tile_does_not_emit_yet() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0100); // 4bpp, 32 B/tile
        for _ in 0..16 {
            m.write(make_addr(0x00, 0x223F), 0x55);
        }
        // Half a tile streamed — destination still empty, no IRQ yet.
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0x00));
        assert!(!m.main_irq_line(), "no IRQ until a full tile is staged");
    }

    #[test]
    fn cc2_cdend_terminates_stream_mid_flight() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0100);
        // Stream half a tile.
        for _ in 0..16 {
            m.write(make_addr(0x00, 0x223F), 0x55);
        }
        // CDEND.
        m.write(make_addr(0x00, 0x2231), 0x80);
        // DCNT.7 cleared and further BBF writes don't do anything.
        let dcnt = m.read(make_addr(0x00, 0x2230)).unwrap();
        assert_eq!(dcnt & 0x80, 0);
        // Subsequent BBF writes get dropped (cc2_active = false).
        for _ in 0..32 {
            m.write(make_addr(0x00, 0x223F), 0xFF);
        }
        // Destination remains untouched.
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0x00));
    }

    // ------------- Phase-5 VLBP tests -------------

    /// Helper — write a `vlen + mode` to VBD and point VDA at the
    /// linear BW-RAM origin (`$40:0000`).
    fn vlbp_setup(m: &mut Sa1Mapper, vbd: u8) {
        m.write(make_addr(0x00, 0x2258), vbd);
        m.write(make_addr(0x00, 0x2259), 0x00);
        m.write(make_addr(0x00, 0x225A), 0x00);
        m.write(make_addr(0x00, 0x225B), 0x40);
    }

    #[test]
    fn vlbp_fixed_4bit_reads_nibbles_in_lsb_first_order() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Stream byte 0 = 0xAB → low nibble first, then high.
        m.write(make_addr(0x40, 0), 0xAB);
        m.write(make_addr(0x40, 1), 0xCD);
        vlbp_setup(&mut m, 0x04); // fixed mode, vlen = 4
        // First read at $230C returns 0xB (low nibble) and advances.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0x0B));
        // Second read returns 0xA (high nibble of byte 0).
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0x0A));
        // Cross into byte 1 → 0xD then 0xC.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0x0D));
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0x0C));
    }

    #[test]
    fn vlbp_fixed_8bit_passthrough() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0), 0xAB);
        m.write(make_addr(0x40, 1), 0xCD);
        m.write(make_addr(0x40, 2), 0xEF);
        vlbp_setup(&mut m, 0x08);
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xAB));
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xCD));
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xEF));
    }

    #[test]
    fn vlbp_fixed_16bit_split_across_lo_then_hi() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0), 0xAB);
        m.write(make_addr(0x40, 1), 0xCD);
        m.write(make_addr(0x40, 2), 0xEF);
        m.write(make_addr(0x40, 3), 0x12);
        // vbd = 0 → vlen = 16, fixed mode.
        vlbp_setup(&mut m, 0x00);
        // $230C returns 0xAB and advances 16 bits.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xAB));
        // $230D after the advance reads the new window:
        // bytes [0xEF, 0x12] → returns high byte 0x12. Actually we
        // re-read fresh because $230D is the high half of the SAME
        // 16-bit value at the *current* bit cursor. After fixed-mode
        // advance, cursor = 16, so $230D returns hi byte of bytes
        // [2..4] = 0x12.
        assert_eq!(m.read(make_addr(0x00, 0x230D)), Some(0x12));
    }

    #[test]
    fn vlbp_variable_mode_advances_on_high_byte_read() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0), 0xAB);
        m.write(make_addr(0x40, 1), 0xCD);
        // vbd = 0x88 → vlen = 8, variable mode.
        vlbp_setup(&mut m, 0x88);
        // Reading $230C repeatedly returns the same byte (no advance).
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xAB));
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xAB));
        // Reading $230D advances.
        assert_eq!(m.read(make_addr(0x00, 0x230D)), Some(0x00));
        // Now $230C returns the next byte.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0xCD));
    }

    #[test]
    fn vlbp_high_address_byte_write_resets_the_bit_cursor() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0), 0x55);
        m.write(make_addr(0x40, 1), 0xAA);
        vlbp_setup(&mut m, 0x04);
        // Consume 3 nibbles.
        let _ = m.read(make_addr(0x00, 0x230C));
        let _ = m.read(make_addr(0x00, 0x230C));
        let _ = m.read(make_addr(0x00, 0x230C));
        // Re-write VDA-high to reset.
        m.write(make_addr(0x00, 0x225B), 0x40);
        // First nibble of byte 0 again.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), Some(0x05));
    }

    // ------------- Phase-5 write-protection tests -------------

    #[test]
    fn sbwe_clear_blocks_main_bwram_writes() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Disable main-CPU BW-RAM writes.
        m.write(make_addr(0x00, 0x2226), 0x00);
        m.write(make_addr(0x40, 0x0000), 0xAA);
        assert_eq!(
            m.read(make_addr(0x40, 0x0000)),
            Some(0x00),
            "write was supposed to be blocked"
        );
        // Re-enable and confirm.
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0000), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0xAA));
    }

    #[test]
    fn bwpa_protects_first_n_pages_of_bwram_from_main() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Protect the first 256 bytes from main.
        m.write(make_addr(0x00, 0x2228), 0x01);
        m.write(make_addr(0x40, 0x0000), 0xAA);
        m.write(make_addr(0x40, 0x0100), 0xBB);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 0x0100)), Some(0xBB));
    }

    #[test]
    fn cbwe_protection_is_independent_of_sbwe() {
        // BW-RAM writes from the SA-1 side are gated by CBWE only;
        // BWPA never applies, and SBWE has no effect.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x00); // SBWE disabled
        m.write(make_addr(0x00, 0x2228), 0x0F); // BWPA = 15 (3840 B)
        // CBWE disabled → SA-1 side write fails.
        m.write(make_addr(0x00, 0x2227), 0x00);
        assert!(
            !m.write_from_sa1(make_addr(0x40, 0x0010), 0xAA) || {
                // The function returns true (claimed) even on a no-op
                // protected write — re-check the actual byte.
                m.read(make_addr(0x40, 0x0010)) == Some(0x00)
            }
        );
        // CBWE enabled → SA-1 side write lands even inside main's
        // protected window.
        m.write(make_addr(0x00, 0x2227), 0x80);
        m.write_from_sa1(make_addr(0x40, 0x0010), 0xCC);
        assert_eq!(m.read(make_addr(0x40, 0x0010)), Some(0xCC));
    }

    #[test]
    fn siwp_page_mask_protects_iram_from_main() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Allow only pages 1 and 3 of I-RAM (2nd + 4th 256-byte pages).
        m.write(make_addr(0x00, 0x2229), 0b0000_1010);
        // Page 0 = $3000-$30FF → blocked.
        m.write(make_addr(0x00, 0x3000), 0xAA);
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0x00));
        // Page 1 = $3100-$31FF → allowed.
        m.write(make_addr(0x00, 0x3100), 0xBB);
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0xBB));
        // Page 2 = $3200-$32FF → blocked.
        m.write(make_addr(0x00, 0x3200), 0xCC);
        assert_eq!(m.read(make_addr(0x00, 0x3200)), Some(0x00));
        // Page 3 = $3300-$33FF → allowed.
        m.write(make_addr(0x00, 0x3300), 0xDD);
        assert_eq!(m.read(make_addr(0x00, 0x3300)), Some(0xDD));
    }

    #[test]
    fn ciwp_protection_only_applies_to_sa1_writes() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // Block all pages from SA-1 side.
        m.write(make_addr(0x00, 0x222A), 0x00);
        // Main side still writes fine.
        m.write(make_addr(0x00, 0x3000), 0xAA);
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0xAA));
        // SA-1 side write is dropped.
        m.write_from_sa1(make_addr(0x00, 0x3100), 0xBB);
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0x00));
    }

    #[test]
    fn sa1_vector_override_reads_crv_cnv_civ_at_bank0_ffex() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        // CRV $1234, CNV $5678, CIV $9ABC.
        m.write(make_addr(0x00, 0x2203), 0x34);
        m.write(make_addr(0x00, 0x2204), 0x12);
        m.write(make_addr(0x00, 0x2205), 0x78);
        m.write(make_addr(0x00, 0x2206), 0x56);
        m.write(make_addr(0x00, 0x2207), 0xBC);
        m.write(make_addr(0x00, 0x2208), 0x9A);
        assert_eq!(m.sa1_vector_override(0, 0xFFFC), Some(0x34));
        assert_eq!(m.sa1_vector_override(0, 0xFFFD), Some(0x12));
        assert_eq!(m.sa1_vector_override(0, 0xFFEA), Some(0x78));
        assert_eq!(m.sa1_vector_override(0, 0xFFEB), Some(0x56));
        assert_eq!(m.sa1_vector_override(0, 0xFFEE), Some(0xBC));
        assert_eq!(m.sa1_vector_override(0, 0xFFEF), Some(0x9A));
    }
}
