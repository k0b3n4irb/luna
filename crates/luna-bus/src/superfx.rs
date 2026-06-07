//! Super FX (GSU — Graphics Support Unit) cartridge mapping + coprocessor.
//!
//! Unlike the SA-1 (which embeds a second 65C816 and therefore needs a
//! `luna-core`-side chip wrapper), the GSU is a self-contained bespoke
//! RISC core with no external CPU dependency. The entire chip — register
//! file, instruction-cache RAM, the SNES↔GSU memory map and the MMIO
//! handshake — lives here in one [`SuperFxMapper`], driven by the existing
//! [`Mapper::step_coproc`] hook.
//!
//! This is the **scaffolding phase**: memory map, the register/MMIO
//! surface, the GO / STOP / IRQ handshake, and a diagnostic snapshot. The
//! GSU *instruction engine* (the opcode interpreter), the ROM/RAM buffer
//! timing and the pixel-plot pipeline land in later phases — `step_coproc`
//! is a no-op for now, so a game can arm the GSU and the SNES side behaves
//! correctly, but the GSU executes no opcodes yet.
//!
//! Reference: `docs/superfx_reference.md` (synthesised ares + Mesen2 spec).
//! Citations like `(spec §1.9)` index that document; ultimate sources are
//! ares `ares/sfc/coprocessor/superfx/*` + `ares/component/processor/gsu/*`
//! and Mesen2 `Core/SNES/Coprocessors/GSU`.

use crate::mapper::{Mapper, MapperKind, SuperFxTraceEvent};
use crate::types::{Addr24, bank_of, offset_of};

// --- SFR (Status Flag Register) bit masks (spec §1.2) ---------------------
const SFR_Z: u16 = 1 << 1; // zero
const SFR_CY: u16 = 1 << 2; // carry
const SFR_S: u16 = 1 << 3; // sign
const SFR_OV: u16 = 1 << 4; // overflow
const SFR_G: u16 = 1 << 5; // go (GSU running)
const SFR_R: u16 = 1 << 6; // ROM-read pending (R14 fetch in flight)
const SFR_ALT1: u16 = 1 << 8; // ALT1 prefix mode
const SFR_ALT2: u16 = 1 << 9; // ALT2 prefix mode
const SFR_B: u16 = 1 << 12; // "with" prefix flag
const SFR_IRQ: u16 = 1 << 15; // interrupt asserted to SNES

/// The GSU register file (spec §1). Owned by [`SuperFxMapper`]; MMIO at
/// `$3000-$303F` reads/writes it, and the engine phase will consume it.
#[derive(Debug)]
struct Registers {
    /// R0–R15. R14 = ROM-load trigger, R15 = PC (spec §1.1).
    r: [u16; 16],
    /// Status flags — raw 16-bit; see the `SFR_*` masks.
    sfr: u16,
    /// Program Bank Register — bank for PC fetches (spec §1.3).
    pbr: u8,
    /// ROM data Bank Register — bank for the R14 ROM-buffer fetch.
    rombr: u8,
    /// RAM data Bank Register — 1-bit RAM bank for LD/ST buffer ops.
    rambr: bool,
    /// Cache Base Register — base PC of the 512-byte cache window.
    cbr: u16,
    /// Screen Base Register — Game Pak RAM tile-map base (`scbr << 10`).
    scbr: u8,
    /// SCMR height select (2 bits, scrambled wire layout — spec §1.4).
    scmr_ht: u8,
    /// SCMR ROM-access grant (1 = GSU owns ROM, SNES locked out).
    scmr_ron: bool,
    /// SCMR RAM-access grant (1 = GSU owns RAM, SNES locked out).
    scmr_ran: bool,
    /// SCMR colour-depth mode (0..3 → bpp 2/4/4/8).
    scmr_md: u8,
    /// Colour register (PLOT colour).
    colr: u8,
    /// Plot Option Register — raw 5 bits (decoded by the plot phase).
    por: u8,
    /// Backup-RAM write enable ($3033).
    bramr: bool,
    /// Version Code Register (read-only $303B, reset 0x04).
    vcr: u8,
    /// Config register — raw byte; bit7 = IRQ mask, bit5 = high-speed mult.
    cfgr: u8,
    /// Clock Select — false = 10.7 MHz, true = 21.4 MHz.
    clsr: bool,
    // --- engine-only state (consumed by the instruction interpreter) -----
    /// The 1-stage prefetched opcode (reset 0x01 = NOP) (spec §2.3).
    pipeline: u8,
    /// Source register selector for the next ALU/move op (spec §2.2).
    sreg: usize,
    /// Destination register selector for the next ALU/move op (spec §2.2).
    dreg: usize,
    /// Last RAM address used by LD/ST (for SBK) (spec §1.7).
    ramaddr: u16,
    /// ROM buffer: clocks until `romdr` is valid, and the data latch (§6.2).
    romcl: u32,
    romdr: u8,
    /// RAM buffer (delayed write): clocks pending, address, data (§6.3).
    ramcl: u32,
    ramar: u16,
    ramdr: u8,
}

impl Registers {
    /// Power / reset state (spec §1.8).
    const fn reset() -> Self {
        Self {
            r: [0; 16],
            sfr: 0,
            pbr: 0,
            rombr: 0,
            rambr: false,
            cbr: 0,
            scbr: 0,
            scmr_ht: 0,
            scmr_ron: false,
            scmr_ran: false,
            scmr_md: 0,
            colr: 0,
            por: 0,
            bramr: false,
            vcr: 0x04,
            cfgr: 0,
            clsr: false,
            pipeline: 0x01,
            sreg: 0,
            dreg: 0,
            ramaddr: 0,
            romcl: 0,
            romdr: 0,
            ramcl: 0,
            ramar: 0,
            ramdr: 0,
        }
    }

    /// Clear the per-instruction prefix state (spec §2.1): `b`, `alt1`,
    /// `alt2` flags and the source/dest selectors. Every *non-prefix*
    /// instruction ends with this, so the ALT/WITH prefixes are consumed by
    /// exactly one following op.
    const fn reset_prefix(&mut self) {
        self.sfr &= !(SFR_B | SFR_ALT1 | SFR_ALT2);
        self.sreg = 0;
        self.dreg = 0;
    }

    /// SCMR wire byte ← fields (spec §1.4): `bit5=ht_hi, bit4=ron,
    /// bit3=ran, bit2=ht_lo, bits1:0=md`.
    const fn scmr_byte(&self) -> u8 {
        ((self.scmr_ht >> 1) << 5)
            | ((self.scmr_ron as u8) << 4)
            | ((self.scmr_ran as u8) << 3)
            | ((self.scmr_ht & 1) << 2)
            | self.scmr_md
    }

    /// SCMR fields ← wire byte (spec §1.4, the scrambled `ht`).
    const fn set_scmr(&mut self, data: u8) {
        self.scmr_ht = (((data & 0x20 != 0) as u8) << 1) | (data & 0x04 != 0) as u8;
        self.scmr_ron = data & 0x10 != 0;
        self.scmr_ran = data & 0x08 != 0;
        self.scmr_md = data & 0x03;
    }
}

/// Diagnostic snapshot of the GSU's architectural registers — the Super FX
/// analogue of [`crate::Sa1Snapshot`]. Lets the CLI / GUI debugger observe
/// GSU state through the same kind of surface SA-1 already exposes.
#[derive(Debug, Clone, Copy)]
pub struct SuperFxSnapshot {
    /// R0–R15 (R15 = PC).
    pub r: [u16; 16],
    /// Status flags (raw 16-bit).
    pub sfr: u16,
    /// Program bank register.
    pub pbr: u8,
    /// ROM data bank register.
    pub rombr: u8,
    /// RAM data bank register (1-bit).
    pub rambr: bool,
    /// Cache base register.
    pub cbr: u16,
    /// Screen base register.
    pub scbr: u8,
    /// Screen-mode register, packed wire byte.
    pub scmr: u8,
    /// Colour register.
    pub colr: u8,
    /// Plot-option register (raw 5 bits).
    pub por: u8,
    /// Backup-RAM write enable.
    pub bramr: bool,
    /// Version code register.
    pub vcr: u8,
    /// Config register (raw byte).
    pub cfgr: u8,
    /// Clock select (false = 10.7 MHz, true = 21.4 MHz).
    pub clsr: bool,
    /// `true` while the GSU is running (SFR go flag set).
    pub running: bool,
}

/// One 8-pixel plot run awaiting writeback to Game Pak RAM (spec §4.1).
/// `offset` = `(y << 5) + (x >> 3)` (the 8-pixel tile-row slot), `bitpend`
/// = per-x written-bit mask, `data` = the colour index per x in the run.
#[derive(Debug, Clone, Copy)]
struct PixelCache {
    offset: u16,
    bitpend: u8,
    data: [u8; 8],
}

impl PixelCache {
    /// Power state: offset = ~0, nothing pending (ares superfx.cpp:73-76).
    const fn reset() -> Self {
        Self {
            offset: 0xFFFF,
            bitpend: 0,
            data: [0; 8],
        }
    }
}

/// Super FX (GSU) mapper + coprocessor state.
pub struct SuperFxMapper {
    /// ROM image, zero-padded up to `rom_mask + 1` (a power of two) so
    /// `offset & rom_mask` always lands in bounds and mirrors cleanly
    /// (spec §3.1: ares rounds the ROM size up to a power of two).
    rom: Vec<u8>,
    /// Original (unpadded) ROM length, reported by [`Mapper::rom_size`].
    rom_len: usize,
    /// Power-of-two mask over the padded ROM image.
    rom_mask: usize,
    /// Game Pak work RAM (the GSU's plot target, shared with the SNES);
    /// allocated to `ram_mask + 1` (a power of two).
    ram: Vec<u8>,
    /// Original (unpadded) RAM length, reported by [`Mapper::sram_size`].
    ram_len: usize,
    ram_mask: usize,
    /// Register file.
    regs: Registers,
    /// 512-byte instruction cache + 32 line-valid flags (spec §5).
    cache: Box<[u8; 512]>,
    cache_valid: [bool; 32],
    /// Primary (`[0]`) + secondary (`[1]`) pixel-plot caches (spec §4.1).
    pixelcache: [PixelCache; 2],
    /// GSU-clock budget carried across `step_coproc` calls. The engine runs
    /// instructions while this is positive, deducting each op's cycle cost.
    clock_deficit: i64,
    /// Per-instruction cycle accumulator (set by `step`), read by `run_one`.
    cycles: u32,
    /// Did the just-executed instruction write R14 / R15? Drives the R14
    /// ROM-buffer re-arm and the R15 post-increment (spec §1.1, ares
    /// superfx.cpp:35-44).
    modified_r14: bool,
    modified_r15: bool,
    /// Optional per-opcode trace: `(events, max_events)`. A ring buffer
    /// (drops the oldest half when full) so a long run captures the GSU's
    /// *current* activity. Enabled via [`Mapper::enable_superfx_trace`].
    trace: Option<(Vec<SuperFxTraceEvent>, usize)>,
}

impl SuperFxMapper {
    /// Build a Super FX mapper around `rom`, with `ram_size` bytes of
    /// Game Pak work RAM.
    #[must_use]
    pub fn new(rom: Vec<u8>, ram_size: usize) -> Self {
        let rom_len = rom.len();
        let rom_alloc = round_up_pow2(rom_len);
        let mut rom = rom;
        rom.resize(rom_alloc, 0);
        let ram_len = ram_size.max(1);
        let ram_alloc = round_up_pow2(ram_len);
        Self {
            rom,
            rom_len,
            rom_mask: rom_alloc - 1,
            ram: vec![0; ram_alloc],
            ram_len,
            ram_mask: ram_alloc - 1,
            regs: Registers::reset(),
            cache: Box::new([0; 512]),
            cache_valid: [false; 32],
            pixelcache: [PixelCache::reset(); 2],
            clock_deficit: 0,
            cycles: 0,
            modified_r14: false,
            modified_r15: false,
            trace: None,
        }
    }

    /// Diagnostic snapshot of the GSU's architectural registers.
    #[must_use]
    pub const fn snapshot(&self) -> SuperFxSnapshot {
        SuperFxSnapshot {
            r: self.regs.r,
            sfr: self.regs.sfr,
            pbr: self.regs.pbr,
            rombr: self.regs.rombr,
            rambr: self.regs.rambr,
            cbr: self.regs.cbr,
            scbr: self.regs.scbr,
            scmr: self.regs.scmr_byte(),
            colr: self.regs.colr,
            por: self.regs.por,
            bramr: self.regs.bramr,
            vcr: self.regs.vcr,
            cfgr: self.regs.cfgr,
            clsr: self.regs.clsr,
            running: self.regs.sfr & SFR_G != 0,
        }
    }

    // --- SFR helpers ------------------------------------------------------
    const fn sfr_get(&self, mask: u16) -> bool {
        self.regs.sfr & mask != 0
    }
    const fn sfr_set(&mut self, mask: u16, on: bool) {
        if on {
            self.regs.sfr |= mask;
        } else {
            self.regs.sfr &= !mask;
        }
    }

    /// Invalidate every instruction-cache line (spec §5, `flushCache`).
    const fn flush_cache(&mut self) {
        self.cache_valid = [false; 32];
    }

    // --- SNES-side ROM / RAM address translation (spec §3.2) -------------

    /// ROM offset for a SNES-CPU access, or `None` if the address is not
    /// in a Super FX ROM window. Two views of the same image:
    /// `$00-$3F/$80-$BF:$8000-$FFFF` `LoROM`, `$40-$5F/$C0-$DF:$0000-$FFFF`
    /// linear (spec §3.2).
    const fn rom_offset(bank: u8, offset: u16) -> Option<usize> {
        let b = bank & 0x7F; // fold $80-$FF mirror onto $00-$7F
        if b <= 0x3F && offset >= 0x8000 {
            Some((b as usize * 0x8000) + (offset as usize - 0x8000))
        } else if 0x40 <= b && b <= 0x5F {
            Some((b as usize - 0x40) * 0x10000 + offset as usize)
        } else {
            None
        }
    }

    /// RAM offset for a SNES-CPU access (spec §3.2): the linear window at
    /// `$70-$71/$F0-$F1:$0000-$FFFF` and the `$00-$3F/$80-$BF:$6000-$7FFF`
    /// 8 KB window.
    const fn ram_offset(bank: u8, offset: u16) -> Option<usize> {
        let b = bank & 0x7F;
        if 0x70 <= b && b <= 0x71 {
            Some((b as usize - 0x70) * 0x10000 + offset as usize)
        } else if b <= 0x3F && 0x6000 <= offset && offset <= 0x7FFF {
            Some(offset as usize - 0x6000)
        } else {
            None
        }
    }

    /// Is `addr` inside the GSU MMIO window `$00-$3F/$80-$BF:$3000-$34FF`?
    /// Returns the normalised internal address `$3000 | (off & 0x3FF)`
    /// (spec §3.2: ares mirrors every 0x400).
    const fn mmio_addr(bank: u8, offset: u16) -> Option<u16> {
        let b = bank & 0x7F;
        if b <= 0x3F && 0x3000 <= offset && offset <= 0x34FF {
            Some(0x3000 | (offset & 0x03FF))
        } else {
            None
        }
    }

    /// The fixed 16-byte "GSU busy" vector the SNES sees in place of ROM
    /// while the GSU is running and owns ROM access (spec §3.3,
    /// ares bus.cpp:11-20).
    const fn busy_rom_vector(addr_low: u8) -> u8 {
        const VECTOR: [u8; 16] = [
            0x00, 0x01, 0x00, 0x01, 0x04, 0x01, 0x00, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0x01,
            0x0C, 0x01,
        ];
        VECTOR[(addr_low & 0x0F) as usize]
    }

    // --- MMIO register file (spec §1.9) ----------------------------------

    /// Read a GSU MMIO register at the normalised address `a`.
    fn mmio_read(&mut self, a: u16) -> u8 {
        // Instruction-cache RAM window $3100-$32FF (spec §1.9, §5).
        if (0x3100..=0x32FF).contains(&a) {
            let idx = (usize::from(a - 0x3100) + usize::from(self.regs.cbr)) & 511;
            return self.cache[idx];
        }
        // R0-R15 little-endian byte pairs $3000-$301F.
        if (0x3000..=0x301F).contains(&a) {
            let reg = (a >> 1) & 15;
            let shift = (a & 1) * 8;
            return (self.regs.r[reg as usize] >> shift) as u8;
        }
        match a {
            0x3030 => self.regs.sfr as u8,
            0x3031 => {
                let hi = (self.regs.sfr >> 8) as u8;
                // Reading SFR high byte acknowledges the IRQ (spec §7.3).
                self.sfr_set(SFR_IRQ, false);
                hi
            }
            0x3034 => self.regs.pbr,
            0x3036 => self.regs.rombr,
            0x303B => self.regs.vcr,
            0x303C => u8::from(self.regs.rambr),
            0x303E => self.regs.cbr as u8,
            0x303F => (self.regs.cbr >> 8) as u8,
            _ => 0x00,
        }
    }

    /// Write a GSU MMIO register at the normalised address `a`.
    fn mmio_write(&mut self, a: u16, data: u8) {
        // Instruction-cache RAM window $3100-$32FF: writing the 16th byte
        // of a line validates it (spec §5, lets the SNES preload code).
        if (0x3100..=0x32FF).contains(&a) {
            let idx = (usize::from(a - 0x3100) + usize::from(self.regs.cbr)) & 511;
            self.cache[idx] = data;
            if (a & 0x0F) == 0x0F {
                self.cache_valid[(idx >> 4) & 31] = true;
            }
            return;
        }
        // R0-R15 byte writes $3000-$301F.
        if (0x3000..=0x301F).contains(&a) {
            let reg = ((a >> 1) & 15) as usize;
            if a & 1 == 0 {
                self.regs.r[reg] = (self.regs.r[reg] & 0xFF00) | u16::from(data);
            } else {
                self.regs.r[reg] = (u16::from(data) << 8) | (self.regs.r[reg] & 0x00FF);
            }
            if reg == 14 {
                // Writing R14 from the SNES arms the ROM-buffer fetch
                // (spec §6.2, ares io.cpp:68).
                self.update_rom_buffer();
            }
            // Writing R15 high byte ($301F) launches the GSU (spec §7.1).
            if a == 0x301F {
                self.sfr_set(SFR_G, true);
            }
            return;
        }
        match a {
            0x3030 => {
                let was_go = self.sfr_get(SFR_G);
                self.regs.sfr = (self.regs.sfr & 0xFF00) | u16::from(data);
                // g 1→0 transition: clear CBR + flush cache (spec §7.2).
                if was_go && !self.sfr_get(SFR_G) {
                    self.regs.cbr = 0x0000;
                    self.flush_cache();
                }
            }
            0x3031 => self.regs.sfr = (u16::from(data) << 8) | (self.regs.sfr & 0x00FF),
            0x3033 => self.regs.bramr = data & 0x01 != 0,
            0x3034 => {
                self.regs.pbr = data & 0x7F;
                self.flush_cache();
            }
            0x3037 => self.regs.cfgr = data,
            0x3038 => self.regs.scbr = data,
            0x3039 => self.regs.clsr = data & 0x01 != 0,
            0x303A => self.regs.set_scmr(data),
            _ => {}
        }
    }
}

/// Round `n` up to the next power of two (≥ 1).
const fn round_up_pow2(n: usize) -> usize {
    if n <= 1 {
        return 1;
    }
    let bits = usize::BITS - (n - 1).leading_zeros();
    1usize << bits
}

// --- POR (Plot Option Register) bits (spec §1.5) --------------------------
const POR_TRANSPARENT: u8 = 1 << 0; // plot even color-0 (transparent) pixels
const POR_DITHER: u8 = 1 << 1; // dither by (x^y)&1 (non-8bpp)
const POR_HIGHNIBBLE: u8 = 1 << 2;
const POR_FREEZEHIGH: u8 = 1 << 3;
const POR_OBJ: u8 = 1 << 4; // OBJ mode — forces the ht==3 tile layout

// ===========================================================================
// GSU instruction engine — phase 2a
//
// The execution spine: fetch / pipeline / instruction-cache, the ALT1/2/3
// prefix mechanism, the TO/FROM/WITH register-select prefixes, the ALU +
// shift ops, branches, and control flow (NOP/STOP/CACHE/LOOP/LINK/JMP/LJMP)
// plus COLOR/CMODE (register-only). The memory ops (LD/ST + RAM/ROM
// buffers), the MULT family and the immediate loads land in phase 2b; the
// pixel-plot pipeline (PLOT/RPIX) in phase 3. Those opcodes are dispatched
// here to operand-aligned placeholders so the pipeline never desyncs.
//
// Semantics are a direct port of ares `component/processor/gsu` +
// `sfc/coprocessor/superfx` (see docs/superfx_reference.md §2, §6).
// ===========================================================================
impl SuperFxMapper {
    /// Source register value (`sr` = `r[sreg]`, spec §2.2).
    const fn sr(&self) -> u16 {
        self.regs.r[self.regs.sreg]
    }

    /// Write `val` to `r[n]`, flagging R14/R15 writes for the post-step
    /// ROM-buffer re-arm / PC-increment logic (spec §1.1).
    const fn write_r(&mut self, n: usize, val: u16) {
        self.regs.r[n] = val;
        if n == 14 {
            self.modified_r14 = true;
        }
        if n == 15 {
            self.modified_r15 = true;
        }
    }

    /// Write the destination register `dr` = `r[dreg]`.
    const fn write_dr(&mut self, val: u16) {
        self.write_r(self.regs.dreg, val);
    }

    /// Set the S (sign, bit 15) and Z (zero) flags from a 16-bit result.
    const fn set_sz(&mut self, val: u16) {
        self.sfr_set(SFR_S, val & 0x8000 != 0);
        self.sfr_set(SFR_Z, val == 0);
    }

    /// `true` when CFGR's IRQ-mask bit (bit 7) is set, suppressing the
    /// STOP IRQ (spec §1.6, §7.3 — note the inverted sense).
    const fn cfgr_irq_masked(&self) -> bool {
        self.regs.cfgr & 0x80 != 0
    }

    /// Memory cycle cost (F5 / S6) for the current clock select (spec §6.1).
    const fn mem_cycles(&self) -> u32 {
        if self.regs.clsr { 5 } else { 6 }
    }
    /// Cache-hit fetch cost (F1 / S2).
    const fn hit_cycles(&self) -> u32 {
        if self.regs.clsr { 1 } else { 2 }
    }

    /// Resolve [`color`](Self::color) of a source byte using COLR + POR
    /// (spec §4.2). Used by COLOR ($4E) and GETC ($DF, phase 2b).
    const fn color(&self, source: u8) -> u8 {
        if self.regs.por & POR_HIGHNIBBLE != 0 {
            return (self.regs.colr & 0xF0) | (source >> 4);
        }
        if self.regs.por & POR_FREEZEHIGH != 0 {
            return (self.regs.colr & 0xF0) | (source & 0x0F);
        }
        source
    }

    // --- GSU-side memory bus (spec §3.1) ---------------------------------

    /// The GSU's own view of ROM / Game Pak RAM. Phase 2 skips the ron/ran
    /// access-stall spin (a timing nicety) and accesses directly.
    fn gsu_read(&self, addr: u32) -> u8 {
        let a = (addr & 0xFF_FFFF) as usize;
        if a & 0xC0_0000 == 0x00_0000 {
            // $00-3F: pack the upper-half $8000-$FFFF of each LoROM bank.
            return self.rom[(((a & 0x3F_0000) >> 1) | (a & 0x7FFF)) & self.rom_mask];
        }
        if a & 0xE0_0000 == 0x40_0000 {
            // $40-5F: linear ROM.
            return self.rom[a & self.rom_mask];
        }
        if a & 0xFE_0000 == 0x70_0000 {
            // $70-71: Game Pak RAM.
            return self.ram[a & self.ram_mask];
        }
        0 // open bus
    }

    /// GSU writes only reach Game Pak RAM at $70-71 (spec §3.1).
    fn gsu_write(&mut self, addr: u32, data: u8) {
        let a = (addr & 0xFF_FFFF) as usize;
        if a & 0xFE_0000 == 0x70_0000 {
            self.ram[a & self.ram_mask] = data;
        }
    }

    // --- timing + delayed ROM/RAM buffers (spec §6) ----------------------

    /// Advance the GSU clock by `clocks`, servicing the delayed ROM read
    /// and RAM write as their countdowns expire (ares timing.cpp:1-19).
    fn step(&mut self, clocks: u32) {
        if self.regs.romcl > 0 {
            self.regs.romcl -= clocks.min(self.regs.romcl);
            if self.regs.romcl == 0 {
                self.sfr_set(SFR_R, false);
                let a = (u32::from(self.regs.rombr) << 16) + u32::from(self.regs.r[14]);
                let v = self.gsu_read(a);
                self.regs.romdr = v;
            }
        }
        if self.regs.ramcl > 0 {
            self.regs.ramcl -= clocks.min(self.regs.ramcl);
            if self.regs.ramcl == 0 {
                let a = 0x70_0000 + (u32::from(self.regs.rambr) << 16) + u32::from(self.regs.ramar);
                let d = self.regs.ramdr;
                self.gsu_write(a, d);
            }
        }
        self.cycles = self.cycles.saturating_add(clocks);
    }

    /// Arm the R14-triggered ROM-buffer fetch (spec §6.2).
    const fn update_rom_buffer(&mut self) {
        self.sfr_set(SFR_R, true);
        self.regs.romcl = self.mem_cycles();
    }

    /// Drain any pending ROM-buffer fetch to completion.
    fn sync_rom_buffer(&mut self) {
        if self.regs.romcl > 0 {
            let c = self.regs.romcl;
            self.step(c);
        }
    }

    /// Read the ROM buffer (draining first). Used by GETB / GETC.
    fn read_rom_buffer(&mut self) -> u8 {
        self.sync_rom_buffer();
        self.regs.romdr
    }

    /// Drain any pending delayed RAM write to completion.
    fn sync_ram_buffer(&mut self) {
        if self.regs.ramcl > 0 {
            let c = self.regs.ramcl;
            self.step(c);
        }
    }

    /// Read a Game Pak RAM byte (reads aren't delayed, but a pending write
    /// must drain first) (spec §6.3).
    fn read_ram_buffer(&mut self, addr: u16) -> u8 {
        self.sync_ram_buffer();
        let a = 0x70_0000 + (u32::from(self.regs.rambr) << 16) + u32::from(addr);
        self.gsu_read(a)
    }

    /// Arm a delayed RAM write (lands `ramcl` cycles later) (spec §6.3).
    fn write_ram_buffer(&mut self, addr: u16, data: u8) {
        self.sync_ram_buffer();
        self.regs.ramcl = self.mem_cycles();
        self.regs.ramar = addr;
        self.regs.ramdr = data;
    }

    // --- fetch / pipeline / instruction cache (spec §2.3, §5) ------------

    /// Fetch the opcode/operand byte at `address` through the 512-byte
    /// instruction cache (ares memory.cpp:43-71).
    fn read_opcode(&mut self, address: u16) -> u8 {
        let offset = address.wrapping_sub(self.regs.cbr);
        if offset < 512 {
            let line = (offset >> 4) as usize;
            if self.cache_valid[line] {
                let hc = self.hit_cycles();
                self.step(hc);
            } else {
                let dp = (offset & 0xFFF0) as usize;
                let sp = (u32::from(self.regs.pbr) << 16)
                    + u32::from(self.regs.cbr.wrapping_add(offset & 0xFFF0) & 0xFFF0);
                for n in 0..16u32 {
                    let mc = self.mem_cycles();
                    self.step(mc);
                    let b = self.gsu_read(sp + n);
                    self.cache[dp + n as usize] = b;
                }
                self.cache_valid[line] = true;
            }
            return self.cache[offset as usize];
        }
        // Outside the cache window: fetch straight from ROM/RAM, draining
        // any pending buffer first.
        if self.regs.pbr <= 0x5F {
            self.sync_rom_buffer();
        } else {
            self.sync_ram_buffer();
        }
        let mc = self.mem_cycles();
        self.step(mc);
        self.gsu_read((u32::from(self.regs.pbr) << 16) | u32::from(address))
    }

    /// Return the prefetched opcode and prefetch the next byte at the
    /// *current* R15 (no increment) (ares memory.cpp:73-78).
    fn peekpipe(&mut self) -> u8 {
        let result = self.regs.pipeline;
        self.regs.pipeline = self.read_opcode(self.regs.r[15]);
        result
    }

    /// Return the prefetched byte and prefetch from `++R15` (ares
    /// memory.cpp:80-85). Used for operand bytes.
    fn pipe(&mut self) -> u8 {
        let result = self.regs.pipeline;
        self.regs.r[15] = self.regs.r[15].wrapping_add(1);
        self.regs.pipeline = self.read_opcode(self.regs.r[15]);
        result
    }

    // --- top-level execution loop ----------------------------------------

    /// Execute exactly one GSU instruction (one pass of ares `main()` with
    /// `sfr.g` set). The per-instruction cycle cost is left in `self.cycles`.
    fn run_one(&mut self) {
        self.cycles = 0;
        self.modified_r14 = false;
        self.modified_r15 = false;
        let opcode = self.peekpipe();
        if self.trace.is_some() {
            let ev = SuperFxTraceEvent {
                pc_full: (u32::from(self.regs.pbr) << 16) | u32::from(self.regs.r[15]),
                opcode,
                sfr: self.regs.sfr,
                r: self.regs.r,
            };
            if let Some((events, max)) = self.trace.as_mut() {
                if *max > 0 {
                    if events.len() >= *max {
                        events.drain(0..*max / 2);
                    }
                    events.push(ev);
                }
            }
        }
        self.execute(opcode);
        if self.modified_r14 {
            self.update_rom_buffer();
        }
        if !self.modified_r15 {
            self.regs.r[15] = self.regs.r[15].wrapping_add(1);
        }
    }

    /// Dispatch one opcode under the active ALT mode (ares instruction.cpp).
    fn execute(&mut self, opcode: u8) {
        let n = (opcode & 0x0F) as usize;
        match opcode {
            0x00 => self.op_stop(),
            0x01 => self.op_nop(),
            0x02 => self.op_cache(),
            0x03 => self.op_lsr(),
            0x04 => self.op_rol(),
            0x05 => self.op_branch(true), // bra
            0x06 => {
                let t = self.sfr_get(SFR_S) ^ self.sfr_get(SFR_OV);
                self.op_branch(!t); // (s^ov)==0
            }
            0x07 => {
                let t = self.sfr_get(SFR_S) ^ self.sfr_get(SFR_OV);
                self.op_branch(t); // (s^ov)==1
            }
            0x08 => {
                let z = self.sfr_get(SFR_Z);
                self.op_branch(!z); // bne
            }
            0x09 => {
                let z = self.sfr_get(SFR_Z);
                self.op_branch(z); // beq
            }
            0x0A => {
                let s = self.sfr_get(SFR_S);
                self.op_branch(!s); // bpl
            }
            0x0B => {
                let s = self.sfr_get(SFR_S);
                self.op_branch(s); // bmi
            }
            0x0C => {
                let c = self.sfr_get(SFR_CY);
                self.op_branch(!c); // bcc
            }
            0x0D => {
                let c = self.sfr_get(SFR_CY);
                self.op_branch(c); // bcs
            }
            0x0E => {
                let o = self.sfr_get(SFR_OV);
                self.op_branch(!o); // bvc
            }
            0x0F => {
                let o = self.sfr_get(SFR_OV);
                self.op_branch(o); // bvs
            }
            0x10..=0x1F => self.op_to_move(n),
            0x20..=0x2F => self.op_with(n),
            0x30..=0x3B => self.op_store(n),
            0x3C => self.op_loop(),
            0x3D => self.op_alt1(),
            0x3E => self.op_alt2(),
            0x3F => self.op_alt3(),
            0x40..=0x4B => self.op_load(n),
            0x4C => self.op_plot_rpix(),
            0x4D => self.op_swap(),
            0x4E => self.op_color_cmode(),
            0x4F => self.op_not(),
            0x50..=0x5F => self.op_add_adc(n),
            0x60..=0x6F => self.op_sub_sbc_cmp(n),
            0x70 => self.op_merge(),
            0x71..=0x7F => self.op_and_bic(n),
            0x80..=0x8F => self.op_mult_umult(n),
            0x90 => self.op_sbk(),
            0x91..=0x94 => self.op_link(n),
            0x95 => self.op_sex(),
            0x96 => self.op_asr_div2(),
            0x97 => self.op_ror(),
            0x98..=0x9D => self.op_jmp_ljmp(n),
            0x9E => self.op_lob(),
            0x9F => self.op_fmult_lmult(),
            0xA0..=0xAF => self.op_ibt_lms_sms(n),
            0xB0..=0xBF => self.op_from_moves(n),
            0xC0 => self.op_hib(),
            0xC1..=0xCF => self.op_or_xor(n),
            0xD0..=0xDE => self.op_inc(n),
            0xDF => self.op_getc_ramb_romb(),
            0xE0..=0xEE => self.op_dec(n),
            0xEF => self.op_getb(),
            0xF0..=0xFF => self.op_iwt_lm_sm(n),
        }
    }

    // --- control flow ----------------------------------------------------

    const fn op_stop(&mut self) {
        if !self.cfgr_irq_masked() {
            // Raise the SNES IRQ (surfaced via coproc_main_irq_pending).
            self.sfr_set(SFR_IRQ, true);
        }
        self.sfr_set(SFR_G, false);
        self.regs.pipeline = 0x01; // nop
        self.regs.reset_prefix();
    }

    const fn op_nop(&mut self) {
        self.regs.reset_prefix();
    }

    const fn op_cache(&mut self) {
        if self.regs.cbr != (self.regs.r[15] & 0xFFF0) {
            self.regs.cbr = self.regs.r[15] & 0xFFF0;
            self.flush_cache();
        }
        self.regs.reset_prefix();
    }

    const fn op_loop(&mut self) {
        let v = self.regs.r[12].wrapping_sub(1);
        self.write_r(12, v);
        self.sfr_set(SFR_S, v & 0x8000 != 0);
        self.sfr_set(SFR_Z, v == 0);
        if !self.sfr_get(SFR_Z) {
            let target = self.regs.r[13];
            self.write_r(15, target);
        }
        self.regs.reset_prefix();
    }

    const fn op_link(&mut self, n: usize) {
        let v = self.regs.r[15].wrapping_add(n as u16);
        self.write_r(11, v);
        self.regs.reset_prefix();
    }

    const fn op_jmp_ljmp(&mut self, n: usize) {
        if self.sfr_get(SFR_ALT1) {
            self.regs.pbr = (self.regs.r[n] & 0x7F) as u8;
            let target = self.sr();
            self.write_r(15, target);
            self.regs.cbr = self.regs.r[15] & 0xFFF0;
            self.flush_cache();
        } else {
            let target = self.regs.r[n];
            self.write_r(15, target);
        }
        self.regs.reset_prefix();
    }

    fn op_branch(&mut self, take: bool) {
        let disp = self.pipe() as i8;
        if take {
            let target = (i32::from(self.regs.r[15]) + i32::from(disp)) as u16;
            self.write_r(15, target);
        }
    }

    // --- ALT / register-select prefixes ----------------------------------

    const fn op_alt1(&mut self) {
        self.sfr_set(SFR_B, false);
        self.sfr_set(SFR_ALT1, true);
    }
    const fn op_alt2(&mut self) {
        self.sfr_set(SFR_B, false);
        self.sfr_set(SFR_ALT2, true);
    }
    const fn op_alt3(&mut self) {
        self.sfr_set(SFR_B, false);
        self.sfr_set(SFR_ALT1, true);
        self.sfr_set(SFR_ALT2, true);
    }

    const fn op_to_move(&mut self, n: usize) {
        if self.sfr_get(SFR_B) {
            let v = self.sr();
            self.write_r(n, v);
            self.regs.reset_prefix();
        } else {
            self.regs.dreg = n;
        }
    }

    const fn op_with(&mut self, n: usize) {
        self.regs.sreg = n;
        self.regs.dreg = n;
        self.sfr_set(SFR_B, true);
    }

    const fn op_from_moves(&mut self, n: usize) {
        if self.sfr_get(SFR_B) {
            let v = self.regs.r[n];
            self.write_dr(v);
            self.sfr_set(SFR_OV, v & 0x80 != 0);
            self.sfr_set(SFR_S, v & 0x8000 != 0);
            self.sfr_set(SFR_Z, v == 0);
            self.regs.reset_prefix();
        } else {
            self.regs.sreg = n;
        }
    }

    // --- ALU + shifts ----------------------------------------------------

    fn op_add_adc(&mut self, n: usize) {
        let op = if self.sfr_get(SFR_ALT2) {
            n as u32
        } else {
            u32::from(self.regs.r[n])
        };
        let carry_in = if self.sfr_get(SFR_ALT1) {
            u32::from(self.sfr_get(SFR_CY))
        } else {
            0
        };
        let src = u32::from(self.sr());
        let r = src + op + carry_in;
        self.sfr_set(SFR_OV, (!(src ^ op) & (op ^ r) & 0x8000) != 0);
        self.sfr_set(SFR_S, r & 0x8000 != 0);
        self.sfr_set(SFR_CY, r >= 0x10000);
        self.sfr_set(SFR_Z, (r & 0xFFFF) == 0);
        self.write_dr((r & 0xFFFF) as u16);
        self.regs.reset_prefix();
    }

    fn op_sub_sbc_cmp(&mut self, n: usize) {
        let alt1 = self.sfr_get(SFR_ALT1);
        let alt2 = self.sfr_get(SFR_ALT2);
        let op = if !alt2 || alt1 {
            i32::from(self.regs.r[n])
        } else {
            n as i32
        };
        let borrow = if !alt2 && alt1 {
            i32::from(!self.sfr_get(SFR_CY))
        } else {
            0
        };
        let src = i32::from(self.sr());
        let r = src - op - borrow;
        self.sfr_set(SFR_OV, ((src ^ op) & (src ^ r) & 0x8000) != 0);
        self.sfr_set(SFR_S, r & 0x8000 != 0);
        self.sfr_set(SFR_CY, r >= 0);
        self.sfr_set(SFR_Z, (r & 0xFFFF) == 0);
        // CMP (alt3 = alt2 && alt1) does not write the result.
        if !alt2 || !alt1 {
            self.write_dr((r & 0xFFFF) as u16);
        }
        self.regs.reset_prefix();
    }

    const fn op_and_bic(&mut self, n: usize) {
        let op = if self.sfr_get(SFR_ALT2) {
            n as u16
        } else {
            self.regs.r[n]
        };
        let operand = if self.sfr_get(SFR_ALT1) { !op } else { op };
        let v = self.sr() & operand;
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_or_xor(&mut self, n: usize) {
        let op = if self.sfr_get(SFR_ALT2) {
            n as u16
        } else {
            self.regs.r[n]
        };
        let v = if self.sfr_get(SFR_ALT1) {
            self.sr() ^ op
        } else {
            self.sr() | op
        };
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_merge(&mut self) {
        let v = (self.regs.r[7] & 0xFF00) | (self.regs.r[8] >> 8);
        self.write_dr(v);
        self.sfr_set(SFR_OV, v & 0xC0C0 != 0);
        self.sfr_set(SFR_S, v & 0x8080 != 0);
        self.sfr_set(SFR_CY, v & 0xE0E0 != 0);
        self.sfr_set(SFR_Z, v & 0xF0F0 != 0);
        self.regs.reset_prefix();
    }

    const fn op_lsr(&mut self) {
        let src = self.sr();
        self.sfr_set(SFR_CY, src & 1 != 0);
        let v = src >> 1;
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    fn op_rol(&mut self) {
        let src = self.sr();
        let carry = src & 0x8000 != 0;
        let v = (src << 1) | u16::from(self.sfr_get(SFR_CY));
        self.write_dr(v);
        self.sfr_set(SFR_S, v & 0x8000 != 0);
        self.sfr_set(SFR_CY, carry);
        self.sfr_set(SFR_Z, v == 0);
        self.regs.reset_prefix();
    }

    fn op_ror(&mut self) {
        let src = self.sr();
        let carry = src & 1 != 0;
        let v = (u16::from(self.sfr_get(SFR_CY)) << 15) | (src >> 1);
        self.write_dr(v);
        self.sfr_set(SFR_S, v & 0x8000 != 0);
        self.sfr_set(SFR_CY, carry);
        self.sfr_set(SFR_Z, v == 0);
        self.regs.reset_prefix();
    }

    fn op_asr_div2(&mut self) {
        let src = self.sr();
        self.sfr_set(SFR_CY, src & 1 != 0);
        let mut v = ((src as i16) >> 1) as u16;
        if self.sfr_get(SFR_ALT1) {
            // DIV2 rounding correction (+1 only when src == 0xFFFF).
            v = v.wrapping_add(((u32::from(src) + 1) >> 16) as u16);
        }
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_swap(&mut self) {
        let v = self.sr().rotate_left(8);
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_not(&mut self) {
        let v = !self.sr();
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_sex(&mut self) {
        let v = ((self.sr() as u8) as i8 as i16) as u16;
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_lob(&mut self) {
        let v = self.sr() & 0x00FF;
        self.write_dr(v);
        self.sfr_set(SFR_S, v & 0x80 != 0); // sign from bit 7
        self.sfr_set(SFR_Z, v == 0);
        self.regs.reset_prefix();
    }

    const fn op_hib(&mut self) {
        let v = self.sr() >> 8;
        self.write_dr(v);
        self.sfr_set(SFR_S, v & 0x80 != 0); // sign from bit 7
        self.sfr_set(SFR_Z, v == 0);
        self.regs.reset_prefix();
    }

    const fn op_inc(&mut self, n: usize) {
        let v = self.regs.r[n].wrapping_add(1);
        self.write_r(n, v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_dec(&mut self, n: usize) {
        let v = self.regs.r[n].wrapping_sub(1);
        self.write_r(n, v);
        self.set_sz(v);
        self.regs.reset_prefix();
    }

    const fn op_color_cmode(&mut self) {
        if self.sfr_get(SFR_ALT1) {
            self.regs.por = (self.sr() & 0xFF) as u8;
        } else {
            let c = self.color((self.sr() & 0xFF) as u8);
            self.regs.colr = c;
        }
        self.regs.reset_prefix();
    }

    // --- memory ops: LD/ST + RAM buffer (spec §2.6) ----------------------

    fn op_store(&mut self, n: usize) {
        self.regs.ramaddr = self.regs.r[n];
        let s = self.sr();
        let addr = self.regs.ramaddr;
        self.write_ram_buffer(addr, s as u8); // STB / STW low byte
        if !self.sfr_get(SFR_ALT1) {
            self.write_ram_buffer(addr ^ 1, (s >> 8) as u8); // STW high byte
        }
        self.regs.reset_prefix();
    }

    fn op_load(&mut self, n: usize) {
        self.regs.ramaddr = self.regs.r[n];
        let addr = self.regs.ramaddr;
        let mut v = u16::from(self.read_ram_buffer(addr)); // LDB / LDW low
        if !self.sfr_get(SFR_ALT1) {
            v |= u16::from(self.read_ram_buffer(addr ^ 1)) << 8; // LDW high
        }
        self.write_dr(v);
        self.regs.reset_prefix();
    }

    fn op_sbk(&mut self) {
        let s = self.sr();
        let addr = self.regs.ramaddr;
        self.write_ram_buffer(addr, s as u8);
        self.write_ram_buffer(addr ^ 1, (s >> 8) as u8);
        self.regs.reset_prefix();
    }

    // --- multiplies (spec §2.5) ------------------------------------------

    fn op_mult_umult(&mut self, n: usize) {
        let op = if self.sfr_get(SFR_ALT2) {
            n as u16
        } else {
            self.regs.r[n]
        };
        let s = self.sr();
        let v = if self.sfr_get(SFR_ALT1) {
            // UMULT: unsigned 8×8 → 16.
            (s as u8 as u16).wrapping_mul(op as u8 as u16)
        } else {
            // MULT: signed 8×8 → 16.
            ((s as u8 as i8 as i16).wrapping_mul(op as u8 as i8 as i16)) as u16
        };
        self.write_dr(v);
        self.set_sz(v);
        self.regs.reset_prefix();
        if self.regs.cfgr & 0x20 == 0 {
            // !ms0: high-speed multiply not selected → extra cycle.
            let c = if self.regs.clsr { 1 } else { 2 };
            self.step(c);
        }
    }

    fn op_fmult_lmult(&mut self) {
        let s = self.sr() as i16;
        let r6 = self.regs.r[6] as i16;
        let result = (i32::from(s) * i32::from(r6)) as u32;
        if self.sfr_get(SFR_ALT1) {
            self.write_r(4, result as u16); // LMULT: low 16 → R4
        }
        let v = (result >> 16) as u16;
        self.write_dr(v);
        self.sfr_set(SFR_S, v & 0x8000 != 0);
        self.sfr_set(SFR_CY, result & 0x8000 != 0);
        self.sfr_set(SFR_Z, v == 0);
        self.regs.reset_prefix();
        let mul = if self.regs.cfgr & 0x20 != 0 { 3 } else { 7 };
        let clk = if self.regs.clsr { 1 } else { 2 };
        self.step(mul * clk);
    }

    // --- ROM buffer ops: GETB / GETC / RAMB / ROMB (spec §2.7) -----------

    fn op_getc_ramb_romb(&mut self) {
        if !self.sfr_get(SFR_ALT2) {
            let b = self.read_rom_buffer();
            self.regs.colr = self.color(b); // GETC
        } else if self.sfr_get(SFR_ALT1) {
            self.sync_rom_buffer();
            self.regs.rombr = (self.sr() & 0x7F) as u8; // ROMB
        } else {
            self.sync_ram_buffer();
            self.regs.rambr = self.sr() & 0x01 != 0; // RAMB
        }
        self.regs.reset_prefix();
    }

    fn op_getb(&mut self) {
        let rom = self.read_rom_buffer();
        let sel = (u8::from(self.sfr_get(SFR_ALT2)) << 1) | u8::from(self.sfr_get(SFR_ALT1));
        let v = match sel {
            0 => u16::from(rom),                               // GETB
            1 => (u16::from(rom) << 8) | (self.sr() & 0x00FF), // GETBH
            2 => (self.sr() & 0xFF00) | u16::from(rom),        // GETBL
            _ => (rom as i8 as i16) as u16,                    // GETBS
        };
        self.write_dr(v);
        self.regs.reset_prefix();
    }

    // --- immediate loads (spec §2.6) -------------------------------------

    fn op_ibt_lms_sms(&mut self, n: usize) {
        if self.sfr_get(SFR_ALT1) {
            // LMS: load word from short address (yy << 1).
            self.regs.ramaddr = u16::from(self.pipe()) << 1;
            let addr = self.regs.ramaddr;
            let lo = self.read_ram_buffer(addr);
            let hi = self.read_ram_buffer(addr ^ 1);
            self.write_r(n, (u16::from(hi) << 8) | u16::from(lo));
        } else if self.sfr_get(SFR_ALT2) {
            // SMS: store word to short address.
            self.regs.ramaddr = u16::from(self.pipe()) << 1;
            let addr = self.regs.ramaddr;
            let v = self.regs.r[n];
            self.write_ram_buffer(addr, v as u8);
            self.write_ram_buffer(addr ^ 1, (v >> 8) as u8);
        } else {
            // IBT: sign-extended immediate byte → r[n].
            let b = self.pipe();
            self.write_r(n, (b as i8 as i16) as u16);
        }
        self.regs.reset_prefix();
    }

    fn op_iwt_lm_sm(&mut self, n: usize) {
        if self.sfr_get(SFR_ALT1) {
            // LM: load word from 16-bit immediate address.
            let lo_a = self.pipe();
            let hi_a = self.pipe();
            self.regs.ramaddr = u16::from(lo_a) | (u16::from(hi_a) << 8);
            let addr = self.regs.ramaddr;
            let lo = self.read_ram_buffer(addr);
            let hi = self.read_ram_buffer(addr ^ 1);
            self.write_r(n, (u16::from(hi) << 8) | u16::from(lo));
        } else if self.sfr_get(SFR_ALT2) {
            // SM: store word to 16-bit immediate address.
            let lo_a = self.pipe();
            let hi_a = self.pipe();
            self.regs.ramaddr = u16::from(lo_a) | (u16::from(hi_a) << 8);
            let addr = self.regs.ramaddr;
            let v = self.regs.r[n];
            self.write_ram_buffer(addr, v as u8);
            self.write_ram_buffer(addr ^ 1, (v >> 8) as u8);
        } else {
            // IWT: 16-bit immediate word → r[n].
            let lo = self.pipe();
            let hi = self.pipe();
            self.write_r(n, u16::from(lo) | (u16::from(hi) << 8));
        }
        self.regs.reset_prefix();
    }

    // ===================== pixel-plot pipeline (phase 3, spec §4) =========

    /// Colour depth in bitplanes for the current SCMR mode (spec §4.6):
    /// md{0,1,2,3} → bpp{2,4,4,8}.
    const fn bpp(&self) -> u32 {
        2u32 << (self.regs.scmr_md - (self.regs.scmr_md >> 1))
    }

    /// Tile (character) number for pixel (x, y) under the active height
    /// mode (`por.obj` forces ht==3) (spec §4.6, ares core.cpp:53-58).
    const fn tile_index(&self, x: u8, y: u8) -> u32 {
        let x = x as u32;
        let y = y as u32;
        let mode = if self.regs.por & POR_OBJ != 0 {
            3
        } else {
            self.regs.scmr_ht
        };
        match mode {
            0 => ((x & 0xF8) << 1) + ((y & 0xF8) >> 3),
            1 => ((x & 0xF8) << 1) + ((x & 0xF8) >> 1) + ((y & 0xF8) >> 3),
            2 => ((x & 0xF8) << 1) + (x & 0xF8) + ((y & 0xF8) >> 3),
            _ => ((y & 0x80) << 2) + ((x & 0x80) << 1) + ((y & 0x78) << 1) + ((x & 0x78) >> 3),
        }
    }

    /// Tile byte address in Game Pak RAM for pixel-row `y` of tile `cn`.
    const fn tile_addr(&self, cn: u32, y: u8) -> u32 {
        0x70_0000 + cn * (self.bpp() << 3) + ((self.regs.scbr as u32) << 10) + ((y & 7) as u32 * 2)
    }

    /// PLOT colour `regs.colr` (with dither) into pixel (x, y) of the
    /// primary pixel cache, flushing the secondary as the run advances
    /// (spec §4.3, ares core.cpp:11-46).
    fn plot(&mut self, x: u8, y: u8) {
        // Transparency: skip color-0 pixels unless POR.transparent.
        if self.regs.por & POR_TRANSPARENT == 0 {
            let transparent = if self.regs.scmr_md == 3 {
                if self.regs.por & POR_FREEZEHIGH != 0 {
                    self.regs.colr & 0x0F == 0
                } else {
                    self.regs.colr == 0
                }
            } else {
                self.regs.colr & 0x0F == 0
            };
            if transparent {
                return;
            }
        }

        let mut color = self.regs.colr;
        if self.regs.por & POR_DITHER != 0 && self.regs.scmr_md != 3 {
            if (x ^ y) & 1 != 0 {
                color >>= 4;
            }
            color &= 0x0F;
        }

        let offset = (u16::from(y) << 5) + (u16::from(x) >> 3);
        if offset != self.pixelcache[0].offset {
            self.flush_pixel_cache(1);
            self.pixelcache[1] = self.pixelcache[0];
            self.pixelcache[0].bitpend = 0;
            self.pixelcache[0].offset = offset;
        }

        let xi = ((x & 7) ^ 7) as usize;
        self.pixelcache[0].data[xi] = color;
        self.pixelcache[0].bitpend |= 1 << xi;
        if self.pixelcache[0].bitpend == 0xFF {
            self.flush_pixel_cache(1);
            self.pixelcache[1] = self.pixelcache[0];
            self.pixelcache[0].bitpend = 0;
        }
    }

    /// RPIX: flush both caches, then read back the colour index of pixel
    /// (x, y) from the bitplanes in Game Pak RAM (spec §4.5).
    fn rpix(&mut self, x: u8, y: u8) -> u16 {
        self.flush_pixel_cache(1);
        self.flush_pixel_cache(0);

        let cn = self.tile_index(x, y);
        let bpp = self.bpp();
        let addr = self.tile_addr(cn, y);
        let xi = (x & 7) ^ 7;
        let mut data: u8 = 0;
        for n in 0..bpp {
            let byte = ((n >> 1) << 4) + (n & 1);
            let mc = self.mem_cycles();
            self.step(mc);
            let b = self.gsu_read(addr + byte);
            data |= ((b >> xi) & 1) << n;
        }
        u16::from(data)
    }

    /// Flush one pixel cache's pending run to its tile in Game Pak RAM,
    /// converting the planar colour indices to bitplane bytes with a
    /// read-modify-write on partial runs (spec §4.4, ares core.cpp:73-103).
    fn flush_pixel_cache(&mut self, idx: usize) {
        let cache = self.pixelcache[idx];
        if cache.bitpend == 0 {
            return;
        }
        let x = (cache.offset << 3) as u8;
        let y = (cache.offset >> 5) as u8;
        let cn = self.tile_index(x, y);
        let bpp = self.bpp();
        let addr = self.tile_addr(cn, y);

        for n in 0..bpp {
            let byte = ((n >> 1) << 4) + (n & 1);
            let mut data: u8 = 0;
            for (xi, &px) in cache.data.iter().enumerate() {
                data |= ((px >> n) & 1) << xi;
            }
            if cache.bitpend != 0xFF {
                // Partial run: read-modify-write the untouched pixels.
                let mc = self.mem_cycles();
                self.step(mc);
                data &= cache.bitpend;
                data |= self.gsu_read(addr + byte) & !cache.bitpend;
            }
            let mc = self.mem_cycles();
            self.step(mc);
            self.gsu_write(addr + byte, data);
        }
        self.pixelcache[idx].bitpend = 0;
    }

    /// `$4C` PLOT (alt0) / RPIX (alt1) (ares instructions.cpp:127-137).
    fn op_plot_rpix(&mut self) {
        let x = self.regs.r[1] as u8;
        let y = self.regs.r[2] as u8;
        if self.sfr_get(SFR_ALT1) {
            let v = self.rpix(x, y);
            self.write_dr(v);
            self.sfr_set(SFR_S, v & 0x8000 != 0);
            self.sfr_set(SFR_Z, v == 0);
        } else {
            self.plot(x, y);
            let r1 = self.regs.r[1].wrapping_add(1);
            self.write_r(1, r1);
        }
        self.regs.reset_prefix();
    }
}

impl Mapper for SuperFxMapper {
    fn kind(&self) -> MapperKind {
        MapperKind::SuperFx
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);

        // GSU MMIO window.
        if let Some(a) = Self::mmio_addr(bank, offset) {
            return Some(self.mmio_read(a));
        }
        // ROM. While the GSU runs and owns ROM, the SNES sees the busy
        // vector instead of ROM data (spec §3.3).
        if let Some(o) = Self::rom_offset(bank, offset) {
            if self.sfr_get(SFR_G) && self.regs.scmr_ron {
                return Some(Self::busy_rom_vector(offset as u8));
            }
            return Some(self.rom[o & self.rom_mask]);
        }
        // Game Pak RAM. While the GSU owns RAM, SNES reads are open bus
        // (spec §3.3); we surface `None` so the bus returns its open-bus.
        if let Some(o) = Self::ram_offset(bank, offset) {
            if self.sfr_get(SFR_G) && self.regs.scmr_ran {
                return None;
            }
            return Some(self.ram[o & self.ram_mask]);
        }
        None
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);

        if let Some(a) = Self::mmio_addr(bank, offset) {
            self.mmio_write(a, value);
            return true;
        }
        if let Some(o) = Self::ram_offset(bank, offset) {
            // ares lets the SNES RAM write land even while the GSU owns RAM
            // (spec §3.3 / divergence #4 — follow ares).
            self.ram[o & self.ram_mask] = value;
            return true;
        }
        // ROM writes are dropped but claimed so the bus doesn't fall through.
        Self::rom_offset(bank, offset).is_some()
    }

    fn rom_size(&self) -> usize {
        self.rom_len
    }

    fn sram_size(&self) -> usize {
        self.ram_len
    }

    /// Advance the GSU by `main_mclk` master cycles of main-CPU progress.
    ///
    /// Phase-2 timing model: accumulate a GSU-clock budget and run
    /// instructions while it stays positive, deducting each op's cycle cost
    /// (roughly 1 master clock ≈ 1 GSU clock at the fast rate; the exact
    /// clsr ratio is the timing phase's job). The loop also exits the moment
    /// a STOP clears the GO flag.
    fn step_coproc(&mut self, main_mclk: u32) {
        if !self.sfr_get(SFR_G) {
            return;
        }
        self.clock_deficit += i64::from(main_mclk);
        while self.sfr_get(SFR_G) && self.clock_deficit > 0 {
            self.run_one();
            // Every instruction fetches at least one byte (≥ 1 cycle), so
            // this always makes progress and the budget bounds the loop.
            self.clock_deficit -= i64::from(self.cycles.max(1));
        }
    }

    /// The GSU asserts the main-CPU IRQ line while `sfr.irq` is latched
    /// (spec §7.3). Set by the STOP opcode (next phase); acknowledged by a
    /// SNES read of SFR high byte ($3031).
    fn coproc_main_irq_pending(&self) -> bool {
        self.sfr_get(SFR_IRQ)
    }

    fn enable_superfx_trace(&mut self, max_events: usize) {
        self.trace = Some((Vec::new(), max_events));
    }

    fn take_superfx_trace(&mut self) -> Vec<SuperFxTraceEvent> {
        match self.trace.as_mut() {
            Some((events, _)) => std::mem::take(events),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    fn fx() -> SuperFxMapper {
        SuperFxMapper::new(ramp_rom(1024 * 1024), 0x8000)
    }

    #[test]
    fn kind_is_superfx() {
        assert_eq!(fx().kind(), MapperKind::SuperFx);
    }

    /// One reference GSU instruction snapshot for the differential harness.
    struct Row {
        pc: u32,
        opcode: u8,
        sfr: u16,
        sreg: usize,
        dreg: usize,
        pbr: u8,
        r: [u16; 16],
    }

    // GSU differential harness: replay a reference (Mesen) GSU instruction
    // trace one op at a time and flag opcodes whose register output diverges.
    // Diagnostic — skips silently unless both the reference CSV and the ROM
    // are present (so CI is unaffected). Run with:
    //   cargo test -p luna-bus gsu_differential -- --nocapture
    #[test]
    fn gsu_differential_vs_mesen() {
        use std::path::Path;
        let csv_path =
            std::env::var("LUNA_GSU_DIFF_CSV").unwrap_or_else(|_| "/tmp/mesen_gsu_full.csv".into());
        // ROM is gitignored; default to the repo-relative path, override
        // with LUNA_SF_ROM. Test skips silently if either file is absent.
        let rom_path = std::env::var("LUNA_SF_ROM")
            .unwrap_or_else(|_| "../../tests/roms/Star Fox (USA) (Rev 2).sfc".into());
        if !Path::new(&csv_path).exists() || !Path::new(&rom_path).exists() {
            eprintln!("gsu_differential: reference CSV or ROM absent — skipping");
            return;
        }
        let rom = std::fs::read(&rom_path).expect("read ROM");
        let text = std::fs::read_to_string(&csv_path).expect("read CSV");

        // Parse rows: seq,pc,opcode,sfr,sreg,dreg,pbr,rombr,colr,r0..r15
        let mut rows: Vec<Row> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if i == 0 {
                continue; // header
            }
            let f: Vec<&str> = line.split(',').collect();
            if f.len() < 25 {
                continue;
            }
            let g = |k: usize| f[k].trim().parse::<i64>().unwrap_or(0);
            let mut r = [0u16; 16];
            for (j, rr) in r.iter_mut().enumerate() {
                *rr = g(9 + j) as u16;
            }
            rows.push(Row {
                pc: g(1) as u32,
                opcode: (g(2) & 0xFF) as u8,
                sfr: g(3) as u16,
                sreg: g(4) as usize,
                dreg: g(5) as usize,
                pbr: g(6) as u8,
                r,
            });
        }

        // Skip opcodes whose register output depends on un-injected state
        // (RAM/ROM reads, pixel cache) — v1 covers pure-register ops.
        let skip = |op: u8| {
            matches!(op, 0x00 | 0x02 | 0x4C | 0x90 | 0xDF | 0xEF)
                || (0x30..=0x3B).contains(&op) // store
                || (0x40..=0x4B).contains(&op) // load
                || (0xA0..=0xAF).contains(&op) // ibt/lms/sms (mem)
                || (0xF0..=0xFF).contains(&op) // iwt/lm/sm (mem)
        };

        let mut m = SuperFxMapper::new(rom, 0x8000);
        let flag_mask: u16 = SFR_Z | SFR_CY | SFR_S | SFR_OV | SFR_ALT1 | SFR_ALT2 | SFR_B;
        let (mut tested, mut data_div, mut flag_div, mut pc_div) = (0u64, 0u64, 0u64, 0u64);
        // per opcode: (tested, data_diverged, pc_diverged)
        let mut by_op: std::collections::BTreeMap<u8, (u64, u64, u64)> =
            std::collections::BTreeMap::new();
        let mut samples: Vec<String> = Vec::new();
        let mut pc_samples: Vec<String> = Vec::new();

        for w in rows.windows(2) {
            let (pre, post) = (&w[0], &w[1]);
            if skip(pre.opcode) {
                continue;
            }
            // Inject pre-state.
            m.regs.r = pre.r;
            m.regs.sfr = pre.sfr;
            m.regs.sreg = pre.sreg;
            m.regs.dreg = pre.dreg;
            m.regs.pbr = pre.pbr;
            m.regs.cbr = 0; // keep PC out of the cache window → ROM fetch
            m.regs.romcl = 0;
            m.regs.ramcl = 0;
            m.regs.pipeline = pre.opcode; // peekpipe() returns this as the opcode
            m.regs.r[15] = (pre.pc & 0xFFFF) as u16 + 1; // luna: r15 = opcode_addr+1
            m.run_one(); // peekpipe (→ operand prefetch from ROM) + execute + r15++

            tested += 1;
            let e = by_op.entry(pre.opcode).or_insert((0, 0, 0));
            e.0 += 1;
            // Compare data registers r0..r14.
            let data_ok = (0..15).all(|k| m.regs.r[k] == post.r[k]);
            // Compare control-flow r15 ONLY for control-flow opcodes. For
            // ordinary ops the next PC is trivial, and single-step replay
            // can't reproduce the GSU's 1-instruction branch *delay slot*
            // (the op physically after a taken branch runs, then the target
            // is taken) — so a NOP/INC/DEC in a delay slot would false-flag.
            let is_branch = matches!(pre.opcode, 0x05..=0x0F | 0x3C | 0x91..=0x94 | 0x98..=0x9D);
            // On a TAKEN branch luna writes r15 = target (= mesen's next
            // opcode addr); on fall-through luna keeps r15 = opcode_addr+1.
            let pc_ok = !is_branch
                || (if m.modified_r15 {
                    m.regs.r[15] == post.r[15]
                } else {
                    m.regs.r[15] == post.r[15].wrapping_add(1)
                });
            // Compare flag bits.
            let flag_ok = (m.regs.sfr & flag_mask) == (post.sfr & flag_mask);
            if !data_ok {
                data_div += 1;
                e.1 += 1;
                if samples.len() < 25 {
                    let diffs: Vec<String> = (0..15)
                        .filter(|&k| m.regs.r[k] != post.r[k])
                        .map(|k| format!("r{k}: got {:04X} exp {:04X}", m.regs.r[k], post.r[k]))
                        .collect();
                    samples.push(format!(
                        "op {:02X} @pc {:06X} sreg{} dreg{}: {}",
                        pre.opcode,
                        pre.pc,
                        pre.sreg,
                        pre.dreg,
                        diffs.join(", ")
                    ));
                }
            }
            if !pc_ok {
                pc_div += 1;
                e.2 += 1;
                if pc_samples.len() < 25 {
                    pc_samples.push(format!(
                        "op {:02X} @pc {:06X}: r15 got {:04X} exp {:04X} (mesen-next {:04X})",
                        pre.opcode,
                        pre.pc,
                        m.regs.r[15],
                        post.r[15].wrapping_add(1),
                        post.r[15]
                    ));
                }
            }
            if !flag_ok {
                flag_div += 1;
            }
        }

        eprintln!(
            "\n=== GSU differential: {tested} ops tested | data-diverge {data_div} | flag-diverge {flag_div} | pc-diverge {pc_div} ==="
        );
        eprintln!("per-opcode divergence (op: data/pc/tested):");
        for (op, (t, d, p)) in &by_op {
            if *d > 0 || *p > 0 {
                eprintln!("  {op:02X}: data {d}, pc {p} / {t}");
            }
        }
        eprintln!("\nsample data divergences:");
        for s in &samples {
            eprintln!("  {s}");
        }
        eprintln!("\nsample pc divergences:");
        for s in &pc_samples {
            eprintln!("  {s}");
        }
    }

    // GSU TRAJECTORY harness: inject the full engine state + work RAM ONCE,
    // then run luna's GSU FREELY (accumulating) and compare each step to the
    // reference trace. Unlike the single-step harness, this catches bugs in
    // loads / accumulation / control flow that only surface over a run. Stops
    // at the first GSU STOP (no CPU to re-GO). Skips silently w/o the files.
    // Run: cargo test -p luna-bus gsu_trajectory -- --nocapture
    #[test]
    fn gsu_trajectory_vs_mesen() {
        use std::path::Path;
        let dir = std::env::var("LUNA_GSU_DIFF_DIR").unwrap_or_else(|_| "/tmp".into());
        let csv = format!("{dir}/mesen_gsu_full.csv");
        let initp = format!("{dir}/mesen_gsu_init.txt");
        let ramp = format!("{dir}/mesen_gsu_ram_start.bin");
        let rom_path = std::env::var("LUNA_SF_ROM")
            .unwrap_or_else(|_| "../../tests/roms/Star Fox (USA) (Rev 2).sfc".into());
        if ![&csv, &initp, &ramp, &rom_path]
            .iter()
            .all(|p| Path::new(p).exists())
        {
            eprintln!("gsu_trajectory: reference files or ROM absent — skipping");
            return;
        }
        let rom = std::fs::read(&rom_path).expect("rom");
        let ram_start = std::fs::read(&ramp).expect("ram");
        let init: std::collections::HashMap<String, i64> = std::fs::read_to_string(&initp)
            .expect("init")
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                Some((k.to_string(), v.trim().parse().ok()?))
            })
            .collect();
        let g = |k: &str| *init.get(k).unwrap_or(&0);

        // Parse the reference instruction trace.
        let text = std::fs::read_to_string(&csv).expect("csv");
        let mut rows: Vec<Row> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if i == 0 {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            if f.len() < 25 {
                continue;
            }
            let gv = |k: usize| f[k].trim().parse::<i64>().unwrap_or(0);
            let mut r = [0u16; 16];
            for (j, rr) in r.iter_mut().enumerate() {
                *rr = gv(9 + j) as u16;
            }
            rows.push(Row {
                pc: gv(1) as u32,
                opcode: (gv(2) & 0xFF) as u8,
                sfr: gv(3) as u16,
                sreg: gv(4) as usize,
                dreg: gv(5) as usize,
                pbr: gv(6) as u8,
                r,
            });
        }
        assert!(rows.len() > 2, "need a trace");

        let ram_size = g("ramSize") as usize;
        let mut m = SuperFxMapper::new(rom, ram_size);
        assert_eq!(m.ram.len(), ram_start.len(), "ram size mismatch");
        m.ram.copy_from_slice(&ram_start);

        // Inject the full engine + register state from row 0 + init.txt.
        let r0 = &rows[0];
        m.regs.r = r0.r;
        m.regs.sfr = r0.sfr | SFR_G; // ensure running
        m.regs.sreg = r0.sreg;
        m.regs.dreg = r0.dreg;
        m.regs.pbr = r0.pbr;
        m.regs.cbr = g("cbr") as u16;
        m.regs.scbr = g("scbr") as u8;
        m.regs.scmr_ht = g("ht") as u8;
        m.regs.scmr_md = g("md") as u8;
        m.regs.scmr_ron = g("ron") != 0;
        m.regs.scmr_ran = g("ran") != 0;
        m.regs.colr = g("colr") as u8;
        m.regs.por = g("por") as u8;
        m.regs.cfgr = g("cfgr") as u8;
        m.regs.clsr = g("clsr") != 0;
        m.regs.rombr = g("rombr") as u8;
        m.regs.rambr = g("rambr") != 0;
        m.regs.romdr = g("romdr") as u8;
        m.regs.ramaddr = g("ramaddr") as u16;
        m.regs.ramar = g("ramar") as u16;
        m.regs.ramdr = g("ramdr") as u8;
        m.regs.romcl = 0;
        m.regs.ramcl = 0;
        for v in &mut m.cache_valid {
            *v = false; // force ROM/RAM refetch (cache mirrors them)
        }
        m.regs.pipeline = r0.opcode;
        m.regs.r[15] = (r0.pc & 0xFFFF) as u16 + 1;

        let flag_mask: u16 = SFR_Z | SFR_CY | SFR_S | SFR_OV | SFR_ALT1 | SFR_ALT2 | SFR_B;
        let mut steps = 0usize;
        let mut diverged = false;
        for i in 0..rows.len() - 1 {
            // Control-flow sync: the op luna is about to run must match the ref.
            let luna_op = m.regs.pipeline;
            if luna_op != rows[i].opcode {
                eprintln!(
                    "\nTRAJECTORY DIVERGE @step {i}: luna about to run op {:02X} but ref ran {:02X} @pc {:06X} (control-flow drift)",
                    luna_op, rows[i].opcode, rows[i].pc
                );
                diverged = true;
                break;
            }
            m.run_one();
            steps += 1;
            if m.regs.sfr & SFR_G == 0 {
                eprintln!(
                    "\nreached GSU STOP at step {i} (op {:02X}) — end of GO run",
                    rows[i].opcode
                );
                break;
            }
            let post = &rows[i + 1];
            let data_diff: Vec<String> = (0..15)
                .filter(|&k| m.regs.r[k] != post.r[k])
                .map(|k| format!("r{k}: luna {:04X} ref {:04X}", m.regs.r[k], post.r[k]))
                .collect();
            let flag_diff = (m.regs.sfr & flag_mask) != (post.sfr & flag_mask);
            if !data_diff.is_empty() || flag_diff {
                eprintln!(
                    "\nTRAJECTORY DIVERGE @step {i}: op {:02X} @pc {:06X} sreg{} dreg{}",
                    rows[i].opcode, rows[i].pc, rows[i].sreg, rows[i].dreg
                );
                eprintln!("  data: {}", data_diff.join(", "));
                if flag_diff {
                    eprintln!(
                        "  flags: luna {:04X} ref {:04X}",
                        m.regs.sfr & flag_mask,
                        post.sfr & flag_mask
                    );
                }
                // a little context: the few preceding ops
                let lo = i.saturating_sub(6);
                eprintln!("  preceding ops:");
                for (j, row) in rows.iter().enumerate().take(i + 1).skip(lo) {
                    eprintln!(
                        "    [{j}] {:02X} @pc {:06X} sreg{} dreg{}",
                        row.opcode, row.pc, row.sreg, row.dreg
                    );
                }
                diverged = true;
                break;
            }
        }
        eprintln!(
            "\n=== GSU trajectory: replayed {steps} instrs, diverged={diverged} ===\n(clean = loads/compute/control-flow all match the reference over a free run)"
        );

        // RAM (PLOT/store) check: registers don't reveal framebuffer writes,
        // so compare luna's work RAM after the GO-run to the reference's RAM
        // at its first STOP. This is where a PLOT/flush bug would surface.
        let stop_ram = format!("{dir}/mesen_gsu_ram_stop1.bin");
        if !diverged && Path::new(&stop_ram).exists() {
            let ref_ram = std::fs::read(&stop_ram).expect("stop ram");
            if ref_ram.len() == m.ram.len() {
                let diffs: Vec<usize> = (0..m.ram.len())
                    .filter(|&i| m.ram[i] != ref_ram[i])
                    .collect();
                eprintln!(
                    "RAM after GO-run: {} / {} bytes differ from reference ({:.2}%)",
                    diffs.len(),
                    m.ram.len(),
                    100.0 * diffs.len() as f64 / m.ram.len() as f64
                );
                // Cluster the diffs into 0x100-byte regions to localize.
                let mut region: std::collections::BTreeMap<usize, usize> =
                    std::collections::BTreeMap::new();
                for &i in &diffs {
                    *region.entry(i & !0xFF).or_insert(0) += 1;
                }
                eprintln!("differing RAM regions ($base: count):");
                for (base, n) in region.iter().take(40) {
                    eprintln!("  ${base:05X}: {n}");
                }
                // Show a few concrete differing bytes.
                eprintln!("sample differing bytes (addr: luna vs ref):");
                for &i in diffs.iter().take(20) {
                    eprintln!("  ${:05X}: {:02X} vs {:02X}", i, m.ram[i], ref_ram[i]);
                }
            }
        }
    }

    #[test]
    fn round_up_pow2_works() {
        assert_eq!(round_up_pow2(0), 1);
        assert_eq!(round_up_pow2(1), 1);
        assert_eq!(round_up_pow2(3), 4);
        assert_eq!(round_up_pow2(1024 * 1024), 1024 * 1024);
        assert_eq!(round_up_pow2(1024 * 1024 + 1), 2 * 1024 * 1024);
    }

    #[test]
    fn reset_state_matches_spec() {
        let m = fx();
        let s = m.snapshot();
        assert_eq!(s.vcr, 0x04, "VCR resets to 0x04 (spec §1.8)");
        assert!(!s.running, "GSU starts halted");
        assert!(m.cache_valid.iter().all(|&v| !v), "cache starts invalid");
        assert_eq!(m.rom_size(), 1024 * 1024);
        assert_eq!(m.sram_size(), 0x8000);
    }

    #[test]
    fn lorom_window_reads_rom() {
        let mut m = fx();
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x8001)), Some(1));
        assert_eq!(m.read(make_addr(0x00, 0xFFFF)), Some(0xFF));
        // $80 mirrors $00.
        assert_eq!(m.read(make_addr(0x80, 0x8000)), Some(0));
    }

    #[test]
    fn linear_window_aliases_lorom() {
        let mut m = fx();
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0));
        assert_eq!(
            m.read(make_addr(0x40, 0x8000)),
            m.read(make_addr(0x01, 0x8000)),
            "linear and LoROM views alias the same ROM byte"
        );
    }

    #[test]
    fn ram_round_trips_via_both_windows() {
        let mut m = fx();
        assert!(m.write(make_addr(0x70, 0x0000), 0xAB));
        assert_eq!(m.read(make_addr(0x70, 0x0000)), Some(0xAB));
        assert_eq!(m.read(make_addr(0x00, 0x6000)), Some(0xAB));
        assert!(m.write(make_addr(0x00, 0x6001), 0xCD));
        assert_eq!(m.read(make_addr(0x70, 0x0001)), Some(0xCD));
    }

    #[test]
    fn register_file_byte_access() {
        let mut m = fx();
        assert!(m.write(make_addr(0x00, 0x3006), 0x34)); // R3 low
        assert!(m.write(make_addr(0x00, 0x3007), 0x12)); // R3 high
        assert_eq!(m.snapshot().r[3], 0x1234);
        assert_eq!(m.read(make_addr(0x00, 0x3006)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0x3007)), Some(0x12));
    }

    #[test]
    fn writing_r15_high_byte_starts_the_gsu() {
        let mut m = fx();
        assert!(!m.snapshot().running);
        m.write(make_addr(0x00, 0x301E), 0x00); // R15 low
        assert!(!m.snapshot().running, "R15 low alone does not GO");
        m.write(make_addr(0x00, 0x301F), 0x80); // R15 high → GO
        assert!(
            m.snapshot().running,
            "writing R15 high byte sets the GO flag"
        );
    }

    #[test]
    fn clearing_go_via_sfr_zeroes_cbr_and_flushes_cache() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x301F), 0x80); // GO
        m.regs.cbr = 0x1230;
        m.cache_valid[0] = true;
        m.write(make_addr(0x00, 0x3030), 0x00); // SFR low, g clear → 1→0
        assert!(!m.snapshot().running);
        assert_eq!(m.snapshot().cbr, 0, "g 1→0 zeroes CBR");
        assert!(m.cache_valid.iter().all(|&v| !v), "g 1→0 flushes cache");
    }

    #[test]
    fn irq_latch_and_acknowledge() {
        let mut m = fx();
        m.sfr_set(SFR_IRQ, true);
        assert!(m.coproc_main_irq_pending(), "IRQ asserted to main CPU");
        let _ = m.read(make_addr(0x00, 0x3031)); // SFR high read acknowledges
        assert!(!m.coproc_main_irq_pending(), "$3031 read clears the IRQ");
    }

    #[test]
    fn scmr_scrambled_bit_layout_round_trips() {
        let mut m = fx();
        // byte bit5=ht_hi, bit4=ron, bit3=ran, bit2=ht_lo, bits1:0=md.
        // ht=0b10, ron=1, ran=0, md=3 → 0b0011_0011 = 0x33
        m.write(make_addr(0x00, 0x303A), 0x33);
        assert_eq!(m.regs.scmr_ht, 0b10);
        assert!(m.regs.scmr_ron);
        assert!(!m.regs.scmr_ran);
        assert_eq!(m.regs.scmr_md, 3);
        assert_eq!(m.snapshot().scmr, 0x33, "round-trips back to the wire byte");
    }

    #[test]
    fn busy_vector_shown_while_gsu_owns_rom() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x303A), 0x10); // ron = 1
        m.write(make_addr(0x00, 0x301F), 0x80); // GO
        // Vector indexed by addr & 0x0F: $FFE4 → idx 4 → 0x04;
        // $FFEE → idx 14 → 0x0C; odd addresses → 0x01.
        assert_eq!(m.read(make_addr(0x00, 0xFFE4)), Some(0x04));
        assert_eq!(m.read(make_addr(0x00, 0xFFEE)), Some(0x0C));
        assert_eq!(m.read(make_addr(0x00, 0xFFEF)), Some(0x01));
    }

    #[test]
    fn cache_ram_write_validates_line_on_byte_15() {
        let mut m = fx();
        assert!(!m.cache_valid[0]);
        for i in 0..16u16 {
            m.write(make_addr(0x00, 0x3100 + i), i as u8);
        }
        assert!(m.cache_valid[0], "writing byte 15 of line 0 validates it");
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x310F)), Some(15));
    }

    #[test]
    fn config_registers_decode() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x3037), 0xA0); // CFGR
        assert_eq!(m.snapshot().cfgr, 0xA0);
        m.write(make_addr(0x00, 0x3039), 0x01); // CLSR fast
        assert!(m.snapshot().clsr);
        m.write(make_addr(0x00, 0x3038), 0x42); // SCBR
        assert_eq!(m.snapshot().scbr, 0x42);
        m.write(make_addr(0x00, 0x3033), 0x01); // BRAMR
        assert!(m.snapshot().bramr);
    }

    #[test]
    fn r14_write_raises_rom_pending() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x301C), 0x00); // R14 low
        assert!(
            m.snapshot().sfr & SFR_R != 0,
            "R14 write arms ROM-read pending"
        );
    }

    // ----- phase 2a: instruction engine ----------------------------------

    /// Load a GSU program into the instruction cache at CBR=0 and prime the
    /// pipeline so the first `run_one()` executes `program[0]`. PBR=0, GO set.
    fn load_and_go(m: &mut SuperFxMapper, program: &[u8]) {
        for (i, &b) in program.iter().enumerate() {
            m.cache[i] = b;
        }
        for line in 0..=(program.len() / 16) {
            m.cache_valid[line] = true;
        }
        m.regs.cbr = 0;
        m.regs.pbr = 0;
        m.regs.r[15] = 1;
        m.regs.pipeline = program[0];
        m.sfr_set(SFR_G, true);
    }

    #[test]
    fn engine_add_registers() {
        let mut m = fx();
        m.regs.r[1] = 10;
        m.regs.r[2] = 20;
        m.regs.sreg = 1;
        m.regs.dreg = 3;
        m.execute(0x52); // add r2 → r3 = r1 + r2
        assert_eq!(m.regs.r[3], 30);
        assert!(!m.sfr_get(SFR_Z));
        assert!(!m.sfr_get(SFR_CY));
    }

    #[test]
    fn engine_sub_sets_carry_and_zero() {
        let mut m = fx();
        m.regs.r[1] = 5;
        m.regs.sreg = 1;
        m.regs.dreg = 1;
        m.execute(0x61); // sub r1 → r1 - r1 = 0
        assert_eq!(m.regs.r[1], 0);
        assert!(m.sfr_get(SFR_Z));
        assert!(m.sfr_get(SFR_CY), "no borrow → carry set");
    }

    #[test]
    fn engine_cmp_does_not_write_dr() {
        let mut m = fx();
        m.regs.r[1] = 5;
        m.regs.r[2] = 5;
        m.regs.sreg = 1;
        m.regs.dreg = 1;
        // CMP = alt3 (alt1 + alt2).
        m.sfr_set(SFR_ALT1, true);
        m.sfr_set(SFR_ALT2, true);
        m.execute(0x62); // cmp r2
        assert_eq!(m.regs.r[1], 5, "CMP must not write dr");
        assert!(m.sfr_get(SFR_Z), "5 - 5 == 0");
    }

    #[test]
    fn engine_and_then_bic() {
        let mut m = fx();
        m.regs.r[1] = 0xFF0F;
        m.regs.r[2] = 0x0F0F;
        m.regs.sreg = 1;
        m.regs.dreg = 3;
        m.execute(0x72); // and r2
        assert_eq!(m.regs.r[3], 0x0F0F);
        m.regs.sreg = 1;
        m.regs.dreg = 4;
        m.sfr_set(SFR_ALT1, true);
        m.execute(0x72); // bic r2 → r1 & ~r2
        assert_eq!(m.regs.r[4], 0xFF0F & !0x0F0F);
    }

    #[test]
    fn engine_inc_dec_wrap_and_flags() {
        let mut m = fx();
        m.regs.r[5] = 0xFFFF;
        m.execute(0xD5); // inc r5 → 0
        assert_eq!(m.regs.r[5], 0);
        assert!(m.sfr_get(SFR_Z));
        m.execute(0xE5); // dec r5 → 0xFFFF
        assert_eq!(m.regs.r[5], 0xFFFF);
        assert!(m.sfr_get(SFR_S));
    }

    #[test]
    fn engine_lsr_shifts_out_carry() {
        let mut m = fx();
        m.regs.r[1] = 0x0003;
        m.regs.sreg = 1;
        m.regs.dreg = 1;
        m.execute(0x03); // lsr
        assert_eq!(m.regs.r[1], 1);
        assert!(m.sfr_get(SFR_CY), "bit 0 was 1");
    }

    #[test]
    fn engine_swap_bytes() {
        let mut m = fx();
        m.regs.r[1] = 0x1234;
        m.regs.sreg = 1;
        m.regs.dreg = 1;
        m.execute(0x4D); // swap
        assert_eq!(m.regs.r[1], 0x3412);
    }

    #[test]
    fn engine_sex_sign_extends_low_byte() {
        let mut m = fx();
        m.regs.r[1] = 0x0080;
        m.regs.sreg = 1;
        m.regs.dreg = 1;
        m.execute(0x95); // sex
        assert_eq!(m.regs.r[1], 0xFF80);
        assert!(m.sfr_get(SFR_S));
    }

    #[test]
    fn engine_merge_packs_r7_r8() {
        let mut m = fx();
        m.regs.r[7] = 0xAB00;
        m.regs.r[8] = 0xCD00;
        m.regs.dreg = 3;
        m.execute(0x70); // merge → (r7 & 0xff00) | (r8 >> 8)
        assert_eq!(m.regs.r[3], 0xABCD);
    }

    #[test]
    fn engine_alt_prefix_consumed_by_one_op() {
        let mut m = fx();
        m.regs.r[0] = 0;
        // ALT2 ; ADD #3 ; ADD r0
        load_and_go(&mut m, &[0x3E, 0x53, 0x50]);
        m.run_one(); // ALT2
        m.run_one(); // ADD #3 → r0 = 0 + 3 (immediate)
        assert_eq!(m.regs.r[0], 3, "immediate add");
        m.run_one(); // ADD r0 → r0 = 3 + 3 (register mode proves alt2 cleared)
        assert_eq!(m.regs.r[0], 6, "second add is register-mode");
    }

    #[test]
    fn engine_loop_branches_until_zero() {
        let mut m = fx();
        m.regs.r[12] = 2; // loop count
        m.regs.r[13] = 0x40; // branch target
        m.execute(0x3C); // loop: r12-- = 1 (non-zero) → r15 = r13
        assert_eq!(m.regs.r[12], 1);
        assert!(m.modified_r15, "branch taken");
        assert_eq!(m.regs.r[15], 0x40);
        m.modified_r15 = false;
        m.execute(0x3C); // r12-- = 0 → no branch
        assert_eq!(m.regs.r[12], 0);
        assert!(!m.modified_r15, "no branch at zero");
        assert!(m.sfr_get(SFR_Z));
    }

    #[test]
    fn engine_stop_clears_go_and_irqs() {
        let mut m = fx();
        m.sfr_set(SFR_G, true);
        m.execute(0x00); // stop, cfgr.irq = 0 → raises IRQ
        assert!(!m.sfr_get(SFR_G), "STOP clears GO");
        assert!(m.coproc_main_irq_pending(), "unmasked STOP raises IRQ");

        let mut masked = fx();
        masked.regs.cfgr = 0x80; // IRQ mask
        masked.sfr_set(SFR_G, true);
        masked.execute(0x00);
        assert!(!masked.sfr_get(SFR_G));
        assert!(
            !masked.coproc_main_irq_pending(),
            "masked STOP does not raise IRQ"
        );
    }

    #[test]
    fn engine_step_coproc_runs_until_stop() {
        let mut m = fx();
        load_and_go(&mut m, &[0xD0, 0x00]); // inc r0 ; stop
        m.step_coproc(1000); // ample budget
        assert!(!m.snapshot().running, "GSU halted on STOP");
        assert_eq!(m.snapshot().r[0], 1, "INC r0 executed exactly once");
    }

    // ----- phase 2b: memory ops, multiplies, immediate loads -------------

    #[test]
    fn engine_store_then_load_word() {
        let mut m = fx();
        m.regs.r[1] = 0x0010; // RAM address
        m.regs.r[2] = 0xABCD; // data
        m.regs.sreg = 2;
        m.execute(0x31); // stw (r1) ← r2
        m.regs.sreg = 0;
        m.regs.dreg = 3;
        m.execute(0x41); // ldw r3 ← (r1)
        assert_eq!(m.regs.r[3], 0xABCD);
        assert_eq!(m.ram[0x10], 0xCD, "little-endian low byte");
        assert_eq!(m.ram[0x11], 0xAB, "little-endian high byte");
    }

    #[test]
    fn engine_store_byte_only_writes_one() {
        let mut m = fx();
        m.ram[0x21] = 0x99;
        m.regs.r[1] = 0x0020;
        m.regs.r[2] = 0x12CD;
        m.regs.sreg = 2;
        m.sfr_set(SFR_ALT1, true);
        m.execute(0x31); // stb (r1) ← r2 low byte
        m.sync_ram_buffer(); // flush the pending delayed write
        assert_eq!(m.ram[0x20], 0xCD);
        assert_eq!(m.ram[0x21], 0x99, "STB must not touch the high byte");
    }

    #[test]
    fn engine_mult_signed() {
        let mut m = fx();
        m.regs.r[1] = 0x00FF; // low byte 0xFF = -1
        m.regs.r[2] = 0x0002;
        m.regs.sreg = 1;
        m.regs.dreg = 3;
        m.execute(0x82); // mult r2 → (i8)-1 * (i8)2 = -2
        assert_eq!(m.regs.r[3], 0xFFFE);
    }

    #[test]
    fn engine_umult_unsigned() {
        let mut m = fx();
        m.regs.r[1] = 0x00FF; // 255
        m.regs.r[2] = 0x0002;
        m.regs.sreg = 1;
        m.regs.dreg = 3;
        m.sfr_set(SFR_ALT1, true);
        m.execute(0x82); // umult r2 → 255 * 2 = 510
        assert_eq!(m.regs.r[3], 0x01FE);
    }

    #[test]
    fn engine_getb_reads_rom_buffer() {
        let mut m = fx();
        // ramp ROM: rom[i] == i & 0xFF. Point the ROM buffer at byte 5.
        m.regs.rombr = 0x00;
        m.regs.r[14] = 0x0005;
        m.update_rom_buffer();
        m.regs.dreg = 3;
        m.execute(0xEF); // getb → dr = romdr
        assert_eq!(m.regs.r[3], 5);
    }

    #[test]
    fn engine_iwt_loads_immediate_word() {
        let mut m = fx();
        load_and_go(&mut m, &[0xF3, 0x34, 0x12]); // iwt r3, #$1234
        m.run_one();
        assert_eq!(m.regs.r[3], 0x1234);
    }

    #[test]
    fn engine_ibt_sign_extends_immediate_byte() {
        let mut m = fx();
        load_and_go(&mut m, &[0xA3, 0x80]); // ibt r3, #$80
        m.run_one();
        assert_eq!(m.regs.r[3], 0xFF80);
    }

    // ----- phase 3: pixel-plot pipeline ----------------------------------

    #[test]
    fn engine_plot_then_rpix_roundtrip() {
        let mut m = fx();
        m.regs.scmr_md = 3; // 8bpp
        m.regs.scbr = 0;
        m.regs.por = 0;
        m.regs.colr = 0x42;
        m.regs.r[1] = 3; // x
        m.regs.r[2] = 5; // y
        m.execute(0x4C); // PLOT (x, y)
        // PLOT incremented r1; restore for the read-back.
        m.regs.r[1] = 3;
        m.regs.r[2] = 5;
        m.regs.dreg = 4;
        m.sfr_set(SFR_ALT1, true);
        m.execute(0x4C); // RPIX → r4
        assert_eq!(m.regs.r[4], 0x42, "RPIX reads back the plotted colour");
    }

    #[test]
    fn engine_plot_skips_transparent_color() {
        let mut m = fx();
        m.regs.scmr_md = 1; // 4bpp
        m.regs.por = 0;
        m.regs.colr = 0x00; // low nibble 0 → transparent
        m.regs.r[1] = 0;
        m.regs.r[2] = 0;
        m.execute(0x4C); // PLOT
        assert_eq!(
            m.pixelcache[0].bitpend, 0,
            "a colour-0 pixel is not plotted"
        );
    }

    #[test]
    fn engine_plot_writes_bitplanes_to_ram() {
        let mut m = fx();
        m.regs.scmr_md = 1; // 4bpp → 4 bitplanes
        m.regs.scbr = 0;
        m.regs.por = 0;
        m.regs.colr = 0x0F; // all four low planes set
        // Plot a full 8-pixel run at y=0, then force the flush via RPIX.
        for x in 0..8u16 {
            m.regs.r[1] = x;
            m.regs.r[2] = 0;
            m.execute(0x4C);
        }
        m.regs.r[1] = 0;
        m.regs.r[2] = 0;
        m.regs.dreg = 5;
        m.sfr_set(SFR_ALT1, true);
        m.execute(0x4C); // RPIX flushes both caches
        // tile 0, row 0: planes 0..3 at byte offsets {0,1,16,17}; a full
        // run of colour 0x0F sets every bit of planes 0..3.
        assert_eq!(m.ram[0], 0xFF, "plane 0 full");
        assert_eq!(m.ram[1], 0xFF, "plane 1 full");
        assert_eq!(m.ram[16], 0xFF, "plane 2 full");
        assert_eq!(m.ram[17], 0xFF, "plane 3 full");
        assert_eq!(m.regs.r[5], 0x0F, "RPIX reads colour 0x0F back");
    }
}
