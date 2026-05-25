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
use luna_bus::sa1::Sa1Mapper;
use luna_bus::{
    Addr24, Bus, MCycles, Mapper, MapperKind, address_speed, bank_of, make_addr, offset_of,
};
use luna_cartridge::Cartridge;
use luna_coproc::Sa1Chip;
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
    /// Video region as decoded from the cartridge header. Drives
    /// the scheduler's scanlines-per-frame + VBlank-entry line and
    /// the PPU's `STAT78` region bit (bit 4).
    pub region: luna_cartridge::Region,

    /// 17-bit WRAM address counter accessed through the
    /// `$2180`/`$2181`/`$2182`/`$2183` (WMDATA / WMADDL / WMADDM /
    /// WMADDH) bus surface. Auto-increments on every WMDATA read or
    /// write; wraps modulo `0x20000`.
    pub wm_addr: u32,
    /// Manual-mode joypad shift state.
    ///
    /// Per ares' `joypad.cpp` + Mesen2's `ControlManager`, when the
    /// game writes bit 0 of `$4016` it pulses LATCH on both
    /// controllers; while LATCH is held high the shift register
    /// stays loaded with the live button mask. Subsequent reads of
    /// `$4016` / `$4017` shift out one bit per access from the
    /// 16-bit register, MSB-first. After 16 reads the register
    /// returns 1 (open-but-pulled-high) as the shift train is
    /// exhausted.
    pub joypad_strobe: bool,
    /// 16-bit shift register for controller 1 manual-mode reads.
    pub joypad1_shift: u16,
    /// 16-bit shift register for controller 2 manual-mode reads.
    pub joypad2_shift: u16,
}

/// Master cycles per PPU scanline on NTSC (1364 mclk = 4 dots × 341).
pub const MCYCLES_PER_SCANLINE: u32 = 1364;

/// Convert the running master-clock counter into the PPU's current
/// (H, V) dot coordinate. 1364 master cycles per scanline; the
/// scanline count depends on region (262 NTSC / 312 PAL). Each
/// "dot" is 4 master cycles, so H is the line-relative master
/// clock divided by 4 (range 0..340).
#[inline]
fn current_hv(mclk_total: u64, scanlines: u16) -> (u16, u16) {
    let per_frame = u64::from(MCYCLES_PER_SCANLINE) * u64::from(scanlines);
    let in_frame = mclk_total % per_frame;
    let v = (in_frame / u64::from(MCYCLES_PER_SCANLINE)) as u16;
    let h = ((in_frame % u64::from(MCYCLES_PER_SCANLINE)) / 4) as u16;
    (h, v)
}

/// Region-aware scanline parameters.
#[inline]
#[must_use]
pub fn scanlines_per_frame(region: luna_cartridge::Region) -> u16 {
    match region {
        luna_cartridge::Region::Pal => PAL_SCANLINES_PER_FRAME,
        _ => NTSC_SCANLINES_PER_FRAME,
    }
}

/// Scanline on which VBlank starts for the given region (the line
/// the scheduler latches the NMI flag, sets HVBJOY.7, and — if
/// NMITIMEN.7 is on — triggers an NMI).
#[inline]
#[must_use]
pub fn vblank_start_line(region: luna_cartridge::Region) -> u16 {
    match region {
        luna_cartridge::Region::Pal => PAL_VBLANK_START_LINE,
        _ => NTSC_VBLANK_START_LINE,
    }
}
/// Total scanlines per NTSC frame (visible + post + vblank).
pub const NTSC_SCANLINES_PER_FRAME: u16 = 262;
/// Total scanlines per PAL frame.
pub const PAL_SCANLINES_PER_FRAME: u16 = 312;
/// Scanline on which VBlank begins on NTSC. The PPU writes the
/// `$4210` "NMI flag" bit and (if `NMITIMEN.7` is set) raises the NMI
/// pin at the start of this line.
pub const NTSC_VBLANK_START_LINE: u16 = 225;
/// PAL is identical except it has more total scanlines, all of which
/// fall inside VBlank — the visible region is still 224 lines (or
/// 239 with overscan, which we don't model yet).
pub const PAL_VBLANK_START_LINE: u16 = 240;

impl Snes {
    /// Build a new machine from a parsed cartridge.
    ///
    /// Panics if the cartridge layout is not supported by the V1 mapper
    /// set — currently LoROM only. HiROM / SA-1 / Super FX land in
    /// later phases.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        let sram_bytes = (cart.header.sram_size_kb as usize) * 1024;
        let region = cart.header.region;
        let mapper: Box<dyn Mapper + Send> = match cart.header.mapper_kind {
            MapperKind::LoRom => Box::new(LoRomMapper::new(cart.rom, sram_bytes)),
            kind @ (MapperKind::HiRom | MapperKind::ExHiRom) => {
                Box::new(HiRomMapper::with_kind(kind, cart.rom, sram_bytes))
            }
            // SA-1 — phase-2: ROM banking + I-RAM + BW-RAM + multiplier
            // MMIO wrapped in a [`Sa1Chip`] that also drives the SA-1's
            // own 65C816 (released from reset by main-CPU writes to
            // `$2200 CCNT`).
            MapperKind::Sa1 => Box::new(Sa1Chip::new(Sa1Mapper::new(cart.rom, sram_bytes))),
            other => {
                panic!(
                    "Cartridge requires coprocessor support not yet implemented: {other:?}. \
                     Super FX / S-DD1 / SPC7110 will land in their own dedicated phases."
                );
            }
        };

        let mut ppu = Ppu::new();
        // Flip STAT78's region bit (bit 4) for PAL carts. NTSC and
        // "Unknown" land at the same default (bit clear).
        if matches!(region, luna_cartridge::Region::Pal) {
            ppu.stat78 |= 0x10;
        }

        Self {
            cpu: Cpu::new(),
            ppu,
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
            region,
            wm_addr: 0,
            joypad_strobe: false,
            joypad1_shift: 0,
            joypad2_shift: 0,
        }
    }

    /// Cached scanlines-per-frame for the current region — propagates
    /// into every [`SnesBus`] borrow.
    #[inline]
    fn region_scanlines(&self) -> u16 {
        scanlines_per_frame(self.region)
    }

    /// Run the CPU reset sequence: read the reset vector at `$00:FFFC`
    /// via the bus and load `PC`.
    pub fn reset(&mut self) {
        let scanlines = self.region_scanlines();
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
        };
        cpu.reset(&mut bus);
    }

    /// Execute one CPU instruction. Returns the master-cycle cost of
    /// that instruction (accumulated through [`Bus::io_cycle`]).
    pub fn step(&mut self) -> MCycles {
        let before = self.total_mclk;
        let scanlines = self.region_scanlines();
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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

        // Catch up any cartridge coprocessor (SA-1 / Super FX / DSP-1
        // / …). Plain LoROM/HiROM mappers no-op here.
        self.mapper.step_coproc(consumed as u32);

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
        let vblank_start = vblank_start_line(self.region);
        let scanlines = scanlines_per_frame(self.region);
        self.ppu_line += 1;
        if self.ppu_line == vblank_start {
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
            //
            // Per ares' `controllerPort.latch()` chained off the
            // auto-poll counter rollover, the same auto-read pulse
            // also re-arms the manual-mode shift register. Games
            // that read $4016/$4017 right after the auto-read
            // window expect the shift register to reflect the
            // just-latched controller state.
            self.cpu_regs.latch_joypad_auto_read();
            if self.cpu_regs.nmitimen & 0x01 != 0 {
                self.joypad1_shift = self.cpu_regs.joypad1_latched;
                self.joypad2_shift = self.cpu_regs.joypad2_latched;
            }
        } else if self.ppu_line == vblank_start + 3 {
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
        if self.ppu_line >= scanlines {
            // Frame wrap: back to line 0, clear VBlank bit, and re-
            // initialise HDMA tables for the new frame.
            self.ppu_line = 0;
            self.cpu_regs.hvbjoy &= !0x80;
            self.frame_count = self.frame_count.saturating_add(1);
            self.hdma_init_frame();
        }
        // After the line counter has advanced, fire HDMA on every
        // active visible scanline. HDMA transfers happen during the
        // line's H-blank, so doing them once per scanline transition
        // is the canonical place.
        if self.ppu_line < vblank_start {
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
        let scanlines = self.region_scanlines();
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
    wm_addr: &'a mut u32,
    joypad_strobe: &'a mut bool,
    joypad1_shift: &'a mut u16,
    joypad2_shift: &'a mut u16,
    mclk_total: &'a mut MCycles,
    /// Total scanlines per frame for the current cart's region —
    /// used by the H/V counter latch path (\$2137 / WRIO) to wrap
    /// the V coordinate at the right boundary.
    scanlines_per_frame: u16,
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

    /// `Some(low_byte_of_offset)` if `addr` is one of the four WRAM-port
    /// registers ($2180-$2183): `0x80` = WMDATA, `0x81` = WMADDL,
    /// `0x82` = WMADDM, `0x83` = WMADDH. Mirror banks $80-BF apply.
    fn wram_port_offset(addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x2180..=0x2183) {
            Some((offset & 0xFF) as u8)
        } else {
            None
        }
    }

    /// `true` if `addr` is the manual-mode joypad serial port at
    /// $4016 (JOYSER0 — write LATCH / read controller-1 bit) or
    /// $4017 (JOYSER1 — read controller-2 bit; writes drive the
    /// expansion port and are ignored).
    fn is_joypad_serial(addr: Addr24) -> Option<u16> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4016..=0x4017) {
            Some(offset)
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
            // $2137 SLHV — reading also latches the H/V counters
            // into OPHCT / OPVCT. The actual returned byte is open
            // bus (we hand back the PPU's open-bus latch).
            if off == luna_ppu::register::SLHV {
                let (h, v) = current_hv(*self.mclk_total, self.scanlines_per_frame);
                self.ppu.latch_counters(h, v);
            }
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
        if let Some(off) = Self::wram_port_offset(addr) {
            // $2180 WMDATA — read byte at the 17-bit counter, advance.
            // $2181-$2183 are write-only; reads return open bus.
            if off == 0x80 {
                let a = (*self.wm_addr & 0x1FFFF) as usize;
                let v = self.wram[a];
                *self.wm_addr = (*self.wm_addr + 1) & 0x1FFFF;
                return v;
            }
            return 0xFF;
        }
        if let Some(offset) = Self::is_joypad_serial(addr) {
            // While LATCH ($4016 bit 0) is held high, both controllers
            // are continuously reloaded — reads return the live D-pad
            // state. Once LATCH falls, each read shifts one MSB-first
            // bit out of the 16-bit shift register; subsequent reads
            // past 16 return 1 (pulled-high serial line).
            let shift = if offset == 0x4016 {
                &mut *self.joypad1_shift
            } else {
                &mut *self.joypad2_shift
            };
            let bit = (*shift >> 15) & 1;
            *shift = shift.wrapping_shl(1) | 1;
            return bit as u8;
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
        if let Some(off) = Self::wram_port_offset(addr) {
            match off {
                // $2180 WMDATA — write byte at the 17-bit counter,
                // auto-advance.
                0x80 => {
                    let a = (*self.wm_addr & 0x1FFFF) as usize;
                    self.wram[a] = value;
                    *self.wm_addr = (*self.wm_addr + 1) & 0x1FFFF;
                }
                // $2181 WMADDL — counter bits 0..7.
                0x81 => *self.wm_addr = (*self.wm_addr & !0x000FF) | u32::from(value),
                // $2182 WMADDM — counter bits 8..15.
                0x82 => {
                    *self.wm_addr = (*self.wm_addr & !0x0FF00) | (u32::from(value) << 8);
                }
                // $2183 WMADDH — counter bit 16 (only bit 0 of the
                // value is used; upper bits ignored).
                0x83 => {
                    *self.wm_addr = (*self.wm_addr & !0x10000) | (u32::from(value & 0x01) << 16);
                }
                _ => {}
            }
            return;
        }
        if let Some(offset) = Self::is_joypad_serial(addr) {
            // $4016 bit 0: write 1 → reload both shift registers
            // from the live joypad state and assert LATCH (continuous
            // refresh). Write 0 → de-assert LATCH (shift register
            // freezes; subsequent reads shift it out).
            if offset == 0x4016 {
                let next_strobe = (value & 0x01) != 0;
                if !*self.joypad_strobe && next_strobe {
                    // Rising edge or held-high: reload.
                    *self.joypad1_shift = self.cpu_regs.joypad1;
                    *self.joypad2_shift = self.cpu_regs.joypad2;
                }
                *self.joypad_strobe = next_strobe;
                if next_strobe {
                    // Keep the shift register sync'd with the live
                    // state while strobe is held high.
                    *self.joypad1_shift = self.cpu_regs.joypad1;
                    *self.joypad2_shift = self.cpu_regs.joypad2;
                }
            }
            // $4017 writes drive the expansion-port output pins —
            // ignored by an emulator that doesn't model the expansion.
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
            let bytes = self.dma.run_mdma(&mut view, value);
            // DMA stalls the CPU during the transfer. Cost model
            // (per fullsnes §"SNES DMA Timing"):
            //   * 8 mclk one-shot overhead at the start of the burst
            //   * 8 mclk per channel that runs (already implicit in
            //     the per-byte cost since we lump the bus overhead
            //     into each byte)
            //   * 8 mclk per byte transferred
            // We charge `8 + 8 × bytes` — close enough for game
            // compatibility without modelling the per-channel
            // header explicitly.
            self.io_cycle(u64::from(8 + bytes.saturating_mul(8)));
            return;
        }
        if Self::is_hdmaen(addr) {
            self.dma.hdmaen = value;
            return;
        }
        if let Some(reg_off) = Self::cpu_reg_offset(addr) {
            // WRIO ($4201) bit 7 0→1 transition latches the PPU H/V
            // counters. Check BEFORE handing off to CpuRegs::write so
            // we can see the previous value.
            if reg_off == 0x4201 {
                let prev = self.cpu_regs.wrio;
                if prev & 0x80 == 0 && value & 0x80 != 0 {
                    let (h, v) = current_hv(*self.mclk_total, self.scanlines_per_frame);
                    self.ppu.latch_counters(h, v);
                }
            }
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
        *self.irq || self.mapper.coproc_main_irq_pending()
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
    fn scanline_helpers_pick_per_region_constants() {
        assert_eq!(scanlines_per_frame(luna_cartridge::Region::Ntsc), 262);
        assert_eq!(scanlines_per_frame(luna_cartridge::Region::Pal), 312);
        assert_eq!(vblank_start_line(luna_cartridge::Region::Ntsc), 225);
        assert_eq!(vblank_start_line(luna_cartridge::Region::Pal), 240);
    }

    #[test]
    fn pal_cart_flips_stat78_region_bit_and_propagates_scanlines() {
        // Patch the country byte in our demo ROM to PAL (0x02 = EU).
        let mut rom = demo_lorom().rom;
        rom[0x7FD9] = 0x02;
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.header.region, luna_cartridge::Region::Pal);
        let snes = Snes::from_cartridge(cart);
        assert_eq!(snes.region, luna_cartridge::Region::Pal);
        assert_eq!(snes.region_scanlines(), 312);
        // STAT78 bit 4 should reflect the region.
        assert_eq!(snes.ppu.stat78 & 0x10, 0x10);
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
        let scanlines = snes.region_scanlines();
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
        };
        bus.write(make_addr(0x00, 0x0100), 0xAA);
        // Read back from the mirror in $00:
        assert_eq!(bus.read(make_addr(0x00, 0x0100)), 0xAA);
        // And from $7E (full WRAM):
        assert_eq!(bus.read(make_addr(0x7E, 0x0100)), 0xAA);
    }

    #[test]
    fn wram_port_round_trips_through_2180_and_address_registers() {
        // Set WMADD to $1F00 (a low-RAM mirror), write 0xAB via $2180,
        // re-set the address, read back via $2180.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        let scanlines = snes.region_scanlines();
        let Snes {
            cpu: _,
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
        };
        // WMADD = $00:1F00.
        bus.write(make_addr(0x00, 0x2181), 0x00);
        bus.write(make_addr(0x00, 0x2182), 0x1F);
        bus.write(make_addr(0x00, 0x2183), 0x00);
        // Write 0xAB via WMDATA (auto-increments).
        bus.write(make_addr(0x00, 0x2180), 0xAB);
        // Reset address back to $1F00.
        bus.write(make_addr(0x00, 0x2181), 0x00);
        bus.write(make_addr(0x00, 0x2182), 0x1F);
        // Read it back.
        assert_eq!(bus.read(make_addr(0x00, 0x2180)), 0xAB);
    }

    #[test]
    fn manual_joypad_serial_shifts_msb_first() {
        // Set joypad1 = $8001 (just bit 15 + bit 0 lit), then drive
        // the $4016 strobe (1 → 0) and shift MSB-first via reads.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu_regs.set_joypad(0, 0x8001);
        let scanlines = snes.region_scanlines();
        let Snes {
            cpu: _,
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
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
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
            scanlines_per_frame: scanlines,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
        };
        // Latch then de-strobe.
        bus.write(make_addr(0x00, 0x4016), 0x01);
        bus.write(make_addr(0x00, 0x4016), 0x00);
        // First read = bit 15 of joypad1 = 1.
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
        // Next 14 reads = bits 14..1 = 0.
        for _ in 0..14 {
            assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 0);
        }
        // 16th read = bit 0 = 1.
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
        // Shift exhausted — subsequent reads return 1 (pulled-high).
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
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

    /// Build a minimal SA-1 cartridge whose main-CPU boot code seeds
    /// the SA-1 I-RAM with NOPs, sets the SA-1 reset vector, then
    /// releases the SA-1 by toggling `$2200 CCNT` bit 7 from 1 → 0.
    /// Used by [`sa1_main_cpu_releases_coproc_and_step_runs_it`].
    fn demo_sa1_cart() -> Cartridge {
        let mut rom = vec![0xEA; 32 * 1024];
        // Reset vector $7FFC = $8000.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Header.
        let off = 0x7FC0;
        for (i, b) in b"LUNA SA1 DEMO        ".iter().enumerate() {
            rom[off + i] = *b;
        }
        rom[off + 0x15] = 0x23; // SA-1 mapping (low nibble = 3)
        rom[off + 0x17] = 0x05;
        rom[off + 0x18] = 0x00;
        rom[off + 0x19] = 0x01;
        rom[off + 0x1C] = 0x34;
        rom[off + 0x1D] = 0x12;
        rom[off + 0x1E] = 0xCB;
        rom[off + 0x1F] = 0xED;
        // Boot code at $00:8000. SA-1 ROM at $00:8000 maps to ROM[0]
        // (CXB = 0 after reset → bank 0 region of the SA-1 mapping).
        //
        //   SEI                        78
        //   CLC, XCE                   18 FB    (native mode)
        //   REP #$30                   C2 30    (16-bit A + X/Y)
        //   LDA #$EAEA                 A9 EA EA
        //   STA $003000                8F 00 30 00
        //   STA $003002                8F 02 30 00
        //   SEP #$20                   E2 20
        //   LDA #$30                   A9 30          ; PC hi byte
        //   STA $002204                8F 04 22 00    ; CRV hi
        //   STZ $002203                9C 03 22       ; CRV lo = 0
        //   LDA #$80                   A9 80
        //   STA $002200                8F 00 22 00    ; CCNT bit 7 set (already default)
        //   STZ $002200                9C 00 22       ; release SA-1
        //   STP                        DB
        let mut p = 0;
        let mut emit = |bytes: &[u8]| {
            for b in bytes {
                rom[p] = *b;
                p += 1;
            }
        };
        emit(&[0x78]);
        emit(&[0x18, 0xFB]);
        emit(&[0xC2, 0x30]);
        emit(&[0xA9, 0xEA, 0xEA]);
        emit(&[0x8F, 0x00, 0x30, 0x00]);
        emit(&[0x8F, 0x02, 0x30, 0x00]);
        emit(&[0xE2, 0x20]);
        emit(&[0xA9, 0x30]);
        emit(&[0x8F, 0x04, 0x22, 0x00]);
        emit(&[0x9C, 0x03, 0x22]);
        emit(&[0xA9, 0x80]);
        emit(&[0x8F, 0x00, 0x22, 0x00]);
        emit(&[0x9C, 0x00, 0x22]);
        emit(&[0xDB]);
        let _ = p;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn sa1_main_cpu_releases_coproc_and_step_runs_it() {
        // End-to-end: the main CPU's boot path runs through `Snes::step`,
        // which (a) routes its $2200/$2203/$2204/$003000 writes through
        // the `Sa1Chip` mapper and (b) calls `step_coproc(consumed)`
        // each instruction. After the main CPU STPs, the SA-1's PC
        // should have advanced past 0x3000 — proving the chip wiring.
        let cart = demo_sa1_cart();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // The main-CPU program is < 40 instructions long; 200 steps is
        // safe headroom even at one instruction per call.
        for _ in 0..200 {
            snes.step();
            if snes.cpu.stopped {
                break;
            }
        }
        assert!(snes.cpu.stopped, "main CPU should have hit STP by now");
        // Reach into the SA-1 chip via its mapper trait. We can't
        // downcast safely, so we verify the side-effect: read the I-RAM
        // NOPs that the main CPU wrote (proves the SA-1 mapper claimed
        // the $3000/$3002 writes), and check that step_coproc
        // produced visible advancement by reading $2200 CCNT back as
        // the released value.
        let iram_3000 = snes.mapper.read(luna_bus::make_addr(0x00, 0x3000));
        let iram_3002 = snes.mapper.read(luna_bus::make_addr(0x00, 0x3002));
        assert_eq!(iram_3000, Some(0xEA), "NOP should be in SA-1 I-RAM");
        assert_eq!(iram_3002, Some(0xEA), "NOP should be in SA-1 I-RAM");
        let ccnt = snes.mapper.read(luna_bus::make_addr(0x00, 0x2200));
        assert_eq!(ccnt, Some(0x00), "CCNT should reflect SA-1 release");
    }

    /// Build an SA-1 cart where the main CPU:
    ///   1. Seeds I-RAM with a small SA-1 program: `CLI` then a NOP
    ///      loop at $3000, and an IRQ handler at $3010 that writes
    ///      sentinel `$AA` to I-RAM `$3500` then `STP`s.
    ///   2. Sets CRV = $3000 and CIV = $3010.
    ///   3. Enables CIE.7 (S-CPU → SA-1 IRQ).
    ///   4. Releases the SA-1 via CCNT 1→0 edge.
    ///   5. Burns through a NOP run-up so the SA-1 has time to start.
    ///   6. Triggers the SA-1 IRQ via CCNT.7 0→1 edge.
    ///   7. NOP-pauses then `STP`s.
    fn demo_sa1_irq_cart() -> Cartridge {
        let mut rom = vec![0xEA; 32 * 1024];
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        let off = 0x7FC0;
        for (i, b) in b"LUNA SA1 IRQ DEMO    ".iter().enumerate() {
            rom[off + i] = *b;
        }
        rom[off + 0x15] = 0x23;
        rom[off + 0x17] = 0x05;
        rom[off + 0x18] = 0x00;
        rom[off + 0x19] = 0x01;
        rom[off + 0x1C] = 0x34;
        rom[off + 0x1D] = 0x12;
        rom[off + 0x1E] = 0xCB;
        rom[off + 0x1F] = 0xED;

        let mut p = 0usize;
        let emit = |bytes: &[u8], rom: &mut [u8], p: &mut usize| {
            for b in bytes {
                rom[*p] = *b;
                *p += 1;
            }
        };
        // SEI, native mode, 16-bit A/X/Y.
        emit(&[0x78], &mut rom, &mut p);
        emit(&[0x18, 0xFB], &mut rom, &mut p);
        emit(&[0xC2, 0x30], &mut rom, &mut p);

        // CRV = $3000:  LDA #$3000 ; STA $002203
        emit(&[0xA9, 0x00, 0x30], &mut rom, &mut p);
        emit(&[0x8F, 0x03, 0x22, 0x00], &mut rom, &mut p);
        // CIV = $3010:  LDA #$3010 ; STA $002207
        emit(&[0xA9, 0x10, 0x30], &mut rom, &mut p);
        emit(&[0x8F, 0x07, 0x22, 0x00], &mut rom, &mut p);

        // Back to 8-bit accumulator for byte writes.
        emit(&[0xE2, 0x20], &mut rom, &mut p);

        // CIE = $80 — enable S-CPU → SA-1 IRQ.
        emit(&[0xA9, 0x80], &mut rom, &mut p);
        emit(&[0x8F, 0x0A, 0x22, 0x00], &mut rom, &mut p);

        // Seed SA-1 program at I-RAM $3000:
        //   $3000: CLI                          58
        //   $3001..$300F: NOP loop              EA…
        //   $3010 (IRQ handler):
        //         LDA #$AA                       A9 AA
        //         STA $3500                      8D 00 35
        //         STP                            DB
        // We store byte-by-byte with STA absolute long ($8F).
        let writes: &[(u32, u8)] = &[
            (0x003000, 0x58), // CLI
            (0x003001, 0xEA),
            (0x003002, 0xEA),
            (0x003003, 0xEA),
            (0x003004, 0xEA),
            (0x003005, 0xEA),
            (0x003006, 0xEA),
            (0x003007, 0xEA),
            (0x003008, 0xEA),
            (0x003009, 0xEA),
            (0x00300A, 0xEA),
            (0x00300B, 0xEA),
            (0x00300C, 0xEA),
            (0x00300D, 0xEA),
            (0x00300E, 0xEA),
            (0x00300F, 0xEA),
            (0x003010, 0xA9), // LDA #
            (0x003011, 0xAA),
            (0x003012, 0x8D), // STA abs
            (0x003013, 0x00),
            (0x003014, 0x35),
            (0x003015, 0xDB), // STP
        ];
        for (addr, byte) in writes {
            // LDA #imm
            emit(&[0xA9, *byte], &mut rom, &mut p);
            // STA $aabbcc (absolute long: 8F lo mid hi)
            emit(
                &[
                    0x8F,
                    (*addr & 0xFF) as u8,
                    ((*addr >> 8) & 0xFF) as u8,
                    ((*addr >> 16) & 0xFF) as u8,
                ],
                &mut rom,
                &mut p,
            );
        }

        // Release SA-1: default CCNT is $20 (bit 5 = reset). A write
        // of $00 clears bit 5, producing the 1→0 release edge.
        emit(&[0xA9, 0x00], &mut rom, &mut p);
        emit(&[0x8F, 0x00, 0x22, 0x00], &mut rom, &mut p);

        // Run-up: 40 NOPs so the SA-1 can reach its NOP loop.
        for _ in 0..40 {
            emit(&[0xEA], &mut rom, &mut p);
        }

        // Trigger SA-1 IRQ: CCNT bit 7 0→1 edge.
        emit(&[0xA9, 0x80], &mut rom, &mut p);
        emit(&[0x8F, 0x00, 0x22, 0x00], &mut rom, &mut p);

        // More NOPs to give the SA-1 time to service the IRQ.
        for _ in 0..60 {
            emit(&[0xEA], &mut rom, &mut p);
        }

        // STP — main CPU done.
        emit(&[0xDB], &mut rom, &mut p);
        let _ = p;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn sa1_main_triggers_irq_and_sa1_handler_runs() {
        // End-to-end IRQ message: main CPU writes CCNT.4 → SA-1 takes
        // an IRQ → SA-1 IRQ handler writes a sentinel into I-RAM. We
        // verify the sentinel landed and the SA-1 has STP'd.
        let cart = demo_sa1_irq_cart();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        for _ in 0..1500 {
            snes.step();
            if snes.cpu.stopped {
                break;
            }
        }
        assert!(snes.cpu.stopped, "main CPU should have STP'd");
        // The IRQ handler wrote $AA at I-RAM $3500.
        let sentinel = snes.mapper.read(luna_bus::make_addr(0x00, 0x3500));
        assert_eq!(
            sentinel,
            Some(0xAA),
            "SA-1 IRQ handler should have written sentinel"
        );
    }
}
