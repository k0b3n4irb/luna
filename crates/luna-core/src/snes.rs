//! Top-level [`Snes`] machine struct.
//!
//! Wires together a `Cpu65816`, 128 KB of WRAM, the cartridge mapper, and
//! a placeholder MMIO stub. The PPU / APU / DMA are still TODOs; reads
//! from their registers return `0xFF` (open-bus convention) and writes
//! are silently dropped.

use crate::apu_stub::ApuStub;
use crate::cpu_regs::CpuRegs;
use luna_apu::Apu;
use luna_bus::hirom::HiRomMapper;
use luna_bus::lorom::LoRomMapper;
use luna_bus::{
    Addr24, Bus, MCycles, Mapper, MapperKind, address_speed, bank_of, make_addr, offset_of,
};
use luna_cartridge::Cartridge;
use luna_cpu_65c816::Cpu;
use luna_dma::{Dma, DmaBus};
use luna_ppu::Ppu;

/// Top-level SNES machine.
pub struct Snes {
    /// Main CPU (65C816).
    pub cpu: Cpu,
    /// Picture Processing Unit — VRAM / CGRAM / OAM + registers.
    pub ppu: Ppu,
    /// DMA controller — 8 channels at `$4300-$437F` plus `$420B/$420C`.
    pub dma: Dma,
    /// CPU-system registers at `$4200-$421F` (NMITIMEN, multiplication,
    /// division, IRQ status, etc.).
    pub cpu_regs: CpuRegs,
    /// 128 KB Work RAM (banks `$7E-$7F` and the LowRAM mirror).
    pub wram: Box<[u8; 0x20000]>,
    /// Cartridge mapper (LoROM in P0.6; other mappers in V1+).
    pub mapper: Box<dyn Mapper + Send>,
    /// FastROM `MEMSEL` bit — when set, ROM in banks `$80-$FF` at
    /// `$8000-$FFFF` is FAST (6 mclk) instead of SLOW (8 mclk).
    pub fast_rom: bool,
    /// Latched NMI line (`$4210` read clears it).
    pub nmi_pending: bool,
    /// IRQ line currently asserted.
    pub irq_pending: bool,
    /// Total master cycles consumed since reset.
    pub total_mclk: MCycles,

    // ------------- APU -------------
    /// Real SPC700 + 64 KB ARAM + IPL ROM + mailboxes. Runs in
    /// parallel with the main CPU at a 21 mclk : 1 spc-cycle ratio.
    pub apu_real: Apu,
    /// `true` once the SPC700 has hit an opcode our handler doesn't
    /// implement (panic-caught). Subsequent reads of `$2140-$2143`
    /// fall back to the cached state and the dumb mailbox stub takes
    /// over for any further CPU writes so the game doesn't deadlock.
    pub apu_panicked: bool,
    /// Legacy heuristic mailbox stub — used only after the real APU
    /// has stopped (panic) so commercial games that depend on
    /// driver-specific acks still have *some* fallback.
    pub apu_stub_fallback: ApuStub,

    // ------------- Scanline-accurate scheduler -------------
    /// Current PPU scanline (0..=261 for NTSC). Lines 0-223 are the
    /// visible region; 224 is the post-visible "1 dot of overlap"
    /// line; 225-261 are vertical blank. VBlank-NMI fires on entry
    /// to line 225.
    pub ppu_line: u16,
    /// Master cycles consumed within the current scanline (0..1364).
    /// Wraps to 0 each time we cross a scanline boundary.
    pub mcycles_in_line: u32,
    /// Number of full PPU frames completed since reset. Increments
    /// once per wrap from line 261 → line 0.
    pub frame_count: u64,
    /// How many NMIs we have actually delivered to the CPU. Stays
    /// behind `frame_count` if `NMITIMEN.7` is off.
    pub nmis_serviced: u64,
}

/// Master cycles per PPU scanline on NTSC (1364 mclk = 4 dots × 341).
pub const MCYCLES_PER_SCANLINE: u32 = 1364;
/// Total scanlines per NTSC frame (visible + post + vblank).
pub const NTSC_SCANLINES_PER_FRAME: u16 = 262;
/// Scanline on which VBlank begins on NTSC. The PPU writes the
/// `$4210` "NMI flag" bit and (if `NMITIMEN.7` is set) raises the NMI
/// pin at the start of this line.
pub const NTSC_VBLANK_START_LINE: u16 = 225;

impl Snes {
    /// Build a new machine from a parsed cartridge.
    ///
    /// Panics if the cartridge layout is not supported by the V1 mapper
    /// set — currently LoROM only. HiROM / SA-1 / Super FX land in
    /// later phases.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        let sram_bytes = (cart.header.sram_size_kb as usize) * 1024;
        let mapper: Box<dyn Mapper + Send> = match cart.header.mapper_kind {
            MapperKind::LoRom => Box::new(LoRomMapper::new(cart.rom, sram_bytes)),
            kind @ (MapperKind::HiRom | MapperKind::ExHiRom) => {
                Box::new(HiRomMapper::with_kind(kind, cart.rom, sram_bytes))
            }
            other => {
                panic!(
                    "Cartridge requires coprocessor support not yet implemented: {other:?}. \
                     SA-1 / Super FX / S-DD1 / SPC7110 will land in their own dedicated phases."
                );
            }
        };

        Self {
            cpu: Cpu::new(),
            ppu: Ppu::new(),
            dma: Dma::new(),
            cpu_regs: CpuRegs::new(),
            wram: Box::new([0; 0x20000]),
            mapper,
            fast_rom: cart.header.fast_rom,
            nmi_pending: false,
            irq_pending: false,
            total_mclk: 0,
            // Compat: post-reset, the IPL ROM has dropped these into
            // the CPU-facing mailbox to signal "audio CPU ready".
            apu_real: Apu::new(),
            apu_panicked: false,
            apu_stub_fallback: ApuStub::new(),
            ppu_line: 0,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
        }
    }

    /// Run the CPU reset sequence: read the reset vector at `$00:FFFC`
    /// via the bus and load `PC`.
    pub fn reset(&mut self) {
        let Snes {
            cpu,
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
            ..
        } = self;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked: *apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        cpu.reset(&mut bus);
    }

    /// Execute one CPU instruction. Returns the master-cycle cost of
    /// that instruction (accumulated through [`Bus::io_cycle`]).
    pub fn step(&mut self) -> MCycles {
        let before = self.total_mclk;
        let Snes {
            cpu,
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
            ..
        } = self;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked: *apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        cpu.step(&mut bus);

        // Advance the PPU scanline counter by the master cycles this
        // instruction consumed. Crossing a scanline boundary triggers
        // line-tick events; crossing into line 225 fires VBlank/NMI.
        let consumed = self.total_mclk - before;
        self.advance_scheduler(consumed as u32);

        // Catch up the APU by the same amount. The SPC700 now stops
        // gracefully when it hits an unimplemented opcode (no more
        // panics) — `Apu::step` simply returns early once
        // `cpu.stopped == true`. We mirror that into `apu_panicked`
        // so the mailbox bus path knows to use the fallback stub.
        if !self.apu_panicked {
            self.apu_real.step(consumed as u32);
            if self.apu_real.cpu.stopped {
                self.apu_panicked = true;
            }
        }

        consumed
    }

    /// Drive the scanline scheduler by `mcycles` of consumed master
    /// cycles. Inspired by bsnes / ares' line-tick approach: we keep
    /// a `(ppu_line, mcycles_in_line)` cursor and walk it forward,
    /// firing events at each line boundary.
    fn advance_scheduler(&mut self, mcycles: u32) {
        self.mcycles_in_line += mcycles;
        while self.mcycles_in_line >= MCYCLES_PER_SCANLINE {
            self.mcycles_in_line -= MCYCLES_PER_SCANLINE;
            self.advance_one_scanline();
        }
    }

    /// Cross one scanline boundary and apply the per-line events the
    /// SNES PPU drives.
    fn advance_one_scanline(&mut self) {
        self.ppu_line += 1;
        if self.ppu_line == NTSC_VBLANK_START_LINE {
            // Entering VBlank. Latch the NMI flag visible at $4210
            // and set the VBlank bit of HVBJOY.
            self.cpu_regs.nmi_flag = true;
            self.cpu_regs.hvbjoy |= 0x80;
            if self.cpu_regs.nmitimen & 0x80 != 0 {
                self.cpu.trigger_nmi();
                self.nmis_serviced = self.nmis_serviced.saturating_add(1);
            }
            // Joypad auto-read: hardware copies the live pad state
            // into $4218-$421F at the start of every VBlank when
            // NMITIMEN.0 is set. Busy bit clears a few lines later.
            self.cpu_regs.latch_joypad_auto_read();
        } else if self.ppu_line == NTSC_VBLANK_START_LINE + 3 {
            // ~3 scanlines after VBlank entry the auto-read sequence
            // is done. Drop HVBJOY.0 so polling games see ready.
            self.cpu_regs.clear_joypad_busy();
        }
        // H- / V-counter IRQ. NMITIMEN bits 5:4 select the trigger:
        //   00 = disabled
        //   01 = H-counter match (every scanline at H = HTIME)
        //   10 = V-counter match (line N == VTIME, H == 0)
        //   11 = H AND V match (line N == VTIME AND H == HTIME)
        //
        // Our scheduler only fires events at scanline boundaries, so
        // H-counter-precise IRQ timing isn't dot-accurate yet. The
        // tracking we do model:
        //   * V-only (mode 10): raise IRQ when we enter `vtime`.
        //   * H+V (mode 11):   same as V-only here — at scanline
        //                       boundary the H-counter is implicitly
        //                       0; if HTIME is also 0 the match is
        //                       exact, otherwise we're a few mclk
        //                       off. Enough for games that just
        //                       want a per-scanline raster IRQ.
        //   * H-only (mode 01): fire on every scanline transition.
        //                       Most games use this with HTIME = 0
        //                       which lands on the canonical
        //                       scanline-start raster point.
        let irq_mode = (self.cpu_regs.nmitimen >> 4) & 0x03;
        let fire_irq = match irq_mode {
            0b00 => false,
            0b01 => true,
            0b10 => self.ppu_line == self.cpu_regs.vtime,
            0b11 => self.ppu_line == self.cpu_regs.vtime && self.cpu_regs.htime == 0,
            _ => false,
        };
        if fire_irq {
            self.cpu_regs.irq_flag = true;
            self.cpu.trigger_irq();
        }
        if self.ppu_line >= NTSC_SCANLINES_PER_FRAME {
            // Frame wrap: back to line 0, clear VBlank bit, and re-
            // initialise HDMA tables for the new frame.
            self.ppu_line = 0;
            self.cpu_regs.hvbjoy &= !0x80;
            self.frame_count = self.frame_count.saturating_add(1);
            self.hdma_init_frame();
        }
        // After the line counter has advanced, fire HDMA on every
        // active channel for any visible scanline (0..=224 NTSC).
        // HDMA transfers happen during the line's H-blank, so doing
        // them once per scanline transition is the canonical place.
        if self.ppu_line < NTSC_VBLANK_START_LINE {
            self.hdma_run_line();
        }
    }

    /// Borrow-split helper: build a [`DmaBusView`] for HDMA against
    /// the same WRAM / mapper / PPU references DMA uses. Mirrors the
    /// pattern in the `$420B` write path.
    fn hdma_init_frame(&mut self) {
        let mut view = DmaBusView {
            wram: &mut self.wram,
            mapper: self.mapper.as_mut(),
            ppu: &mut self.ppu,
        };
        self.dma.hdma_init(&mut view);
    }

    fn hdma_run_line(&mut self) {
        let mut view = DmaBusView {
            wram: &mut self.wram,
            mapper: self.mapper.as_mut(),
            ppu: &mut self.ppu,
        };
        self.dma.hdma_run_line(&mut view);
    }

    /// Set the live joypad state for controller `idx` (0 = pad 1,
    /// 1 = pad 2). The new mask becomes visible to the game on the
    /// next VBlank auto-read latch — typically within ~16.7 ms.
    ///
    /// Bit layout (matches SNES hardware, MSB → LSB):
    /// `B Y SEL START Up Down Left Right A X L R 0 0 0 0`.
    pub fn set_joypad(&mut self, idx: usize, mask: u16) {
        self.cpu_regs.set_joypad(idx, mask);
    }

    /// Read 8 bytes starting at the current `PB:PC`. Used by the GUI to
    /// show the instruction stream around the CPU's program counter
    /// without disturbing emulation state.
    ///
    /// Reads go through the real bus, so PPU/CPU/MMIO regs *would* be
    /// observed if PC were in an MMIO window — but that's never the
    /// case for executable code in practice.
    #[must_use]
    pub fn peek_pc_bytes(&mut self, count: usize) -> Vec<u8> {
        let pc = self.cpu.pc;
        let pb = self.cpu.pb;
        let Snes {
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
            ..
        } = self;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked: *apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        (0..count)
            .map(|i| {
                let off = pc.wrapping_add(i as u16);
                bus.read(make_addr(pb, off))
            })
            .collect()
    }
}

// =============================================================================
// SnesBus
// =============================================================================

/// View of the machine exposed to the CPU during one instruction. Re-built
/// from scratch on each [`Snes::step`] so the borrow checker can prove
/// that the CPU and the bus borrow disjoint fields of `Snes`.
struct SnesBus<'a> {
    wram: &'a mut [u8; 0x20000],
    mapper: &'a mut dyn Mapper,
    ppu: &'a mut Ppu,
    dma: &'a mut Dma,
    cpu_regs: &'a mut CpuRegs,
    /// Real SPC700 + ARAM + IPL ROM. CPU mailbox reads pull from
    /// `apu_real.to_cpu_ports`; writes land in `apu_real.to_spc_ports`.
    apu_real: &'a mut Apu,
    /// Legacy heuristic stub — used when [`Snes::apu_panicked`] is
    /// `true` (i.e. the real SPC700 hit an unimplemented opcode).
    apu_stub_fallback: &'a mut ApuStub,
    /// Mirror of `Snes::apu_panicked` — captured at the start of the
    /// bus borrow so we know which mailbox path to use.
    apu_panicked: bool,
    fast_rom: bool,
    nmi: &'a mut bool,
    irq: &'a mut bool,
    mclk_total: &'a mut MCycles,
}

impl<'a> SnesBus<'a> {
    /// Resolve `addr` against the WRAM regions; returns the in-array
    /// offset if it maps to WRAM, else `None`.
    fn wram_offset(addr: Addr24) -> Option<usize> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        // LowRAM mirror: banks $00-$3F and $80-$BF, offsets $0000-$1FFF.
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset < 0x2000 {
            return Some(usize::from(offset));
        }
        // Full WRAM: banks $7E-$7F at any offset.
        if matches!(bank, 0x7E..=0x7F) {
            let high = usize::from(bank - 0x7E) << 16;
            return Some(high | usize::from(offset));
        }
        None
    }
}

impl<'a> SnesBus<'a> {
    /// Returns `Some(offset)` if `addr` falls in the PPU MMIO range
    /// (`$00-$3F:$2100-$213F` and the `$80-$BF` mirror). The offset is
    /// relative to `$2100` (0x00-0x3F).
    fn ppu_offset(addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x2100..=0x213F) {
            Some((offset - 0x2100) as u8)
        } else {
            None
        }
    }

    /// Returns `Some(offset)` if `addr` falls in the DMA register
    /// window (`$00-$3F:$4300-$437F` and the `$80-$BF` mirror).
    fn dma_offset(addr: Addr24) -> Option<u16> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4300..=0x437F) {
            Some(offset)
        } else {
            None
        }
    }

    /// `true` if `addr` is the `MDMAEN` register `$420B`.
    fn is_mdmaen(addr: Addr24) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset == 0x420B
    }

    /// `true` if `addr` is the `HDMAEN` register `$420C`.
    fn is_hdmaen(addr: Addr24) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset == 0x420C
    }

    /// Returns `Some(offset)` if `addr` is a CPU-system register at
    /// `$4200-$421F` (excluding the DMA-enable registers, which are
    /// routed to the DMA controller).
    fn cpu_reg_offset(addr: Addr24) -> Option<u16> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4200..=0x421F) {
            Some(offset)
        } else {
            None
        }
    }

    /// Returns `Some(port_idx)` (0-3) if `addr` is an APU mailbox port
    /// at `$2140-$2143` (or its `$80-$BF` mirror).
    fn apu_port(addr: Addr24) -> Option<usize> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x2140..=0x2143) {
            Some(usize::from(offset - 0x2140))
        } else {
            None
        }
    }
}

/// DMA-side view of the system: holds the minimum needed for a sync
/// transfer (WRAM + cartridge + PPU) **without** carrying a reference
/// to the [`Dma`] controller, so the controller can borrow itself
/// mutably while running.
struct DmaBusView<'a> {
    wram: &'a mut [u8; 0x20000],
    mapper: &'a mut dyn Mapper,
    ppu: &'a mut Ppu,
}

impl<'a> DmaBus for DmaBusView<'a> {
    fn read_a(&mut self, addr: Addr24) -> u8 {
        if let Some(o) = SnesBus::wram_offset(addr) {
            return self.wram[o];
        }
        // A-side ROM / SRAM reads via mapper; everything else is open
        // bus until those subsystems land.
        self.mapper.read(addr).unwrap_or(0xFF)
    }

    fn write_a(&mut self, addr: Addr24, value: u8) {
        if let Some(o) = SnesBus::wram_offset(addr) {
            self.wram[o] = value;
            return;
        }
        // SRAM writes go through the mapper; ROM writes drop.
        let _ = self.mapper.write(addr, value);
    }

    fn read_b(&mut self, b_offset: u8) -> u8 {
        // B-bus range $00-$3F = PPU. Other regions (APU $40-$43, WRAM
        // port $80-$83) read open-bus until those subsystems land.
        if b_offset <= 0x3F {
            self.ppu.read(b_offset)
        } else {
            0xFF
        }
    }

    fn write_b(&mut self, b_offset: u8, value: u8) {
        if b_offset <= 0x3F {
            self.ppu.write(b_offset, value);
        }
    }
}

impl<'a> Bus for SnesBus<'a> {
    fn read(&mut self, addr: Addr24) -> u8 {
        let speed = address_speed(addr, self.fast_rom);
        self.io_cycle(speed.mcycles());

        if let Some(o) = Self::wram_offset(addr) {
            return self.wram[o];
        }
        if let Some(off) = Self::ppu_offset(addr) {
            return self.ppu.read(off);
        }
        if let Some(port) = Self::apu_port(addr) {
            // Mailbox reads: prefer the real SPC (now timer-driven,
            // so its driver actually loops). Fall back to the
            // heuristic stub only if the SPC has stopped on an
            // unimplemented opcode.
            return if self.apu_panicked {
                self.apu_stub_fallback.read(port)
            } else {
                self.apu_real.cpu_read_port(port)
            };
        }
        if let Some(offset) = Self::dma_offset(addr) {
            return self.dma.read_register(offset).unwrap_or(0xFF);
        }
        if Self::is_mdmaen(addr) || Self::is_hdmaen(addr) {
            // MDMAEN / HDMAEN are write-only; reads return open bus.
            return 0xFF;
        }
        if let Some(reg_off) = Self::cpu_reg_offset(addr) {
            if let Some(v) = self.cpu_regs.read(reg_off) {
                return v;
            }
            // Write-only registers fall through to open bus.
            return 0xFF;
        }
        if let Some(v) = self.mapper.read(addr) {
            return v;
        }
        // Open bus stub.
        0xFF
    }

    fn write(&mut self, addr: Addr24, value: u8) {
        let speed = address_speed(addr, self.fast_rom);
        self.io_cycle(speed.mcycles());

        if let Some(o) = Self::wram_offset(addr) {
            self.wram[o] = value;
            return;
        }
        if let Some(off) = Self::ppu_offset(addr) {
            self.ppu.write(off, value);
            return;
        }
        if let Some(port) = Self::apu_port(addr) {
            // CPU writes the byte to BOTH the real APU's to_spc port
            // (so the SPC700 reads it at $F4-$F7) and the fallback
            // stub (in case the SPC has panicked and we need it
            // later). Cheap, no consistency issues since the stub
            // is only consulted when the real APU is dead.
            self.apu_real.cpu_write_port(port, value);
            self.apu_stub_fallback.write(port, value);
            return;
        }
        if let Some(offset) = Self::dma_offset(addr) {
            self.dma.write_register(offset, value);
            return;
        }
        if Self::is_mdmaen(addr) {
            // Trigger sync DMA on every channel selected in `value`.
            // We splat the SnesBus borrows: `dma` is mutated by
            // run_mdma, and the other refs flow into DmaBusView. This
            // is the borrow-split that lets the DMA call itself
            // recursively without re-entering the Bus impl.
            let mut view = DmaBusView {
                wram: self.wram,
                mapper: self.mapper,
                ppu: self.ppu,
            };
            self.dma.run_mdma(&mut view, value);
            return;
        }
        if Self::is_hdmaen(addr) {
            self.dma.hdmaen = value;
            return;
        }
        if let Some(reg_off) = Self::cpu_reg_offset(addr) {
            if self.cpu_regs.write(reg_off, value) {
                return;
            }
            // CpuRegs returned false → maybe a register that lives
            // elsewhere (e.g. $420D MEMSEL → fast_rom). Handle here.
            if reg_off == 0x420D {
                self.fast_rom = value & 0x01 != 0;
            }
            return;
        }
        // Mapper claims SRAM writes; anything not yet routed drops.
        let _ = self.mapper.write(addr, value);
    }

    fn io_cycle(&mut self, mcycles: MCycles) {
        *self.mclk_total = self.mclk_total.saturating_add(mcycles);
    }

    fn nmi_pending(&self) -> bool {
        *self.nmi
    }

    fn irq_pending(&self) -> bool {
        *self.irq
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::make_addr;

    /// Build a 32 KB LoROM that starts with `LDA #$42 ; STA $7E0000 ; STP`
    /// and has its reset vector pointing at `$8000`.
    fn demo_lorom() -> Cartridge {
        let mut rom = vec![0xEA; 32 * 1024]; // NOP-padded
        // Reset vector at $7FFC = $8000 (already in bank 0 LoROM space).
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Header at $7FC0.
        let off = 0x7FC0;
        for (i, b) in b"LUNA P0.6 DEMO       ".iter().enumerate() {
            rom[off + i] = *b;
        }
        rom[off + 0x15] = 0x20; // LoROM
        rom[off + 0x17] = 0x05; // 32 KB
        rom[off + 0x18] = 0x00; // no SRAM
        rom[off + 0x19] = 0x01; // NTSC
        // Checksum complement: 0x1234, checksum: !0x1234 = 0xEDCB.
        rom[off + 0x1C] = 0x34;
        rom[off + 0x1D] = 0x12;
        rom[off + 0x1E] = 0xCB;
        rom[off + 0x1F] = 0xED;
        // Program at $8000 (file offset 0):
        //   LDA #$42        A9 42
        //   STA $7E:0000    8F 00 00 7E
        //   STP             DB
        rom[0x0000] = 0xA9;
        rom[0x0001] = 0x42;
        rom[0x0002] = 0x8F;
        rom[0x0003] = 0x00;
        rom[0x0004] = 0x00;
        rom[0x0005] = 0x7E;
        rom[0x0006] = 0xDB;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn from_cartridge_sets_initial_state() {
        let cart = demo_lorom();
        let snes = Snes::from_cartridge(cart);
        assert_eq!(snes.total_mclk, 0);
        assert!(!snes.nmi_pending);
        assert!(!snes.irq_pending);
    }

    #[test]
    fn reset_loads_pc_from_vector_via_lorom_mapper() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        assert_eq!(snes.cpu.pc, 0x8000);
        assert_eq!(snes.cpu.pb, 0x00);
    }

    #[test]
    fn step_lda_imm_then_sta_long() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // LDA #$42
        snes.step();
        assert_eq!(snes.cpu.a8(), 0x42);
        // STA $7E:0000 — write goes through WRAM
        snes.step();
        assert_eq!(snes.wram[0], 0x42);
        // STP — CPU halts
        snes.step();
        assert!(snes.cpu.stopped);
    }

    #[test]
    fn wram_low_mirror_aliases_bank_7e() {
        // Direct write via the bus to bank 0, offset 0x100 should land
        // in WRAM[0x100] and be visible from bank 0x7E offset 0x100.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        let Snes {
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
            ..
        } = &mut snes;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked: *apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        bus.write(make_addr(0x00, 0x0100), 0xAA);
        // Read back from the mirror in $00:
        assert_eq!(bus.read(make_addr(0x00, 0x0100)), 0xAA);
        // And from $7E (full WRAM):
        assert_eq!(bus.read(make_addr(0x7E, 0x0100)), 0xAA);
    }

    #[test]
    fn dma_uploads_palette_via_mdmaen_trigger() {
        // End-to-end integration: CPU writes the DMA channel 0 setup
        // bytes, then writes $01 to $420B → the DMA controller pulls
        // 4 bytes from WRAM and pumps them through PPU $2122 (CGDATA).
        //
        // Program at $8000 (long-hand because we don't yet have
        // store-immediate; we LDA / STA each byte):
        //   LDA #$22 ; STA $4301  ; channel 0 BBAD = $22 → $2122
        //   LDA #$00 ; STA $4302  ; A1TL = $00
        //   LDA #$20 ; STA $4303  ; A1TH = $20 → A-bus addr $002000
        //   LDA #$7E ; STA $4304  ; A1B  = $7E
        //   LDA #$04 ; STA $4305  ; DAS  = $0004
        //   LDA #$00 ; STA $4300  ; DMAP = mode 0, +1, A→B
        //   LDA #$01 ; STA $420B  ; MDMAEN bit 0
        //   STP
        //
        // (DAS high byte stays at 0 from reset.)
        let cart = demo_lorom();
        let mut rom = cart.rom.clone();
        let prog = [
            0xA9, 0x22, 0x8D, 0x01, 0x43, // LDA #$22 ; STA $4301
            0xA9, 0x00, 0x8D, 0x02, 0x43, // LDA #$00 ; STA $4302
            0xA9, 0x20, 0x8D, 0x03, 0x43, // LDA #$20 ; STA $4303
            0xA9, 0x7E, 0x8D, 0x04, 0x43, // LDA #$7E ; STA $4304
            0xA9, 0x04, 0x8D, 0x05, 0x43, // LDA #$04 ; STA $4305
            0xA9, 0x00, 0x8D, 0x00, 0x43, // LDA #$00 ; STA $4300
            0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01 ; STA $420B (trigger)
            0xDB, // STP
        ];
        rom[..prog.len()].copy_from_slice(&prog);
        let cart = Cartridge::from_bytes(rom).unwrap();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu.db = 0;
        // Seed the palette bytes in WRAM at $7E:2000.
        // (CGRAM expects a low/high pair per color → 2 colors here.)
        snes.wram[0x2000] = 0x1F; // red.low
        snes.wram[0x2001] = 0x00; // red.high  → BGR555 = 0x001F (pure red)
        snes.wram[0x2002] = 0xE0; // green.low
        snes.wram[0x2003] = 0x03; // green.high → 0x03E0 (pure green)
        // Make sure the CGRAM word address starts at 0.
        snes.ppu.cgram.set_address(0);

        // Run until the STP halts the CPU. The longest path is 8
        // groups of LDA+STA = 16 instructions, plus the STP.
        for _ in 0..32 {
            if snes.cpu.stopped {
                break;
            }
            snes.step();
        }
        assert!(snes.cpu.stopped, "program should reach STP");

        // After the DMA, CGRAM colors 0 and 1 should be red then green.
        assert_eq!(snes.ppu.cgram.color(0), 0x001F, "color 0 = red");
        assert_eq!(snes.ppu.cgram.color(1), 0x03E0, "color 1 = green");
        // DAS is zeroed by hardware on completion.
        assert_eq!(snes.dma.channels[0].das, 0);
    }

    #[test]
    fn cpu_writes_to_ppu_register_reach_the_ppu() {
        // Build a program that writes $42 to PPU $2100 (INIDISP).
        // Reuse demo_lorom() so the SRAM exponent / checksum etc. are
        // all set correctly — then patch in the program bytes.
        let cart = demo_lorom();
        let mut rom = cart.rom.clone();
        // Program at $8000 (file offset 0): LDA #$42, STA $2100
        rom[0] = 0xA9;
        rom[1] = 0x42;
        rom[2] = 0x8D;
        rom[3] = 0x00;
        rom[4] = 0x21;
        // Re-checksum so the header parser still accepts the ROM (we
        // overwrote the demo_lorom's STA-target program at offset 0).
        // For now, demo_lorom's checksum bytes at $7FDC-$7FDF are
        // already valid for the original ROM. Since we're patching
        // only 5 bytes, just keep the same checksum (parser only checks
        // complement vs checksum XOR, not against ROM contents).
        let cart = Cartridge::from_bytes(rom).unwrap();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu.db = 0; // ensure data bank is 0 for abs addressing
        snes.step(); // LDA #$42
        snes.step(); // STA $2100
        assert_eq!(snes.ppu.inidisp, 0x42, "PPU INIDISP must reflect the write");
    }

    #[test]
    fn step_accumulates_master_cycles() {
        // NOP costs 6 master cycles (FastROM not set on a default LoROM,
        // so it's actually SLOW = 8). LDA #$42 reads opcode + operand =
        // 2 × 8 = 16. After running 1 NOP we should have ≥ 8 mclk total.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset(); // already pays 2 reads for the reset vector.
        let before = snes.total_mclk;
        snes.step(); // LDA #$42
        let after = snes.total_mclk;
        assert!(after > before, "step should advance master clock");
    }

    #[test]
    fn scheduler_advances_to_next_scanline_after_one_line_of_mcycles() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        let line_before = snes.ppu_line;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, line_before + 1);
        assert_eq!(snes.mcycles_in_line, 0);
    }

    #[test]
    fn scheduler_fires_nmi_at_vblank_start_when_nmitimen_set() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu_regs.nmitimen = 0x80; // NMI on VBlank enabled
        snes.ppu_line = NTSC_VBLANK_START_LINE - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, NTSC_VBLANK_START_LINE);
        assert!(snes.cpu_regs.nmi_flag);
        assert_eq!(snes.cpu_regs.hvbjoy & 0x80, 0x80);
        assert_eq!(snes.nmis_serviced, 1);
    }

    #[test]
    fn scheduler_does_not_trigger_nmi_when_masked_but_still_sets_flag() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu_regs.nmitimen = 0x00; // NMI masked
        snes.ppu_line = NTSC_VBLANK_START_LINE - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, NTSC_VBLANK_START_LINE);
        assert!(snes.cpu_regs.nmi_flag);
        assert_eq!(snes.nmis_serviced, 0);
    }

    #[test]
    fn scheduler_wraps_to_line_zero_after_full_frame_and_clears_vblank() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // Pretend we're at the last scanline of the frame, with VBlank
        // currently set (as it would be).
        snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
        snes.cpu_regs.hvbjoy = 0x80;
        let frame_before = snes.frame_count;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, 0);
        assert_eq!(snes.cpu_regs.hvbjoy & 0x80, 0);
        assert_eq!(snes.frame_count, frame_before + 1);
    }

    #[test]
    fn scheduler_handles_multi_line_advance_in_a_single_call() {
        // A single instruction can in theory consume more than one
        // scanline's worth of mcycles (e.g. inside a DMA burst). The
        // scheduler must run all line ticks instead of dropping them.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        let line_before = snes.ppu_line;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE * 5 + 100);
        assert_eq!(snes.ppu_line, line_before + 5);
        assert_eq!(snes.mcycles_in_line, 100);
    }
}
