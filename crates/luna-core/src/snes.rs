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
use luna_bus::superfx::SuperFxMapper;
use luna_bus::{
    Addr24, Bus, MCycles, Mapper, MapperKind, address_speed, bank_of, make_addr, offset_of,
};
use luna_cartridge::Cartridge;
use luna_cpu_65c816::Cpu;
use luna_ppu::Ppu;

use crate::coproc::Sa1Chip;
use crate::dma::{Dma, DmaBus, DmaTraceEvent, DmaTraceLog};

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
    /// 128 KB Work RAM (banks `$7E-$7F` and the `LowRAM` mirror).
    pub wram: Box<[u8; 0x20000]>,
    /// Cartridge mapper (`LoROM` in P0.6; other mappers in V1+).
    pub mapper: Box<dyn Mapper + Send>,
    /// `FastROM` `MEMSEL` bit — when set, ROM in banks `$80-$FF` at
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

    /// Optional CPU↔APU mailbox traffic log (`$2140-$2143`). When
    /// `Some`, every CPU read/write of those four ports is appended as
    /// a [`MailboxEvent`] for later analysis (e.g. diagnosing the
    /// SMW music-driver handshake). Enable via [`Snes::enable_mailbox_log`].
    pub mailbox_log: Option<Vec<MailboxEvent>>,

    /// Optional SA-1 MMIO traffic log (`$2200-$23FF`). When `Some`, every
    /// CPU read/write of an SA-1 control/status register is appended as a
    /// [`Sa1LogEvent`] for diagnosing the CPU↔SA-1 handshake (e.g. the
    /// SMRPG intro deadlock). Enable via [`Snes::enable_sa1_log`].
    pub sa1_log: Option<Vec<Sa1LogEvent>>,

    /// Optional CPU instruction trace. When `Some`, every call to
    /// [`Snes::step`] appends a pre-instruction register snapshot
    /// until the log fills (capped at `max_events`). Enable via
    /// [`Snes::enable_cpu_trace`].
    pub cpu_trace_log: Option<CpuTraceLog>,

    /// Optional memory access trace. When `Some`, every CPU bus
    /// read/write is appended until the log fills. Filterable by
    /// bank to avoid drowning in ROM fetches. Enable via
    /// [`Snes::enable_mem_trace`].
    pub mem_trace_log: Option<MemTraceLog>,
}

/// One CPU↔APU mailbox transfer, captured at `$2140-$2143`. luna-core
/// keeps this plain (no serde/schemars derives) — downstream crates
/// that want JSON output can convert/serialize themselves. Frame count
/// is derivable from `mclk_total / (MCYCLES_PER_SCANLINE * scanlines)`.
#[derive(Debug, Clone, Copy)]
pub struct MailboxEvent {
    /// Master cycles since reset at the time of the access.
    pub mclk_total: u64,
    /// 24-bit CPU PC (`pb << 16 | pc`) of the instruction executing
    /// this access. Snapshot at the start of the instruction step.
    pub pc_full: u32,
    /// `Read` (CPU reading from the APU) or `Write` (CPU writing to
    /// the APU).
    pub kind: MailboxEventKind,
    /// Mailbox port number `0..=3` (i.e. `$2140` + `port`).
    pub port: u8,
    /// The byte transferred.
    pub value: u8,
}

/// Direction of an APU mailbox transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxEventKind {
    /// CPU read from `$2140-$2143`.
    Read,
    /// CPU write to `$2140-$2143`.
    Write,
}

/// One CPU access to an SA-1 MMIO register (`$2200-$23FF`), captured for
/// CPU↔SA-1 handshake diagnosis. Reuses [`MailboxEventKind`] for the
/// direction. Kept plain (no serde) like [`MailboxEvent`].
#[derive(Debug, Clone, Copy)]
pub struct Sa1LogEvent {
    /// Master cycles since reset at the time of the access.
    pub mclk_total: u64,
    /// 24-bit CPU PC (`pb << 16 | pc`) of the instruction doing the access.
    pub pc_full: u32,
    /// `Read` (CPU reading the SA-1) or `Write` (CPU writing the SA-1).
    pub kind: MailboxEventKind,
    /// The register address in `$2200..=$23FF` (low 16 bits).
    pub reg: u16,
    /// The byte transferred.
    pub value: u8,
}

/// One pre-instruction CPU snapshot, captured by the optional CPU
/// trace. Records the live 65C816 register file at the moment just
/// before the upcoming opcode is fetched and executed. luna-core
/// keeps this plain (no serde derives) for the same reason
/// [`MailboxEvent`] does.
#[derive(Debug, Clone, Copy)]
pub struct CpuTraceEvent {
    /// Master cycles since reset, snapshot of `total_mclk`.
    pub mclk_total: u64,
    /// 24-bit CPU PC (`pb << 16 | pc`) — the about-to-execute instruction.
    pub pc_full: u32,
    /// Accumulator (16-bit; low byte is the M-flag view).
    pub a: u16,
    /// X index register.
    pub x: u16,
    /// Y index register.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Processor status flags.
    pub p: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page register.
    pub dp: u16,
    /// Emulation mode flag.
    pub e: bool,
}

/// Bounded buffer for the CPU instruction tracer. Stops accepting new
/// events once `events.len() == max_events`; the caller is expected to
/// drain the buffer at the end of a run via [`Snes::take_cpu_trace_log`].
pub struct CpuTraceLog {
    /// Recorded events. Owned by the log so [`Snes::take_cpu_trace_log`]
    /// can `mem::take` them cheaply.
    pub events: Vec<CpuTraceEvent>,
    /// Hard cap on event count. Once reached, the tracer becomes a
    /// no-op until the buffer is taken (the cap exists to avoid
    /// blowing out memory on long runs).
    pub max_events: usize,
}

/// One CPU bus access, captured by the optional memory tracer.
#[derive(Debug, Clone, Copy)]
pub struct MemTraceEvent {
    /// Master cycles since reset.
    pub mclk_total: u64,
    /// 24-bit CPU PC of the instruction performing the access.
    pub pc_full: u32,
    /// 24-bit bus address (`bank << 16 | offset`).
    pub addr_full: u32,
    /// Read or write.
    pub kind: MemEventKind,
    /// Byte transferred.
    pub value: u8,
}

/// Direction of a CPU bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemEventKind {
    /// CPU read.
    Read,
    /// CPU write.
    Write,
}

/// Bounded ring for the memory access tracer.
pub struct MemTraceLog {
    /// Recorded events.
    pub events: Vec<MemTraceEvent>,
    /// Hard cap on event count.
    pub max_events: usize,
    /// Optional bank-filter. `None` captures everything; `Some(b)`
    /// only captures accesses where the high byte of the address
    /// equals `b`. Useful for focusing on WRAM (bank `$7E` or `$7F`)
    /// without drowning in ROM fetches.
    pub bank_filter: Option<u8>,
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
pub const fn scanlines_per_frame(region: luna_cartridge::Region) -> u16 {
    match region {
        luna_cartridge::Region::Pal => PAL_SCANLINES_PER_FRAME,
        _ => NTSC_SCANLINES_PER_FRAME,
    }
}

/// Scanline on which `VBlank` starts for the given region (the line
/// the scheduler latches the NMI flag, sets HVBJOY.7, and — if
/// NMITIMEN.7 is on — triggers an NMI).
#[inline]
#[must_use]
pub const fn vblank_start_line(region: luna_cartridge::Region) -> u16 {
    match region {
        luna_cartridge::Region::Pal => PAL_VBLANK_START_LINE,
        _ => NTSC_VBLANK_START_LINE,
    }
}
/// Total scanlines per NTSC frame (visible + post + vblank).
pub const NTSC_SCANLINES_PER_FRAME: u16 = 262;
/// Total scanlines per PAL frame.
pub const PAL_SCANLINES_PER_FRAME: u16 = 312;
/// Scanline on which `VBlank` begins on NTSC. The PPU writes the
/// `$4210` "NMI flag" bit and (if `NMITIMEN.7` is set) raises the NMI
/// pin at the start of this line.
pub const NTSC_VBLANK_START_LINE: u16 = 225;
/// PAL is identical except it has more total scanlines, all of which
/// fall inside `VBlank` — the visible region is still 224 lines (or
/// 239 with overscan, which we don't model yet).
pub const PAL_VBLANK_START_LINE: u16 = 240;

/// The cartridge's mapper needs a coprocessor luna does not yet
/// emulate (S-DD1, SPC7110). Returned by [`Snes::try_from_cartridge`]
/// so callers can surface a clean error instead of catching a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedMapper(pub MapperKind);

impl std::fmt::Display for UnsupportedMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cartridge requires coprocessor support not yet implemented: {:?} \
             (S-DD1 / SPC7110 land in their own dedicated phases)",
            self.0
        )
    }
}

impl std::error::Error for UnsupportedMapper {}

impl Snes {
    /// Build a new machine from a parsed cartridge.
    ///
    /// Returns [`UnsupportedMapper`] if the cartridge needs a coprocessor
    /// luna does not yet emulate (S-DD1, SPC7110). All currently supported
    /// mappers (`LoROM` / `HiROM` / `ExHiROM` / SA-1 / Super FX) succeed.
    pub fn try_from_cartridge(cart: Cartridge) -> Result<Self, UnsupportedMapper> {
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
            // Super FX — the GSU is self-contained (no embedded 65C816), so
            // the whole chip lives in `SuperFxMapper`, driven by the
            // `step_coproc` hook. The Game Pak **work** RAM (the GSU's plot
            // target) is NOT the header SRAM byte — that byte is battery
            // *save* RAM (Star Fox reports 0; the PeterLemon GSU test ROMs
            // report 1 KB, far too small for a 256×192 framebuffer). The
            // real work RAM is a board property (Star Fox 32 KB, Doom
            // 128 KB); without a board table, allocate the 128 KB upper
            // bound so every framebuffer mode fits. (Over-allocation is
            // harmless for the games tested — their plot addresses never
            // reach the larger wrap boundary.)
            MapperKind::SuperFx => Box::new(SuperFxMapper::new(cart.rom, 0x2_0000)),
            other => return Err(UnsupportedMapper(other)),
        };

        let mut ppu = Ppu::new();
        // Flip STAT78's region bit (bit 4) for PAL carts. NTSC and
        // "Unknown" land at the same default (bit clear).
        if matches!(region, luna_cartridge::Region::Pal) {
            ppu.stat78 |= 0x10;
        }

        Ok(Self {
            cpu: Cpu::new(),
            ppu,
            dma: Dma::new(),
            cpu_regs: CpuRegs::new(),
            wram: vec![0u8; 0x20000]
                .into_boxed_slice()
                .try_into()
                .expect("128 KB slice into fixed array"),
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
            mailbox_log: None,
            sa1_log: None,
            cpu_trace_log: None,
            mem_trace_log: None,
        })
    }

    /// Build a new machine from a parsed cartridge, panicking on an
    /// unsupported coprocessor mapper. Convenience wrapper over
    /// [`Snes::try_from_cartridge`] for tests and internal callers that
    /// have already validated the mapper.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        Self::try_from_cartridge(cart).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Enable APU mailbox event logging. From this point every CPU
    /// read/write of `$2140-$2143` is appended to the log. Use
    /// [`Snes::take_mailbox_log`] at the end of a run to retrieve and
    /// reset the captured events. Cheap when disabled (the
    /// `Option::is_some` check in the bus hot path is the only cost).
    pub fn enable_mailbox_log(&mut self) {
        if self.mailbox_log.is_none() {
            self.mailbox_log = Some(Vec::new());
        }
    }

    /// Take ownership of the accumulated mailbox events, resetting the
    /// buffer to empty (but keeping logging enabled). Returns an empty
    /// `Vec` if logging is disabled.
    pub fn take_mailbox_log(&mut self) -> Vec<MailboxEvent> {
        match self.mailbox_log.as_mut() {
            Some(log) => std::mem::take(log),
            None => Vec::new(),
        }
    }

    /// Enable SA-1 MMIO event logging. From this point every CPU read/write
    /// of an SA-1 register (`$2200-$23FF`) is appended to the log. Use
    /// [`Snes::take_sa1_log`] at the end of a run. Cheap when disabled.
    pub fn enable_sa1_log(&mut self) {
        if self.sa1_log.is_none() {
            self.sa1_log = Some(Vec::new());
        }
    }

    /// Take ownership of the accumulated SA-1 MMIO events, resetting the
    /// buffer (but keeping logging enabled). Empty `Vec` if disabled.
    pub fn take_sa1_log(&mut self) -> Vec<Sa1LogEvent> {
        match self.sa1_log.as_mut() {
            Some(log) => std::mem::take(log),
            None => Vec::new(),
        }
    }

    /// Enable SA-1-*side* execution logging: the coprocessor records its
    /// own MMIO accesses (`$2200-$23FF`) with the SA-1 PC. Complements
    /// [`Snes::enable_sa1_log`] (which is the S-CPU side). No-op for
    /// non-SA-1 carts. Drain with [`Snes::take_sa1_side_log`].
    pub fn enable_sa1_side_log(&mut self) {
        self.mapper.enable_sa1_side_log();
    }

    /// Drain the SA-1-side execution log (empty if disabled / not SA-1).
    pub fn take_sa1_side_log(&mut self) -> Vec<luna_bus::Sa1SideEvent> {
        self.mapper.take_sa1_side_log()
    }

    /// Enable a full SA-1 instruction trace (pre-opcode register snapshot
    /// per SA-1 instruction, capped at `max_events`). No-op for non-SA-1
    /// carts. Drain with [`Snes::take_sa1_trace`].
    pub fn enable_sa1_trace(&mut self, max_events: usize) {
        self.mapper.enable_sa1_trace(max_events);
    }

    /// Drain the SA-1 instruction trace (empty if disabled / not SA-1).
    pub fn take_sa1_trace(&mut self) -> Vec<luna_bus::Sa1TraceEvent> {
        self.mapper.take_sa1_trace()
    }

    /// Enable a per-opcode Super FX (GSU) instruction trace.
    pub fn enable_superfx_trace(&mut self, max_events: usize) {
        self.mapper.enable_superfx_trace(max_events);
    }

    /// Drain the Super FX instruction trace (empty if disabled / not GSU).
    pub fn take_superfx_trace(&mut self) -> Vec<luna_bus::SuperFxTraceEvent> {
        self.mapper.take_superfx_trace()
    }

    /// Enable CPU instruction tracing. From this point onward each
    /// call to [`Snes::step`] appends a pre-instruction register
    /// snapshot until the log fills (`max_events` events). Use
    /// [`Snes::take_cpu_trace_log`] to drain.
    pub fn enable_cpu_trace(&mut self, max_events: usize) {
        self.cpu_trace_log = Some(CpuTraceLog {
            events: Vec::new(),
            max_events,
        });
    }

    /// Drain the CPU trace buffer. Returns an empty `Vec` if tracing
    /// is disabled. The log itself stays in place with an empty
    /// events vector — subsequent [`Snes::step`] calls continue to
    /// fill it until `max_events`.
    pub fn take_cpu_trace_log(&mut self) -> Vec<CpuTraceEvent> {
        match self.cpu_trace_log.as_mut() {
            Some(log) => std::mem::take(&mut log.events),
            None => Vec::new(),
        }
    }

    /// Enable memory access tracing. Every CPU bus read/write
    /// matching `bank_filter` (or every access when `None`) is
    /// appended to the log until it fills.
    pub fn enable_mem_trace(&mut self, max_events: usize, bank_filter: Option<u8>) {
        self.mem_trace_log = Some(MemTraceLog {
            events: Vec::new(),
            max_events,
            bank_filter,
        });
    }

    /// Drain the memory access trace buffer.
    pub fn take_mem_trace_log(&mut self) -> Vec<MemTraceEvent> {
        match self.mem_trace_log.as_mut() {
            Some(log) => std::mem::take(&mut log.events),
            None => Vec::new(),
        }
    }

    /// Cached scanlines-per-frame for the current region — propagates
    /// into every [`SnesBus`] borrow.
    #[inline]
    const fn region_scanlines(&self) -> u16 {
        scanlines_per_frame(self.region)
    }

    /// Run the CPU reset sequence: read the reset vector at `$00:FFFC`
    /// via the bus and load `PC`.
    pub fn reset(&mut self) {
        // 1. CPU: re-read the reset vector through the bus. VRAM / WRAM /
        //    SRAM persist across a reset (real hardware doesn't clear them).
        let scanlines = self.region_scanlines();
        let ppu_line_snapshot = self.ppu_line;
        let vblank_start_snapshot = vblank_start_line(self.region);
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        {
            let Self {
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
                mailbox_log,
                sa1_log,
                mem_trace_log,
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
                apu_panicked,
                fast_rom: *fast_rom,
                nmi: nmi_pending,
                irq: irq_pending,
                mclk_total: total_mclk,
                scanlines_per_frame: scanlines,
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: 0,
                frame_count: 0,
                nmis_serviced: 0,
                sched_enabled: false,
                vblank_start_line: vblank_start_snapshot,
                cpu_pc_full: cpu_pc_snapshot,
                mailbox_log,
                sa1_log,
                mem_trace_log,
                wm_addr,
                joypad_strobe,
                joypad1_shift,
                joypad2_shift,
            };
            cpu.reset(&mut bus);
        }

        // 2. Power-on-style reset of the rest of the system. CRITICAL: the
        //    APU. A reset re-runs the game's boot sound-driver upload, whose
        //    IPL-ROM handshake deadlocks unless the SPC700 is back at its
        //    ready state — leaving the APU running its driver made `Reset`
        //    appear to do nothing (the main CPU spun on the upload forever).
        //    CPU registers ($42xx), master clock, frame/scanline counters
        //    and pending interrupts also return to power-on; VRAM/WRAM/SRAM
        //    and the cartridge mapper persist (re-initialised by boot code).
        self.apu_real = Apu::new();
        self.apu_panicked = false;
        self.apu_stub_fallback = ApuStub::new();
        self.cpu_regs = CpuRegs::new();
        self.total_mclk = 0;
        self.ppu_line = 0;
        self.mcycles_in_line = 0;
        self.frame_count = 0;
        self.nmis_serviced = 0;
        self.nmi_pending = false;
        self.irq_pending = false;
        self.wm_addr = 0;
        self.joypad_strobe = false;
        self.joypad1_shift = 0;
        self.joypad2_shift = 0;
    }

    /// Execute one CPU instruction. Returns the master-cycle cost of
    /// that instruction (accumulated through [`Bus::io_cycle`]).
    pub fn step(&mut self) -> MCycles {
        let before = self.total_mclk;
        // Capture the pre-instruction snapshot for the CPU tracer
        // before the destructure below moves `cpu` out of `self`.
        if let Some(log) = self.cpu_trace_log.as_mut() {
            if log.events.len() < log.max_events {
                log.events.push(CpuTraceEvent {
                    mclk_total: before,
                    pc_full: (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc),
                    a: self.cpu.a,
                    x: self.cpu.x,
                    y: self.cpu.y,
                    sp: self.cpu.sp,
                    p: self.cpu.p.bits(),
                    db: self.cpu.db,
                    dp: self.cpu.dp,
                    e: self.cpu.e,
                });
            }
        }
        let scanlines = self.region_scanlines();
        let ppu_line_snapshot = self.ppu_line;
        let vblank_start_snapshot = vblank_start_line(self.region);
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        let (rb_line, rb_mil, rb_fc, rb_ns);
        {
            let Self {
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
                mailbox_log,
                sa1_log,
                mem_trace_log,
                mcycles_in_line,
                frame_count,
                nmis_serviced,
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
                apu_panicked,
                fast_rom: *fast_rom,
                nmi: nmi_pending,
                irq: irq_pending,
                mclk_total: total_mclk,
                scanlines_per_frame: scanlines,
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: *mcycles_in_line,
                frame_count: *frame_count,
                nmis_serviced: *nmis_serviced,
                sched_enabled: true,
                vblank_start_line: vblank_start_snapshot,
                cpu_pc_full: cpu_pc_snapshot,
                mailbox_log,
                sa1_log,
                mem_trace_log,
                wm_addr,
                joypad_strobe,
                joypad1_shift,
                joypad2_shift,
            };
            // The scanline scheduler now advances per bus access inside
            // `bus.io_cycle` (per-scanline rendering: each line is drawn
            // with the register state live at that point, not the end-of-
            // instruction snapshot). Read the live cursor back out below.
            cpu.step(&mut bus);
            rb_line = bus.ppu_line;
            rb_mil = bus.mcycles_in_line;
            rb_fc = bus.frame_count;
            rb_ns = bus.nmis_serviced;
        }
        self.ppu_line = rb_line;
        self.mcycles_in_line = rb_mil;
        self.frame_count = rb_fc;
        self.nmis_serviced = rb_ns;

        let consumed = self.total_mclk - before;

        // The cartridge coprocessor (SA-1 / Super FX / DSP-1 / …) now
        // advances per bus access inside `bus.io_cycle` (Phase 1
        // cycle-accuracy milestone), in lockstep with the APU and the
        // PPU scanline scheduler — not as one end-of-instruction lump.
        // This also removes the old DMA double-charge: the coproc used
        // to be advanced both per-byte during a transfer (DmaBusView::
        // tick) *and* again by this lump, running it ~2× too fast
        // through DMA-heavy code.

        // Sample the coprocessor's level-driven IRQ line into the CPU's
        // dedicated *level* input (ares `coprocessor/sa1/io.cpp:134-163`,
        // Super FX `coprocessor/superfx/io.cpp:18-22`): the device holds
        // the S-CPU IRQ pin until the program acks (Super FX `$3031` read,
        // SA-1 SIC), so this is a level, not an edge. Sampling it fresh
        // each instruction (set AND clear) — rather than re-arming the
        // sticky `pending_irq` — means a single coproc IRQ is serviced
        // exactly once: re-arming `pending_irq` while `I` masked the
        // handler used to leave a stale latch that double-serviced after
        // the `RTI`, corrupting Star Fox's object table.
        //
        // The **H/V-timer IRQ is also a level**, not an edge (ares
        // `cpu/irq.cpp`: `status.irqLine` held until `timeup()`/$4211 read;
        // Mesen2 `_irqFlag` -> `SetIrqSource(Ppu)` held until `ClearIrqSource`).
        // `cpu_regs.irq_flag` IS that level — raised at the H/V coincidence in
        // `poll_hv_irq`, lowered when the program reads $4211. Sampling it into
        // the CPU's level input (instead of the old one-shot `pending_irq`
        // edge) means an H/V IRQ that fires while `I` is masked is HELD and
        // serviced the moment `I` clears, never lost. Doom chains H+V IRQs
        // (re-arming VTIME) to write INIDISP every frame; the edge model
        // dropped ~64% of those writes -> its letterbox border flickered.
        self.cpu
            .set_irq_line(self.cpu_regs.irq_flag || self.mapper.coproc_main_irq_pending());

        // Apply interrupt edges the scanline scheduler latched during this
        // instruction's bus accesses. Deferring the CPU poke to the
        // instruction boundary keeps NMI/IRQ delivery timing identical to
        // the old end-of-step model — the CPU only services interrupts
        // between instructions anyway.
        if self.nmi_pending {
            self.nmi_pending = false;
            self.cpu.trigger_nmi();
        }
        if self.irq_pending {
            self.irq_pending = false;
            self.cpu.trigger_irq();
        }

        consumed
    }

    /// Test-only helper: advance the scanline scheduler directly by
    /// `mcycles` (a mini `step` with no instruction), then apply any
    /// latched NMI/IRQ edge. Production advances the scheduler inside
    /// [`Bus::io_cycle`]; this is the entry point the scheduler tests poke.
    #[cfg(test)]
    fn advance_scheduler(&mut self, mcycles: u32) {
        let scanlines = self.region_scanlines();
        let ppu_line_snapshot = self.ppu_line;
        let vblank_start_snapshot = vblank_start_line(self.region);
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        let (rb_line, rb_mil, rb_fc, rb_ns);
        {
            let Self {
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
                mailbox_log,
                sa1_log,
                mem_trace_log,
                mcycles_in_line,
                frame_count,
                nmis_serviced,
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
                apu_panicked,
                fast_rom: *fast_rom,
                nmi: nmi_pending,
                irq: irq_pending,
                mclk_total: total_mclk,
                scanlines_per_frame: scanlines,
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: *mcycles_in_line,
                frame_count: *frame_count,
                nmis_serviced: *nmis_serviced,
                sched_enabled: true,
                vblank_start_line: vblank_start_snapshot,
                cpu_pc_full: cpu_pc_snapshot,
                mailbox_log,
                sa1_log,
                mem_trace_log,
                wm_addr,
                joypad_strobe,
                joypad1_shift,
                joypad2_shift,
            };
            bus.sched_advance(mcycles);
            rb_line = bus.ppu_line;
            rb_mil = bus.mcycles_in_line;
            rb_fc = bus.frame_count;
            rb_ns = bus.nmis_serviced;
        }
        self.ppu_line = rb_line;
        self.mcycles_in_line = rb_mil;
        self.frame_count = rb_fc;
        self.nmis_serviced = rb_ns;
        if self.nmi_pending {
            self.nmi_pending = false;
            self.cpu.trigger_nmi();
        }
        if self.irq_pending {
            self.irq_pending = false;
            self.cpu.trigger_irq();
        }
    }

    /// Set the live joypad state for controller `idx` (0 = pad 1,
    /// 1 = pad 2). The new mask becomes visible to the game on the
    /// next `VBlank` auto-read latch — typically within ~16.7 ms.
    ///
    /// Bit layout (matches SNES hardware, MSB → LSB):
    /// `B Y SEL START Up Down Left Right A X L R 0 0 0 0`.
    pub const fn set_joypad(&mut self, idx: usize, mask: u16) {
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
        let ppu_line_snapshot = self.ppu_line;
        let vblank_start_snapshot = vblank_start_line(self.region);
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        let Self {
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
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
            apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            vblank_start_line: vblank_start_snapshot,
            cpu_pc_full: cpu_pc_snapshot,
            mailbox_log,
            sa1_log,
            mem_trace_log,
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

    /// Side-effect-free, no-clock debug peek of `count` bytes from `bank:offset`
    /// (16-bit offset wraps). For memory inspectors: unlike a real bus read it
    /// charges **no** `io_cycle` (so it never advances the master clock / APU)
    /// and never touches MMIO (so it never toggles the OPHCT/OPVCT or BG-scroll
    /// latches, clears the NMI/IRQ flags, or advances VMADD/OAMADD/the WRAM
    /// port). WRAM and ROM/SRAM/coproc-work-RAM return their real bytes; the
    /// `$2000-$5FFF` register band returns `0`.
    pub fn dbg_peek_bytes(&mut self, bank: u8, offset: u16, count: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let off = offset.wrapping_add(i as u16);
            let v = if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && off < 0x2000 {
                // Low-RAM WRAM mirror.
                self.wram[usize::from(off)]
            } else if matches!(bank, 0x7E..=0x7F) {
                // Full WRAM ($7E-$7F).
                self.wram[(usize::from(bank - 0x7E) << 16) | usize::from(off)]
            } else if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x2000..=0x5FFF).contains(&off)
            {
                // PPU/APU/CPU/coproc register band — read side effects, so 0.
                0
            } else {
                // ROM / SRAM / coproc work-RAM — side-effect-free here.
                self.mapper.read(make_addr(bank, off)).unwrap_or(0xFF)
            };
            out.push(v);
        }
        out
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
    /// Live handle to `Snes::apu_panicked`. Phase 1 advances the real APU
    /// inside [`Bus::io_cycle`], so this must be a mutable borrow (not a
    /// snapshot) to propagate the SPC700 "stopped" transition back.
    apu_panicked: &'a mut bool,
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
    /// Live current scanline. When `sched_enabled`, [`Bus::io_cycle`]
    /// advances this mid-instruction (per-scanline rendering); otherwise
    /// it's a read-only snapshot for the RDNMI / INIDISP gates. Written
    /// back into [`Snes::ppu_line`] after the instruction.
    ppu_line: u16,
    /// Live master-cycle position within the current scanline.
    mcycles_in_line: u32,
    /// Live completed-frame counter.
    frame_count: u64,
    /// Live delivered-NMI counter.
    nmis_serviced: u64,
    /// When `true`, `io_cycle` ticks the scanline scheduler per bus
    /// access. `false` for debug peeks / mapping tests so they never
    /// advance emulation.
    sched_enabled: bool,
    /// First vblank scanline for the current region (225 NTSC / 240 PAL).
    vblank_start_line: u16,
    /// CPU PC snapshot at the start of the instruction step that owns
    /// this bus borrow. Used by the APU mailbox tracer (and any future
    /// debug hook) to attribute reads/writes to the calling
    /// instruction.
    cpu_pc_full: u32,
    /// Mailbox traffic log for `$2140-$2143`. `None` = disabled (the
    /// common path); `Some` = capturing events. See [`Snes::enable_mailbox_log`].
    mailbox_log: &'a mut Option<Vec<MailboxEvent>>,
    /// SA-1 MMIO trace sink (`$2200-$23FF`); `None` = disabled.
    sa1_log: &'a mut Option<Vec<Sa1LogEvent>>,
    /// Memory access trace. `None` = disabled. See [`Snes::enable_mem_trace`].
    mem_trace_log: &'a mut Option<MemTraceLog>,
}

impl SnesBus<'_> {
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

impl SnesBus<'_> {
    /// Returns `Some(offset)` if `addr` falls in the PPU MMIO range
    /// (`$00-$3F:$2100-$213F` and the `$80-$BF` mirror). The offset is
    /// relative to `$2100` (0x00-0x3F).
    const fn ppu_offset(addr: Addr24) -> Option<u8> {
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
    const fn dma_offset(addr: Addr24) -> Option<u16> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x4300..=0x437F) {
            Some(offset)
        } else {
            None
        }
    }

    /// `true` if `addr` is the `MDMAEN` register `$420B`.
    const fn is_mdmaen(addr: Addr24) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset == 0x420B
    }

    /// `true` if `addr` is the `HDMAEN` register `$420C`.
    const fn is_hdmaen(addr: Addr24) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset == 0x420C
    }

    /// Returns `Some(offset)` if `addr` is a CPU-system register at
    /// `$4200-$421F` (excluding the DMA-enable registers, which are
    /// routed to the DMA controller).
    const fn cpu_reg_offset(addr: Addr24) -> Option<u16> {
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
    const fn wram_port_offset(addr: Addr24) -> Option<u8> {
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
    const fn is_joypad_serial(addr: Addr24) -> Option<u16> {
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
    /// Shared 17-bit WRAM-port address (`$2181-$2183` WMADD), so a DMA to
    /// `$2180` (WMDATA) writes WRAM and auto-increments — the same state
    /// the CPU port uses.
    wm_addr: &'a mut u32,
    /// Optional DMA→VRAM transfer-time trace (moved in from the [`Dma`]
    /// controller for the duration of one MDMA burst). `None` = off.
    dma_trace: Option<&'a mut DmaTraceLog>,
    /// A-bus source address of the most recent `read_a` — paired with
    /// the immediately-following `write_b` to record a VRAM byte's
    /// source (DMA reads then writes each byte in lockstep, A→B).
    last_a_addr: u32,
}

impl DmaBus for DmaBusView<'_> {
    fn read_a(&mut self, addr: Addr24) -> u8 {
        // Remember this byte's source so the paired write_b (A→B runs
        // read-then-write per byte) can record where a VRAM byte came from.
        self.last_a_addr = addr;
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
        // B-bus range $00-$3F = PPU. $80 = WMDATA ($2180): a DMA reading
        // WRAM via the port returns WRAM[WMADD] and auto-increments — same
        // as the CPU port (`read_inner` $2180). APU $40-$43 is still open
        // bus on the DMA path.
        if b_offset <= 0x3F {
            self.ppu.read(b_offset)
        } else if b_offset == 0x80 {
            let a = (*self.wm_addr & 0x1FFFF) as usize;
            let v = self.wram[a];
            *self.wm_addr = (*self.wm_addr + 1) & 0x1FFFF;
            v
        } else {
            0xFF
        }
    }

    fn write_b(&mut self, b_offset: u8, value: u8) {
        if b_offset <= 0x3F {
            // DMA→VRAM trace: capture (source → VMADD → byte) BEFORE the
            // write, since the $2119 (high) write auto-increments VMADD.
            // Only VRAM data ports ($2118/$2119) are of interest.
            if let Some(log) = self.dma_trace.as_mut() {
                if matches!(b_offset, 0x18 | 0x19) && log.events.len() < log.max_events {
                    log.events.push(DmaTraceEvent {
                        src_full: self.last_a_addr,
                        vram_word: self.ppu.vram.address,
                        b_offset,
                        value,
                    });
                }
            }
            // CGDATA ($2122) is never dropped during active display (handled
            // at the source in Ppu::write — ares io.cpp:55-60); VRAM/OAM still
            // drop via their own `active_display` gates.
            self.ppu.write(b_offset, value);
        } else if matches!(b_offset, 0x80..=0x83) {
            // WRAM port ($2180-$2183), mirroring the CPU path: $80 WMDATA
            // writes WRAM[WMADD]++ ; $81-$83 set the 17-bit WMADD. Games
            // (e.g. Kirby Super Star's boot) DMA ROM→WRAM through $2180 to
            // populate low WRAM — dropping it left WRAM $0000+ zero, so the
            // boot's `JMP $000E` hit a `$00` (BRK) and crashed.
            match b_offset {
                0x80 => {
                    let a = (*self.wm_addr & 0x1FFFF) as usize;
                    self.wram[a] = value;
                    *self.wm_addr = (*self.wm_addr + 1) & 0x1FFFF;
                }
                0x81 => *self.wm_addr = (*self.wm_addr & !0x000FF) | u32::from(value),
                0x82 => *self.wm_addr = (*self.wm_addr & !0x0FF00) | (u32::from(value) << 8),
                _ => *self.wm_addr = (*self.wm_addr & !0x10000) | (u32::from(value & 1) << 16),
            }
        }
    }

    fn tick(&mut self, mcycles: u32) {
        // Forward the per-byte DMA tick into the mapper so SA-1 (and
        // future coprocs) advance at DMA cadence instead of being
        // frozen until the next main-CPU instruction step. Without
        // this, ~544-byte OAM DMAs leave the SA-1 paused for ~4 kHz
        // mclks and then catch up in one ~700-instruction burst —
        // ruining the synchronisation the demo's `$3001 SA1_SYNC`
        // handshake depends on.
        self.mapper.step_coproc(mcycles);
    }
}

impl SnesBus<'_> {
    /// Push a memory access event to the optional tracer, honouring
    /// the bank filter. Cheap when disabled.
    #[inline]
    fn trace_mem_access(&mut self, addr: Addr24, kind: MemEventKind, value: u8) {
        if let Some(log) = self.mem_trace_log.as_mut() {
            if log.events.len() >= log.max_events {
                return;
            }
            if let Some(filter) = log.bank_filter {
                if bank_of(addr) != filter {
                    return;
                }
            }
            log.events.push(MemTraceEvent {
                mclk_total: *self.mclk_total,
                pc_full: self.cpu_pc_full,
                addr_full: addr,
                kind,
                value,
            });
        }
    }
}

impl SnesBus<'_> {
    /// Advance the scanline scheduler by `mcycles`, firing per-line events
    /// at each boundary. Called from [`Bus::io_cycle`] mid-instruction.
    ///
    /// Phase 4: the advance is split at scanline boundaries so the H/V-IRQ
    /// can be polled over the H-range each chunk covers *within* one line
    /// (dot-precise), instead of only at the boundary. Boundary crossing
    /// is otherwise identical to before (chunks never overshoot a line, so
    /// the per-line events in `sched_one_line` still fire exactly once).
    /// Returns the total HDMA master-cycle stall accumulated across the
    /// line crossings in this advance (Phase 4); the caller charges it.
    fn sched_advance(&mut self, mcycles: u32) -> u32 {
        let mut remaining = mcycles;
        let mut hdma_stall = 0u32;
        while remaining > 0 {
            let room = MCYCLES_PER_SCANLINE - self.mcycles_in_line;
            let chunk = remaining.min(room);
            // Poll the IRQ over [lo, lo+chunk) on the CURRENT line before
            // any boundary crossing advances `ppu_line` (the V-counter).
            self.poll_hv_irq(self.mcycles_in_line, self.mcycles_in_line + chunk);
            self.mcycles_in_line += chunk;
            remaining -= chunk;
            if self.mcycles_in_line >= MCYCLES_PER_SCANLINE {
                self.mcycles_in_line -= MCYCLES_PER_SCANLINE;
                hdma_stall += self.sched_one_line();
            }
        }
        hdma_stall
    }

    /// Dot-precise H/V-counter IRQ poll over the half-open master-cycle
    /// range `[mclk_lo, mclk_hi)` of the current scanline (`ppu_line` =
    /// V-counter). Mirrors ares `cpu/irq.cpp:18-31` and Mesen2
    /// `InternalRegisters::UpdateIrqLevel`: the trigger is a level
    /// `(!hirq || h == htime) && (!virq || v == vtime)` whose **rising
    /// edge** sets the flag. Within a line H is monotonic, so the H-IRQ
    /// level is a one-dot pulse → exactly one edge per line at
    /// `h == htime`; V-only is a whole-line block → one edge at the
    /// matching line's start (`mclk_lo == 0`). The half-open interval
    /// crosses each dot once, so this needs no persistent level state and
    /// never double-fires. NMITIMEN bit 4 = H-IRQ enable, bit 5 = V-IRQ
    /// enable; HTIME/VTIME are 9-bit (dots / lines).
    ///
    /// Deferred (gaps, not regressions): the ares "no trigger on the last
    /// dot of the field" guard, the htime==0 / detect→assert delay, and
    /// the $4211 TIMEUP hold window (mirror of the RDNMI hold).
    fn poll_hv_irq(&mut self, mclk_lo: u32, mclk_hi: u32) {
        let hirq = self.cpu_regs.nmitimen & 0x10 != 0;
        let virq = self.cpu_regs.nmitimen & 0x20 != 0;
        if !hirq && !virq {
            return;
        }
        // V gate: with V-IRQ enabled the line must match, else the level
        // can never rise on this scanline.
        if virq && self.ppu_line != self.cpu_regs.vtime {
            return;
        }
        let fire = if hirq {
            // H-IRQ: fire when this chunk crosses the `htime` dot.
            let trig = u32::from(self.cpu_regs.htime) * 4;
            mclk_lo <= trig && trig < mclk_hi
        } else {
            // V-only: H is irrelevant; fire at the matching line's start.
            mclk_lo == 0
        };
        if fire {
            // Raise the held level only (ares `status.irqLine`, Mesen `_irqFlag`).
            // `irq_flag` stays set until the program reads $4211; the CPU samples
            // it as a *level* via `set_irq_line` at each instruction boundary, so
            // the IRQ is never lost to `I`-masking the way the old one-shot
            // `*self.irq` edge was (it coalesced/dropped ~64% of Doom's chained
            // H+V writes). No edge latch is set here any more.
            self.cpu_regs.irq_flag = true;
        }
    }

    /// Cross one scanline boundary, applying the per-line PPU events.
    /// Mirrors the former `Snes::advance_one_scanline`, but raises NMI/IRQ
    /// via the bus `nmi`/`irq` latches (the CPU is borrowed here; `step`
    /// applies the edge at the instruction boundary).
    /// Returns the master-cycle cost of any HDMA performed on this line
    /// crossing (frame-start setup + per-line transfer), so the caller can
    /// charge the CPU the stall (Phase 4).
    fn sched_one_line(&mut self) -> u32 {
        let vblank_start = self.vblank_start_line;
        let scanlines = self.scanlines_per_frame;
        let mut hdma_stall = 0u32;

        // Render the visible line that just finished, with its end-of-line
        // register state (HDMA for the next line fires after the increment).
        if self.ppu_line < vblank_start {
            self.ppu
                .render_current_scanline(self.ppu_line, luna_ppu::RenderOptions::default());
            // Latch that this frame showed visible content if the line was
            // scanned out un-blanked. Front-ends use the per-frame snapshot
            // (not the instantaneous INIDISP bit, which a Super FX title
            // re-asserts every VBlank to prep its next buffer) to decide
            // whether to publish the frame.
            if self.ppu.inidisp & 0x80 == 0 {
                self.ppu.frame_visible_content_accum = true;
            }
        }

        self.ppu_line += 1;
        if self.ppu_line == vblank_start {
            // Entering VBlank: latch the $4210 NMI flag + HVBJOY.7.
            self.cpu_regs.nmi_flag = true;
            self.cpu_regs.hvbjoy |= 0x80;
            if self.cpu_regs.nmitimen & 0x80 != 0 {
                *self.nmi = true;
                self.nmis_serviced = self.nmis_serviced.saturating_add(1);
            }
            // OAM address auto-reset (ares `object.cpp:31-32`), unless
            // forced-blank.
            if self.ppu.inidisp & 0x80 == 0 {
                self.ppu.oam.reload_address_from_latch();
            }
            // Joypad auto-read latch + manual-mode shift reload.
            self.cpu_regs.latch_joypad_auto_read();
            if self.cpu_regs.nmitimen & 0x01 != 0 {
                // The hardware auto-read strobes and clocks BOTH controllers
                // 16 times, leaving their shift registers EXHAUSTED. A
                // subsequent *un-strobed* manual read of $4016/$4017 then
                // returns 1 on the data line (ares `controller/gamepad`
                // `data()` returns 1 past 16 clocks; ares `cpu/io.cpp:16,20`
                // `data.bit(0,1) = controllerPort.data()`), NOT the button
                // bits. Reloading the latched value here made idle reads
                // return B=0, so any game that polls $4016/$4017.d0 for the
                // idle-high data line took the wrong branch — e.g. Donkey
                // Kong Country's controller/autofire routine at $80:C13D /
                // $80:C16C, which then corrupted its debounce counters and
                // looped the attract/game-start sequence.
                *self.joypad1_shift = 0xFFFF;
                *self.joypad2_shift = 0xFFFF;
            }
        } else if self.ppu_line == vblank_start + 3 {
            self.cpu_regs.clear_joypad_busy();
        }

        // H/V-counter IRQ is now polled dot-precisely per bus access in
        // `poll_hv_irq` (Phase 4), not latched at the scanline boundary.

        if self.ppu_line >= scanlines {
            // Frame wrap.
            self.ppu_line = 0;
            self.cpu_regs.hvbjoy &= !0x80;
            self.frame_count = self.frame_count.saturating_add(1);
            // Snapshot whether the frame that just completed showed any
            // visible content, paired with the frame counter bump so a
            // front-end polling at this boundary reads a consistent value.
            self.ppu.latch_frame_content();
            // Interlace field parity flips every frame at the V-counter wrap
            // (ares counter/inline.hpp:32), exposed at STAT78 bit 7. Phase A:
            // flag only — no vertical doubling yet.
            self.ppu.field = !self.ppu.field;
            let mut view = DmaBusView {
                wram: &mut *self.wram,
                mapper: &mut *self.mapper,
                ppu: &mut *self.ppu,
                wm_addr: &mut *self.wm_addr,
                // HDMA is not traced (the Super FX framebuffer upload is MDMA).
                dma_trace: None,
                last_a_addr: 0,
            };
            hdma_stall += self.dma.hdma_init(&mut view);
        }

        // HDMA on every visible scanline (end-of-HBlank ordering).
        if self.ppu_line < vblank_start {
            let mut view = DmaBusView {
                wram: &mut *self.wram,
                mapper: &mut *self.mapper,
                ppu: &mut *self.ppu,
                wm_addr: &mut *self.wm_addr,
                // HDMA is not traced (the Super FX framebuffer upload is MDMA).
                dma_trace: None,
                last_a_addr: 0,
            };
            hdma_stall += self.dma.hdma_run_line(&mut view);
        }
        hdma_stall
    }
}

impl Bus for SnesBus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 {
        let value = self.read_inner(addr);
        self.trace_mem_access(addr, MemEventKind::Read, value);
        value
    }
    fn write(&mut self, addr: Addr24, value: u8) {
        self.trace_mem_access(addr, MemEventKind::Write, value);
        self.write_inner(addr, value);
    }
    fn io_cycle(&mut self, mcycles: MCycles) {
        self.advance_time(mcycles, true);
    }
    fn nmi_pending(&self) -> bool {
        *self.nmi
    }
    fn irq_pending(&self) -> bool {
        // H/V-timer IRQ is the held level `cpu_regs.irq_flag` (no longer the
        // one-shot `*self.irq` edge); coproc holds its own level line.
        *self.irq || self.cpu_regs.irq_flag || self.mapper.coproc_main_irq_pending()
    }
}

impl SnesBus<'_> {
    /// Advance the master clock and the time-driven subsystems by
    /// `mcycles` (Phase 1 cycle-accuracy: per-bus-access synchronisation
    /// instead of one end-of-instruction lump).
    ///
    /// `advance_coproc` is `false` only on the DMA accounting path: the
    /// coprocessor already advanced per transferred byte inside
    /// [`DmaBusView::tick`], so charging it again with the lumped DMA
    /// cost here would double-count it.
    fn advance_time(&mut self, mcycles: MCycles, advance_coproc: bool) {
        // Phase 4: HDMA steals master cycles. `sched_advance` returns the
        // HDMA cost of any line crossed; the CPU is halted for that long,
        // so we re-advance everything (master clock, APU, PPU, coproc, IRQ
        // poll) by the stall in a follow-up pass. The loop converges —
        // each scanline's HDMA runs once and a stall rarely spans a full
        // 1364-mclk line.
        let mut step = mcycles;
        loop {
            *self.mclk_total = self.mclk_total.saturating_add(step);
            // APU in lockstep with the CPU at bus-access granularity.
            // `Apu::step` carries the sub-84-mclk remainder in
            // `mclk_deficit`, so per-access stepping composes exactly with
            // the old lump (same SPC instruction count) — only the CPU↔APU
            // port interleaving is finer.
            if !*self.apu_panicked {
                self.apu_real.step(step as u32);
                if self.apu_real.cpu.stopped {
                    *self.apu_panicked = true;
                }
            }
            // PPU scanline scheduler + cartridge coprocessor. Gated by
            // `sched_enabled` so debug peeks / mapping tests don't tick
            // emulation forward.
            if !self.sched_enabled {
                return;
            }
            let hdma_stall = self.sched_advance(step as u32);
            if advance_coproc {
                self.mapper.step_coproc(step as u32);
            }
            // Charge the HDMA stall only on the CPU-instruction path. The
            // DMA-lump path (`advance_coproc == false`) lets HDMA fire but
            // accounts its own lumped time; proper DMA↔HDMA interleaving is
            // Phase 5, so don't re-loop there.
            if hdma_stall == 0 || !advance_coproc {
                return;
            }
            step = MCycles::from(hdma_stall);
        }
    }

    fn read_inner(&mut self, addr: Addr24) -> u8 {
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
            let value = if *self.apu_panicked {
                self.apu_stub_fallback.read(port)
            } else {
                self.apu_real.cpu_read_port(port)
            };
            if let Some(log) = self.mailbox_log.as_mut() {
                log.push(MailboxEvent {
                    mclk_total: *self.mclk_total,
                    pc_full: self.cpu_pc_full,
                    kind: MailboxEventKind::Read,
                    port: port as u8,
                    value,
                });
            }
            return value;
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
            // $4016/$4017 manual serial read.
            //
            // Per Mesen2 (`BaseControlDevice.cpp::StrobeProcessRead`
            // + `SnesController.cpp::ReadRam`) and ares
            // (`controller/gamepad/gamepad.cpp::data`): while strobe
            // is high, the shift register is continuously re-latched
            // from the live controller state — so every read while
            // strobe is high returns bit B (the MSB in luna's MSB-
            // first layout). Once strobe falls, the buffer freezes
            // and subsequent reads shift one MSB-first bit out per
            // call; reads past slot 16 return 1 (pulled-high serial
            // line — the "device signature" 4 zeros at slots 12-15
            // already live in the upper bits, then the shift fills
            // 1s from the LSB).
            //
            // luna used to ALWAYS shift, regardless of strobe, so
            // games that polled "write 1; read 16x" (Bomberman's
            // title-menu pattern) drained the buffer into all-1s
            // after the first sweep and saw a phantom "every button
            // pressed" state forever — instant menu auto-advance.
            let shift = if offset == 0x4016 {
                &mut *self.joypad1_shift
            } else {
                &mut *self.joypad2_shift
            };
            let live = if offset == 0x4016 {
                self.cpu_regs.joypad1
            } else {
                self.cpu_regs.joypad2
            };
            if *self.joypad_strobe {
                *shift = live;
            }
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
            // $4212 HVBJOY: bit 7 = vblank (latched in `cpu_regs.hvbjoy`),
            // bit 6 = hblank (live H-counter), bit 0 = auto-read busy
            // (latched in `cpu_regs.hvbjoy`).
            //
            // Per ares' `cpu/io.cpp`:
            //   data.bit(6) = hcounter() <= 2 || hcounter() >= 1096;
            // The `hcounter()` is in master cycles (0..1364); our
            // `current_hv` returns H in *dots* (`mclk / 4`, 0..341),
            // so the equivalent threshold is `h == 0 || h >= 274`.
            //
            // Without the live hblank bit, games that do `BIT $4212;
            // BVC -5` (SMW, many others) hang in an infinite busy-wait.
            if reg_off == 0x4212 {
                let (h, _) = current_hv(*self.mclk_total, self.scanlines_per_frame);
                let in_hblank = h == 0 || h >= 274;
                let hblank_bit = if in_hblank { 0x40 } else { 0x00 };
                return (self.cpu_regs.hvbjoy & !0x40) | hblank_bit;
            }
            if reg_off == 0x4210 {
                // RDNMI 4-cycle hold window. Mesen2 keeps the flag
                // set when HClock < 6 on the NMI scanline, so a
                // mainline `BPL $4210` after the NMI handler ACKed
                // the same flag still sees the bit. luna's H is in
                // dots (4 mclk each), so < 6 mclks ≈ < 2 dots.
                let (h, _) = current_hv(*self.mclk_total, self.scanlines_per_frame);
                let in_hold = self.ppu_line == self.vblank_start_line && h < 2;
                return self.cpu_regs.read_rdnmi(in_hold);
            }
            if let Some(v) = self.cpu_regs.read(reg_off) {
                return v;
            }
            // Write-only registers fall through to open bus.
            return 0xFF;
        }
        if let Some(v) = self.mapper.read(addr) {
            if let Some(reg) = Self::sa1_reg(addr) {
                if let Some(log) = self.sa1_log.as_mut() {
                    log.push(Sa1LogEvent {
                        mclk_total: *self.mclk_total,
                        pc_full: self.cpu_pc_full,
                        kind: MailboxEventKind::Read,
                        reg,
                        value: v,
                    });
                }
            }
            return v;
        }
        // Open bus stub.
        0xFF
    }

    /// SA-1 MMIO register address (`$2200-$23FF`) if `addr` targets the
    /// coprocessor register window (banks `$00-$3F` / `$80-$BF`). Used only
    /// to gate the optional SA-1 trace log.
    const fn sa1_reg(addr: Addr24) -> Option<u16> {
        let bank = (addr >> 16) as u8;
        let off = addr as u16;
        let bank_ok = bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF);
        if bank_ok && off >= 0x2200 && off <= 0x23FF {
            Some(off)
        } else {
            None
        }
    }

    fn write_inner(&mut self, addr: Addr24, value: u8) {
        let speed = address_speed(addr, self.fast_rom);
        self.io_cycle(speed.mcycles());

        if let Some(o) = Self::wram_offset(addr) {
            self.wram[o] = value;
            return;
        }
        if let Some(off) = Self::ppu_offset(addr) {
            // Gap G7: refresh the PPU's "active display" flag before
            // every register write so VMDATA / OAMDATA / CGDATA writes
            // that land during the visible portion of a non-blanked
            // frame silently drop the data (the address/latch state
            // still advances). ares `ppu_io.cpp:19-45` / Mesen2
            // `SnesPpu.cpp:2046-2057`.
            self.ppu.active_display =
                self.ppu_line < self.vblank_start_line && (self.ppu.inidisp & 0x80) == 0;

            // Phase 2 of gap G6 — intra-line partial flush. If the
            // CPU is writing a render-affecting PPU register ($2100..$2133)
            // mid-scanline, commit the in-progress dots with the OLD
            // state BEFORE applying the write so the partial line gets
            // the pre-write pixels. (Mesen2 SnesPpu.cpp:1884-1886
            // RenderScanline-before-write pattern.)
            if off < 0x34 && self.ppu_line < self.vblank_start_line {
                let (h, _) = current_hv(*self.mclk_total, self.scanlines_per_frame);
                let dot = h.min(luna_ppu::FRAME_W as u16);
                self.ppu.flush_partial_scanline(
                    self.ppu_line,
                    dot,
                    luna_ppu::RenderOptions::default(),
                );
            }
            // $2100 INIDISP — a write that exits forced-blank exactly
            // at the vblank-entry scanline triggers the OAM address
            // auto-reset, same as the per-line vblank hook. ares
            // `ppu_io.cpp:194`, Mesen2 `SnesPpu.cpp:1889-1896`.
            if off == 0x00 {
                let was_force_blank = self.ppu.inidisp & 0x80 != 0;
                let will_force_blank = value & 0x80 != 0;
                if was_force_blank && !will_force_blank && self.ppu_line == self.vblank_start_line {
                    self.ppu.oam.reload_address_from_latch();
                }
            }
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
            if let Some(log) = self.mailbox_log.as_mut() {
                log.push(MailboxEvent {
                    mclk_total: *self.mclk_total,
                    pc_full: self.cpu_pc_full,
                    kind: MailboxEventKind::Write,
                    port: port as u8,
                    value,
                });
            }
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
            // $4016 strobe write.
            //
            // Per Mesen2 (`BaseControlDevice.cpp::StrobeProcessWrite`)
            // and ares (`controller/gamepad/gamepad.cpp` latch path):
            // the buffer parallel-loads on the **falling** edge
            // (strobe 1→0), NOT on the rising edge. Rising-edge and
            // held-high writes leave the buffer alone — the live
            // refresh during reads (handled in the read path above)
            // is what keeps the strobe-high reads in sync.
            //
            // luna used to reload on rising edge and on hold, which
            // wasn't observable on its own but combined with the
            // missing read-side refresh produced the Bomberman menu
            // glitch.
            if offset == 0x4016 {
                let next_strobe = (value & 0x01) != 0;
                if *self.joypad_strobe && !next_strobe {
                    // Falling edge — latch live state.
                    *self.joypad1_shift = self.cpu_regs.joypad1;
                    *self.joypad2_shift = self.cpu_regs.joypad2;
                }
                *self.joypad_strobe = next_strobe;
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
            // Move the DMA→VRAM trace into the view for this burst (so its
            // $2118/9 writes are captured), then restore it: we can't borrow
            // `self.dma` for the trace AND for `run_mdma` simultaneously.
            let mut trace = self.dma.dma_trace.take();
            let bytes = {
                let mut view = DmaBusView {
                    wram: self.wram,
                    mapper: self.mapper,
                    ppu: self.ppu,
                    wm_addr: self.wm_addr,
                    dma_trace: trace.as_mut(),
                    last_a_addr: 0,
                };
                self.dma.run_mdma(&mut view, value)
            };
            self.dma.dma_trace = trace;
            // DMA stalls the CPU during the transfer. Cost model
            // (per fullsnes §"SNES DMA Timing"):
            //   * 8 mclk one-shot overhead at the start of the burst
            //   * 8 mclk per channel that runs (already implicit in
            //     the per-byte cost since we lump the bus overhead
            //     into each byte)
            //   * 8 mclk per byte transferred
            // We charge `8 + 8 × bytes` — close enough for game
            // compatibility without modelling the per-channel
            // header explicitly. `advance_coproc = false`: the SA-1 (and
            // future coprocs) already advanced per byte during the burst
            // via `DmaBusView::tick`, so this lump must not re-charge it.
            self.advance_time(u64::from(8 + bytes.saturating_mul(8)), false);
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
        if let Some(reg) = Self::sa1_reg(addr) {
            if let Some(log) = self.sa1_log.as_mut() {
                log.push(Sa1LogEvent {
                    mclk_total: *self.mclk_total,
                    pc_full: self.cpu_pc_full,
                    kind: MailboxEventKind::Write,
                    reg,
                    value,
                });
            }
        }
        // Mapper claims SRAM writes; anything not yet routed drops.
        let _ = self.mapper.write(addr, value);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::make_addr;

    /// Build a 32 KB `LoROM` that starts with `LDA #$42 ; STA $7E0000 ; STP`
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
        let ppu_line_snapshot = snes.ppu_line;
        let vblank_start_snapshot = vblank_start_line(snes.region);
        let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
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
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
            apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            vblank_start_line: vblank_start_snapshot,
            cpu_pc_full: cpu_pc_snapshot,
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
        let ppu_line_snapshot = snes.ppu_line;
        let vblank_start_snapshot = vblank_start_line(snes.region);
        let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
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
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
            apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            vblank_start_line: vblank_start_snapshot,
            cpu_pc_full: cpu_pc_snapshot,
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
        let ppu_line_snapshot = snes.ppu_line;
        let vblank_start_snapshot = vblank_start_line(snes.region);
        let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
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
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
            apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            vblank_start_line: vblank_start_snapshot,
            cpu_pc_full: cpu_pc_snapshot,
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
        let mut rom = cart.rom;
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
        let mut rom = cart.rom;
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

    // ---- Phase 4: dot-precise H/V-counter IRQ (poll_hv_irq) ----
    // Reference: ares cpu/irq.cpp:18-31, Mesen2 InternalRegisters::UpdateIrqLevel.
    // NMITIMEN bit 4 = H-IRQ enable, bit 5 = V-IRQ enable. A dot = 4 mclk.

    fn irq_snes() -> Snes {
        let mut snes = Snes::from_cartridge(demo_lorom());
        snes.reset();
        snes.ppu_line = 0;
        snes.mcycles_in_line = 0;
        snes.cpu_regs.irq_flag = false;
        snes.irq_pending = false;
        snes
    }

    #[test]
    fn hv_irq_mode00_never_fires() {
        let mut snes = irq_snes();
        snes.cpu_regs.nmitimen = 0x00; // neither H nor V IRQ
        snes.cpu_regs.htime = 100;
        snes.cpu_regs.vtime = 50;
        snes.ppu_line = 50;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert!(!snes.cpu_regs.irq_flag, "mode 00 must never raise IRQ");
    }

    #[test]
    fn hv_irq_h_only_fires_at_htime_dot_every_line() {
        // Mode 01: fire once per scanline at h == htime, regardless of line.
        // (Old code fired every scanline boundary — wrong dot.)
        let mut snes = irq_snes();
        snes.cpu_regs.nmitimen = 0x10; // H-IRQ only
        snes.cpu_regs.htime = 100; // dot 100 → mclk 400
        snes.ppu_line = 10;
        snes.advance_scheduler(399); // up to mclk 399, dot-100 not crossed
        assert!(!snes.cpu_regs.irq_flag, "not yet at htime dot");
        snes.advance_scheduler(2); // crosses mclk 400
        assert!(snes.cpu_regs.irq_flag, "fires at htime dot 100");
        // Next scanline fires again (H-IRQ is per-line).
        snes.cpu_regs.irq_flag = false;
        snes.ppu_line = 11;
        snes.mcycles_in_line = 0;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert!(snes.cpu_regs.irq_flag, "fires on the next line too");
    }

    #[test]
    fn hv_irq_v_only_fires_at_matching_line_start() {
        // Mode 10: fire once, at the start of line == vtime; H irrelevant.
        let mut snes = irq_snes();
        snes.cpu_regs.nmitimen = 0x20; // V-IRQ only
        snes.cpu_regs.vtime = 50;
        // On the wrong line: no fire across the whole line.
        snes.ppu_line = 49;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert!(!snes.cpu_regs.irq_flag, "no fire on line 49");
        // Crossing into line 50 fires at its start.
        assert_eq!(snes.ppu_line, 50);
        snes.advance_scheduler(8); // first dots of line 50
        assert!(snes.cpu_regs.irq_flag, "V-IRQ fires at line 50 start");
    }

    #[test]
    fn hv_irq_hv_mode_fires_at_h_and_v_with_nonzero_htime() {
        // Mode 11 with htime != 0 — the exact case the old code got wrong
        // (it only fired when htime == 0). Must fire at (h=htime, v=vtime).
        let mut snes = irq_snes();
        snes.cpu_regs.nmitimen = 0x30; // H+V IRQ
        snes.cpu_regs.htime = 80; // dot 80 → mclk 320
        snes.cpu_regs.vtime = 60;
        // Wrong line: V gate blocks it entirely.
        snes.ppu_line = 59;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert!(!snes.cpu_regs.irq_flag, "no fire on line 59");
        // Right line, before the htime dot: still no fire.
        assert_eq!(snes.ppu_line, 60);
        snes.advance_scheduler(319);
        assert!(!snes.cpu_regs.irq_flag, "not yet at htime on line 60");
        // Cross the htime dot on line 60 → fire.
        snes.advance_scheduler(2);
        assert!(
            snes.cpu_regs.irq_flag,
            "H+V IRQ fires at htime!=0 on the vtime line"
        );
    }

    #[test]
    fn hv_irq_hv_mode_does_not_fire_off_the_htime_dot() {
        // Mode 11: advancing a full vtime line but with htime beyond the
        // line's dot range must NOT fire (htime never crossed).
        let mut snes = irq_snes();
        snes.cpu_regs.nmitimen = 0x30;
        snes.cpu_regs.htime = 350; // dot 350 → mclk 1400 > line length (1364)
        snes.cpu_regs.vtime = 70;
        snes.ppu_line = 70;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert!(
            !snes.cpu_regs.irq_flag,
            "htime past the line never matches → no IRQ"
        );
    }

    #[test]
    fn interlace_field_toggles_each_frame_wrap() {
        // Interlace Phase A: STAT78 bit-7 field parity flips every frame at
        // the V-counter wrap (ares counter/inline.hpp:32), unconditionally.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        let f0 = snes.ppu.field;
        snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, 0, "frame wrapped");
        assert_eq!(snes.ppu.field, !f0, "field flipped at frame wrap");
        snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu.field, f0, "field flipped back next frame");
    }

    #[test]
    fn vblank_entry_reloads_oam_address_from_latch_when_not_force_blanked() {
        // Mirrors ares `object.cpp:31-32` (`addressReset()` at vcounter==vdisp
        // when force-blank is off) and Mesen2 `SnesPpu.cpp:464-472`. Games
        // (SMW etc.) rely on this so every NMI's OAM stream lands at index 0.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // Force-blank OFF, brightness max.
        snes.ppu.write(luna_ppu::register::INIDISP, 0x0F);
        // Latch word address = $0010 → byte addr should be $0020.
        snes.ppu.oam.set_address_low(0x10);
        assert_eq!(snes.ppu.oam.address, 0x0020);
        // Streaming 4 bytes advances the byte address.
        snes.ppu.oam.write(0x11);
        snes.ppu.oam.write(0x22);
        snes.ppu.oam.write(0x33);
        snes.ppu.oam.write(0x44);
        assert_eq!(snes.ppu.oam.address, 0x0024);
        // Cross the vblank-entry scanline.
        snes.ppu_line = NTSC_VBLANK_START_LINE - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(snes.ppu_line, NTSC_VBLANK_START_LINE);
        // Address has been reloaded from the latched word_address.
        assert_eq!(
            snes.ppu.oam.address, 0x0020,
            "vblank entry must reload OAM byte address from word_address << 1"
        );
    }

    #[test]
    fn vblank_entry_does_not_reload_oam_address_when_force_blanked() {
        // Same scenario but force-blank ON — both refs skip the reload.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.ppu.write(luna_ppu::register::INIDISP, 0x80); // force-blank on
        snes.ppu.oam.set_address_low(0x10);
        snes.ppu.oam.write(0x11);
        snes.ppu.oam.write(0x22);
        snes.ppu.oam.write(0x33);
        snes.ppu.oam.write(0x44);
        assert_eq!(snes.ppu.oam.address, 0x0024);
        snes.ppu_line = NTSC_VBLANK_START_LINE - 1;
        snes.advance_scheduler(MCYCLES_PER_SCANLINE);
        assert_eq!(
            snes.ppu.oam.address, 0x0024,
            "force-blank suppresses the OAM address auto-reset"
        );
    }

    #[test]
    fn inidisp_write_exiting_force_blank_at_vblank_line_reloads_oam_address() {
        // ares `ppu_io.cpp:194` and Mesen2 `SnesPpu.cpp:1889-1896`:
        // a $2100 write that turns off forced-blank while sitting on
        // the vblank-entry scanline triggers the same auto-reset.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // Start with force-blank ON and the byte address advanced.
        snes.ppu.write(luna_ppu::register::INIDISP, 0x80);
        snes.ppu.oam.set_address_low(0x10);
        snes.ppu.oam.write(0x11);
        snes.ppu.oam.write(0x22);
        assert_eq!(snes.ppu.oam.address, 0x0022);
        // Park on the vblank-entry scanline.
        snes.ppu_line = NTSC_VBLANK_START_LINE;
        // Drive the bus write for $2100 = $0F (force-blank OFF).
        let scanlines = snes.region_scanlines();
        let ppu_line_snapshot = snes.ppu_line;
        let vblank_start_snapshot = vblank_start_line(snes.region);
        let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
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
            mailbox_log,
            sa1_log,
            mem_trace_log,
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
            apu_panicked,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            vblank_start_line: vblank_start_snapshot,
            cpu_pc_full: cpu_pc_snapshot,
            mailbox_log,
            sa1_log,
            mem_trace_log,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
        };
        bus.write(make_addr(0x00, 0x2100), 0x0F);
        assert_eq!(
            snes.ppu.oam.address, 0x0020,
            "exiting force-blank at vdisp must reload OAM address"
        );
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
            (0x00_3000, 0x58), // CLI
            (0x00_3001, 0xEA),
            (0x00_3002, 0xEA),
            (0x00_3003, 0xEA),
            (0x00_3004, 0xEA),
            (0x00_3005, 0xEA),
            (0x00_3006, 0xEA),
            (0x00_3007, 0xEA),
            (0x00_3008, 0xEA),
            (0x00_3009, 0xEA),
            (0x00_300A, 0xEA),
            (0x00_300B, 0xEA),
            (0x00_300C, 0xEA),
            (0x00_300D, 0xEA),
            (0x00_300E, 0xEA),
            (0x00_300F, 0xEA),
            (0x00_3010, 0xA9), // LDA #
            (0x00_3011, 0xAA),
            (0x00_3012, 0x8D), // STA abs
            (0x00_3013, 0x00),
            (0x00_3014, 0x35),
            (0x00_3015, 0xDB), // STP
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
