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
//! by the `LoROM` mapper, scaled by the super-bank offset (`bank << 20`
//! into ROM).
//!
//! BW-RAM (up to 256 KB) appears as the cart's SRAM window at
//! `$00-$3F:$6000-$7FFF` (an 8 KB sliding window selected by
//! `$2224 BMAPS`) and linearly at `$40-$4F:$0000-$FFFF` (the
//! contiguous 256 KB view). I-RAM (2 KB, shared with the SA-1 CPU)
//! appears at `$00-$3F:$3000-$37FF`.

use crate::mapper::{Mapper, MapperKind, MapperStateError, check_state_len, decode_state};
use crate::types::{Addr24, bank_of, offset_of};

/// Up-to-256 KB SA-1 BW-RAM.
const BWRAM_SIZE: usize = 0x40000;
/// 2 KB SA-1 / main-CPU shared I-RAM.
const IRAM_SIZE: usize = 0x800;
/// SA-1 MMIO byte range — we memory-back the whole window.
const MMIO_SIZE: usize = 0x200;

/// SA-1 cartridge mapper (Mode 23).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Sa1Mapper {
    /// Cartridge ROM image — NOT part of the save-state (restored from the
    /// live cart). `serde(skip)` defaults it to an empty `Vec` on decode;
    /// [`Sa1Mapper::load_state`] re-attaches the live ROM afterwards.
    #[serde(skip)]
    rom: Vec<u8>,
    bwram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    iram: [u8; IRAM_SIZE],
    /// Memory-backed I/O register file at `$2200-$23FF`. Specific
    /// registers (banking + multiplier) have first-class semantics
    /// below; everything else lands here as a generic write/read.
    #[serde(with = "serde_bytes")]
    mmio: [u8; MMIO_SIZE],
    /// $2220 CXB super-bank selector for `$00-$1F` / `$80-$9F`.
    cxb: u8,
    /// $2221 DXB super-bank selector for `$20-$3F` / `$A0-$BF`.
    dxb: u8,
    /// $2222 EXB super-bank selector for `$40-$5F`.
    exb: u8,
    /// $2223 FXB super-bank selector for `$60-$7D`.
    fxb: u8,
    /// $2224 BMAPS (SBM) — BW-RAM 8 KB window select for the `$6000-$7FFF`
    /// window as seen by the **main CPU**.
    bmaps: u8,
    /// `$2225 BMAP (CBM)` — BW-RAM 8 KB window select for the `$6000-$7FFF`
    /// window as seen by the **SA-1**. Bit 7 (`sw46`) = the window is a
    /// bitmap projection (ares `bwram.cpp:45-67`): bits 0-6 pick one of
    /// 128 × 8 KB pixel pages; with bit 7 clear bits 0-4 pick one of 32 ×
    /// 8 KB linear pages. Previously dropped — the SA-1 then wrongly read
    /// the main CPU's SBM bank, livelocking SMRPG's attract-demo cmd-$11
    /// routine.
    #[serde(default)]
    cbm: u8,
    /// `$223F BBF` bit 7 — bitmap pixel format for the SA-1's bitmap
    /// projections (`$60-$6F` and the `sw46` window): `false` = 4 bpp
    /// (two pixels a byte), `true` = 2 bpp (four pixels a byte).
    #[serde(default)]
    bbf: bool,
    /// `$2302-$2305` HCR / VCR — the timer counters as latched by the
    /// `$2302` read (ares `io.cpp:39-50`), in dots.
    #[serde(default)]
    hcr: u16,
    #[serde(default)]
    vcr: u16,
    /// SA-1 steps a normal DMA cost since the chip driver last drained
    /// them (ares `dma.cpp:2-46` charges `step()`s per byte; the batched
    /// driver charges them to its budget in one go).
    #[serde(default)]
    dma_steps: u32,
    /// The S-CPU's last bus address for the current SA-1 batch (ares
    /// `cpu.r.mar`), for a DMA's `conflict()` steps when the SA-1 side
    /// triggers it.
    #[serde(default)]
    scpu_mar: u32,
    /// Multiplier / divider operands and result.
    /// `$2251/$2252 MA` — multiplicand (signed 16-bit, write-twice).
    ma: i16,
    /// `$2253/$2254 MB` — multiplier (signed 16-bit, write-twice).
    /// Writing the high byte triggers the operation per `mcnt`.
    mb: i16,
    /// `$2250 MCNT` — operation select (bit 0 = arithmetic mode:
    /// 0 = multiply, 1 = divide; bit 1 = accumulator mode).
    mcnt: u8,
    /// `$2306-$230A` — 32-bit multiply / quotient+remainder result, or a
    /// 40-bit cumulative-sum accumulator (sigma mode).
    mr: i64,
    /// `$230B` (OF) — sigma-mode overflow flag (set when the 40-bit
    /// accumulator overflows; ares `io.cpp:420`).
    overflow: bool,

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
    /// Live CCNT bit 7 (ares `io.sa1_irq`, Mesen2 `Sa1IrqRequested`): the
    /// S-CPU → SA-1 IRQ *request level*. Both references drop the request
    /// when CCNT is rewritten with bit 7 clear, while the CFR flag
    /// ([`Self::main_irq_to_sa1`]) stays latched until CIC.
    #[serde(default)]
    ccnt_irq_level: bool,
    /// CCNT bit 6 (`sa1_rdyb`): the S-CPU is holding the SA-1 parked. Its
    /// timer keeps running; only instruction execution stops.
    #[serde(default)]
    sa1_wait: bool,
    /// One-shot S-CPU → SA-1 NMI delivery event, consumed by the SA-1 CPU
    /// driver ([`Self::take_sa1_nmi_event`]). Armed when CCNT bit 4 is
    /// written with the NMI enabled, or when CIE enables the NMI while
    /// its flag is pending (ares `io.cpp` `$2200` / `$220A` `sa1_nmicl =
    /// 0`; Mesen2 `ProcessInterrupts` → `SetNmiFlag`). The 65c816 NMI is
    /// edge-triggered, so a level here would re-fire it forever.
    #[serde(default)]
    sa1_nmi_event: bool,
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

    // ---- SA-1 timer ($2210-$2215) — faithful ares port ----
    /// `$2210 TMC` — timer control. bit 7 = mode (0 = HV timer, 1 =
    /// linear timer), bit 1 = V enable, bit 0 = H enable.
    tmc: u8,
    /// `$2212/$2213 HCNT` — H compare (write, in dots).
    hcnt_lo: u8,
    hcnt_hi: u8,
    /// `$2214/$2215 VCNT` — V compare (write, in dots).
    vcnt_lo: u8,
    vcnt_hi: u8,
    /// Internal H/V timer counters, in CLOCKS (4 clocks = 1 dot), per ares
    /// `SA1::status`. HV mode wraps H at 1364 and V at `scanlines`; linear
    /// mode is an 11-bit H + 9-bit V free-runner. Read back as dots at
    /// `$2302-$2305` (HCR/VCR).
    hcounter: u16,
    vcounter: u16,
    /// Active scanline count (262 NTSC / 312 PAL) for the HV-mode V wrap.
    scanlines: u16,
    /// Leftover odd master clock between `tick_timer` calls so the timer
    /// advances in ares' exact 2-clock steps regardless of batch size.
    timer_rem: u32,

    // ---- Phase-3 DMA ($2230-$2239) ----
    /// `$2230 DCNT` — DMA control byte (bit 7 = enable, bit 6 =
    /// priority, bit 5 = CC enable, bit 4 = CC type select, bit 2 =
    /// destination device, bits 1..0 = source device).
    ///
    /// Per ares + Mesen2, DCNT writes only *configure* the DMA — the
    /// actual trigger is on the final DDA byte write ($2236 for I-RAM
    /// destinations + CC1, $2237 for BW-RAM destinations) or on the
    /// trailing BRF write ($2247 / $224F) for CC2.
    dcnt: u8,
    /// DCNT bit 7 — DMA enable. Carried in `dcnt` too, broken out
    /// here for fast checking in the DDA-write trigger path.
    dma_en: bool,
    /// DCNT bit 5 — character-conversion enable.
    dma_cden: bool,
    /// DCNT bit 4 — CC type select. `false` = Type-2 streaming
    /// (CC2), `true` = Type-1 one-shot (CC1) per ares' `io.cpp`.
    dma_cdsel: bool,
    /// DCNT bit 2 — destination device. `false` = I-RAM, `true` =
    /// BW-RAM. Selects which DDA byte fires normal-mode DMA.
    dma_dd: bool,
    /// `$2231 CDMA` — character-conversion parameters (colour depth +
    /// tile width). Stored for the Type-1 path; the normal-DMA fast
    /// path ignores it.
    cdma: u8,
    /// CDMA bits 0-1 (`dmacb` in ares `io.cpp:454`, clamped to 2): colour
    /// depth of the character-conversion source — 0 = 8bpp, 1 = 4bpp,
    /// 2 = 2bpp.
    #[serde(default)]
    dmacb: u8,
    /// CDMA bits 2-4 (`dmasize`, clamped to 5): the virtual bitmap is
    /// `1 << dmasize` characters wide.
    #[serde(default)]
    dmasize: u8,
    /// ares `bwram.dma`: a Type-1 conversion is armed, so S-CPU reads of
    /// BW-RAM are served by [`Sa1Mapper::dma_cc1_read`] instead of the
    /// memory itself. Set by the `$2236` trigger, cleared by CDMA bit 7.
    #[serde(default)]
    bwram_dma: bool,
    /// `$2240-$224F` BRF, the Type-2 register file the SA-1 streams pixels
    /// through (ares `io.brf`).
    #[serde(default)]
    brf: [u8; 16],
    /// Type-2 line counter, 4 bits (ares `dma.line`): which of the tile's
    /// 16 rows the next BRF half feeds. Reset when DCNT clears DMA enable.
    #[serde(default)]
    cc2_line: u8,
    /// `$2232-$2234` SDA — 24-bit source address.
    sda: u32,
    /// `$2235-$2237` DDA — 24-bit destination address.
    dda: u32,
    /// `$2238/$2239` DTC — 16-bit transfer byte counter.
    dtc: u16,

    // ---- VLBP (variable-length bit processing), ares `io.cpp:427-444` ----
    /// `$2258 VBD` — bit 7 (`hl`): 0 = fixed mode, the cursor advances
    /// by `vb` bits on the **`$2258` write** itself; 1 = auto-increment
    /// mode, it advances on the **`$230D` read**. Bits 3..0 = `vb`, the
    /// length 1..15 (0 means 16).
    vbd: u8,
    /// `$2259-$225B VDA` (`va`) — the byte the 24-bit window starts at;
    /// advanced by whole bytes as the cursor moves. Writing `$225B`
    /// zeroes `vbit`.
    va: u32,
    /// Bit position 0..7 of the cursor inside `va` (`io.vbit`).
    vbit: u8,

    // ---- Phase-5 memory write protection ----
    /// `$2226 SBWE` — S-CPU BW-RAM write-enable (bit 7). Both enables
    /// come up **clear** (ares `sa1.cpp:230-234`, Mesen2 `Reset`).
    sbwe: u8,
    /// `$2227 CBWE` — SA-1 BW-RAM write-enable (bit 7).
    cbwe: u8,
    /// `$2228 BWPA` — BW-RAM write-protected-area size: while **both**
    /// enables are clear, the first `256 << (bwpa & 0x0F)` bytes refuse
    /// writes from either side. Comes up `$0F` (ares `io.bwp = 0x0f`,
    /// Mesen2 `CpuRegisterWrite(0x2228, 0xFF)`): all of BW-RAM is
    /// write-protected until a game enables one side.
    bwpa: u8,
    /// `$222A SIWP` — main-CPU I-RAM page write-enable mask. Each of
    /// the 8 bits gates one 256-byte page of the 2 KB I-RAM; bit
    /// set = writable, clear = protected.
    siwp: u8,
    /// `$222B CIWP` — SA-1 I-RAM page write-enable mask (same shape
    /// as SIWP).
    ciwp: u8,
}

/// Where an SA-1-side BW-RAM access lands (ares `bwram.cpp`): a linear
/// byte, or one pixel of the bitmap projection.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum BwTarget {
    Linear(usize),
    Bitmap(u32),
}

/// Which CPU side is performing an access — the two sides see different
/// registers and different BW-RAM views, and check different protection.
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
/// BRF index (0-15) of a `$2240-$224F` register address.
const fn value_reg_index(absolute: u16) -> u8 {
    (absolute - 0x2240) as u8
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
            cbm: 0x00,
            ma: 0,
            mb: 0,
            mcnt: 0,
            mr: 0,
            overflow: false,
            sie: 0,
            cie: 0,
            s_irq_to_main: false,
            s_nmi_to_main: false,
            cc1_irq_to_main: false,
            main_irq_to_sa1: false,
            main_nmi_to_sa1: false,
            timer_irq_to_sa1: false,
            dma_irq_to_sa1: false,
            ccnt_irq_level: false,
            sa1_wait: false,
            sa1_nmi_event: false,
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
            hcounter: 0,
            vcounter: 0,
            scanlines: 262,
            timer_rem: 0,
            dcnt: 0,
            dma_en: false,
            dma_cden: false,
            dma_cdsel: false,
            dma_dd: false,
            cdma: 0,
            dmacb: 0,
            dmasize: 0,
            bwram_dma: false,
            brf: [0; 16],
            cc2_line: 0,
            sda: 0,
            dda: 0,
            dtc: 0,
            vbd: 0,
            va: 0,
            vbit: 0,
            bbf: false,
            hcr: 0,
            vcr: 0,
            dma_steps: 0,
            scpu_mar: 0,
            // Deliberate deviation from ares (`coprocessor/sa1/sa1.cpp:239
            // → io.siwp = 0; io.cpp:112-113 → io.ciwp = 0`) and Mesen2
            // (`Sa1Types.h::CpuIRamWriteProtect/Sa1IRamWriteProtect`
            // value-init to 0, plus Sa1::CpuRegisterWrite case $2200
            // reset path setting `Sa1IRamWriteProtect = 0`): both
            // reference emulators reset CIWP/SIWP to `0x00` (block-all).
            //
            // luna starts from "allow all" because home-brew SA-1 carts
            // routinely write only CIWP at init and leave SIWP untouched,
            // expecting an open default. opensnes' sa1_starfield demo is
            // one such (sa1_boot.asm only stores $FF into $222A, never
            // touches $2229) — switching to the reference 0x00 default
            // makes the main CPU's IRAM seed silently drop and the
            // screen go black in luna-gui. The 0xFF default keeps those
            // carts working at the cost of one reset-time bit-pattern
            // mismatch that no test ROM yet observes.
            // BW-RAM protection powers up ARMED (ares `sa1.cpp:230-237`,
            // Mesen2 `Sa1::Reset`): both write enables clear and BWPA =
            // $0F, so every byte refuses writes until the game sets SBWE
            // or CBWE — which every title does before touching BW-RAM.
            sbwe: 0x00,
            cbwe: 0x00,
            bwpa: 0x0F,
            siwp: 0xFF,
            ciwp: 0xFF,
        }
    }

    /// Re-power the SA-1 MMIO / register state on a system reset (ares
    /// `SA1::power()`): every `$2200-$23FF` register, the IRAM and the
    /// math/DMA/character-conversion state return to power-on, including
    /// the CCNT.5 reset bit so the SA-1 boots held in reset again. ROM
    /// and battery-backed BW-RAM persist (ares clears IRAM but not
    /// BW-RAM). Implemented by rebuilding from [`Sa1Mapper::new`] — the
    /// single source of truth for the power-on layout — then restoring
    /// the ROM and BW-RAM buffers.
    pub fn power_reset(&mut self) {
        let rom = std::mem::take(&mut self.rom);
        let bwram = std::mem::take(&mut self.bwram);
        // `new` re-derives the clamped BW-RAM size from `bwram.len()`,
        // so the restored buffer length matches exactly.
        *self = Self::new(rom, bwram.len());
        self.bwram = bwram;
    }

    const fn iram_writable_for(&self, byte_off: usize, side: WriteSide) -> bool {
        let mask = match side {
            WriteSide::Main => self.siwp,
            WriteSide::Sa1 => self.ciwp,
        };
        let page = (byte_off / 256) & 7;
        (mask >> page) & 1 != 0
    }

    fn bwram_writable_for(&self, byte_off: usize) -> bool {
        // Per ares (`coprocessor/sa1/bwram.cpp:40-43, 73-84`) and
        // Mesen2 (`CpuBwRamHandler.h:45-57`, `Sa1BwRamHandler.h:41-50`):
        // BWRAM writes are gated by the **OR** of SBWE and CBWE bit 7,
        // not each side independently. If EITHER enable is set, the
        // write goes through from BOTH sides. Only when both are
        // cleared does BWPA's protected first-region check kick in,
        // and even then writes OUTSIDE the protected area still
        // succeed. ares quotes Kirby's Dream Land 3 as the witness
        // game: `BWPA=$02, SWEN=$80, CWEN=$00`, SA-1 writes to
        // `$4001Ax`/`$40032x` must succeed.
        //
        // luna previously treated SBWE/CBWE as per-side hard gates;
        // SMRPG (which never writes SBWE/CBWE during init, trusting
        // the real-hardware default) had every main-CPU BWRAM write
        // silently dropped, deadlocking the main↔SA-1 mailbox at
        // `$40:3D00`. The reset-time backing-mmio default of `$00`
        // also made the broken-gate case the default, so the bug
        // affected most SA-1 carts.
        //
        // BWPA size formula `0x100 << min(bwp, 10)` matches Mesen2's
        // clamp (max 256 KiB protection); ares uses the raw 4-bit
        // value but no real cart sets `bwp > 10` so both behave the
        // same in practice. The gate is symmetric — one check for
        // both sides.
        if (self.sbwe & 0x80) != 0 || (self.cbwe & 0x80) != 0 {
            return true;
        }
        let bwp = (self.bwpa & 0x0F).min(0x0A);
        let prot_bytes = 0x100usize << bwp;
        byte_off >= prot_bytes
    }

    /// `vb`: the VLBP length in bits, 1..=16 (VBD bits 0-3, 0 = 16).
    const fn vlbp_vb(&self) -> u32 {
        let n = (self.vbd & 0x0F) as u32;
        if n == 0 { 16 } else { n }
    }

    /// Move the VLBP cursor on by `vb` bits (ares `io.cpp:434-437`,
    /// `io.cpp:83-86`): whole bytes go into `va`, the rest stays in `vbit`.
    fn vlbp_advance(&mut self) {
        let vbit = u32::from(self.vbit) + self.vlbp_vb();
        self.va = self.va.wrapping_add(vbit >> 3) & 0x00FF_FFFF;
        self.vbit = (vbit & 7) as u8;
    }

    /// The 24-bit window at `va`, shifted down to the cursor (ares
    /// `io.cpp:63-68`): `$230C` returns its low byte, `$230D` its next.
    /// Unmasked — the reader takes as many bits as it wants.
    fn vlbp_window(&self) -> u32 {
        let b = |i: u32| u32::from(self.read_vbr(self.va.wrapping_add(i) & 0x00FF_FFFF));
        (b(0) | (b(1) << 8) | (b(2) << 16)) >> self.vbit
    }

    /// The VLBP's own bus (ares `memory.cpp:113-133`): ROM through the
    /// SA-1's mapping, BW-RAM and I-RAM raw — never the I/O registers,
    /// so a VDA inside `$2200-$23FF` reads `$FF` instead of a port.
    fn read_vbr(&self, address: u32) -> u8 {
        let address = address & 0x00FF_FFFF;
        if address & 0x40_8000 == 0x00_8000 || address & 0xC0_0000 == 0xC0_0000 {
            return self.rom_read_sa1(address);
        }
        if address & 0x40_E000 == 0x00_6000 || address & 0xF0_0000 == 0x40_0000 {
            return self.bwram_raw(address);
        }
        if address & 0x40_F800 == 0x00_0000 || address & 0x40_F800 == 0x00_3000 {
            return self.iram[(address & 0x7FF) as usize];
        }
        0xFF
    }

    /// BW-RAM by raw 24-bit address, mirrored into the array (ares
    /// `BWRAM::read`, `bus.mirror(address, size())`) — what the DMA
    /// engine and the VLBP use: no window, no bank register.
    fn bwram_raw(&self, address: u32) -> u8 {
        if self.bwram.is_empty() {
            return 0xFF;
        }
        self.bwram[(address & 0x00FF_FFFF) as usize % self.bwram.len()]
    }

    fn bwram_raw_write(&mut self, address: u32, value: u8) {
        if self.bwram.is_empty() {
            return;
        }
        let len = self.bwram.len();
        self.bwram[(address & 0x00FF_FFFF) as usize % len] = value;
    }

    /// ROM as the SA-1 side addresses it (ares `rom.cpp:61-66` →
    /// `readCPU`): a `$00-$3F / $80-$BF:8000-FFFF` address is folded to
    /// its linear position first, then the four super-MMC registers pick
    /// the megabyte — a bank-mode register clear leaves the low banks at
    /// their default megabyte, set redirects them. Any 24-bit address
    /// resolves to ROM (the DMA engine reads its source through this
    /// even when SDA points elsewhere), mirrored into the image.
    fn rom_read_sa1(&self, address: u32) -> u8 {
        if self.rom.is_empty() {
            return 0xFF;
        }
        let mut address = address & 0x00FF_FFFF;
        if address & 0x40_8000 == 0x00_8000 {
            address = (address & 0x80_0000) >> 2 | (address & 0x3F_0000) >> 1 | address & 0x7FFF;
        }
        let lo = address < 0x40_0000;
        let address = address & 0x3F_FFFF;
        let reg = match address >> 20 {
            0 => self.cxb,
            1 => self.dxb,
            2 => self.exb,
            _ => self.fxb,
        };
        let off = if lo && reg & 0x80 == 0 {
            address
        } else {
            (u32::from(reg & 0x07) << 20) | (address & 0x0F_FFFF)
        };
        self.rom[crate::types::rom_mirror(off as usize, self.rom.len())]
    }

    /// S-CPU bus address to charge a DMA's `conflict()` steps against
    /// (ares `cpu.r.mar`): the S-CPU's last access for this batch when
    /// the SA-1 side fires the DMA, the `$2236` / `$2237` write itself
    /// when the S-CPU does — an I/O address, so it never conflicts.
    pub const fn set_scpu_mar(&mut self, mar: u32) {
        self.scpu_mar = mar;
    }

    /// SA-1 steps charged by DMA since the last drain — the chip driver
    /// subtracts them from its budget so the SA-1 CPU stalls for the
    /// transfer, as ares' `step()`s inside `dmaNormal` do.
    pub const fn take_dma_steps(&mut self) -> u32 {
        let n = self.dma_steps;
        self.dma_steps = 0;
        n
    }

    /// `true` while CCNT bit 5 holds the SA-1 in reset (the last value
    /// the S-CPU wrote there).
    #[must_use]
    pub const fn ccnt_reset_held(&self) -> bool {
        self.mmio[0] & 0x20 != 0
    }

    /// CRV (`$2203/$2204`): where the SA-1 starts when the S-CPU releases
    /// it from reset.
    #[must_use]
    pub const fn crv(&self) -> u16 {
        (self.mmio[0x03] as u16) | ((self.mmio[0x04] as u16) << 8)
    }

    /// Normal DMA — a line-for-line port of ares `SA1::dmaNormal`
    /// (`sa1/dma.cpp:2-46`; Mesen2 `Sa1::RunDma`). DCNT names the
    /// devices: source `sd` = ROM (0) / BW-RAM (1) / I-RAM (2),
    /// destination `dd` = I-RAM (0) / BW-RAM (1); only the four
    /// ROM→BW-RAM, ROM→I-RAM, BW-RAM→I-RAM and I-RAM→BW-RAM pairs move
    /// bytes, any other pair just runs the counter down. ROM is read
    /// through the SA-1's mapping, BW-RAM and I-RAM by raw address —
    /// SDA / DDA are offsets into the device, not bus addresses. Each
    /// byte costs the SA-1 its steps (plus `conflict()` steps when the
    /// S-CPU holds the same resource). Completion raises the DMA IRQ
    /// flag; DMA enable stays set, so a game re-fires by rewriting DDA.
    fn run_normal_dma(&mut self, side: WriteSide) {
        const SRC_ROM: u8 = 0;
        const SRC_BWRAM: u8 = 1;
        const SRC_IRAM: u8 = 2;
        let sd = self.dcnt & 0x03;
        let dd_bwram = self.dcnt & 0x04 != 0;
        let mar = match side {
            WriteSide::Main => 0x00_2236,
            WriteSide::Sa1 => self.scpu_mar,
        };
        let rom_c = u32::from(Self::scpu_rom_conflict(mar));
        let bw_c = u32::from(Self::scpu_bwram_conflict(mar));
        let iram_c = u32::from(Self::scpu_iram_conflict(mar));
        while self.dtc != 0 {
            self.dtc -= 1;
            let source = self.sda;
            self.sda = self.sda.wrapping_add(1) & 0x00FF_FFFF;
            let target = self.dda;
            self.dda = self.dda.wrapping_add(1) & 0x00FF_FFFF;
            match (sd, dd_bwram) {
                (SRC_ROM, true) => {
                    self.dma_steps += 2 + bw_c + bw_c;
                    let data = self.rom_read_sa1(source);
                    self.bwram_raw_write(target, data);
                }
                (SRC_ROM, false) => {
                    self.dma_steps += 1 + (iram_c | rom_c) + iram_c;
                    let data = self.rom_read_sa1(source);
                    self.iram[(target & 0x7FF) as usize] = data;
                }
                (SRC_BWRAM, false) => {
                    self.dma_steps += 2 + (bw_c | iram_c) + bw_c;
                    let data = self.bwram_raw(source);
                    self.iram[(target & 0x7FF) as usize] = data;
                }
                (SRC_IRAM, true) => {
                    self.dma_steps += 2 + (bw_c | iram_c) + bw_c;
                    let data = self.iram[(source & 0x7FF) as usize];
                    self.bwram_raw_write(target, data);
                }
                _ => {}
            }
        }
        self.dma_irq_to_sa1 = true;
    }

    /// Arm a Type-1 character conversion (ares `SA1::dmaCC1`): the
    /// conversion itself happens lazily, on the S-CPU's own reads of
    /// BW-RAM, and the chip raises its char-conversion IRQ right away.
    const fn dma_cc1(&mut self) {
        self.bwram_dma = true;
        self.cc1_irq_to_main = true;
    }

    /// Serve one S-CPU BW-RAM read while a Type-1 conversion is armed —
    /// a line-for-line port of ares `SA1::dmaCC1Read` (`sa1/dma.cpp:63`).
    ///
    /// Hardware converts ONE character at a time, when the read crosses
    /// into it, and answers from the I-RAM copy at DDA. luna used to
    /// convert the whole transfer up front at the `$2236` write and leave
    /// BW-RAM reads untouched, which is neither the timing nor the data
    /// path the hardware has.
    fn dma_cc1_read(&mut self, address: u32) -> u8 {
        // 16 bytes/char (2bpp), 32 (4bpp), 64 (8bpp).
        let charmask: u32 = (1 << (6 - u32::from(self.dmacb))) - 1;
        if address & charmask == 0 && !self.bwram.is_empty() {
            let bpp: u32 = 2 << (2 - u32::from(self.dmacb));
            let bpl: u32 = (8 << u32::from(self.dmasize)) >> u32::from(self.dmacb);
            let bwmask = (self.bwram.len() as u32).saturating_sub(1);
            let tile = (address.wrapping_sub(self.sda) & bwmask) >> (6 - u32::from(self.dmacb));
            let ty = tile >> u32::from(self.dmasize);
            let tx = tile & ((1 << u32::from(self.dmasize)) - 1);
            let mut bwaddr = self.sda.wrapping_add(ty * 8 * bpl).wrapping_add(tx * bpp);

            for y in 0..8u32 {
                let mut data: u64 = 0;
                for byte in 0..bpp {
                    let a = (bwaddr.wrapping_add(byte) & bwmask) as usize;
                    data |= u64::from(self.bwram[a % self.bwram.len()]) << (byte << 3);
                }
                bwaddr = bwaddr.wrapping_add(bpl);

                // Planar rows, LSB of each pixel first — the bit order the
                // hardware shifts out (luna used to read pixels MSB-first).
                let mut out = [0u8; 8];
                for x in 0..8u32 {
                    out[0] |= ((data & 1) as u8) << (7 - x);
                    data >>= 1;
                    out[1] |= ((data & 1) as u8) << (7 - x);
                    data >>= 1;
                    if self.dmacb == 2 {
                        continue;
                    }
                    out[2] |= ((data & 1) as u8) << (7 - x);
                    data >>= 1;
                    out[3] |= ((data & 1) as u8) << (7 - x);
                    data >>= 1;
                    if self.dmacb == 1 {
                        continue;
                    }
                    for slot in out.iter_mut().skip(4) {
                        *slot |= ((data & 1) as u8) << (7 - x);
                        data >>= 1;
                    }
                }

                for byte in 0..bpp {
                    // `((byte & 6) << 3) + (byte & 1)` maps a byte index
                    // 0..7 onto the planar layout {0,1,16,17,32,33,48,49}.
                    let p = self
                        .dda
                        .wrapping_add(y << 1)
                        .wrapping_add((byte & 6) << 3)
                        .wrapping_add(byte & 1);
                    self.iram[(p & 0x07FF) as usize] = out[byte as usize];
                }
            }
        }
        let idx = self.dda.wrapping_add(address & charmask) & 0x07FF;
        self.iram[idx as usize]
    }

    /// Type-2 character conversion (ares `SA1::dmaCC2`, `sa1/dma.cpp:108`):
    /// the SA-1 streams 8 source pixels through the BRF register file, and
    /// each completed half converts one tile row into I-RAM at DDA.
    ///
    /// luna previously treated the `$223F` / `$2247` / `$224F` bytes as
    /// packed pixel data and emitted a whole tile at a time, ignoring the
    /// register file and the 4-bit line counter entirely.
    fn dma_cc2(&mut self) {
        let base = usize::from(self.cc2_line & 1) << 3;
        let brf = &self.brf[base..base + 8];
        let bpp: u32 = 2 << (2 - u32::from(self.dmacb));
        let mut address = self.dda & 0x07FF;
        address &= !((1u32 << (7 - u32::from(self.dmacb))) - 1);
        address += u32::from(self.cc2_line & 8) * bpp;
        address += u32::from(self.cc2_line & 7) * 2;

        for byte in 0..bpp {
            let mut output = 0u8;
            for bit in 0..8u32 {
                output |= ((brf[bit as usize] >> byte) & 1) << (7 - bit);
            }
            let p = address.wrapping_add((byte & 6) << 3).wrapping_add(byte & 1);
            self.iram[(p & 0x07FF) as usize] = output;
        }

        self.cc2_line = (self.cc2_line + 1) & 15;
    }

    /// HCNT/VCNT compare values (9-bit, in dots) from their lo/hi pairs.
    fn timer_compare(&self) -> (u16, u16) {
        let hcnt = ((u16::from(self.hcnt_hi) << 8) | u16::from(self.hcnt_lo)) & 0x01FF;
        let vcnt = ((u16::from(self.vcnt_hi) << 8) | u16::from(self.vcnt_lo)) & 0x01FF;
        (hcnt, vcnt)
    }

    /// Advance the SA-1 timer by `ticks` master clocks, in ares' exact
    /// 2-clock steps (`SA1::step`). Both HV and linear modes keep their own
    /// H/V counters; only the advance differs, the IRQ compare is the same
    /// `hen | ven<<1` switch in either mode. `timer_rem` carries any odd
    /// clock between calls so the cadence is independent of batch size.
    pub fn tick_timer(&mut self, ticks: u32) {
        self.timer_rem += ticks;
        while self.timer_rem >= 2 {
            self.timer_rem -= 2;
            self.timer_step2();
        }
    }

    /// One ares timer step (+2 master clocks): advance the counters per the
    /// selected mode, then test the H/V compare for the timer IRQ.
    fn timer_step2(&mut self) {
        if self.tmc & 0x80 == 0 {
            // HV timer — H wraps each scanline (1364 clocks), V each frame.
            self.hcounter += 2;
            if self.hcounter >= 1364 {
                self.hcounter = 0;
                self.vcounter += 1;
                if self.vcounter >= self.scanlines {
                    self.vcounter = 0;
                }
            }
        } else {
            // Linear timer — an 11-bit H feeding a 9-bit V.
            self.hcounter += 2;
            self.vcounter = self.vcounter.wrapping_add(self.hcounter >> 11);
            self.hcounter &= 0x07FF;
            self.vcounter &= 0x01FF;
        }
        // Timer IRQ compare. Mode = hen | ven<<1 = TMC[1:0]. The flag stays
        // set (level) until the SA-1 clears it via $220B; the bare `==` fires
        // once per H period (line) / V period (frame), like ares.
        let (hcnt, vcnt) = self.timer_compare();
        match self.tmc & 0x03 {
            1 if self.hcounter == hcnt << 2 => self.timer_irq_to_sa1 = true,
            2 if self.vcounter == vcnt && self.hcounter == 0 => self.timer_irq_to_sa1 = true,
            3 if self.vcounter == vcnt && self.hcounter == hcnt << 2 => {
                self.timer_irq_to_sa1 = true;
            }
            _ => {}
        }
    }

    /// `true` while the SA-1 is asserting an IRQ line onto the main
    /// CPU. The bus ORs this into the main CPU's `irq_pending` so the
    /// CPU services it through its normal IRQ path.
    #[must_use]
    pub const fn main_irq_line(&self) -> bool {
        // SIE layout (per ares + Mesen2):
        //   bit 7 = SA-1 → S-CPU IRQ enable
        //   bit 5 = CC IRQ (Type-1 char-conv) enable
        (self.s_irq_to_main && (self.sie & 0x80) != 0)
            || (self.cc1_irq_to_main && (self.sie & 0x20) != 0)
    }

    /// `true` while the SA-1 is taking an IRQ from any of the three
    /// enabled sources (S-CPU IRQ, timer, DMA).
    #[must_use]
    pub const fn sa1_irq_line(&self) -> bool {
        // CIE layout (per ares + Mesen2):
        //   bit 7 = S-CPU → SA-1 IRQ enable
        //   bit 6 = timer IRQ enable
        //   bit 5 = DMA IRQ enable
        //   bit 4 = S-CPU → SA-1 NMI enable
        (self.main_irq_to_sa1 && self.ccnt_irq_level && (self.cie & 0x80) != 0)
            || (self.timer_irq_to_sa1 && (self.cie & 0x40) != 0)
            || (self.dma_irq_to_sa1 && (self.cie & 0x20) != 0)
    }

    /// `true` while CCNT bit 6 (RDYB) holds the SA-1 parked.
    #[must_use]
    pub const fn sa1_waiting(&self) -> bool {
        self.sa1_wait
    }

    /// Consume the pending S-CPU → SA-1 NMI delivery event (see
    /// [`Self::sa1_nmi_event`]). The SA-1 CPU driver calls this once per
    /// instruction boundary and latches an NMI on `true`.
    pub const fn take_sa1_nmi_event(&mut self) -> bool {
        let fired = self.sa1_nmi_event;
        self.sa1_nmi_event = false;
        fired
    }

    /// `true` while the S-CPU has raised an NMI to the SA-1 and the
    /// SA-1's enable mask permits it.
    #[must_use]
    pub const fn sa1_nmi_line(&self) -> bool {
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
    const fn main_vector_override(&self, bank: u8, offset: u16) -> Option<u8> {
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
    pub const fn sa1_vector_override(&self, bank: u8, offset: u16) -> Option<u8> {
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
    const fn read_sfr(&self) -> u8 {
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
    const fn read_cfr(&self) -> u8 {
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
    /// * Cleared → `LoROM` half-bank windows use the *default* MB
    ///   slot for that bank (Banks[0]→MB0, [1]→MB1, [2]→MB2, [3]→MB3).
    /// * Set → `LoROM` half-bank windows use the MB selected by bits
    ///   0..2 of the bank register.
    ///
    /// The `HiROM` full-bank windows (`$C0+`) always use the banked MB
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
        // Mirror, don't fall off the end: ares `SA1::ROM::read` runs every
        // address through `bus.mirror(address, size())`, so a cart smaller
        // than the 4 MB the super-MMC banks address repeats instead of
        // reading open bus. With the default EXB = 2 / FXB = 3, a 1-2 MB
        // SA-1 cart addresses real ROM through `$80-$BF`.
        if self.rom.is_empty() {
            None
        } else {
            Some(crate::types::rom_mirror(off, self.rom.len()))
        }
    }

    /// I-RAM access from the **main** CPU's view: 2 KB at
    /// `$3000-$37FF` of banks `$00-$3F` and `$80-$BF`.
    fn iram_offset(bank: u8, offset: u16) -> Option<usize> {
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
    fn iram_offset_sa1(bank: u8, offset: u16) -> Option<usize> {
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

    /// BW-RAM as the **S-CPU** sees it (ares `bwram.cpp:22-43`), gated
    /// by the cart having declared SRAM:
    ///   * the 8 KB window at `$00-$3F / $80-$BF:6000-7FFF`, page
    ///     `SBM` (`$2224`);
    ///   * the linear view at `$40-$4F:0000-FFFF`.
    fn bwram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
        if self.bwram.is_empty() {
            return None;
        }
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x6000..=0x7FFF).contains(&offset) {
            let off = usize::from(self.bmaps & 0x1F) * 0x2000 + usize::from(offset - 0x6000);
            return Some(off % self.bwram.len());
        }
        if matches!(bank, 0x40..=0x4F) {
            let off = usize::from(bank - 0x40) * 0x1_0000 + usize::from(offset);
            return Some(off % self.bwram.len());
        }
        None
    }

    /// BW-RAM as the **SA-1** sees it (ares `memory.cpp:39-50`,
    /// `bwram.cpp:45-67`): three views.
    ///   * `$40-$5F:0000-FFFF` — linear, mirrored into the array;
    ///   * `$60-$6F:0000-FFFF` — the bitmap projection: one pixel per
    ///     address, 4 or 2 bpp per BBF;
    ///   * the `$6000-$7FFF` window, page `CBM` (`$2225`): linear over 32
    ///     pages with bit 7 clear, bitmap over 128 pages with it set.
    fn bwram_target_sa1(&self, bank: u8, offset: u16) -> Option<BwTarget> {
        if self.bwram.is_empty() {
            return None;
        }
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x6000..=0x7FFF).contains(&offset) {
            let a = u32::from(offset & 0x1FFF);
            return Some(if self.cbm & 0x80 == 0 {
                let off = usize::from(self.cbm & 0x1F) * 0x2000 + a as usize;
                BwTarget::Linear(off % self.bwram.len())
            } else {
                BwTarget::Bitmap((u32::from(self.cbm & 0x7F) * 0x2000 + a) & 0x000F_FFFF)
            });
        }
        let full = (u32::from(bank) << 16) | u32::from(offset);
        if matches!(bank, 0x40..=0x5F) {
            return Some(BwTarget::Linear(full as usize % self.bwram.len()));
        }
        if matches!(bank, 0x60..=0x6F) {
            return Some(BwTarget::Bitmap(full & 0x000F_FFFF));
        }
        None
    }

    /// One pixel of the bitmap projection (ares `BWRAM::readBitmap`):
    /// 4 bpp packs two pixels a byte, low nibble first; 2 bpp packs
    /// four, low pair first.
    fn bitmap_read(&self, pixel: u32) -> u8 {
        let len = self.bwram.len();
        if self.bbf {
            let byte = self.bwram[(pixel >> 2) as usize % len];
            (byte >> ((pixel & 3) * 2)) & 0x03
        } else {
            let byte = self.bwram[(pixel >> 1) as usize % len];
            (byte >> ((pixel & 1) * 4)) & 0x0F
        }
    }

    /// Write one pixel of the bitmap projection (ares `BWRAM::writeBitmap`):
    /// a read-modify-write of the byte's other pixels. ares applies no
    /// BWPA protection on this path (Mesen2 does — `Sa1BwRamHandler::
    /// WriteValue`); the two only differ while both write enables are
    /// clear, a state no known title draws in.
    fn bitmap_write(&mut self, pixel: u32, value: u8) {
        let len = self.bwram.len();
        if self.bbf {
            let o = (pixel >> 2) as usize % len;
            let shift = (pixel & 3) * 2;
            self.bwram[o] = (self.bwram[o] & !(0x03 << shift)) | ((value & 0x03) << shift);
        } else {
            let o = (pixel >> 1) as usize % len;
            let shift = (pixel & 1) * 4;
            self.bwram[o] = (self.bwram[o] & !(0x0F << shift)) | ((value & 0x0F) << shift);
        }
    }

    /// Registers the **S-CPU** may write (ares `writeIOCPU`): control,
    /// its own enables and vectors, the super-MMC banks, its BW-RAM /
    /// I-RAM protection, and the shared DMA address block.
    const fn cpu_side_register(absolute: u16) -> bool {
        matches!(
            absolute,
            0x2200..=0x2208 | 0x2220..=0x2224 | 0x2226 | 0x2228 | 0x2229 | 0x2231..=0x2237
        )
    }

    /// Registers the **SA-1** may write (ares `writeIOSA1`): S-CPU
    /// control, its own enables / clears / vectors, the timer, its
    /// BW-RAM / I-RAM protection, DMA, the bitmap file, the math unit
    /// and the VLBP.
    const fn sa1_side_register(absolute: u16) -> bool {
        matches!(
            absolute,
            0x2209..=0x2215
                | 0x2225
                | 0x2227
                | 0x222A
                | 0x2230..=0x2239
                | 0x223F..=0x224F
                | 0x2250..=0x2254
                | 0x2258..=0x225B
        )
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
    /// Run the arithmetic op latched by the `$2254` (MBH) write — a
    /// direct port of ares `io.cpp:398-423`. The accumulate flag
    /// (`acm`, MCNT bit 1) takes precedence over the multiply/divide
    /// select (`md`, bit 0).
    fn update_arith(&mut self) {
        let acm = self.mcnt & 0x02 != 0;
        let md = self.mcnt & 0x01 != 0;
        if acm {
            // Sigma: cumulative MAC into a 40-bit accumulator with an
            // overflow flag (ares io.cpp:419-422).
            let product = i64::from(self.ma) * i64::from(self.mb);
            let acc = (self.mr as u64).wrapping_add(product as u64);
            self.overflow = (acc >> 40) & 1 != 0;
            self.mr = (acc & ((1u64 << 40) - 1)) as i64;
        } else if md {
            // Division: SIGNED dividend ÷ UNSIGNED divisor, floored
            // (non-negative remainder). MA is reset (MB below).
            if self.mb == 0 {
                self.mr = 0;
            } else {
                let dividend = i32::from(self.ma);
                let divisor = i32::from(self.mb as u16);
                let remainder = if dividend >= 0 {
                    dividend % divisor
                } else {
                    (dividend % divisor + divisor) % divisor
                };
                let quotient = (dividend - remainder) / divisor;
                self.mr = (i64::from(remainder as u16) << 16) | i64::from(quotient as u16);
            }
            self.ma = 0;
        } else {
            // Signed 16×16 → unsigned-32-bit multiply.
            let product = i32::from(self.ma) * i32::from(self.mb);
            self.mr = i64::from(product as u32);
        }
        // Every op resets MB (ares io.cpp:402,415,422).
        self.mb = 0;
    }
}

impl Mapper for Sa1Mapper {
    fn kind(&self) -> MapperKind {
        MapperKind::Sa1
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        // The S-CPU's register window (ares `readIOCPU`): only SFR reads
        // back; every other address — including the SA-1's own CFR, the
        // counters, the math result and the VLBP ports — is open bus.
        if let Some(idx) = Self::mmio_offset(addr) {
            return match 0x2200 + idx as u16 {
                0x2300 => Some(self.read_sfr()),
                _ => None,
            };
        }
        // Main-CPU vector override — when the SA-1 is currently
        // asserting an IRQ/NMI to the S-CPU and the matching IVSW /
        // NMIVW bit is set, the bank-0 vector slots read back as the
        // SA-1's SIV / SNV instead of the real ROM bytes.
        if let Some(v) = self.main_vector_override(bank, offset) {
            return Some(v);
        }
        if let Some(o) = Self::iram_offset(bank, offset) {
            return Some(self.iram[o]);
        }
        if let Some(o) = self.bwram_offset(bank, offset) {
            // With a Type-1 conversion armed, the S-CPU's own BW-RAM reads
            // are what drives it: each read that crosses into a new
            // character converts it into I-RAM and answers from there
            // (ares `BWRAM::readCPU` → `dmaCC1Read`). The address is the
            // translated linear one, which `bwram_offset` already produced.
            if self.bwram_dma {
                return Some(self.dma_cc1_read(o as u32));
            }
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

    fn save_state(&self) -> Vec<u8> {
        bincode::serde::encode_to_vec(self, bincode::config::standard()).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), MapperStateError> {
        let mut tmp: Self = decode_state(data, "SA-1")?;
        // BW-RAM is indexed `% bwram.len()`: a wrong (or empty) size would
        // mis-map or divide by zero.
        check_state_len("SA-1 BW-RAM", tmp.bwram.len(), self.bwram.len())?;
        // Keep the live ROM (it is `serde(skip)`-defaulted to empty in
        // `tmp`); swap in every other field by replacing `self` wholesale.
        tmp.rom = std::mem::take(&mut self.rom);
        *self = tmp;
        Ok(())
    }
}

impl Sa1Mapper {
    /// The SA-1's own view of the bus (ares `memory.cpp:21-63`): its
    /// register set (`readIOSA1`), I-RAM at both `$3000-$37FF` and the
    /// `$0000-$07FF` direct-page mirror, BW-RAM in its three views, ROM
    /// through the super-MMC. The S-CPU's vector override does not apply
    /// (the SA-1 has [`Sa1Mapper::sa1_vector_override`], applied by
    /// [`super::Sa1Bus`]). An unmapped address reads `None`.
    pub fn read_from_sa1(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if let Some(idx) = Self::mmio_offset(addr) {
            return self.read_io_sa1(0x2200 + idx as u16);
        }
        if let Some(o) = Self::iram_offset_sa1(bank, offset) {
            return Some(self.iram[o]);
        }
        match self.bwram_target_sa1(bank, offset) {
            Some(BwTarget::Linear(o)) => return Some(self.bwram[o]),
            Some(BwTarget::Bitmap(pixel)) => return Some(self.bitmap_read(pixel)),
            None => {}
        }
        if let Some(o) = self.rom_offset(bank, offset) {
            return Some(self.rom[o]);
        }
        None
    }

    /// The SA-1's register reads (ares `readIOSA1`, `io.cpp:24-94`): CFR,
    /// the timer counters (latched together by the `$2302` read), the
    /// math result and overflow flag, the two VLBP ports. Everything else
    /// in the window — SFR included — reads `None` (ares: `$FF`).
    fn read_io_sa1(&mut self, absolute: u16) -> Option<u8> {
        Some(match absolute {
            0x2301 => self.read_cfr(),
            0x2302 => {
                self.hcr = self.hcounter >> 2;
                self.vcr = self.vcounter;
                self.hcr as u8
            }
            0x2303 => (self.hcr >> 8) as u8,
            0x2304 => self.vcr as u8,
            0x2305 => (self.vcr >> 8) as u8,
            0x2306 => self.mr as u8,
            0x2307 => (self.mr >> 8) as u8,
            0x2308 => (self.mr >> 16) as u8,
            0x2309 => (self.mr >> 24) as u8,
            0x230A => (self.mr >> 32) as u8,
            0x230B => u8::from(self.overflow) << 7,
            // VDPL: the window's low byte, never advancing.
            0x230C => self.vlbp_window() as u8,
            // VDPH: the next byte; in auto-increment mode the read moves
            // the cursor on.
            0x230D => {
                let hi = (self.vlbp_window() >> 8) as u8;
                if self.vbd & 0x80 != 0 {
                    self.vlbp_advance();
                }
                hi
            }
            _ => return None,
        })
    }

    /// Base SA-1 access cost in **SA-1 steps** (1 step = 2 master cycles)
    /// for `addr`, faithful to ares `coprocessor/sa1/memory.cpp`: IO = 1,
    /// ROM = 1, IRAM = 1, BWRAM = 2 (+ `conflict()` contention steps, which
    /// are Increment B). The region order mirrors [`Self::read_from_sa1`]
    /// exactly so the cycle decode can never drift from the data path.
    #[must_use]
    pub fn sa1_region_steps(&self, addr: Addr24) -> u8 {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        // BWRAM is the only 2-step region; MMIO/IRAM/ROM/open-bus are all
        // 1 step. Honour the same priority as `read_from_sa1` (MMIO and
        // IRAM win over BWRAM), so a BWRAM cost is charged only when the
        // address actually resolves to BWRAM.
        let is_bwram = Self::mmio_offset(addr).is_none()
            && Self::iram_offset_sa1(bank, offset).is_none()
            && self.bwram_target_sa1(bank, offset).is_some();
        if is_bwram { 2 } else { 1 }
    }

    /// Extra SA-1 bus-**contention** steps for an access at `sa1_addr` while
    /// the S-CPU holds `scpu_mar` on the shared bus — ares' `conflict()`
    /// model (`coprocessor/sa1/{rom,bwram,iram}.cpp` + the conditional
    /// `step()`s in `memory.cpp` read/write). The SA-1's own access region
    /// (ROM / BW-RAM / I-RAM, classified by ares' raw address masks, MMIO
    /// and ROM winning over BW-RAM/I-RAM) selects which resource is in play;
    /// the penalty fires only when the **S-CPU**'s last access address
    /// (`scpu_mar`, ares `cpu.r.mar`) targets that same resource:
    /// ROM `+1`, BW-RAM `+2`, I-RAM `+2`. MMIO / open-bus never contend.
    ///
    /// Charged on top of [`Self::sa1_region_steps`] (the base cost). This is
    /// the "Increment B" the Phase-5b doc deferred.
    ///
    /// Two faithful approximations vs ares, both bounded: (1) luna evaluates
    /// `scpu_mar` once per SA-1 batch (the deficit model runs ~1-2 SA-1
    /// instructions per S-CPU access, so the S-CPU address is effectively
    /// fixed across the batch); (2) the I-RAM exemption during S-CPU DRAM
    /// refresh (`iram.conflict()` returns `cpu.refresh()==0`) is not modelled
    /// — a ≤2-step over-charge confined to the ~40-mclk refresh window.
    #[must_use]
    pub fn sa1_conflict_steps(&self, sa1_addr: Addr24, scpu_mar: u32) -> u8 {
        let a = sa1_addr & 0xFF_FFFF;
        // MMIO ($2200-23ff): no shared-bus contention.
        if a & 0x40_FE00 == 0x00_2200 {
            return 0;
        }
        // SA-1 ROM region → conflict iff the S-CPU is also in ROM (+1).
        if a & 0x40_8000 == 0x00_8000 || a & 0xC0_0000 == 0xC0_0000 {
            return u8::from(Self::scpu_rom_conflict(scpu_mar));
        }
        // SA-1 BW-RAM region → conflict iff the S-CPU is also in BW-RAM (+2).
        if a & 0x40_E000 == 0x00_6000 || a & 0xE0_0000 == 0x40_0000 || a & 0xF0_0000 == 0x60_0000 {
            return if Self::scpu_bwram_conflict(scpu_mar) {
                2
            } else {
                0
            };
        }
        // SA-1 I-RAM region ($0000-07ff mirror or $3000-37ff) → conflict iff
        // the S-CPU is also in I-RAM (+2).
        if a & 0x40_F800 == 0x00_0000 || a & 0x40_F800 == 0x00_3000 {
            return if Self::scpu_iram_conflict(scpu_mar) {
                2
            } else {
                0
            };
        }
        // Open-bus fall-through: 1 base step, no contention.
        0
    }

    /// ares `SA1::ROM::conflict()` — S-CPU in `00-3f/80-bf:8000-ffff` or
    /// `c0-ff:0000-ffff`.
    const fn scpu_rom_conflict(mar: u32) -> bool {
        mar & 0x40_8000 == 0x00_8000 || mar & 0xC0_0000 == 0xC0_0000
    }

    /// ares `SA1::BWRAM::conflict()` — S-CPU in `00-3f/80-bf:6000-7fff` or
    /// `40-4f:0000-ffff` (note: narrower than the SA-1's own BW-RAM region).
    const fn scpu_bwram_conflict(mar: u32) -> bool {
        mar & 0x40_E000 == 0x00_6000 || mar & 0xF0_0000 == 0x40_0000
    }

    /// ares `SA1::IRAM::conflict()` — S-CPU in `00-3f/80-bf:3000-37ff` (the
    /// `cpu.refresh()==0` exemption is approximated as always-active; see
    /// [`Self::sa1_conflict_steps`]).
    const fn scpu_iram_conflict(mar: u32) -> bool {
        mar & 0x40_F800 == 0x00_3000
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
            // Each side owns its registers (ares `writeIOCPU` /
            // `writeIOSA1`, Mesen2 `CpuRegisterWrite` / `Sa1RegisterWrite`):
            // the S-CPU cannot set the SA-1's CIE or start its DMA, the
            // SA-1 cannot rebank ROM or release itself. A write to the
            // other side's register is dropped — the access is still
            // claimed, the window is nothing else on the bus.
            let owned = match side {
                WriteSide::Main => Self::cpu_side_register(absolute),
                WriteSide::Sa1 => Self::sa1_side_register(absolute),
            };
            if !owned {
                return true;
            }
            let prev = self.mmio[idx];
            self.mmio[idx] = value;
            match absolute {
                // -------- S-CPU → SA-1 control --------
                0x2200 => {
                    // CCNT bit layout (per ares `io.cpp` $2200 +
                    // Mesen2 `Sa1.cpp:240-258`):
                    //   bit 7 = SA-1 IRQ request (level-driven)
                    //   bit 6 = SA-1 wait (not modelled)
                    //   bit 5 = SA-1 reset (handled in Sa1Chip::write)
                    //   bit 4 = SA-1 NMI request (level-driven)
                    //   bits 3..0 = message to SA-1
                    // Both refs latch the IRQ/NMI flag on every write
                    // whose corresponding bit is set, regardless of the
                    // previous value. luna previously edge-detected
                    // (0→1) which silently dropped re-trigger requests
                    // — games that re-write CCNT=$80 after a single ack
                    // never got the second IRQ. Acks are explicit via
                    // CIC ($220B), not implicit on a CCNT clear.
                    let _ = prev;
                    self.ccnt_msg = value & 0x0F;
                    self.ccnt_irq_level = (value & 0x80) != 0;
                    // Bit 6 (RDYB) parks the SA-1: ares `sa1.cpp:46-50`
                    // steps the clock and returns without executing while
                    // `sa1_rdyb` is set, and Mesen2 gates `Run` on
                    // `Sa1Wait`. luna ran straight through it.
                    self.sa1_wait = (value & 0x40) != 0;
                    if (value & 0x80) != 0 {
                        self.main_irq_to_sa1 = true;
                    }
                    if (value & 0x10) != 0 {
                        self.main_nmi_to_sa1 = true;
                        if (self.cie & 0x10) != 0 {
                            self.sa1_nmi_event = true;
                        }
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
                    // SCNT bit layout (per ares `io.cpp` $2209 lines
                    // ~233-246 + Mesen2 `Sa1.cpp:84-110`):
                    //   bit 7 = SA-1 → S-CPU IRQ request (level-driven)
                    //   bit 6 = IVSW (vector override for S-CPU IRQ)
                    //   bit 4 = NMIVW (vector override for S-CPU NMI)
                    //   bits 3..0 = message to S-CPU
                    // The IRQ latch is set on every write whose bit 7
                    // is set, regardless of prior value. luna's old
                    // edge-detect dropped SMRPG's repeated SCNT=$87
                    // pulses — after the first ack via SIC, the second
                    // and subsequent SCNT writes never re-latched, so
                    // the main↔SA-1 mailbox deadlocked on the second
                    // handshake. Acks are explicit through SIC ($2202).
                    let _ = prev;
                    self.scnt = value;
                    if (value & 0x80) != 0 {
                        self.s_irq_to_main = true;
                    }
                }
                // CIE bit layout (per ares + Mesen2):
                //   bit 7 = S-CPU → SA-1 IRQ enable
                //   bit 6 = timer IRQ enable
                //   bit 5 = DMA IRQ enable
                //   bit 4 = S-CPU → SA-1 NMI enable
                0x220A => {
                    // Enabling the NMI while its flag is pending delivers it
                    // (ares `io.cpp` `$220A`: `!nmien && bit4 && nmifl`).
                    if (prev & 0x10) == 0 && (value & 0x10) != 0 && self.main_nmi_to_sa1 {
                        self.sa1_nmi_event = true;
                    }
                    self.cie = value;
                }
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
                0x2210 => self.tmc = value,
                0x2211 => {
                    // CTR — restart the timer (ares io.cpp $2211).
                    self.hcounter = 0;
                    self.vcounter = 0;
                    self.timer_rem = 0;
                }
                0x2212 => self.hcnt_lo = value,
                0x2213 => self.hcnt_hi = value,
                0x2214 => self.vcnt_lo = value,
                0x2215 => self.vcnt_hi = value,

                // -------- SA-1 DMA --------
                //
                // DCNT only *configures* the DMA. Per ares' `io.cpp`
                // and Mesen2's `WriteSharedRegisters`, the actual
                // trigger is on the final DDA byte for normal /
                // CC1 DMA, and on BRF[7] / BRF[15] for CC2.
                0x2230 => {
                    self.dcnt = value;
                    self.dma_en = (value & 0x80) != 0;
                    self.dma_cden = (value & 0x20) != 0;
                    self.dma_cdsel = (value & 0x10) != 0;
                    self.dma_dd = (value & 0x04) != 0;
                    // ares `io.cpp:327`: clearing DMA enable resets the
                    // Type-2 line counter, nothing else.
                    if !self.dma_en {
                        self.cc2_line = 0;
                    }
                }
                0x2231 => {
                    // CDMA (ares `io.cpp:452-461`): colour depth in bits
                    // 0-1, virtual bitmap width in bits 2-4 — luna had the
                    // two fields swapped — and bit 7 (CDEND) ends an armed
                    // Type-1 conversion.
                    self.cdma = value;
                    self.dmacb = (value & 0x03).min(2);
                    self.dmasize = ((value >> 2) & 0x07).min(5);
                    if (value & 0x80) != 0 {
                        self.bwram_dma = false;
                    }
                }
                0x2232 => self.sda = (self.sda & !0x00_00FF) | u32::from(value),
                0x2233 => self.sda = (self.sda & !0x00_FF00) | (u32::from(value) << 8),
                0x2234 => self.sda = (self.sda & !0xFF_0000) | (u32::from(value) << 16),
                0x2235 => self.dda = (self.dda & !0x00_00FF) | u32::from(value),
                0x2236 => {
                    self.dda = (self.dda & !0x00_FF00) | (u32::from(value) << 8);
                    // Trigger: normal DMA → I-RAM, or CC1.
                    if self.dma_en {
                        if !self.dma_cden && !self.dma_dd {
                            self.run_normal_dma(side);
                        } else if self.dma_cden && self.dma_cdsel {
                            self.dma_cc1();
                        }
                    }
                }
                0x2237 => {
                    self.dda = (self.dda & !0xFF_0000) | (u32::from(value) << 16);
                    // Trigger: normal DMA → BW-RAM.
                    if self.dma_en && !self.dma_cden && self.dma_dd {
                        self.run_normal_dma(side);
                    }
                }
                0x2238 => self.dtc = (self.dtc & 0xFF00) | u16::from(value),
                0x2239 => self.dtc = (self.dtc & 0x00FF) | (u16::from(value) << 8),
                // BBF: the bitmap projection's pixel format.
                0x223F => self.bbf = value & 0x80 != 0,
                // BRF ($2240-$224F): the Type-2 register file. Writing the
                // last byte of either half converts one tile row (ares
                // `io.cpp:348-368`).
                0x2240..=0x224F => {
                    self.brf[usize::from(value_reg_index(absolute))] = value;
                    if matches!(absolute, 0x2247 | 0x224F)
                        && self.dma_en
                        && self.dma_cden
                        && !self.dma_cdsel
                    {
                        self.dma_cc2();
                    }
                }

                0x2220 => self.cxb = value,
                0x2221 => self.dxb = value,
                0x2222 => self.exb = value,
                0x2223 => self.fxb = value,
                0x2224 => self.bmaps = value,
                // $2225 BMAP (CBM) — the SA-1's BW-RAM $6000-$7FFF bank.
                0x2225 => self.cbm = value,
                0x2226 => self.sbwe = value,
                0x2227 => self.cbwe = value,
                0x2228 => self.bwpa = value,
                // Per ares + Mesen2: SIWP at $2229, CIWP at $222A.
                // (Older docs had these one address higher.)
                0x2229 => self.siwp = value,
                0x222A => self.ciwp = value,
                0x2250 => {
                    self.mcnt = value;
                    // ares io.cpp:374 — entering sigma mode (acm, bit 1)
                    // clears the accumulator, regardless of the other bits.
                    if value & 0x02 != 0 {
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
                // -------- VLBP (ares `io.cpp:427-444`) --------
                // VBD: in fixed mode (bit 7 clear) the write itself moves
                // the cursor on by `vb` bits — the game reads `$230C/D`
                // first, then writes VBD to consume what it took.
                0x2258 => {
                    self.vbd = value;
                    if value & 0x80 == 0 {
                        self.vlbp_advance();
                    }
                }
                0x2259 => self.va = (self.va & !0x00_00FF) | u32::from(value),
                0x225A => self.va = (self.va & !0x00_FF00) | (u32::from(value) << 8),
                0x225B => {
                    self.va = (self.va & !0xFF_0000) | (u32::from(value) << 16);
                    self.vbit = 0;
                }
                _ => {}
            }
            return true;
        }
        // SA-1 writes can also reach the I-RAM mirror at $0000-$07FF;
        // the main CPU only sees I-RAM at $3000-$37FF (so its writes
        // into $0000-$07FF should fall through to the bus's WRAM).
        let iram = match side {
            WriteSide::Main => Self::iram_offset(bank, offset),
            WriteSide::Sa1 => Self::iram_offset_sa1(bank, offset),
        };
        if let Some(o) = iram {
            if self.iram_writable_for(o, side) {
                self.iram[o] = value;
            }
            // Always claim the access — the bus would otherwise fall
            // through to WRAM, which isn't what protection means.
            return true;
        }
        match side {
            WriteSide::Main => {
                if let Some(o) = self.bwram_offset(bank, offset) {
                    if self.bwram_writable_for(o) {
                        self.bwram[o] = value;
                    }
                    return true;
                }
            }
            WriteSide::Sa1 => match self.bwram_target_sa1(bank, offset) {
                Some(BwTarget::Linear(o)) => {
                    if self.bwram_writable_for(o) {
                        self.bwram[o] = value;
                    }
                    return true;
                }
                Some(BwTarget::Bitmap(pixel)) => {
                    self.bitmap_write(pixel, value);
                    return true;
                }
                None => {}
            },
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
    fn load_state_refuses_a_bwram_of_the_wrong_size() {
        // BW-RAM is indexed `% bwram.len()`; a foreign size would mis-map.
        let small = Sa1Mapper::new(ramp_rom(0x1_0000), 0x2000).save_state();
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x8000);
        let before = m.save_state();
        assert!(m.load_state(&small).is_err());
        assert!(m.load_state(&[0xFF; 9]).is_err());
        assert_eq!(
            m.save_state(),
            before,
            "a refused state must not modify the mapper"
        );
        m.load_state(&before).unwrap();
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
        m.write(make_addr(0x00, 0x2226), 0x80); // SBWE: the S-CPU may write
        let addr = make_addr(0x00, 0x6000);
        assert!(m.write(addr, 0xAB));
        assert_eq!(m.read(addr), Some(0xAB));
    }

    #[test]
    fn bwram_linear_view_at_bank_40() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10_0000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        let addr = make_addr(0x40, 0x1234);
        assert!(m.write(addr, 0x99));
        assert_eq!(m.read(addr), Some(0x99));
    }

    /// Power-on (ares `sa1.cpp:230-237`, Mesen2 `Sa1::Reset`): both
    /// write enables clear and BWPA = $0F, so every BW-RAM byte refuses
    /// writes from either side until a game enables one; a reset arms
    /// the protection again.
    #[test]
    fn bwram_is_write_protected_at_power_on() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0x0000), 0xAA);
        m.write(make_addr(0x40, 0xFFFF), 0xAA);
        m.write_from_sa1(make_addr(0x40, 0x0100), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 0xFFFF)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 0x0100)), Some(0x00));
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0000), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0xAA));
        m.power_reset();
        m.write(make_addr(0x40, 0x0001), 0xBB);
        assert_eq!(m.read(make_addr(0x40, 0x0001)), Some(0x00), "armed again");
        assert_eq!(
            m.read(make_addr(0x40, 0x0000)),
            Some(0xAA),
            "BW-RAM persists"
        );
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
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x00);
        // MA = 7 (signed)
        m.write_from_sa1(make_addr(0x00, 0x2251), 0x07);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0x00);
        // MB = 8 (signed) → high-byte write triggers
        m.write_from_sa1(make_addr(0x00, 0x2253), 0x08);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0x00);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2306)), Some(56));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2307)), Some(0));
    }

    #[test]
    fn divider_16_div_16_packs_quotient_and_remainder() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x01); // divide
        // MA = 100, MB = 7 → q = 14, r = 2.
        m.write_from_sa1(make_addr(0x00, 0x2251), 100);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0);
        m.write_from_sa1(make_addr(0x00, 0x2253), 7);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2306)), Some(14)); // quotient lo
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2307)), Some(0));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2308)), Some(2)); // remainder lo
    }

    #[test]
    fn multiplier_signed_negative() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x00);
        // MA = -1 ($FFFF)
        m.write_from_sa1(make_addr(0x00, 0x2251), 0xFF);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0xFF);
        // MB = 100
        m.write_from_sa1(make_addr(0x00, 0x2253), 100);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0);
        // Result = -100 = 0xFFFFFF9C.
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2306)), Some(0x9C));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2307)), Some(0xFF));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2308)), Some(0xFF));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2309)), Some(0xFF));
    }

    /// Read the packed 32-bit MR (quotient = low 16, remainder = high 16).
    fn read_mr_lo32(m: &mut Sa1Mapper) -> (u16, u16) {
        let q = u16::from(m.read_from_sa1(make_addr(0x00, 0x2306)).unwrap())
            | (u16::from(m.read_from_sa1(make_addr(0x00, 0x2307)).unwrap()) << 8);
        let r = u16::from(m.read_from_sa1(make_addr(0x00, 0x2308)).unwrap())
            | (u16::from(m.read_from_sa1(make_addr(0x00, 0x2309)).unwrap()) << 8);
        (q, r)
    }

    #[test]
    fn divider_negative_dividend_is_floored() {
        // ares: signed dividend ÷ unsigned divisor, floored (remainder
        // ≥ 0). MA = -100 ($FF9C) ÷ MB = 7 → q = -15 ($FFF1), r = 5.
        // (The old signed/signed truncated path gave q = -14, r = -2.)
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x01);
        m.write_from_sa1(make_addr(0x00, 0x2251), 0x9C);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0xFF);
        m.write_from_sa1(make_addr(0x00, 0x2253), 7);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0);
        assert_eq!(read_mr_lo32(&mut m), (0xFFF1, 5));
    }

    #[test]
    fn divider_treats_divisor_as_unsigned() {
        // MB = $8000 is 32768 (unsigned), not -32768. MA = 100 → q = 0,
        // r = 100.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x01);
        m.write_from_sa1(make_addr(0x00, 0x2251), 100);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0);
        m.write_from_sa1(make_addr(0x00, 0x2253), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0x80);
        assert_eq!(read_mr_lo32(&mut m), (0, 100));
    }

    #[test]
    fn multiply_resets_mb_after_op() {
        // ares zeroes MB after a multiply. Triggering a second op with
        // only MBH written (=0) leaves MB = 0 → product 0. (Old code kept
        // MB and gave 15 again.)
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2251), 5);
        m.write_from_sa1(make_addr(0x00, 0x2252), 0);
        m.write_from_sa1(make_addr(0x00, 0x2253), 3);
        m.write_from_sa1(make_addr(0x00, 0x2254), 0);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2306)), Some(15));
        m.write_from_sa1(make_addr(0x00, 0x2254), 0); // MB low was reset to 0
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2306)), Some(0));
    }

    #[test]
    fn sigma_accumulates_into_40_bit_result() {
        // acm mode (MCNT bit 1) clears MR then accumulates ma·mb. MB is
        // reset each op, so it's re-loaded for the second accumulation.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2250), 0x02); // acm → clears MR
        m.write_from_sa1(make_addr(0x00, 0x2251), 0xE8); // MA = 1000
        m.write_from_sa1(make_addr(0x00, 0x2252), 0x03);
        m.write_from_sa1(make_addr(0x00, 0x2253), 0xE8); // MB = 1000
        m.write_from_sa1(make_addr(0x00, 0x2254), 0x03); // MR += 1_000_000
        m.write_from_sa1(make_addr(0x00, 0x2253), 0xE8); // re-load MB (was reset)
        m.write_from_sa1(make_addr(0x00, 0x2254), 0x03); // MR += 1_000_000
        let mr = (0u64..5).fold(0u64, |acc, i| {
            acc | (u64::from(m.read_from_sa1(make_addr(0x00, 0x2306 + i as u16)).unwrap())
                << (8 * i))
        });
        assert_eq!(mr, 2_000_000);
        // No overflow at this magnitude.
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x230B)), Some(0));
    }

    #[test]
    fn unowned_register_slots_read_open_bus_on_both_sides() {
        // No register is memory-backed: a slot nobody decodes reads open
        // bus (`None`) from the S-CPU (ares `readIOCPU` returns the MDR)
        // and `None` (ares `$FF`) from the SA-1.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write(make_addr(0x00, 0x22FF), 0x5A);
        m.write_from_sa1(make_addr(0x00, 0x22FF), 0x5A);
        assert_eq!(m.read(make_addr(0x00, 0x22FF)), None);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x22FF)), None);
        // A register the other side owns is open bus too: the S-CPU
        // cannot read CFR (Kirby Super Star polls `$2301` 2.9 million
        // times and must see open bus, not the SA-1's flags), the SA-1
        // cannot read SFR.
        m.write(make_addr(0x00, 0x2200), 0x85); // CCNT: IRQ + message 5
        assert_eq!(m.read(make_addr(0x00, 0x2301)), None);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2301)), Some(0x85));
        m.write_from_sa1(make_addr(0x00, 0x2209), 0x03); // SCNT: message 3
        assert_eq!(m.read(make_addr(0x00, 0x2300)), Some(0x03));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2300)), None);
    }

    #[test]
    fn register_writes_are_owned_by_one_side() {
        // ares `writeIOCPU` / `writeIOSA1`, Mesen2 `CpuRegisterWrite` /
        // `Sa1RegisterWrite`: the S-CPU cannot arm the SA-1's interrupt
        // enable, the SA-1 cannot rebank ROM; the shared DMA address block
        // takes both.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x220A), 0x80); // CIE from the S-CPU: dropped
        m.write(make_addr(0x00, 0x2200), 0x80); // CCNT: IRQ to the SA-1
        assert!(!m.sa1_irq_line(), "the S-CPU's CIE write must not count");
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x80);
        assert!(m.sa1_irq_line());
        m.write_from_sa1(make_addr(0x00, 0x2220), 0x81); // CXB from the SA-1: dropped
        assert_eq!(m.cxb, 0x00);
        m.write(make_addr(0x00, 0x2220), 0x81);
        assert_eq!(m.cxb, 0x81);
        m.write(make_addr(0x00, 0x2232), 0x11); // SDA is shared
        m.write_from_sa1(make_addr(0x00, 0x2233), 0x22);
        assert_eq!(m.sda, 0x2211);
        // The dropped write is still a claimed access — the window is
        // nothing else on the bus.
        assert!(m.write(make_addr(0x00, 0x220A), 0x80));
    }

    #[test]
    fn hcr_vcr_latch_on_the_2302_read() {
        // ares `io.cpp:39-50`: reading HCR-low latches both counters;
        // the three other bytes return the latched pair, not the live one.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x80); // linear timer
        m.tick_timer(400); // H = 100 dots
        assert_eq!(
            m.read_from_sa1(make_addr(0x00, 0x2303)),
            Some(0),
            "nothing latched yet"
        );
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2302)), Some(100));
        m.tick_timer(400); // H = 200 dots live
        assert_eq!(
            m.read_from_sa1(make_addr(0x00, 0x2302)),
            Some(200),
            "a new latch"
        );
        m.tick_timer(4 * 341);
        assert_eq!(
            m.read_from_sa1(make_addr(0x00, 0x2303)),
            Some(0),
            "latched high byte"
        );
        assert_eq!(
            m.read_from_sa1(make_addr(0x00, 0x2304)),
            Some(0),
            "latched V"
        );
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
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x80);
        assert!(!m.sa1_irq_line(), "no IRQ until the S-CPU triggers it");
        // CCNT bit 7 0→1 latches the IRQ (per ares + Mesen2).
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(m.sa1_irq_line(), "edge should latch + gate through CIE");
        // CIC bit 7 clears the latch.
        m.write_from_sa1(make_addr(0x00, 0x220B), 0x80);
        assert!(!m.sa1_irq_line());
    }

    #[test]
    fn main_to_sa1_irq_relatches_on_every_set_write() {
        // Per ares + Mesen2: CCNT bit 7 is level-driven — every write
        // whose bit 7 is set raises the IRQ flag, regardless of the
        // previous value. The ack path is explicit through CIC ($220B
        // bit 7), not implicit on a 1→1 retain. luna previously
        // edge-detected, which silently dropped re-trigger requests
        // and deadlocked SMRPG's second mailbox handshake.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x80);
        // First write with bit 7 set → latch.
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(m.sa1_irq_line());
        // CIC bit 7 acks the flag.
        m.write_from_sa1(make_addr(0x00, 0x220B), 0x80);
        assert!(!m.sa1_irq_line());
        // Re-writing $80 with no intervening clear must re-latch
        // (level-driven, not edge-detect).
        m.write(make_addr(0x00, 0x2200), 0x80);
        assert!(
            m.sa1_irq_line(),
            "1→1 same-bit retain MUST re-trigger under level semantics"
        );
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
        m.write_from_sa1(make_addr(0x00, 0x2209), 0x80);
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
        m.write_from_sa1(make_addr(0x00, 0x2209), 0x80 | 0x05);
        let sfr = m.read(make_addr(0x00, 0x2300)).unwrap();
        assert_eq!(sfr & 0x80, 0x80, "bit 7 = SA-1 IRQ");
        assert_eq!(sfr & 0x0F, 0x05, "low nibble = message");
    }

    #[test]
    fn cfr_reflects_main_to_sa1_irq_latch_and_message_nibble() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x80);
        // CCNT: bit 7 = IRQ trigger, bits 0..3 = message.
        m.write(make_addr(0x00, 0x2200), 0x80 | 0x0A);
        let cfr = m.read_from_sa1(make_addr(0x00, 0x2301)).unwrap();
        assert_eq!(cfr & 0x80, 0x80);
        assert_eq!(cfr & 0x0F, 0x0A);
    }

    #[test]
    fn main_irq_vector_overrides_to_siv_when_ivsw_and_latched() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220E), 0x34); // SIV lo
        m.write_from_sa1(make_addr(0x00, 0x220F), 0x12); // SIV hi
        m.write(make_addr(0x00, 0x2201), 0x80); // SIE.7 enable
        // SCNT: IVSW (bit 5… err, in our impl we use $40) + IRQ trigger.
        // IVSW = bit 5 of SCNT per Anomie; our scheme uses $40.
        m.write_from_sa1(make_addr(0x00, 0x2209), 0x80 | 0x40);
        // Now main reads $00:FFEE/FFEF — they should reflect SIV.
        assert_eq!(m.read(make_addr(0x00, 0xFFEE)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0xFFEF)), Some(0x12));
        assert_eq!(m.read(make_addr(0x00, 0xFFFE)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0xFFFF)), Some(0x12));
    }

    #[test]
    fn main_irq_vector_falls_back_to_rom_when_ivsw_clear() {
        let mut m = Sa1Mapper::new(ramp_rom(0x10_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220E), 0x34);
        m.write_from_sa1(make_addr(0x00, 0x220F), 0x12);
        m.write(make_addr(0x00, 0x2201), 0x80);
        // IVSW (SCNT bit 6) clear → no override. ares + Mesen2
        // explicitly treat the vector override as level-only on the
        // IVSW bit, *not* gated by whether the IRQ line is pending.
        m.write_from_sa1(make_addr(0x00, 0x2209), 0x00);
        let v = m.read(make_addr(0x00, 0xFFEE)).unwrap();
        assert_ne!(v, 0x34, "no override without IVSW");
    }

    // ------------- Phase-3 timer tests -------------

    #[test]
    fn timer_linear_mode_fires_irq_on_h_match() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x40); // CIE.6 = timer IRQ enable
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x81); // linear mode + H enable
        m.write_from_sa1(make_addr(0x00, 0x2212), 100); // HCNT = 100 dots → 400 clocks
        m.write_from_sa1(make_addr(0x00, 0x2213), 0);
        m.tick_timer(398); // hcounter = 398, not yet the compare
        assert!(!m.sa1_irq_line());
        m.tick_timer(2); // hcounter reaches 400 == HCNT<<2
        assert!(m.sa1_irq_line(), "fires when hcounter == HCNT<<2");
    }

    #[test]
    fn timer_reset_via_ctr_clears_the_counter() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x81);
        m.tick_timer(400); // 400 clocks
        // HCR ($2302) reads back in DOTS: 400 >> 2 = 100.
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2302)), Some(100));
        m.write_from_sa1(make_addr(0x00, 0x2211), 0x00); // CTR restart
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x2302)), Some(0));
    }

    #[test]
    fn timer_hv_mode_fires_irq_on_h_match() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x40); // CIE.6 = timer IRQ enable
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x01); // HV mode + H enable
        m.write_from_sa1(make_addr(0x00, 0x2212), 100); // HCNT = 100 dots → 400 clocks
        m.tick_timer(398);
        assert!(!m.sa1_irq_line());
        m.tick_timer(2); // hcounter == 400
        assert!(m.sa1_irq_line(), "HV-mode H match fires the timer IRQ");
    }

    #[test]
    fn timer_hv_mode_fires_on_v_match() {
        // The raster-timing use case: fire at the start of a target scanline.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x40); // CIE.6 = timer IRQ enable
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x02); // HV mode + V enable
        m.write_from_sa1(make_addr(0x00, 0x2214), 2); // VCNT = scanline 2
        // V increments once per 1364-clock line; reach the start of line 2.
        m.tick_timer(2 * 1364 - 2);
        assert!(!m.sa1_irq_line());
        m.tick_timer(2); // vcounter == 2 with hcounter == 0
        assert!(m.sa1_irq_line(), "HV-mode V match fires at the line start");
    }

    #[test]
    fn timer_irq_refires_each_period_after_clear() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0);
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x40); // CIE.6 = timer IRQ enable
        m.write_from_sa1(make_addr(0x00, 0x2210), 0x81); // linear mode + H enable
        m.write_from_sa1(make_addr(0x00, 0x2212), 50); // HCNT = 50 → 200 clocks
        m.tick_timer(200);
        assert!(m.sa1_irq_line());
        m.write_from_sa1(make_addr(0x00, 0x220B), 0x40); // CIC.6 clears the flag
        assert!(!m.sa1_irq_line());
        // The flag is level, not one-shot: it re-fires one full H period
        // later (linear H wraps at 0x800 = 2048 clocks) when hcounter hits
        // the compare again — no re-arm write needed.
        m.tick_timer(2048);
        assert!(m.sa1_irq_line(), "fires again on the next period");
    }

    // ------------- Phase-3 normal DMA tests -------------

    /// Arm a normal DMA the way a game does: DCNT names the devices, SDA /
    /// DDA are raw offsets into them, the final DDA byte fires it.
    fn dma_setup(m: &mut Sa1Mapper, dcnt: u8, sda: u32, dtc: u16) {
        m.write_from_sa1(make_addr(0x00, 0x2230), dcnt);
        m.write(make_addr(0x00, 0x2232), sda as u8);
        m.write(make_addr(0x00, 0x2233), (sda >> 8) as u8);
        m.write(make_addr(0x00, 0x2234), (sda >> 16) as u8);
        m.write_from_sa1(make_addr(0x00, 0x2238), dtc as u8);
        m.write_from_sa1(make_addr(0x00, 0x2239), (dtc >> 8) as u8);
    }

    /// DDA in three bytes; `$2237` last fires a BW-RAM-bound DMA.
    fn dma_fire_bwram(m: &mut Sa1Mapper, dda: u32) {
        m.write(make_addr(0x00, 0x2235), dda as u8);
        m.write(make_addr(0x00, 0x2236), (dda >> 8) as u8);
        m.write(make_addr(0x00, 0x2237), (dda >> 16) as u8);
    }

    /// DDA with `$2236` last: an I-RAM-bound DMA fires on the middle byte.
    fn dma_fire_iram(m: &mut Sa1Mapper, dda: u32) {
        m.write(make_addr(0x00, 0x2235), dda as u8);
        m.write(make_addr(0x00, 0x2237), (dda >> 16) as u8);
        m.write(make_addr(0x00, 0x2236), (dda >> 8) as u8);
    }

    #[test]
    fn normal_dma_copies_iram_to_bwram_and_raises_irq() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        for i in 0..16 {
            m.write(make_addr(0x00, 0x3000 + i), 0xA0 + i as u8);
        }
        m.write_from_sa1(make_addr(0x00, 0x220A), 0x20); // CIE bit 5 = DMA IRQ
        // sd = I-RAM (2), dd = BW-RAM (bit 2); SDA is an I-RAM offset.
        dma_setup(&mut m, 0x86, 0x00_0000, 16);
        dma_fire_bwram(&mut m, 0x40_0000);
        for i in 0..16 {
            assert_eq!(m.read(make_addr(0x40, i as u16)), Some(0xA0 + i as u8));
        }
        assert!(m.sa1_irq_line(), "DMA IRQ should be asserted");
        // 2 steps a byte, no contention from an S-CPU-fired DMA.
        assert_eq!(m.take_dma_steps(), 32);
        // DMA enable stays set (ares / Mesen2 never clear it): a second
        // burst needs only a new count and destination.
        m.write(make_addr(0x00, 0x3000), 0x5A);
        m.write_from_sa1(make_addr(0x00, 0x2238), 1);
        m.write_from_sa1(make_addr(0x00, 0x2232), 0x00);
        dma_fire_bwram(&mut m, 0x40_0020);
        assert_eq!(m.read(make_addr(0x40, 0x0020)), Some(0x5A));
    }

    #[test]
    fn normal_dma_from_rom_to_bwram() {
        let rom = (0..0x1_0000).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let mut m = Sa1Mapper::new(rom, 0x10000);
        // sd = ROM (0): the source goes through the SA-1's ROM map, so
        // `$00:8000` is ROM[0].
        dma_setup(&mut m, 0x84, 0x00_8000, 4);
        dma_fire_bwram(&mut m, 0x40_0000);
        for i in 0..4u16 {
            assert_eq!(m.read(make_addr(0x40, i)), Some(i as u8));
        }
        assert_eq!(m.take_dma_steps(), 8);
    }

    #[test]
    fn normal_dma_from_rom_to_iram_fires_on_the_middle_dda_byte() {
        let rom = (0..0x1_0000).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let mut m = Sa1Mapper::new(rom, 0x10000);
        // sd = ROM, dd = I-RAM; `$C0:0010` is ROM[0x10] through CXB.
        dma_setup(&mut m, 0x80, 0xC0_0010, 3);
        dma_fire_iram(&mut m, 0x00_3100); // an I-RAM offset: `& $7FF` = $100
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0x10));
        assert_eq!(m.read(make_addr(0x00, 0x3102)), Some(0x12));
        assert_eq!(m.take_dma_steps(), 3, "one step a byte");
    }

    #[test]
    fn normal_dma_from_bwram_to_iram_uses_raw_offsets() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0010), 0xC3);
        // sd = BW-RAM (1), dd = I-RAM: SDA $000010 is BW-RAM byte $10, no
        // bank decoding.
        dma_setup(&mut m, 0x81, 0x00_0010, 1);
        dma_fire_iram(&mut m, 0x00_0000);
        assert_eq!(m.read(make_addr(0x00, 0x3000)), Some(0xC3));
        assert_eq!(m.take_dma_steps(), 2);
    }

    #[test]
    fn normal_dma_over_an_unsupported_device_pair_moves_nothing() {
        // sd = I-RAM, dd = I-RAM: no such transfer on the chip — the
        // counter runs down, the IRQ fires, memory and the budget are
        // untouched.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x3000), 0x77);
        dma_setup(&mut m, 0x82, 0x00_0000, 4);
        dma_fire_iram(&mut m, 0x00_0100);
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0x00));
        assert_eq!(m.dtc, 0);
        assert!(m.dma_irq_to_sa1);
        assert_eq!(m.take_dma_steps(), 0);
    }

    #[test]
    fn normal_dma_fired_by_the_sa1_pays_contention_for_the_scpu_address() {
        // ares `dma.cpp:8-15`: ROM→BW-RAM costs 2 steps a byte, plus 2
        // more while the S-CPU is on BW-RAM.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.set_scpu_mar(0x40_0100);
        m.write_from_sa1(make_addr(0x00, 0x2230), 0x84);
        m.write_from_sa1(make_addr(0x00, 0x2232), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2233), 0x80);
        m.write_from_sa1(make_addr(0x00, 0x2234), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2238), 4);
        m.write_from_sa1(make_addr(0x00, 0x2239), 0);
        m.write_from_sa1(make_addr(0x00, 0x2235), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2236), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x2237), 0x40);
        assert_eq!(m.take_dma_steps(), 4 * 4);
        // The same burst fired by the S-CPU: its own address is the DMA
        // port, which never contends.
        dma_setup(&mut m, 0x84, 0x00_8000, 4);
        dma_fire_bwram(&mut m, 0x40_0000);
        assert_eq!(m.take_dma_steps(), 4 * 2);
    }

    // ------------- Character-conversion DMA (ares port) -------------

    /// Arm a Type-1 conversion the way a game does: source, CDMA,
    /// DCNT (enable + CC + cdsel), then DDA — the `$2236` write fires it.
    /// `cdma` follows the hardware layout: colour depth in bits 0-1,
    /// virtual width in bits 2-4.
    fn cc1_setup(m: &mut Sa1Mapper, cdma: u8, dda: u32) {
        m.write(make_addr(0x00, 0x2201), 0x20); // CC IRQ to the S-CPU
        m.write(make_addr(0x00, 0x2232), 0x00); // SDA = 0 (linear BW-RAM)
        m.write(make_addr(0x00, 0x2233), 0x00);
        m.write(make_addr(0x00, 0x2234), 0x00);
        m.write(make_addr(0x00, 0x2231), cdma);
        m.write_from_sa1(make_addr(0x00, 0x2230), 0xB0); // enable + CC + cdsel
        m.write(make_addr(0x00, 0x2235), dda as u8);
        m.write(make_addr(0x00, 0x2237), (dda >> 16) as u8);
        m.write(make_addr(0x00, 0x2236), (dda >> 8) as u8);
    }

    #[test]
    fn cdma_decodes_colour_in_bits_0_1_and_width_in_bits_2_4() {
        // ares `io.cpp:454-459`. luna had the two fields swapped, so every
        // conversion ran at the wrong depth AND the wrong bitmap pitch.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2231), 0b0001_0010); // width 4, 2bpp
        assert_eq!((m.dmacb, m.dmasize), (2, 4));
        m.write(make_addr(0x00, 0x2231), 0b0000_0001); // width 1, 4bpp
        assert_eq!((m.dmacb, m.dmasize), (1, 0));
        // Both fields clamp like ares (`dmacb ≤ 2`, `dmasize ≤ 5`).
        m.write(make_addr(0x00, 0x2231), 0b0001_1111);
        assert_eq!((m.dmacb, m.dmasize), (2, 5));
    }

    #[test]
    fn cc1_converts_on_the_scpu_read_not_at_the_trigger() {
        // Hardware converts one character at a time, when the S-CPU reads
        // it, and answers out of I-RAM at DDA (ares `dmaCC1Read`). Before
        // the first read, nothing has been converted.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80); // SBWE: the S-CPU seeds BW-RAM
        // 2bpp, 1 character wide: 8 source bytes, one per row. Row y has
        // pixel bits taken LSB-first, so 0x03 = the two low planes set for
        // the leftmost four pixels of the row.
        for y in 0..8u16 {
            m.write(make_addr(0x40, y), 0xFF);
        }
        cc1_setup(&mut m, 0b0000_0010, 0x3000); // dmacb = 2 (2bpp), width 1
        assert!(m.bwram_dma, "the $2236 write arms the conversion");
        assert!(m.cc1_irq_to_main, "and raises the char-conversion IRQ");
        assert_eq!(
            m.iram[0], 0,
            "nothing is converted until the S-CPU reads BW-RAM"
        );
        // First read of the character converts it and returns byte 0.
        let first = m.read(make_addr(0x40, 0)).unwrap();
        assert_eq!(first, 0xFF, "plane 0 of a row of colour-3 pixels");
        assert_eq!(m.iram[1], 0xFF, "plane 1 of the same row");
        // CDEND ($2231 bit 7) disarms it: reads go back to raw BW-RAM.
        m.write(make_addr(0x00, 0x2231), 0x80);
        assert!(!m.bwram_dma);
        assert_eq!(m.read(make_addr(0x40, 0)), Some(0xFF), "raw byte again");
    }

    #[test]
    fn cc1_planar_layout_matches_the_ares_byte_map() {
        // The `((byte & 6) << 3) + (byte & 1)` map puts the 8 planes of a
        // row at {0,1,16,17,32,33,48,49} — verified here in 4bpp, where a
        // row of colour 5 lights planes 0 and 2 only.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80); // SBWE: the S-CPU seeds BW-RAM
        for i in 0..32u16 {
            // 4bpp packs 2 pixels per byte; 0x55 = colour 5 twice.
            m.write(make_addr(0x40, i), 0x55);
        }
        cc1_setup(&mut m, 0b0000_0001, 0x3000); // dmacb = 1 (4bpp), width 1
        let _ = m.read(make_addr(0x40, 0));
        assert_eq!(m.iram[0], 0xFF, "row 0, plane 0");
        assert_eq!(m.iram[1], 0x00, "row 0, plane 1");
        assert_eq!(m.iram[16], 0xFF, "row 0, plane 2 lives at +16");
        assert_eq!(m.iram[17], 0x00, "row 0, plane 3 at +17");
    }

    /// Arm a Type-2 conversion: CDMA, then DCNT with cden set and cdsel
    /// clear, then DDA. Rows are fed through BRF afterwards.
    fn cc2_setup(m: &mut Sa1Mapper, cdma: u8) {
        m.write(make_addr(0x00, 0x2231), cdma);
        m.write_from_sa1(make_addr(0x00, 0x2230), 0xA0); // enable + CC, cdsel = 0
        m.write(make_addr(0x00, 0x2235), 0x00); // DDA = $00:3000
        m.write(make_addr(0x00, 0x2237), 0x00);
        m.write(make_addr(0x00, 0x2236), 0x30);
    }

    #[test]
    fn cc2_converts_one_row_per_brf_half() {
        // ares `dmaCC2`: the SA-1 writes 8 pixels into BRF[0..7], and the
        // write to BRF[7] ($2247) converts that row into I-RAM. luna used
        // to ignore the register file entirely and treat the bytes as
        // packed pixels.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0001); // 4bpp
        for (i, px) in [1u8, 0, 1, 0, 1, 0, 1, 0].iter().enumerate() {
            m.write_from_sa1(make_addr(0x00, 0x2240 + i as u16), *px);
        }
        assert_eq!(m.iram[0], 0xAA, "bit 0 of each pixel, MSB = pixel 0");
        assert_eq!(m.iram[1], 0x00, "plane 1: no pixel has bit 1 set");
        assert_eq!(m.cc2_line, 1, "the line counter advanced");

        // The second half (BRF[8..15], written through $224F) is row 1.
        for i in 8..16u16 {
            m.write_from_sa1(make_addr(0x00, 0x2240 + i), 0x02);
        }
        assert_eq!(m.iram[2], 0x00, "row 1, plane 0");
        assert_eq!(m.iram[3], 0xFF, "row 1, plane 1 — every pixel has bit 1");
        assert_eq!(m.cc2_line, 2);
    }

    #[test]
    fn cc2_line_counter_resets_when_dma_enable_drops() {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        cc2_setup(&mut m, 0b0000_0001);
        for i in 0..8u16 {
            m.write_from_sa1(make_addr(0x00, 0x2240 + i), 0x01);
        }
        assert_eq!(m.cc2_line, 1);
        m.write_from_sa1(make_addr(0x00, 0x2230), 0x00); // DMA enable off
        assert_eq!(m.cc2_line, 0, "ares io.cpp:327");
    }

    // ------------- Phase-5 VLBP tests -------------

    /// Helper — write a `vlen + mode` to VBD and point VDA at the
    /// linear BW-RAM origin (`$40:0000`).
    /// A mapper with `[0xAB, 0xCD, 0xEF, 0x12]` at the start of BW-RAM and
    /// VDA pointing at it (`$40:0000` on the VLBP's own bus is BW-RAM
    /// byte 0).
    fn vlbp_mapper() -> Sa1Mapper {
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        for (i, b) in [0xAB, 0xCD, 0xEF, 0x12].into_iter().enumerate() {
            m.write(make_addr(0x40, i as u16), b);
        }
        m.write_from_sa1(make_addr(0x00, 0x2259), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x225A), 0x00);
        m.write_from_sa1(make_addr(0x00, 0x225B), 0x40);
        m
    }

    fn vdpl(m: &mut Sa1Mapper) -> u8 {
        m.read_from_sa1(make_addr(0x00, 0x230C)).unwrap()
    }

    fn vdph(m: &mut Sa1Mapper) -> u8 {
        m.read_from_sa1(make_addr(0x00, 0x230D)).unwrap()
    }

    #[test]
    fn vlbp_fixed_mode_advances_on_the_vbd_write_not_on_reads() {
        // ares `io.cpp:433-438`: with bit 7 clear the `$2258` write itself
        // consumes `vb` bits; the ports only ever show the window at the
        // cursor, unmasked.
        let mut m = vlbp_mapper();
        assert_eq!(vdpl(&mut m), 0xAB);
        assert_eq!(vdpl(&mut m), 0xAB, "reads never advance in fixed mode");
        m.write_from_sa1(make_addr(0x00, 0x2258), 0x04); // vb = 4: consume 4 bits
        assert_eq!(vdpl(&mut m), 0xDA, "the window shifted by 4: $CDAB >> 4");
        m.write_from_sa1(make_addr(0x00, 0x2258), 0x04);
        assert_eq!(vdpl(&mut m), 0xCD, "8 bits consumed: `va` moved a byte");
        assert_eq!((m.va, m.vbit), (0x40_0001, 0));
        m.write_from_sa1(make_addr(0x00, 0x2258), 0x00); // vb = 0 means 16
        assert_eq!(vdpl(&mut m), 0x12);
        assert_eq!(m.va, 0x40_0003);
    }

    #[test]
    fn vlbp_auto_increment_mode_advances_on_the_high_byte_read() {
        // ares `io.cpp:81-86`: with bit 7 set the `$230D` read consumes
        // `vb` bits; `$230C` is free to re-read.
        let mut m = vlbp_mapper();
        m.write_from_sa1(make_addr(0x00, 0x2258), 0x88); // hl = 1, vb = 8
        assert_eq!((m.va, m.vbit), (0x40_0000, 0), "the write did not advance");
        assert_eq!(vdpl(&mut m), 0xAB);
        assert_eq!(vdpl(&mut m), 0xAB);
        assert_eq!(vdph(&mut m), 0xCD, "the high byte, then the cursor moves");
        assert_eq!(vdpl(&mut m), 0xCD);
        assert_eq!(vdph(&mut m), 0xEF);
        assert_eq!(m.va, 0x40_0002);
    }

    #[test]
    fn vlbp_high_address_byte_write_resets_the_bit_cursor() {
        let mut m = vlbp_mapper();
        m.write_from_sa1(make_addr(0x00, 0x2258), 0x03); // consume 3 bits
        assert_eq!(m.vbit, 3);
        m.write_from_sa1(make_addr(0x00, 0x225B), 0x40); // VDA high: vbit = 0
        assert_eq!((m.va, m.vbit), (0x40_0000, 0));
        assert_eq!(vdpl(&mut m), 0xAB);
    }

    #[test]
    fn vlbp_reads_rom_through_the_sa1_map_and_never_the_registers() {
        // ares `memory.cpp:113-133` (`readVBR`): ROM by the SA-1's
        // mapping, I-RAM raw, and never an I/O port — a VDA inside the
        // register window reads `$FF`.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write_from_sa1(make_addr(0x00, 0x2259), 0x10);
        m.write_from_sa1(make_addr(0x00, 0x225A), 0x80);
        m.write_from_sa1(make_addr(0x00, 0x225B), 0x00); // $00:8010 = ROM[$10]
        assert_eq!(vdpl(&mut m), 0x10);
        assert_eq!(vdph(&mut m), 0x11);
        m.write(make_addr(0x00, 0x3005), 0x5A);
        m.write_from_sa1(make_addr(0x00, 0x2259), 0x05);
        m.write_from_sa1(make_addr(0x00, 0x225A), 0x30);
        m.write_from_sa1(make_addr(0x00, 0x225B), 0x00); // $00:3005 = I-RAM
        assert_eq!(vdpl(&mut m), 0x5A);
        m.write_from_sa1(make_addr(0x00, 0x225A), 0x23); // $00:2305 = a register
        m.write_from_sa1(make_addr(0x00, 0x225B), 0x00);
        assert_eq!(vdpl(&mut m), 0xFF);
        // The ports belong to the SA-1: the S-CPU reads open bus.
        assert_eq!(m.read(make_addr(0x00, 0x230C)), None);
    }

    // ------------- BW-RAM bitmap projection (ares `bwram.cpp`) -------------

    #[test]
    fn sa1_reads_bwram_as_pixels_through_banks_60_to_6f() {
        // `$60-$6F` is the bitmap projection: one pixel per address, two a
        // byte at 4 bpp (low nibble first), four a byte at 2 bpp.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0000), 0x21);
        m.write(make_addr(0x40, 0x0001), 0xE4);
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0000)), Some(0x1));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0001)), Some(0x2));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0002)), Some(0x4));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0003)), Some(0xE));
        // A pixel write is a read-modify-write of its byte.
        m.write_from_sa1(make_addr(0x60, 0x0001), 0xFF);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0xF1));
        // BBF bit 7: 2 bpp.
        m.write_from_sa1(make_addr(0x00, 0x223F), 0x80);
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0004)), Some(0b00));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0005)), Some(0b01));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0006)), Some(0b10));
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x0007)), Some(0b11));
        m.write_from_sa1(make_addr(0x60, 0x0004), 0x03);
        assert_eq!(m.read(make_addr(0x40, 0x0001)), Some(0xE7));
        // Pixel space is 20 bits: bank $61 continues where $60 ends.
        m.write(make_addr(0x40, 0x4000), 0x39);
        m.write_from_sa1(make_addr(0x00, 0x223F), 0x00);
        assert_eq!(m.read_from_sa1(make_addr(0x60, 0x8000)), Some(0x9));
        // The S-CPU has no such view.
        assert_eq!(m.read(make_addr(0x60, 0x0000)), None);
        // A BW-RAM access either way, for the SA-1's cycle cost.
        assert_eq!(m.sa1_region_steps(make_addr(0x60, 0x0000)), 2);
    }

    #[test]
    fn cbm_bit_7_turns_the_sa1_window_into_a_bitmap_page() {
        // ares `bwram.cpp:45-67`: with `sw46` clear the `$6000-$7FFF`
        // window is linear page `CBM & $1F`; with it set the window is
        // bitmap page `CBM & $7F` — 8 KB of pixels, i.e. 4 KB of bytes at
        // 4 bpp.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x2000), 0x5A); // linear page 1, byte 0
        m.write(make_addr(0x40, 0x1000), 0x34); // pixel page 1 = bytes $1000..
        m.write_from_sa1(make_addr(0x00, 0x2225), 0x01);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x6000)), Some(0x5A));
        m.write_from_sa1(make_addr(0x00, 0x2225), 0x81);
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x6000)), Some(0x4));
        assert_eq!(m.read_from_sa1(make_addr(0x00, 0x6001)), Some(0x3));
        m.write_from_sa1(make_addr(0x00, 0x6000), 0x0C);
        assert_eq!(m.read(make_addr(0x40, 0x1000)), Some(0x3C));
        // The S-CPU's window keeps its own page register and stays linear.
        m.write(make_addr(0x00, 0x2224), 0x01);
        assert_eq!(m.read(make_addr(0x00, 0x6000)), Some(0x5A));
    }

    #[test]
    fn sa1_linear_bwram_spans_banks_40_to_5f() {
        // ares `memory.cpp:40`: the SA-1's linear view is `$40-$5F`,
        // mirrored into the array; the S-CPU's stops at `$4F`.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0123), 0x77);
        assert_eq!(m.read_from_sa1(make_addr(0x50, 0x0123)), Some(0x77));
        assert_eq!(m.read_from_sa1(make_addr(0x5F, 0x0123)), Some(0x77));
        assert_eq!(m.read(make_addr(0x50, 0x0123)), None);
        m.write_from_sa1(make_addr(0x51, 0x0123), 0x88);
        assert_eq!(m.read(make_addr(0x40, 0x0123)), Some(0x88));
    }

    // ------------- Phase-5 write-protection tests -------------

    #[test]
    fn bwram_write_passes_when_either_enable_is_set() {
        // Per ares (`coprocessor/sa1/bwram.cpp:40-43, 73-84`) and
        // Mesen2 (`CpuBwRamHandler.h:45-57`): the gate is the OR of
        // SBWE and CBWE — either side's enable lets both sides write.
        // Power-on has both clear, so the first write is refused.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        m.write(make_addr(0x40, 0x0100), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0100)), Some(0x00));
        // The SA-1 enables its side: the S-CPU's write lands too.
        m.write_from_sa1(make_addr(0x00, 0x2227), 0x80);
        m.write(make_addr(0x40, 0x0100), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0100)), Some(0xAA));
        // Swap: only SBWE set, the SA-1's write lands.
        m.write_from_sa1(make_addr(0x00, 0x2227), 0x00);
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write_from_sa1(make_addr(0x40, 0x0101), 0xBB);
        assert_eq!(m.read(make_addr(0x40, 0x0101)), Some(0xBB));
        // Both clear again: BWPA's zone (all of it at the $0F default)
        // refuses both sides.
        m.write(make_addr(0x00, 0x2226), 0x00);
        m.write(make_addr(0x40, 0x0040), 0xCC);
        m.write_from_sa1(make_addr(0x40, 0x0041), 0xCC);
        assert_eq!(m.read(make_addr(0x40, 0x0040)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 0x0041)), Some(0x00));
    }

    #[test]
    fn bwpa_protects_first_n_pages_only_when_both_enables_disabled() {
        // Per ares/Mesen2: BWPA's first-block protect zone applies
        // only while SBWE AND CBWE are both clear; with either enable
        // set, BWPA is inert.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        // BWPA = 1 → prot_bytes = 0x100 << 1 = 512 bytes.
        m.write(make_addr(0x00, 0x2228), 0x01);
        m.write(make_addr(0x40, 0x0000), 0xAA); // inside → blocked
        m.write(make_addr(0x40, 0x0200), 0xBB); // outside → lands
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0x00));
        assert_eq!(m.read(make_addr(0x40, 0x0200)), Some(0xBB));
        // Enable either side → BWPA goes inert, protected slot writes.
        m.write(make_addr(0x00, 0x2226), 0x80);
        m.write(make_addr(0x40, 0x0000), 0xDD);
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0xDD));
    }

    #[test]
    fn sa1_side_bwram_write_uses_the_same_or_of_enables_gate() {
        // The SA-1's linear writes pass the same gate; only its own CBWE
        // (`$2227`, an SA-1-side register) or the S-CPU's SBWE opens it.
        let mut m = Sa1Mapper::new(ramp_rom(0x1_0000), 0x10000);
        assert!(m.write_from_sa1(make_addr(0x40, 0x0010), 0xAA), "claimed");
        assert_eq!(m.read(make_addr(0x40, 0x0010)), Some(0x00), "refused");
        m.write(make_addr(0x00, 0x2227), 0x80); // from the S-CPU: not its register
        m.write_from_sa1(make_addr(0x40, 0x0010), 0xAA);
        assert_eq!(m.read(make_addr(0x40, 0x0010)), Some(0x00));
        m.write_from_sa1(make_addr(0x00, 0x2227), 0x80);
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
        m.write_from_sa1(make_addr(0x00, 0x222A), 0x00);
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

    #[test]
    fn sa1_conflict_steps_charge_only_on_same_shared_resource() {
        // Address-only contention model (ares `conflict()`); mapper config
        // is irrelevant. Use a mapper with BW-RAM so the regions resolve.
        let m = Sa1Mapper::new(ramp_rom(0x20_0000), 0x10000);

        // S-CPU addresses representative of each shared resource + WRAM.
        let scpu_rom = make_addr(0x01, 0x9000); // 01:8000-ffff → ROM
        let scpu_rom_hi = make_addr(0xC5, 0x0000); // c0-ff → ROM
        let scpu_bwram = make_addr(0x40, 0x0000); // 40-4f:0000 → BW-RAM
        let scpu_iram = make_addr(0x00, 0x3100); // 00:3000-37ff → I-RAM
        let scpu_wram = make_addr(0x00, 0x0100); // low RAM → no resource

        // SA-1 ROM access: +1 iff the S-CPU also holds ROM.
        let sa1_rom = make_addr(0x00, 0x8000);
        assert_eq!(m.sa1_conflict_steps(sa1_rom, scpu_rom), 1);
        assert_eq!(m.sa1_conflict_steps(sa1_rom, scpu_rom_hi), 1);
        assert_eq!(m.sa1_conflict_steps(sa1_rom, scpu_bwram), 0);
        assert_eq!(m.sa1_conflict_steps(sa1_rom, scpu_wram), 0);

        // SA-1 BW-RAM access: +2 iff the S-CPU also holds BW-RAM.
        let sa1_bwram = make_addr(0x00, 0x6000);
        assert_eq!(m.sa1_conflict_steps(sa1_bwram, scpu_bwram), 2);
        assert_eq!(m.sa1_conflict_steps(sa1_bwram, scpu_rom), 0);
        assert_eq!(m.sa1_conflict_steps(sa1_bwram, scpu_iram), 0);

        // SA-1 I-RAM access: +2 iff the S-CPU also holds I-RAM (the
        // $3000-37ff window only — the $0000-07ff mirror does not count,
        // faithful to ares `iram.cpp`).
        let sa1_iram = make_addr(0x00, 0x3000);
        assert_eq!(m.sa1_conflict_steps(sa1_iram, scpu_iram), 2);
        assert_eq!(m.sa1_conflict_steps(sa1_iram, scpu_wram), 0);
        assert_eq!(m.sa1_conflict_steps(sa1_iram, scpu_rom), 0);

        // MMIO ($2200-23ff) never contends, whatever the S-CPU holds.
        let sa1_mmio = make_addr(0x00, 0x2200);
        assert_eq!(m.sa1_conflict_steps(sa1_mmio, scpu_rom), 0);
        assert_eq!(m.sa1_conflict_steps(sa1_mmio, scpu_iram), 0);
    }
}
