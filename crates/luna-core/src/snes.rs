//! Top-level [`Snes`] machine struct.
//!
//! Wires together the `Cpu65816` main CPU, 128 KB of WRAM, the cartridge
//! mapper, the `Ppu`, the real APU (`apu_real`: SPC700 + S-DSP, with an
//! [`ApuStub`] kept only as a panic-fallback), and the DMA / HDMA and
//! coprocessor subsystems — all driven by the master-clock scheduler.

use crate::apu_stub::ApuStub;
use crate::cpu_regs::CpuRegs;
use luna_apu::Apu;
use luna_bus::hirom::HiRomMapper;
use luna_bus::lorom::LoRomMapper;
use luna_bus::sa1::Sa1Mapper;
use luna_bus::sdd1::Sdd1Mapper;
use luna_bus::superfx::SuperFxMapper;
use luna_bus::{
    Addr24, Bus, InterruptSample, MCycles, Mapper, MapperKind, address_speed, bank_of, make_addr,
    offset_of,
};
use luna_cartridge::Cartridge;
use luna_cpu_65c816::Cpu;
use luna_ppu::Ppu;

use crate::coproc::{Dsp1Mapper, Sa1Chip};
use crate::dma::{Dma, DmaBus, DmaTraceEvent, DmaTraceLog, HDMA_CHANNEL_FLAG};
use crate::mclk::{MclkAccounting, MclkKind};
use crate::power::{PowerOnRng, PowerOnState};

/// `serde` helper for a heap-boxed fixed byte array (`Box<[u8; N]>`),
/// which `serde_bytes` does not cover directly. Used for the 128 KB WRAM.
pub(crate) mod boxed_byte_array {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize `Box<[u8; N]>` as raw bytes (the `&Box` from serde's
    /// `with` call site deref-coerces to this `&[u8; N]`).
    pub(crate) fn serialize<S, const N: usize>(
        data: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&data[..])
    }

    /// Deserialize a byte blob back into `Box<[u8; N]>` (length must match).
    pub(crate) fn deserialize<'de, D, const N: usize>(
        deserializer: D,
    ) -> Result<Box<[u8; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <serde_bytes::ByteBuf>::deserialize(deserializer)?;
        let arr: [u8; N] = bytes
            .into_vec()
            .try_into()
            .map_err(|_| serde::de::Error::custom("byte array length mismatch"))?;
        Ok(Box::new(arr))
    }
}

/// Placeholder mapper used while a freshly-deserialized [`Snes`] has no
/// real cartridge attached. The save-state layer swaps the live mapper
/// back in immediately after `bincode::deserialize`. (`Box<dyn Mapper>`
/// cannot derive `Deserialize`, so the `mapper` field is `serde(skip)` and
/// defaults to this.)
fn placeholder_mapper() -> Box<dyn Mapper + Send> {
    Box::new(luna_bus::NullMapper)
}

/// Top-level SNES machine.
#[derive(serde::Serialize, serde::Deserialize)]
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
    #[serde(with = "boxed_byte_array")]
    pub wram: Box<[u8; 0x20000]>,
    /// Cartridge mapper (`LoROM` in P0.6; other mappers in V1+).
    ///
    /// Not serialized — the trait object cannot derive `Deserialize`, and
    /// the ROM must not be baked into the save-state. The save-state layer
    /// keeps the live mapper and only round-trips its mutable state via
    /// [`Mapper::save_state`] / [`Mapper::load_state`]. On decode this
    /// defaults to a [`luna_bus::NullMapper`] placeholder.
    #[serde(skip, default = "placeholder_mapper")]
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
    /// Who consumed `total_mclk` (CPU active / `WAI` / `STP`, DMA, HDMA,
    /// DRAM refresh), cumulative and per last frame (issue #223). An
    /// exact partition: every master clock charged lands in one bucket.
    #[serde(default)]
    pub mclk_acc: MclkAccounting,

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

    /// CPU memory-data register (open-bus latch): the last byte driven on
    /// the CPU data bus by a read or write. Reads of unmapped / write-only
    /// addresses return this instead of a fixed `0xFF` (ares' default bus
    /// reader `[](n24,n8 data){ return data; }` returns the MDR).
    pub mdr: u8,

    /// H/V-IRQ assert point that slipped past the end of its scanline
    /// (ares samples the counters 10 clocks in the past — `vcounter(10)`
    /// / `hcounter(10)`, irq.cpp:26-28 — so an `htime` near the line end
    /// asserts in the first clocks of the NEXT line). Latched by
    /// [`Self::poll_hv_irq`] and consumed on the following line; never
    /// latched across a field boundary (the ares `vcounter(6) ||
    /// hcounter(6)` "no IRQ on the last dot of a field" guard).
    irq_wrap_trig: Option<u32>,

    /// Optional CPU↔APU mailbox traffic log (`$2140-$2143`). When
    /// `Some`, every CPU read/write of those four ports is appended as
    /// a [`MailboxEvent`] for later analysis (e.g. diagnosing the
    /// SMW music-driver handshake). Enable via [`Snes::enable_mailbox_log`].
    #[serde(skip)]
    pub mailbox_log: Option<Vec<MailboxEvent>>,

    /// Optional SA-1 MMIO traffic log (`$2200-$23FF`). When `Some`, every
    /// CPU read/write of an SA-1 control/status register is appended as a
    /// [`Sa1LogEvent`] for diagnosing the CPU↔SA-1 handshake (e.g. the
    /// SMRPG intro deadlock). Enable via [`Snes::enable_sa1_log`].
    #[serde(skip)]
    pub sa1_log: Option<Vec<Sa1LogEvent>>,

    /// Optional CPU instruction trace. When `Some`, every call to
    /// [`Snes::step`] appends a pre-instruction register snapshot
    /// until the log fills (capped at `max_events`). Enable via
    /// [`Snes::enable_cpu_trace`].
    #[serde(skip)]
    pub cpu_trace_log: Option<CpuTraceLog>,

    /// Optional per-PC profiler (issue #227): instructions + master
    /// cycles paid by each instruction address, including the stalls
    /// (DMA, HDMA, refresh) charged during it. `None` = off.
    #[serde(skip)]
    pub profile: Option<Profile>,

    /// Optional memory access trace. When `Some`, every CPU bus
    /// read/write is appended until the log fills. Filterable by
    /// bank to avoid drowning in ROM fetches. Enable via
    /// [`Snes::enable_mem_trace`].
    #[serde(skip)]
    pub mem_trace_log: Option<MemTraceLog>,

    /// Optional breakpoint/watchpoint registry (issue #66). When `Some`,
    /// `SnesBus::trace_mem_access` matches every CPU bus access against
    /// the registered watchpoints and parks the first hit for the driving
    /// run loop; exec breakpoints are checked by the driver against the
    /// live `PB:PC` before each instruction. Debug infrastructure — never
    /// part of a save-state.
    #[serde(skip)]
    pub breakpoints: Option<Box<crate::breakpoints::BreakpointSet>>,

    /// Optional capture of the SDK debug TTY: every byte the program writes
    /// to `$21FC` (the no$/Mesen "Nocash" console port — opensnes'
    /// `consoleNocashMessage` / `SNES_NOCASH`). When `Some`, each `$21FC`
    /// write byte is appended (bounded), so a headless harness can read the
    /// program's own log/assert output. Enable via [`Snes::enable_nocash_log`].
    #[serde(skip)]
    pub nocash_log: Option<Vec<u8>>,
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
    /// PPU scanline at the access (instruction-start snapshot).
    pub line: u16,
    /// Exact horizontal master-clock (0..1363) at the access — Mesen2's
    /// `GetHClock`. Pairs with `line` to place the event on a frame grid
    /// (the Event Viewer plots events at `(hclock, line)`, column `hclock/2`).
    pub hclock: u16,
    /// `true` if the PPU is in the vertical-blank window
    /// (`line >= vblank_start`).
    pub blank: bool,
    /// `true` if INIDISP (`$2100`) forced-blank (bit 7) was set at the access.
    /// A VRAM write is safe iff `blank || force_blank`.
    pub force_blank: bool,
    /// Who performed the access (issue #226): the CPU, or a DMA / HDMA
    /// channel writing the B-bus (`$21xx`) or the A-bus.
    pub origin: MemOrigin,
}

/// Cost paid by one instruction address (issue #227).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileSample {
    /// Instructions executed at this PC (a parked `WAI` / `STP` tick is
    /// not one).
    pub instructions: u64,
    /// Master cycles the machine spent while this PC was the current
    /// instruction — bus accesses, internal cycles, and the DMA / HDMA /
    /// refresh stalls charged during it.
    pub mclk: u64,
    /// The part of `mclk` spent parked in `WAI` / `STP` at this PC.
    pub idle_mclk: u64,
}

/// Per-PC profile (issue #227): where the master cycles went, by the
/// 24-bit address of the instruction that paid them. Folding by symbol
/// is the API's job.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    /// `pc_full` → cost.
    pub samples: std::collections::HashMap<u32, ProfileSample>,
    /// PPU frame the in-progress per-frame bucket belongs to.
    pub frame: u64,
    /// Master cycles per `pc_full` in the frame in progress (`OpenSNES`
    /// R-B: the per-frame cost of a symbol).
    pub current: std::collections::HashMap<u32, u64>,
    /// Completed frames not yet drained — `(frame, mclk per pc_full)`.
    /// The API folds and empties this on every run call, so it holds at
    /// most the frames one call spanned.
    pub completed: Vec<(u64, std::collections::HashMap<u32, u64>)>,
}

impl Profile {
    /// An empty profile whose first per-frame bucket is `frame`.
    #[must_use]
    pub fn starting_at(frame: u64) -> Self {
        Self {
            frame,
            ..Self::default()
        }
    }

    /// Credit one step at `pc` that cost `mclk` (`idle` = a parked tick),
    /// executed in PPU frame `frame` — a step that crosses the frame edge
    /// is credited to the frame it ends in. A parked tick that cost
    /// nothing (a `STP`-halted CPU) leaves no sample — it is not time,
    /// and not an instruction.
    pub fn record(&mut self, pc: u32, mclk: u64, idle: bool, frame: u64) {
        // One bucket per frame, even for a frame no step ended in — a
        // 64 KB DMA burst spans more than one, and the frame it skipped
        // still counts (as 0) in every row's mean.
        while self.frame < frame {
            let done = std::mem::take(&mut self.current);
            self.completed.push((self.frame, done));
            self.frame += 1;
        }
        if idle && mclk == 0 {
            return;
        }
        let f = self.current.entry(pc).or_default();
        *f = f.saturating_add(mclk);
        let s = self.samples.entry(pc).or_default();
        s.mclk = s.mclk.saturating_add(mclk);
        if idle {
            s.idle_mclk = s.idle_mclk.saturating_add(mclk);
        } else {
            s.instructions = s.instructions.saturating_add(1);
        }
    }

    /// Total master cycles across every sample.
    #[must_use]
    pub fn total_mclk(&self) -> u64 {
        self.samples.values().map(|s| s.mclk).sum()
    }
}

/// Who performed a traced bus access (issue #226).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemOrigin {
    /// An instruction's own bus access.
    Cpu,
    /// A general-purpose DMA burst on this channel (0-7).
    Dma(u8),
    /// An HDMA transfer / table fetch on this channel (0-7).
    Hdma(u8),
}

impl MemOrigin {
    /// From the controller's active-channel tag (`HDMA_CHANNEL_FLAG | ch`
    /// for an HDMA transfer, the bare channel for a DMA burst).
    pub(crate) const fn from_channel_tag(tag: u8) -> Self {
        if tag & HDMA_CHANNEL_FLAG != 0 {
            Self::Hdma(tag & 7)
        } else {
            Self::Dma(tag & 7)
        }
    }

    /// The CSV / MCP spelling: `cpu`, `dma<n>`, `hdma<n>`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Cpu => "cpu".to_string(),
            Self::Dma(ch) => format!("dma{ch}"),
            Self::Hdma(ch) => format!("hdma{ch}"),
        }
    }
}

/// Direction of a CPU bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemEventKind {
    /// CPU read.
    Read,
    /// CPU write.
    Write,
    /// Synthetic delivery-timing marker: the NMI line was raised (V-blank
    /// entry with NMITIMEN.7 set). `value` = NMITIMEN, `addr_full` = `$4210`.
    NmiSignal,
    /// Synthetic delivery-timing marker: the H/V-timer IRQ line was raised.
    /// `value` = NMITIMEN, `addr_full` = `$4211`.
    IrqSignal,
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
    /// Optional offset-range filter `(lo, hi)` (inclusive) on the
    /// low-16-bit address — `None` captures every offset. Catches an
    /// MMIO window (e.g. `$2100-$21FF`) across all banks without the
    /// bank filter's code-fetch flood. Composes with `bank_filter`
    /// (both must match).
    pub offset_filter: Option<(u16, u16)>,
    /// Optional explicit offset list (issue #226): only accesses whose
    /// low 16 bits are in the list are kept (`--trace-writes 2121,2122`).
    /// Composes with the other filters.
    pub only_offsets: Option<Vec<u16>>,
    /// Keep writes only (reads and the NMI/IRQ markers are dropped).
    pub writes_only: bool,
}

/// The filters a memory trace can be opened with (issue #226) — every
/// field composes (all must match). `Default` = record everything.
#[derive(Debug, Clone, Default)]
pub struct MemTraceFilter {
    /// Bank (high byte of the 24-bit address) to keep.
    pub bank: Option<u8>,
    /// Inclusive low-16-bit offset range to keep.
    pub offsets: Option<(u16, u16)>,
    /// Explicit low-16-bit offsets to keep.
    pub only_offsets: Option<Vec<u16>>,
    /// Keep writes only.
    pub writes_only: bool,
}

impl MemTraceLog {
    /// Whether an access to `addr` of `kind` passes the filters and fits
    /// under the cap.
    #[must_use]
    pub fn accepts(&self, addr: Addr24, kind: MemEventKind) -> bool {
        if self.events.len() >= self.max_events {
            return false;
        }
        if self.writes_only && !matches!(kind, MemEventKind::Write) {
            return false;
        }
        if let Some(filter) = self.bank_filter
            && bank_of(addr) != filter
        {
            return false;
        }
        if let Some((lo, hi)) = self.offset_filter
            && !(lo..=hi).contains(&offset_of(addr))
        {
            return false;
        }
        if let Some(list) = &self.only_offsets
            && !list.contains(&offset_of(addr))
        {
            return false;
        }
        true
    }
}

/// Master cycles per PPU scanline on NTSC (1364 mclk = 4 dots × 341).
pub const MCYCLES_PER_SCANLINE: u32 = 1364;

/// Line-relative master-cycle position at which the once-per-scanline DRAM
/// refresh halts the CPU — ares `cpu/timing.cpp:71`:
///
/// ```text
/// status.dramRefreshPosition = 530 + 8 - dmaCounter();   // dmaCounter() = counter.cpu & 7
/// ```
///
/// It is **not** a constant: the refresh aligns to the DMA clock divider, a
/// mod-8 counter on the CPU's master clock sampled at the start of each
/// scanline, so it ranges over 531..=538. luna used to pin it at 538 (the
/// `dmaCounter() == 0` case), which halted the wrong instruction by up to
/// seven clocks — Mesen2 puts it at 534 for this ROM.
#[inline]
const fn dram_refresh_pos(line_start_mclk: u64) -> u32 {
    530 + 8 - (line_start_mclk & 7) as u32
}

/// Master clocks an MDMA burst costs, ares' model.
///
/// ares `cpu/timing.cpp:125-130` wraps the transfer in two alignment steps and
/// `cpu/dma.cpp:16-22,108-120` adds a fixed preamble plus a per-channel one:
///
/// ```text
/// step(counter.dma = 8 - dmaCounter());          // align to the DMA clock
/// counter.dma += 8; step(8);                     // dmaRun() preamble
///   per ENABLED channel: step(8), then 8 mclk per byte
/// step(clockCount - counter.dma % clockCount);   // realign to the CPU clock
/// ```
///
/// `dmaCounter()` is `counter.cpu & 7` — the DMA clock divider — and
/// `clockCount` is the cost of the access that wrote `$420B` (6 mclk). Note
/// `step()` does not touch `counter.dma`, so it is just `(8 - dmaCounter) + 8`
/// when the realignment is computed.
///
/// luna used to charge a flat 8-mclk overhead, so every burst finished early
/// by the alignment steps plus 8 mclk per channel — phase error that never
/// comes back.
#[inline]
const fn mdma_cost(mclk_at_write: u64, channels: u32, bytes: u64, clock_count: u32) -> u64 {
    let align = 8 - (mclk_at_write & 7) as u32; // 1..=8
    let counter_dma = align + 8;
    let realign = clock_count - counter_dma % clock_count;
    (counter_dma + channels * 8 + realign) as u64 + bytes * 8
}

/// Master clocks the CPU's reset sequence burns before its first opcode fetch.
///
/// ares `cpu/cpu.cpp` `CPU::main()` handles the pending reset as `step(132)`
/// followed by the vector-fetch `interrupt()` sequence, and annotates where
/// that lands the CPU: **`//H=186`**. Mesen2 agrees — its first executed
/// instruction is at `masterClock` 186.
///
/// So the CPU does not begin executing at H=0 of scanline 0: it begins 186
/// master clocks in, while the PPU has been free-running the whole time. luna
/// used to start it at H=0, which put its **entire CPU-vs-scanline phase 186
/// clocks early, permanently** — and that phase is what decides where a
/// free-running poll loop lands and which instruction the once-per-line DRAM
/// refresh halts.
const RESET_SEQUENCE_MCLK: u32 = 186;

/// Master cycles the CPU is halted for DRAM refresh each scanline. ares
/// `cpu/timing.cpp:24-28` performs 5 refresh accesses of `step(6)+step(2)` =
/// 8 mclk each → 40 mclk total. luna applies it as one lump stall (the CPU
/// does no work; the APU/PPU/coproc keep running), shifting the CPU↔APU phase
/// exactly as hardware does. Omitting this made luna's CPU run ~40 mclk/line
/// fast vs ares/Mesen, accumulating CPU↔SPC drift (e.g. the SMRPG Akao
/// timer-poll freeze).
const DRAM_REFRESH_CYCLES: u32 = 40;

/// Master clocks of a CPU read that elapse *after* the bus is sampled.
///
/// ares splits a read (`cpu/memory.cpp:8-19`): it steps all but the last four
/// master clocks of the access, samples the bus, then steps the remaining
/// four:
///
/// ```text
/// step(clockCount - 4);  data = bus.read(address, r.mdr);  step(4);
/// ```
///
/// Mesen2 does the same (`SnesMemoryManager::Read`: `_execRead()` = the
/// `speed - 4` step, then `handler->Read`, then `IncMasterClock4`). luna used
/// to charge the whole access up front and sample at its END, so every read
/// observed the bus four clocks late — visible on any register whose value
/// depends on the H-clock at the moment of the read (`$4210` RDNMI, `$4212`
/// HVBJOY's live H-blank bit, `$2137` SLHV and the OPHCT/OPVCT latches).
/// Issue #109. Writes need no split: ares steps the full `clockCount`
/// *before* `bus.write` (`memory.cpp:21-28`), which luna already does.
const READ_SAMPLE_TAIL: MCycles = 4;

/// H-clock at which the S-CPU raises the NMI line on the `VBlank` scanline.
///
/// ares polls the line every four clocks and tests `vcounter(2) >= vdisp`
/// (`cpu/irq.cpp:13`) — the V-counter as it was two clocks ago, its model of
/// the "hardware communication delay between opcode and interrupt units"; the
/// first poll that sees the new scanline is the one at H=2. Mesen2 raises it
/// in the same place (`ProcessIrqCounters`' `hClock == 2` branch). A `$4210`
/// read sampled before this reads the flag clear (and, being inside the hold,
/// cannot clear it either).
const RDNMI_RAISE_HCLOCK: u16 = 2;

/// H-clock from which a `$4210` read may CLEAR the NMI flag.
///
/// The S-CPU holds the line for four master clocks after raising it — ares'
/// `nmiHold` (`cpu/irq.cpp:14`, "hold /NMI for four cycles", checked by
/// `rdnmi()` at `irq.cpp:52-58`); Mesen2 spells out the same window: "the CPU
/// forces the flag to remain set for 4 cycles, only allowing it to be cleared
/// starting on cycle 6" (`InternalRegisters.cpp:234-241`). A read landing in
/// `[RAISE, HOLD)` reads the flag **set** and leaves it set — which is what
/// stops an NMI handler acknowledging `$4210` from starving a mainline
/// `BPL $4210` poll of the same flag (Mesen2 names Terranigma; Chrono Trigger
/// uses the same Square / Quintet idiom).
///
/// This is the faithful ares/Mesen2 pair, live since the CPU↔scanline phase
/// locked (issue #109). The `$4210` masking shipped for #107 — the flag held
/// invisible below H=6 — existed only because luna's phase was wrong, landing
/// its poll loop *inside* this window where hardware's lands clear of it:
/// measured on `WaveHDMA`, the faithful rule now gives exactly one poll pass
/// on 139/139 frames, sampling at H-clocks {16..46}, never inside the hold.
const RDNMI_HOLD_HCLOCK: u16 = 6;

/// Master clocks in scanline `line` — ares' `PPUcounter::hperiod()`.
///
/// Scanlines are 1364 master clocks, **except** line 240 of a non-interlaced
/// odd field, which is four clocks short (1360). That is the NTSC "short
/// scanline", and it is why a real frame alternates 357368 / 357364 master
/// clocks — confirmed against a Mesen2 trace.
///
/// luna used to assume uniform lines and derive (H, V) from the master clock
/// **by division**, which made the short scanline unrepresentable. That is not
/// a rounding detail: four clocks every other frame is enough to make the
/// CPU's phase against the scanline *sweep* where hardware's stays put, and
/// that phase is what a free-running poll loop rides on. (H, V) is now read
/// from the incremental counters (`mcycles_in_line`, `ppu_line`) instead, so
/// the line length can vary.
#[inline]
const fn line_period(line: u16, interlace: bool, odd_field: bool) -> u32 {
    if line == 240 && !interlace && odd_field {
        MCYCLES_PER_SCANLINE - 4
    } else {
        MCYCLES_PER_SCANLINE
    }
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

/// Scanline on which `VBlank` starts — the line the scheduler latches the
/// NMI flag, sets HVBJOY.7 and, if NMITIMEN.7 is on, triggers an NMI. It
/// is the PPU's `vdisp`, and it depends on **overscan only** (SETINI
/// `$2133` bit 2), never on the region: ares `ppu/io.cpp:641`
/// (`state.vdisp = !io.overscan ? 225 : 240`) and Mesen2
/// `SnesPpu.cpp:559` both compute it without looking at the console.
/// PAL differs only in its taller frame (312 lines, all the extra ones
/// inside `VBlank`).
#[inline]
#[must_use]
pub const fn vblank_start_line(overscan: bool) -> u16 {
    if overscan {
        OVERSCAN_VBLANK_START_LINE
    } else {
        VBLANK_START_LINE
    }
}
/// A powered-on APU whose CPU→SPC clock ratio matches `region`'s master
/// clock. The SPC has its own crystal in both regions (ares `smp.cpp`,
/// Mesen2 `Spc.cpp:126`), so a PAL console must not reuse the NTSC ratio.
fn apu_for_region(region: luna_cartridge::Region) -> Apu {
    let mut apu = Apu::new();
    if matches!(region, luna_cartridge::Region::Pal) {
        apu.set_master_clock_hz(luna_apu::PAL_MASTER_CLOCK_HZ);
    }
    apu
}

/// Total scanlines per NTSC frame (visible + post + vblank).
pub const NTSC_SCANLINES_PER_FRAME: u16 = 262;
/// Total scanlines per PAL frame.
pub const PAL_SCANLINES_PER_FRAME: u16 = 312;
/// Scanline on which `VBlank` begins without overscan. The PPU writes the
/// `$4210` "NMI flag" bit and (if `NMITIMEN.7` is set) raises the NMI
/// pin at the start of this line. Same in both regions.
pub const VBLANK_START_LINE: u16 = 225;
/// Scanline on which `VBlank` begins with overscan (SETINI bit 2): the
/// picture is 15 lines taller, so `VBlank` starts that much later and is
/// that much shorter. luna's framebuffer still stores the first 224 rows
/// — the extra picture lines are not displayed yet, but every timing
/// consumer (NMI, HVBJOY, HDMA, the VRAM/OAM access gate) follows the
/// hardware line.
pub const OVERSCAN_VBLANK_START_LINE: u16 = 240;

/// The cartridge needs a coprocessor luna does not yet emulate.
/// Returned by [`Snes::try_from_cartridge`] so callers can surface a
/// clean, named error instead of a game that boots and then hangs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedMapper {
    /// A known mapper kind with no implementation yet (SPC7110).
    Mapper(MapperKind),
    /// A coprocessor identified from the header (Cx4, OBC1, DSP-2/3/4, …).
    Chip(luna_cartridge::UnsupportedChip),
}

impl std::fmt::Display for UnsupportedMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mapper(kind) => write!(f, "cartridge requires the {kind:?} mapper")?,
            Self::Chip(chip) => write!(f, "cartridge requires the {chip} coprocessor")?,
        }
        f.write_str(
            ", which luna does not emulate yet (supported: SA-1, Super FX, DSP-1, \
             S-DD1). Forcing a bare mapper loads it anyway, without the chip.",
        )
    }
}

impl std::error::Error for UnsupportedMapper {}

impl Snes {
    /// Build a new machine from a parsed cartridge.
    ///
    /// Returns [`UnsupportedMapper`] if the cartridge needs a coprocessor
    /// luna does not yet emulate (SPC7110, or a header-identified chip such
    /// as Cx4 / OBC1 / DSP-2). All supported mappers (`LoROM` / `HiROM` /
    /// `ExHiROM` / SA-1 / Super FX / DSP-1 / S-DD1) succeed.
    pub fn try_from_cartridge(cart: Cartridge) -> Result<Self, UnsupportedMapper> {
        Self::try_from_cartridge_with(cart, PowerOnState::Zero)
    }

    /// [`Self::try_from_cartridge`] with an explicit power-on memory state
    /// (issue #224): WRAM, VRAM, CGRAM (15-bit), OAM and APU RAM are filled
    /// per `power_on` before anything runs. A later [`Self::reset`] keeps
    /// them, as hardware / ares / Mesen2 do.
    pub fn try_from_cartridge_with(
        cart: Cartridge,
        power_on: PowerOnState,
    ) -> Result<Self, UnsupportedMapper> {
        let mut snes = Self::build(cart)?;
        snes.apply_power_on(power_on);
        Ok(snes)
    }

    /// Fill every RAM array per `power_on` from one seeded generator in a
    /// fixed order (WRAM, VRAM, CGRAM, OAM, ARAM) — a seed reproduces the
    /// exact machine. CGRAM entries keep 15 bits (ares `ppu.cpp:124`).
    pub fn apply_power_on(&mut self, power_on: PowerOnState) {
        let mut rng = power_on.rng();
        power_on.fill(&mut self.wram[..], &mut rng);
        power_on.fill(self.ppu.vram.raw_mut(), &mut rng);
        let cgram = self.ppu.cgram.raw_mut();
        power_on.fill(cgram, &mut rng);
        for hi in cgram.iter_mut().skip(1).step_by(2) {
            *hi &= 0x7F;
        }
        power_on.fill(self.ppu.oam.raw_mut(), &mut rng);
        power_on.fill(&mut self.apu_real.aram[..], &mut rng);
        if power_on.randomises_registers() {
            Self::randomise_power_on_registers(&mut self.ppu, &mut rng);
        }
    }

    /// Randomise the registers and latches that come up undefined on
    /// hardware — the second half of issue #224, ported from ares
    /// `PPU::power` (`ppu.cpp`), which draws each of these from its own
    /// `random()` on power and leaves them alone on reset.
    ///
    /// Only the fields ares randomises are touched, in its order: the two
    /// chip MDRs, the OAM address trio, the Mode 7 latch and its six
    /// matrix registers, VMAIN and the VRAM address, CGADD and its latch,
    /// M7SEL's three flags, and SETINI's EXTBG / pseudo-hires bits.
    /// INIDISP stays forced-blank at brightness 0 and BGMODE stays 0, as
    /// ares sets them explicitly.
    ///
    /// DMA channel registers are NOT randomised: both references power
    /// them up at `$FF` (ares `cpu.hpp:217-251`, Mesen2's constructor) and
    /// `$420C` at `$00` (anomie-regs), which is what
    /// [`Dma::power_on_defaults`] applies for every power-on state.
    fn randomise_power_on_registers(ppu: &mut Ppu, rng: &mut PowerOnRng) {
        ppu.ppu1_mdr = rng.next_u8();
        ppu.ppu2_mdr = rng.next_u8();

        // $2102/$2103 OAMADD: base address (bit 0 clear), live address and
        // the priority-rotation flag.
        ppu.oam.word_address = rng.next_u16() & 0x01FF;
        ppu.oam.address = rng.next_u16() & 0x03FF;
        ppu.oam.priority_rotation = rng.next_bool();

        // $2115 VMAIN: ares biases the increment mode towards 1 and
        // randomises the remap field; the step stays 1.
        let vmain = (u8::from(rng.next_bool()) << 7) | ((rng.next_u8() & 0x03) << 2);
        ppu.write(luna_ppu::register::VMAIN, vmain);
        // $2116/$2117 VMADD.
        ppu.vram.address = rng.next_u16();

        // $211A M7SEL (screen-over, V-flip, H-flip) and $211B-$2120.
        ppu.m7sel = (rng.next_u8() & 0xC0) | (rng.next_u8() & 0x03);
        ppu.m7a = rng.next_u16() as i16;
        ppu.m7b = rng.next_u16() as i16;
        ppu.m7c = rng.next_u16() as i16;
        ppu.m7d = rng.next_u16() as i16;
        ppu.m7x = rng.next_u16() as i16;
        ppu.m7y = rng.next_u16() as i16;

        // $2121 CGADD + its low/high latch.
        ppu.cgram.address = rng.next_u8();
        ppu.cgram.set_high_pending(rng.next_bool());

        // $2133 SETINI: EXTBG and pseudo-hires are undefined; overscan and
        // interlace come up clear.
        ppu.setini = (u8::from(rng.next_bool()) << 6) | (u8::from(rng.next_bool()) << 3);
    }

    fn build(cart: Cartridge) -> Result<Self, UnsupportedMapper> {
        if let Some(chip) = cart.header.unsupported_chip {
            return Err(UnsupportedMapper::Chip(chip));
        }
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
            // *save* RAM. The work-RAM size comes from the extended-header
            // expansion-RAM byte `$FFBD` (`1024 << n`); GSU carts that don't
            // carry it default to 64 KB (Mesen2 `BaseCartridge.cpp`). YI is
            // 32 KB, Doom / Stunt Race 64 KB, Star Fox 64 KB (default).
            // Over-allocating wraps GSU RAM/RAMBR addressing wrong, so size
            // it faithfully rather than to a 128 KB upper bound.
            MapperKind::SuperFx => {
                let ram = if cart.header.expansion_ram_kb > 0 {
                    cart.header.expansion_ram_kb as usize * 1024
                } else {
                    0x1_0000
                };
                Box::new(SuperFxMapper::new(cart.rom, ram))
            }
            // DSP-1 (Super Mario Kart, Pilotwings) — a base ROM/SRAM map
            // (HiROM or LoROM per the board) plus the NEC uPD7725 chip,
            // fed the cartridge's `dsp1b.rom` firmware. Without firmware
            // the chip stays inert (the game still runs).
            MapperKind::Dsp1 => {
                let hirom = cart.header.dsp_hirom;
                let firmware = cart.coprocessor_firmware().map(<[u8]>::to_vec);
                Box::new(Dsp1Mapper::new(
                    cart.rom,
                    sram_bytes,
                    firmware.as_deref(),
                    hirom,
                ))
            }
            // S-DD1 — graphics decompression chip (Star Ocean, SF Alpha 2);
            // LoROM-based MMC, the whole chip in `Sdd1Mapper`.
            MapperKind::Sdd1 => Box::new(Sdd1Mapper::new(cart.rom, sram_bytes)),
            other => return Err(UnsupportedMapper::Mapper(other)),
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
            dma: Dma::power_on(),
            cpu_regs: CpuRegs::new(),
            wram: vec![0u8; 0x20000]
                .into_boxed_slice()
                .try_into()
                .expect("128 KB slice into fixed array"),
            mapper,
            // MEMSEL powers up SLOW whatever the header's FastROM bit says
            // (ares `cpu.hpp` `romSpeed = 8`); the game opts in via `$420D`.
            fast_rom: false,
            nmi_pending: false,
            irq_pending: false,
            total_mclk: 0,
            mclk_acc: MclkAccounting::default(),
            // Compat: post-reset, the IPL ROM has dropped these into
            // the CPU-facing mailbox to signal "audio CPU ready".
            apu_real: apu_for_region(region),
            apu_panicked: false,
            apu_stub_fallback: ApuStub::new(),
            ppu_line: 0,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            region,
            wm_addr: 0,
            mdr: 0,
            irq_wrap_trig: None,
            joypad_strobe: false,
            joypad1_shift: 0,
            joypad2_shift: 0,
            mailbox_log: None,
            sa1_log: None,
            cpu_trace_log: None,
            profile: None,
            mem_trace_log: None,
            breakpoints: None,
            nocash_log: None,
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

    /// Enable a DSP-1 (`µPD77C25`) microcode + port trace (issue #158).
    pub fn enable_dsp1_trace(&mut self, max_events: usize, ports_only: bool) {
        self.mapper.enable_dsp1_trace(max_events, ports_only);
    }

    /// Drain the DSP-1 trace (empty if disabled / not a DSP-1 cart).
    pub fn take_dsp1_trace(&mut self) -> Vec<luna_bus::Dsp1TraceEvent> {
        self.mapper.take_dsp1_trace()
    }

    /// DSP-1 instructions executed since power-on — the coproc-liveness
    /// counter, available without enabling a trace.
    pub fn dsp1_instructions(&self) -> Option<u64> {
        self.mapper.dsp1_instructions()
    }

    /// Enable a per-opcode SPC700 instruction trace on the real APU.
    pub fn enable_spc_trace(&mut self, max_events: usize) {
        self.apu_real.enable_spc_trace(max_events);
    }

    /// Drain the SPC700 instruction trace (empty if disabled).
    pub fn take_spc_trace(&mut self) -> Vec<luna_apu::Spc700TraceEvent> {
        self.apu_real.take_spc_trace()
    }

    /// Start (or restart, emptied) the per-PC profiler (issue #227).
    pub fn enable_profile(&mut self) {
        self.profile = Some(Profile::starting_at(self.frame_count));
    }

    /// Take the accumulated profile, leaving the profiler enabled and
    /// empty. Empty when it was never enabled.
    pub fn take_profile(&mut self) -> Profile {
        let frame = self.frame_count;
        match self.profile.as_mut() {
            Some(p) => std::mem::replace(p, Profile::starting_at(frame)),
            None => Profile::default(),
        }
    }

    /// Drain the profiler's completed per-frame buckets (`OpenSNES` R-B);
    /// the frame in progress stays. Empty when profiling is off.
    pub fn take_profile_frames(&mut self) -> Vec<(u64, std::collections::HashMap<u32, u64>)> {
        self.profile
            .as_mut()
            .map(|p| std::mem::take(&mut p.completed))
            .unwrap_or_default()
    }

    /// Stop the profiler and drop its samples.
    pub fn disable_profile(&mut self) {
        self.profile = None;
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
    pub fn enable_mem_trace(
        &mut self,
        max_events: usize,
        bank_filter: Option<u8>,
        offset_filter: Option<(u16, u16)>,
    ) {
        self.enable_mem_trace_filtered(
            max_events,
            MemTraceFilter {
                bank: bank_filter,
                offsets: offset_filter,
                ..MemTraceFilter::default()
            },
        );
    }

    /// [`Self::enable_mem_trace`] with the full filter set (issue #226):
    /// also an explicit offset list and writes-only. DMA / HDMA writes
    /// (B-bus and A-bus) are recorded too, tagged by [`MemOrigin`].
    pub fn enable_mem_trace_filtered(&mut self, max_events: usize, filter: MemTraceFilter) {
        self.mem_trace_log = Some(MemTraceLog {
            events: Vec::new(),
            max_events,
            bank_filter: filter.bank,
            offset_filter: filter.offsets,
            only_offsets: filter.only_offsets,
            writes_only: filter.writes_only,
        });
    }

    /// Drain the memory access trace buffer.
    pub fn take_mem_trace_log(&mut self) -> Vec<MemTraceEvent> {
        match self.mem_trace_log.as_mut() {
            Some(log) => std::mem::take(&mut log.events),
            None => Vec::new(),
        }
    }

    /// Stop the memory access tracer and release its buffer.
    pub fn disable_mem_trace(&mut self) {
        self.mem_trace_log = None;
    }

    /// Enable capture of `$21FC` Nocash-TTY writes (the SDK's
    /// `SNES_NOCASH`/`SNES_ASSERT` debug output).
    pub fn enable_nocash_log(&mut self) {
        if self.nocash_log.is_none() {
            self.nocash_log = Some(Vec::new());
        }
    }

    /// Drain the captured `$21FC` Nocash byte stream.
    pub fn take_nocash_log(&mut self) -> Vec<u8> {
        match self.nocash_log.as_mut() {
            Some(buf) => std::mem::take(buf),
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
        // 0. Cartridge coprocessor (Super FX / SA-1 / …): re-power it
        //    first, BEFORE the main CPU re-reads its reset vector. The
        //    reset line on real hardware resets the cart chip too, not
        //    just the main CPU. Doing this first guarantees a mid-run GSU
        //    isn't still owning the ROM bus when the CPU fetches the
        //    vector. ROM and battery SRAM persist; the coproc registers,
        //    internal RAM and its own CPU return to power-on. Leaving the
        //    GSU mid-execution here froze Doom on `Reset` (it never
        //    rebooted). No-op for plain LoROM / HiROM carts.
        self.mapper.reset();

        // 1. CPU: re-read the reset vector through the bus. VRAM / WRAM /
        //    SRAM persist across a reset (real hardware doesn't clear them).
        let ppu_line_snapshot = self.ppu_line;
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        {
            let (cpu, mut bus) = self.cpu_and_bus(BusCursor {
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: 0,
                frame_count: 0,
                nmis_serviced: 0,
                sched_enabled: false,
                cpu_pc_full: cpu_pc_snapshot,
            });
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
        // APU RAM persists across a reset (ares `dsp.cpp:199` randomises
        // it on power only; Mesen2 `Spc::Reset` never touches it) — only the
        // SPC700 / DSP / mailbox state returns to power-on (issue #224).
        let aram = std::mem::replace(
            &mut self.apu_real.aram,
            vec![0u8; 0x10000]
                .into_boxed_slice()
                .try_into()
                .expect("64 KB slice into fixed array"),
        );
        self.apu_real = apu_for_region(self.region);
        self.apu_real.aram = aram;
        self.apu_panicked = false;
        self.apu_stub_fallback = ApuStub::new();
        // Controller ports are host configuration, not console state: the
        // Reset button does not unplug a Mouse / Super Scope (Mesen2
        // `InternalRegisters::Reset` only clears the register file). Carry
        // the devices and the live host inputs across; everything else in
        // the $42xx register file returns to power-on.
        let host_ports = std::mem::take(&mut self.cpu_regs);
        self.cpu_regs = CpuRegs {
            port1: host_ports.port1,
            port2: host_ports.port2,
            mouse: host_ports.mouse,
            super_scope: host_ports.super_scope,
            multitap: host_ports.multitap,
            joypad1: host_ports.joypad1,
            joypad2: host_ports.joypad2,
            joypad_tap: host_ports.joypad_tap,
            ..CpuRegs::new()
        };
        // MEMSEL returns to SLOW: ares `CPU::power` runs `io = {}` on reset
        // too. (Mesen2 `InternalRegisters::Reset` keeps it; both agree it
        // powers up slow. ares is the gold standard.)
        self.fast_rom = false;
        self.total_mclk = 0;
        self.mclk_acc = MclkAccounting::default();
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
        self.dma.pending_mdma = 0;
        // $420B/$420C clear on reset (ares `CPU::power` `channels[id] = {}`;
        // anomie-regs: HDMAEN "$00 on power on or reset"). Leaving HDMAEN set
        // kept HDMA firing from the previous run's stale tables during boot.
        // ares `CPU::power(reset)` rebuilds every channel, so the `$43xx`
        // registers return to their `$FF` power-on values along with
        // `$420B` / `$420C` clearing.
        self.dma = Dma::power_on();

        // 3. Charge the reset sequence — see `RESET_SEQUENCE_MCLK`. The PPU
        //    and APU run through it, so drive it via the scheduler rather than
        //    just biasing the counter.
        self.advance_no_instruction(RESET_SEQUENCE_MCLK, true);
    }

    /// Execute one CPU instruction. Returns the master-cycle cost of
    /// that instruction (accumulated through [`Bus::io_cycle`]).
    pub fn step(&mut self) -> MCycles {
        let before = self.total_mclk;
        // Capture the pre-instruction snapshot for the CPU tracer
        // before the destructure below moves `cpu` out of `self`.
        if let Some(log) = self.cpu_trace_log.as_mut()
            && log.events.len() < log.max_events
        {
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
        let ppu_line_snapshot = self.ppu_line;
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        // Who this step's own clocks belong to (issue #223). A parked `WAI`
        // tick is idle time — unless an interrupt is pending, in which case
        // the core wakes and runs the dispatch this very step (see
        // `Cpu::step`), which is CPU work.
        let idle_kind = if self.cpu.stopped {
            Some(MclkKind::CpuStp)
        } else if self.cpu.waiting && !self.interrupt_line_asserted() {
            Some(MclkKind::CpuWai)
        } else {
            None
        };
        self.mclk_acc.current = idle_kind.unwrap_or(MclkKind::CpuActive);
        if idle_kind.is_some() {
            self.mclk_acc.steps_idle = self.mclk_acc.steps_idle.saturating_add(1);
        }
        let (rb_line, rb_mil, rb_fc, rb_ns);
        {
            let (cpu, mut bus) = self.cpu_and_bus(BusCursor {
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: self.mcycles_in_line,
                frame_count: self.frame_count,
                nmis_serviced: self.nmis_serviced,
                sched_enabled: true,
                cpu_pc_full: cpu_pc_snapshot,
            });
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
        if let Some(prof) = self.profile.as_mut() {
            prof.record(
                cpu_pc_snapshot,
                consumed,
                idle_kind.is_some(),
                self.frame_count,
            );
        }

        // The cartridge coprocessor (SA-1 / Super FX / DSP-1 / …) now
        // advances per bus access inside `bus.io_cycle` (Phase 1
        // cycle-accuracy milestone), in lockstep with the APU and the
        // PPU scanline scheduler — not as one end-of-instruction lump.
        // This also removes the old DMA double-charge: the coproc used
        // to be advanced both per-byte during a transfer (DmaBusView::
        // tick) *and* again by this lump, running it ~2× too fast
        // through DMA-heavy code.

        // Nothing is poked into the CPU here any more. Both interrupt
        // lines reach it through `SnesBus::last_cycle` — the poll one
        // cycle before the running instruction's final bus access, which
        // is where ares samples them. Delivering at this boundary instead
        // let an interrupt that arrived during the final access in up to
        // a whole instruction early.

        consumed
    }

    /// Is any interrupt line asserted right now?
    ///
    /// Used only to classify a parked `WAI` tick as idle or CPU time
    /// (issue #223) — the lines themselves reach the CPU through
    /// [`SnesBus::last_cycle`].
    fn interrupt_line_asserted(&self) -> bool {
        self.nmi_pending
            || self.irq_pending
            || self.cpu_regs.irq_flag
            || self.mapper.coproc_main_irq_pending()
    }

    /// Advance the scanline scheduler by `mcycles` without executing an
    /// instruction, moving the PPU cursor only — the entry point the scheduler
    /// tests poke. Production advances it inside [`Bus::io_cycle`].
    #[cfg(test)]
    fn advance_scheduler(&mut self, mcycles: u32) {
        self.advance_no_instruction(mcycles, false);
    }

    /// Advance time by `mcycles` with no instruction executed. With
    /// `charge_time`, the master clock, APU and coprocessor move too (exactly
    /// as they do inside [`Bus::io_cycle`] during an instruction) — that is
    /// how [`Self::reset`] charges the CPU's reset sequence
    /// (`RESET_SEQUENCE_MCLK`). Without it, only the PPU line cursor moves.
    fn advance_no_instruction(&mut self, mcycles: u32, charge_time: bool) {
        // The reset sequence is the CPU's own time (issue #223).
        self.mclk_acc.current = MclkKind::CpuActive;
        let ppu_line_snapshot = self.ppu_line;
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        let (rb_line, rb_mil, rb_fc, rb_ns);
        {
            let (_, mut bus) = self.cpu_and_bus(BusCursor {
                ppu_line: ppu_line_snapshot,
                mcycles_in_line: self.mcycles_in_line,
                frame_count: self.frame_count,
                nmis_serviced: self.nmis_serviced,
                sched_enabled: true,
                cpu_pc_full: cpu_pc_snapshot,
            });
            if charge_time {
                bus.io_cycle(MCycles::from(mcycles));
            } else {
                bus.sched_advance(mcycles);
            }
            rb_line = bus.ppu_line;
            rb_mil = bus.mcycles_in_line;
            rb_fc = bus.frame_count;
            rb_ns = bus.nmis_serviced;
        }
        self.ppu_line = rb_line;
        self.mcycles_in_line = rb_mil;
        self.frame_count = rb_fc;
        self.nmis_serviced = rb_ns;
        // No CPU poke: an edge latched while no instruction was running
        // stays latched, and the next instruction's `last_cycle` poll
        // picks it up.
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
        let ppu_line_snapshot = self.ppu_line;
        // H/V now come from the incremental counters, so a debug bus must be
        // handed the live line cursor — otherwise every peek would look like
        // it happened at H=0 (i.e. permanently in H-blank).
        let mcycles_in_line_snapshot = self.mcycles_in_line;
        let cpu_pc_snapshot = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
        let (_, mut bus) = self.cpu_and_bus(BusCursor {
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: mcycles_in_line_snapshot,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            cpu_pc_full: cpu_pc_snapshot,
        });
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
    /// `$2000-$5FFF` register band returns `0`, except the DMA channel
    /// registers at `$4300-$437F`, which are side-effect-free to read and
    /// return their real values.
    pub fn dbg_peek_bytes(&mut self, bank: u8, offset: u16, count: usize) -> Vec<u8> {
        self.dbg_peek_bytes_checked(bank, offset, count).0
    }

    /// [`Self::dbg_peek_bytes`] plus the number of bytes in the range that
    /// nothing maps (open bus — reported as `$FF`, the same value the bus
    /// returns). A harness peeking `$40:0000` on a `LoROM` cart gets
    /// `(vec![0xFF; n], n)` instead of a silent run of `$FF` (issue #222).
    pub fn dbg_peek_bytes_checked(
        &mut self,
        bank: u8,
        offset: u16,
        count: usize,
    ) -> (Vec<u8>, usize) {
        let mut out = Vec::with_capacity(count);
        let mut unmapped = 0usize;
        for i in 0..count {
            // Walk the 24-bit address, so a range that runs off the end of a
            // bank continues into the next one ($7E:FFFF → $7F:0000, the
            // contiguous half of WRAM). Wrapping inside the bank silently
            // re-read the bank's own start — a debugger range never means
            // that.
            let (bank, off) = next_debug_addr(bank, offset, i);
            let v = if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && off < 0x2000 {
                // Low-RAM WRAM mirror.
                self.wram[usize::from(off)]
            } else if matches!(bank, 0x7E..=0x7F) {
                // Full WRAM ($7E-$7F).
                self.wram[(usize::from(bank - 0x7E) << 16) | usize::from(off)]
            } else if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x3000..=0x37FF).contains(&off)
            {
                // SA-1 I-RAM ($3000-$37FF) is side-effect-free — read it from
                // the mapper. (Lumping it into the register band below made
                // every I-RAM peek return 0, which silently broke SA-1 I-RAM
                // inspection and cross-emulator I-RAM differentials.)
                self.mapper.read(make_addr(bank, off)).unwrap_or_else(|| {
                    unmapped += 1;
                    0xFF
                })
            } else if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x4300..=0x437F).contains(&off)
            {
                // DMA channel registers: reading them has no side effect on
                // hardware either, so the debug peek returns the register
                // file (`OpenSNES` asked for it to probe the power-on values —
                // `$43xx` comes up `$FF`, see issue #224).
                self.dma.channels[usize::from((off >> 4) & 0x7)].read((off & 0xF) as u8)
            } else if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && (0x2000..=0x5FFF).contains(&off)
            {
                // PPU/APU/CPU/coproc register band — read side effects, so 0.
                0
            } else {
                // ROM / SRAM / coproc work-RAM — side-effect-free here.
                self.mapper.read(make_addr(bank, off)).unwrap_or_else(|| {
                    unmapped += 1;
                    0xFF
                })
            };
            out.push(v);
        }
        (out, unmapped)
    }

    /// Debug poke: write `data` to WRAM (`$7E-$7F` or the `$00-3F`/`$80-BF`
    /// low-RAM mirror) directly, bypassing the bus. For injecting a test
    /// state without a full save-state. Addresses outside WRAM are ignored
    /// (poking ROM/registers is meaningless / unsafe). Returns bytes written.
    pub fn dbg_poke_bytes(&mut self, bank: u8, offset: u16, data: &[u8]) -> usize {
        let mut written = 0;
        for (i, &b) in data.iter().enumerate() {
            // Same 24-bit walk as `dbg_peek_bytes_checked`: two bytes poked at
            // $7E:FFFF used to land at $7E:FFFF and $7E:0000, clobbering the
            // direct page instead of writing $7F:0000.
            let (bank, off) = next_debug_addr(bank, offset, i);
            let idx = if matches!(bank, 0x7E..=0x7F) {
                (usize::from(bank - 0x7E) << 16) | usize::from(off)
            } else if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && off < 0x2000 {
                usize::from(off)
            } else {
                continue;
            };
            self.wram[idx] = b;
            written += 1;
        }
        written
    }
}

/// Event ceiling for the diagnostic ring-less logs (mailbox, SA-1 side).
/// A log the caller forgets to drain must not grow until the process dies:
/// the MCP server is long-running, and a game polling `$2140` produces
/// millions of events per second. Past the cap events are dropped;
/// `take_*` empties the buffer and logging resumes.
pub const DEBUG_LOG_MAX_EVENTS: usize = 1 << 20;

/// `bank:offset` advanced by `i` bytes through the flat 24-bit address
/// space (it wraps at `$FF:FFFF`, as the bus does). Debug peeks and pokes
/// walk memory, not a single bank.
const fn next_debug_addr(bank: u8, offset: u16, i: usize) -> (u8, u16) {
    let base = ((bank as u32) << 16) | offset as u32;
    let addr = base.wrapping_add(i as u32) & 0x00FF_FFFF;
    ((addr >> 16) as u8, addr as u16)
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
    /// Live handle to `Snes::fast_rom` so a `$420D` write persists past the
    /// instruction that made it (a by-value copy was silently discarded).
    fast_rom: &'a mut bool,
    nmi: &'a mut bool,
    irq: &'a mut bool,
    wm_addr: &'a mut u32,
    joypad_strobe: &'a mut bool,
    joypad1_shift: &'a mut u16,
    joypad2_shift: &'a mut u16,
    /// CPU open-bus latch (memory-data register). Open-bus reads return
    /// this; reads and writes update it. See [`Snes::mdr`].
    mdr: &'a mut u8,
    /// Cross-line H/V-IRQ assert point (see `Snes::irq_wrap_trig`).
    irq_wrap_trig: &'a mut Option<u32>,
    /// S-CPU's last bus-access address (ares `cpu.r.mar`). Set at the top of
    /// every `read_inner`/`write_inner`, held across internal cycles, and
    /// passed to `step_coproc` so the SA-1 can model shared-bus `conflict()`
    /// contention. Resets to 0 per instruction (the opcode fetch — always an
    /// instruction's first access — sets it before any coproc step runs;
    /// addr 0 is WRAM, never a contention region, so the reset is inert).
    scpu_mar: u32,
    /// Cost in master clocks of the CPU access currently in flight — ares'
    /// `status.clockCount`, which the DMA / HDMA alignment steps realign
    /// against.
    clock_count: u32,
    mclk_total: &'a mut MCycles,
    /// Master-cycle accounting by consumer (issue #223): the owner sets
    /// `current` to who this bus borrow's own clocks belong to; stalls
    /// are credited to their own buckets inside `advance_time`.
    mclk: &'a mut MclkAccounting,
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
    /// Breakpoint/watchpoint registry (issue #66) — watch hits are parked
    /// on the registry's `pending_hit` for the driving loop.
    breakpoints: &'a mut Option<Box<crate::breakpoints::BreakpointSet>>,
    /// `$21FC` Nocash-TTY capture (the SDK's `SNES_NOCASH`/`SNES_ASSERT`).
    nocash_log: &'a mut Option<Vec<u8>>,
}

impl SnesBus<'_> {
    /// The PPU's live `vdisp`: the first `VBlank` scanline, 225 or — with
    /// overscan armed in SETINI (`$2133` bit 2) — 240. ares recomputes it
    /// on every `$2133` write (`updateVideoMode`, `io.cpp:633,641`) and
    /// every timing consumer reads it through `ppu.vdisp()`, so luna reads
    /// the register directly rather than snapshotting per instruction.
    /// (Mesen2 latches it once per frame at scanline 0, `SnesPpu.cpp:407`;
    /// ares is the gold standard, and anomie-regs describes a mid-frame
    /// toggle as moving the NMI line.)
    #[inline]
    const fn vblank_start_line(&self) -> u16 {
        vblank_start_line(self.ppu.setini & 0x04 != 0)
    }

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

    /// Returns `Some(port_idx)` (0-3) if `addr` is an APU mailbox port:
    /// `$2140-$2143`, mirrored every 4 bytes across `$2140-$217F` (ares
    /// `cpu.cpp:74` maps `2140-217f` with `address.bit(0,1)`; Mesen2
    /// `RegisterHandlerB` `addr & 3`), in banks `$00-$3F` / `$80-$BF`.
    fn apu_port(addr: Addr24) -> Option<usize> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x2140..=0x217F) {
            Some(usize::from(offset & 0x03))
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

/// The per-borrow cursor a [`SnesBus`] starts from; everything else in it
/// is a split borrow of the [`Snes`] (see [`Snes::cpu_and_bus`]).
#[derive(Clone, Copy)]
struct BusCursor {
    ppu_line: u16,
    mcycles_in_line: u32,
    frame_count: u64,
    nmis_serviced: u64,
    sched_enabled: bool,
    cpu_pc_full: u32,
}

impl Snes {
    /// Split the machine into its CPU and a bus over everything else — the
    /// one place a production [`SnesBus`] is assembled (it used to be
    /// written out field by field at every call site, ~65 lines each).
    fn cpu_and_bus(&mut self, cursor: BusCursor) -> (&mut Cpu, SnesBus<'_>) {
        let scanlines = self.region_scanlines();
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
            mdr,
            irq_wrap_trig,
            mclk_acc,
            mailbox_log,
            sa1_log,
            mem_trace_log,
            breakpoints,
            nocash_log,
            ..
        } = self;
        let bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            dma,
            cpu_regs,
            apu_real,
            apu_stub_fallback,
            apu_panicked,
            fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
            scanlines_per_frame: scanlines,
            scpu_mar: 0,
            clock_count: 8,
            ppu_line: cursor.ppu_line,
            mcycles_in_line: cursor.mcycles_in_line,
            frame_count: cursor.frame_count,
            nmis_serviced: cursor.nmis_serviced,
            sched_enabled: cursor.sched_enabled,
            cpu_pc_full: cursor.cpu_pc_full,
            mailbox_log,
            sa1_log,
            mem_trace_log,
            breakpoints,
            nocash_log,
            wm_addr,
            joypad_strobe,
            joypad1_shift,
            joypad2_shift,
            mdr,
            irq_wrap_trig,
            mclk: mclk_acc,
        };
        (cpu, bus)
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
    /// CPU data-bus latch (`Snes::mdr`) — a DMA read of a write-only
    /// PPU register returns it, same as a CPU read (ares passes
    /// `r.mdr` as the `data` param of every bus read).
    mdr: &'a mut u8,
    /// Shared 17-bit WRAM-port address (`$2181-$2183` WMADD), so a DMA to
    /// `$2180` (WMDATA) writes WRAM and auto-increments — the same state
    /// the CPU port uses.
    wm_addr: &'a mut u32,
    /// The APU mailbox (`$2140-$217F`, 4 ports mirrored), reachable by DMA
    /// on the B-bus exactly like the CPU port — ares routes a DMA B-bus
    /// access through the same `bus.read/write(0x2100 | addr)`.
    apu: &'a mut Apu,
    /// Fallback mailbox, consulted only once the SPC700 has panicked (the
    /// CPU path's rule, mirrored).
    apu_stub: &'a mut ApuStub,
    apu_panicked: bool,
    /// Optional DMA→VRAM transfer-time trace (moved in from the [`Dma`]
    /// controller for the duration of one MDMA burst). `None` = off.
    dma_trace: Option<&'a mut DmaTraceLog>,
    /// A-bus source address of the most recent `read_a` — paired with
    /// the immediately-following `write_b` to record a VRAM byte's
    /// source (DMA reads then writes each byte in lockstep, A→B).
    last_a_addr: u32,
    /// Frame / scanline / vblank snapshot at the start of this DMA burst,
    /// stamped onto each `DmaTraceEvent` for per-VBlank bucketing.
    trace_frame: u64,
    trace_line: u16,
    trace_blank: bool,
    /// Exact horizontal master-clock (0..1363) at this DMA burst/line,
    /// stamped onto each `DmaTraceEvent` so the Event Viewer can plot
    /// `(hclock, line)` at full column precision (Mesen2 `GetHClock`).
    trace_hclock: u16,
    /// DMA channel (0-7) currently driving the transfer — set by the
    /// controller via [`DmaBus::set_active_channel`] before each channel's
    /// segment, so captured B-bus writes carry their source channel
    /// (Mesen2 `dma->GetActiveChannel()`).
    dma_channel: u8,
    /// The CPU's memory trace, so DMA / HDMA writes land in the same
    /// stream as instruction writes, tagged by origin (issue #226).
    mem_trace: Option<&'a mut MemTraceLog>,
    /// The watchpoint registry: a `run_until_mem_write` / `bp_add mem`
    /// fires on a DMA / HDMA write too (issue #226).
    breakpoints: Option<&'a mut crate::breakpoints::BreakpointSet>,
    /// PC of the instruction whose bus access ran this burst / line —
    /// stamped as the event's `pc_full`.
    cpu_pc: u32,
    /// Master clock at the burst / line start — stamped as `mclk_total`
    /// (byte-level advance within a burst is not modelled).
    trace_mclk: u64,
}

impl DmaBusView<'_> {
    /// Watchpoints + memory trace for a DMA-side write (B-bus register
    /// `$21xx` as `$00:21xx`, or an A-bus address).
    fn note_write(&mut self, addr: Addr24, value: u8) {
        if let Some(bp) = self.breakpoints.as_deref_mut() {
            bp.check_mem(addr, MemEventKind::Write, value, self.cpu_pc);
        }
        if let Some(log) = self.mem_trace.as_deref_mut()
            && log.accepts(addr, MemEventKind::Write)
        {
            log.events.push(MemTraceEvent {
                mclk_total: self.trace_mclk,
                pc_full: self.cpu_pc,
                addr_full: addr,
                kind: MemEventKind::Write,
                value,
                line: self.trace_line,
                hclock: self.trace_hclock,
                blank: self.trace_blank,
                force_blank: self.ppu.inidisp & 0x80 != 0,
                origin: MemOrigin::from_channel_tag(self.dma_channel),
            });
        }
    }
}

impl DmaBus for DmaBusView<'_> {
    fn read_a(&mut self, addr: Addr24) -> u8 {
        // Remember this byte's source so the paired write_b (A→B runs
        // read-then-write per byte) can record where a VRAM byte came from.
        self.last_a_addr = addr;
        if let Some(o) = SnesBus::wram_offset(addr) {
            return self.wram[o];
        }
        // A-side ROM / SRAM reads via the mapper; anything unmapped reads
        // the open bus — the MDR (ares `bus.read(address, cpu.r.mdr)`).
        self.mapper.read(addr).unwrap_or(*self.mdr)
    }

    fn write_a(&mut self, addr: Addr24, value: u8) {
        self.note_write(addr, value);
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
        // as the CPU port (`read_inner` $2180). $40-$7F = the APU mailbox.
        // Everything else reads the open bus (ares `bus.read(0x2100 |
        // address, cpu.r.mdr)`).
        if b_offset <= 0x3F {
            self.ppu.read(b_offset, *self.mdr)
        } else if (0x40..=0x7F).contains(&b_offset) {
            // APU mailbox, 4 ports mirrored across $2140-$217F — the CPU
            // path's `apu_port` rule and stub fallback.
            let port = usize::from(b_offset & 0x03);
            if self.apu_panicked {
                self.apu_stub.read(port)
            } else {
                self.apu.cpu_read_port(port)
            }
        } else if b_offset == 0x80 {
            let a = (*self.wm_addr & 0x1FFFF) as usize;
            let v = self.wram[a];
            *self.wm_addr = (*self.wm_addr + 1) & 0x1FFFF;
            v
        } else {
            *self.mdr
        }
    }

    fn latch_mdr(&mut self, value: u8) {
        *self.mdr = value;
    }

    fn write_b(&mut self, b_offset: u8, value: u8) {
        self.note_write(0x2100 | u32::from(b_offset), value);
        if b_offset <= 0x3F {
            // Mirror of the CPU path's intra-line partial flush (gap G6,
            // `write_inner` $21xx): a DMA byte hitting a render-affecting
            // register mid-line must commit the in-progress dots with the
            // OLD state BEFORE the write lands. The HiColor class (gap #7)
            // is the canonical victim: an H-IRQ handler DMAs new palette
            // entries into CGRAM during HBlank, and the just-scanned
            // line's pixels used the PRE-DMA palette — without this flush
            // the line-end render saw the post-DMA colours (one DMA batch
            // too new; wrong on every line whose pixels use entries its
            // own HBlank DMA rewrites). `trace_line`/`trace_hclock` are
            // the burst-start stamps — byte-level clock advance within a
            // burst is below the whole-line renderer's floor.
            if b_offset < 0x34 && !self.trace_blank {
                let dot = (self.trace_hclock / 4).min(luna_ppu::FRAME_W as u16);
                self.ppu.flush_partial_scanline(
                    self.trace_line,
                    dot,
                    luna_ppu::RenderOptions::default(),
                );
            }
            // The picture gate for a DMA'd CGRAM byte (ares `writeCGRAM`):
            // a burst that lands mid-picture is redirected like a CPU
            // write; the usual HBlank palette DMA (dot ≥ 274) is not.
            // (`trace_blank` is the VBlank stamp; the display gate also
            // needs INIDISP forced blank — SMW uploads its palette by DMA
            // at picture lines with the screen blanked.)
            if b_offset == luna_ppu::register::CGDATA {
                self.ppu.active_display = self.trace_line
                    < vblank_start_line(self.ppu.setini & 0x04 != 0)
                    && (self.ppu.inidisp & 0x80) == 0;
                self.ppu.beam_dot = self.trace_hclock / 4;
            }
            // DMA B-bus trace: capture (source → VMADD → byte) BEFORE the
            // write, since the $2119 (high) write auto-increments VMADD.
            // Captures EVERY PPU B-bus write ($2100-$213F), not just the
            // VRAM ports — the Event Viewer categorises OAM ($2104),
            // CGRAM ($2122), etc. DMA writes too. `vram_word` is only
            // meaningful for the $2118/$2119 ports; VRAM-only consumers
            // (the CLI `--dma-trace` CSV) filter on `b_offset`.
            if let Some(log) = self.dma_trace.as_mut()
                && log.events.len() < log.max_events
            {
                log.events.push(DmaTraceEvent {
                    src_full: self.last_a_addr,
                    vram_word: self.ppu.vram.address,
                    b_offset,
                    value,
                    channel: self.dma_channel,
                    frame: self.trace_frame,
                    line: self.trace_line,
                    hclock: self.trace_hclock,
                    blank: self.trace_blank,
                    force_blank: self.ppu.inidisp & 0x80 != 0,
                });
            }
            // CGDATA ($2122) is never dropped during active display (handled
            // at the source in Ppu::write — ares io.cpp:55-60); VRAM/OAM still
            // drop via their own `active_display` gates.
            self.ppu.write(b_offset, value);
        } else if (0x40..=0x7F).contains(&b_offset) {
            // APU mailbox — both the real APU and the fallback stub, as the
            // CPU path does.
            let port = usize::from(b_offset & 0x03);
            self.apu.cpu_write_port(port, value);
            self.apu_stub.write(port, value);
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
        //
        // No S-CPU bus access drives this DMA-side tick (the CPU is halted
        // for the transfer), so there is no shared-bus `conflict()` partner —
        // pass `mar = 0` (the ares `dma.cpp` DMA-vs-coproc contention is a
        // separate, finer refinement).
        self.mapper.step_coproc(mcycles, 0);
    }

    fn set_active_channel(&mut self, channel: u8) {
        self.dma_channel = channel;
    }
}

impl SnesBus<'_> {
    /// Push a memory access event to the optional tracer, honouring
    /// the bank filter. Cheap when disabled.
    #[inline]
    fn trace_mem_access(&mut self, addr: Addr24, kind: MemEventKind, value: u8) {
        // Watchpoints (issue #66) FIRST — the trace-log branch below has
        // early returns (cap / filters) that must never mask a hit. One
        // `Option` check when no registry is installed — the same cost
        // class as the trace hook itself.
        if let Some(bp) = self.breakpoints.as_mut() {
            bp.check_mem(addr, kind, value, self.cpu_pc_full);
        }
        let hclock = self.hclock();
        let blank_now = self.ppu_line >= self.vblank_start_line();
        if let Some(log) = self.mem_trace_log.as_mut()
            && log.accepts(addr, kind)
        {
            log.events.push(MemTraceEvent {
                mclk_total: *self.mclk_total,
                pc_full: self.cpu_pc_full,
                addr_full: addr,
                kind,
                value,
                line: self.ppu_line,
                hclock,
                blank: blank_now,
                force_blank: self.ppu.inidisp & 0x80 != 0,
                origin: MemOrigin::Cpu,
            });
        }
    }

    /// Emit a synthetic NMI/IRQ delivery-timing marker into the memory trace
    /// (P0 of the cycle-accuracy roadmap). Unlike a bus access it bypasses the
    /// bank/offset filters — it is a *when did the line get raised* event, the
    /// thing the deferred Phase-4 NMI/IRQ work needs to diff against ares/Mesen.
    #[inline]
    fn trace_irq_signal(&mut self, kind: MemEventKind, value: u8) {
        let pc = self.cpu_pc_full;
        let mclk = *self.mclk_total;
        let line = self.ppu_line;
        let blank = line >= self.vblank_start_line();
        let force_blank = self.ppu.inidisp & 0x80 != 0;
        let hclock = self.hclock();
        if let Some(log) = self.mem_trace_log.as_mut() {
            if log.events.len() >= log.max_events {
                return;
            }
            let addr_full = if matches!(kind, MemEventKind::NmiSignal) {
                0x00_4210
            } else {
                0x00_4211
            };
            log.events.push(MemTraceEvent {
                mclk_total: mclk,
                pc_full: pc,
                addr_full,
                kind,
                value,
                line,
                hclock,
                blank,
                force_blank,
                origin: MemOrigin::Cpu,
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
    /// Returns the `(DRAM refresh, HDMA)` master-cycle stalls accumulated
    /// across this advance (Phase 4; split per consumer for issue #223); the
    /// caller charges their sum.
    fn sched_advance(&mut self, mcycles: u32) -> (u32, u32) {
        let mut remaining = mcycles;
        let mut refresh = 0u32;
        let mut hdma = 0u32;
        while remaining > 0 {
            let period = self.line_period();
            let room = period - self.mcycles_in_line;
            let chunk = remaining.min(room);
            let lo = self.mcycles_in_line;
            let hi = lo + chunk;
            // Absolute master clock this scanline began at: `mclk_total` was
            // already advanced by the whole access, so back out what is still
            // unprocessed plus how far into the line we are.
            let line_start = self
                .mclk_total
                .saturating_sub(u64::from(remaining))
                .saturating_sub(u64::from(lo));
            let refresh_pos = dram_refresh_pos(line_start);
            // Poll the IRQ over [lo, lo+chunk) on the CURRENT line before
            // any boundary crossing advances `ppu_line` (the V-counter).
            self.poll_hv_irq(lo, hi, line_start);
            // DRAM refresh: once per scanline the CPU is halted ~40 mclk
            // (ares `cpu/timing.cpp`). Fire when this chunk crosses the
            // refresh position — like `poll_hv_irq`'s htime, the half-open
            // [lo, hi) covers each line position once, so this triggers
            // exactly once per line with no persistent flag. Charged as a
            // stall the caller re-advances every subsystem by, so the APU/PPU
            // run during the halt (the CPU↔APU phase shift hardware produces).
            // ares fires it as soon as the clock REACHES the position
            // (`cpu/timing.cpp:21`: `hcounter() >= status.dramRefreshPosition`,
            // tested after each step), so the trigger is `pos in (lo, hi]` —
            // not the half-open `[lo, hi)` luna used, which missed the chunk
            // that lands exactly on the position and pushed the 40-clock halt
            // onto the following instruction.
            if lo < refresh_pos && refresh_pos <= hi {
                refresh += DRAM_REFRESH_CYCLES;
            }
            self.mcycles_in_line += chunk;
            remaining -= chunk;
            if self.mcycles_in_line >= period {
                self.mcycles_in_line -= period;
                hdma += self.sched_one_line(line_start + u64::from(period));
            }
        }
        (refresh, hdma)
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
    /// The three ares terms formerly deferred here are now ported: the
    /// 10-clock detect→assert counter-sampling delay (with its next-line
    /// wrap), the "no IRQ on the last dot of a field" guard, and — at the
    /// `$4211` read site — the 4-clock TIMEUP hold window (mirror of the
    /// RDNMI hold; see [`CpuRegs::read_timeup`]).
    fn poll_hv_irq(&mut self, mclk_lo: u32, mclk_hi: u32, line_start: u64) {
        // A trigger that slipped past the previous line's end asserts in
        // this line's first clocks (against the PREVIOUS line's V match —
        // ares' `vcounter(10)` still reads it there).
        if let Some(w) = *self.irq_wrap_trig
            && mclk_lo <= w
            && w < mclk_hi
        {
            *self.irq_wrap_trig = None;
            self.raise_hv_irq(line_start + u64::from(w));
            return;
        }
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
        // ares samples the counters 10 clocks in the past (`vcounter(10)
        // == vtime && hcounter(10) == htime`, irq.cpp:26-28) — the
        // opcode↔interrupt-unit communication delay — so the assert point
        // is 10 clocks AFTER the counters actually match. V-only IRQs
        // assert 10 clocks into the matching line.
        let match_clk = if hirq {
            u32::from(self.cpu_regs.htime) * 4
        } else {
            0
        };
        let period = self.line_period();
        if match_clk >= period {
            // The htime dot does not exist on this line — the counters can
            // never match (ares' `hcounter(10) == htime` is simply never
            // true). Distinct from the wrap below, which is only the
            // 10-clock ASSERT delay spilling past the line end.
            return;
        }
        let trig = match_clk + 10;
        if trig >= period {
            // The assert point lands on the NEXT line. Latch it once (at
            // the chunk that reaches the line end) — unless this is the
            // field's last line: ares' `(vcounter(6) || hcounter(6))`
            // guard (irq.cpp:29) forbids a trigger on the last dot of a
            // field, which is exactly this wrap.
            if mclk_hi >= period
                && u32::from(self.ppu_line) + 1 < u32::from(self.scanlines_per_frame)
            {
                *self.irq_wrap_trig = Some(trig - period);
            }
            return;
        }
        if mclk_lo <= trig && trig < mclk_hi {
            self.raise_hv_irq(line_start + u64::from(trig));
        }
    }

    /// Raise the held H/V-IRQ level (ares `status.irqLine`, Mesen
    /// `_irqFlag`) and stamp the raise clock for the `$4211` hold window.
    /// `irq_flag` stays set until the program reads `$4211`; the CPU
    /// samples it as a *level* in `SnesBus::last_cycle`, so the IRQ is
    /// never lost to `I`-masking the way the old one-shot edge was (it
    /// coalesced/dropped ~64% of Doom's chained H+V writes).
    fn raise_hv_irq(&mut self, raise_mclk: u64) {
        self.cpu_regs.irq_flag = true;
        self.cpu_regs.irq_raise_mclk = raise_mclk;
        self.trace_irq_signal(MemEventKind::IrqSignal, self.cpu_regs.nmitimen);
    }

    /// Cross one scanline boundary, applying the per-line PPU events.
    /// Mirrors the former `Snes::advance_one_scanline`, but raises NMI/IRQ
    /// via the bus `nmi`/`irq` latches (the CPU is borrowed here; `step`
    /// applies the edge at the instruction boundary).
    /// Returns the master-cycle cost of any HDMA performed on this line
    /// crossing (frame-start setup + per-line transfer), so the caller can
    /// charge the CPU the stall (Phase 4).
    fn sched_one_line(&mut self, line_start_mclk: u64) -> u32 {
        let clock_count = self.clock_count;
        let vblank_start = self.vblank_start_line();
        let scanlines = self.scanlines_per_frame;
        let mut hdma_stall = 0u32;

        // Super Scope light-gun: when the CRT beam reaches the aimed scanline,
        // the gun strobes IOBit, latching the PPU H/V counters (OPHCT/OPVCT)
        // the game reads back for the beam position.
        if let Some((h, v)) = self.cpu_regs.scope_latch_target(self.ppu_line) {
            self.ppu.latch_counters(h, v);
        }

        // Render the framebuffer row the beam just finished scanning.
        // Hardware line origin (gap #7, proven by the HiColor chart —
        // Mesen2's frame == the corpus reference, luna's was one line
        // late): the displayed picture is PPU lines 1..=224, and fb row
        // `r` is drawn DURING PPU line `r+1`. So the crossing at the end
        // of line L commits row L-1 — with line L's end-of-line register
        // state, one line fresher than the old `row L at end of L`
        // mapping. Static content is unchanged (the renderer is keyed by
        // the row index); per-line dynamic state (HiColor CGRAM DMAs,
        // HDMA gradients) lands on the hardware row.
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

        // Line `ppu_line`'s HDMA transfer — fired at the END of the line
        // (hardware runs it at dot ~278 of line V, i.e. AFTER the visible
        // pixels of framebuffer row V-1, which the render above just
        // committed). Firing it at the start of the line (the previous
        // model) made every HDMA-driven row one table entry too new once
        // the hardware line origin landed (RedSpace9BitHDMA's starfield
        // was the tripwire: 4x worse vs the hardware reference). Same
        // per-line table cursor — only the application point moved one
        // crossing later, between the same two rendered rows.
        if self.ppu_line < vblank_start {
            // Trace HDMA register writes for the Event Viewer, exactly like
            // MDMA: hdma_run_line stamps each channel via set_active_channel
            // (HdmaChannelFlag | ch, Mesen2 SnesDmaController.cpp:264), and the
            // shared DmaBusView::write_b records the DmaTraceEvent. The trace
            // log is moved into the view for the line and returned after.
            let mut trace = self.dma.dma_trace.take();
            let trace_hclock = self.hclock();
            let trace_blank_now = self.ppu_line >= self.vblank_start_line();
            let mut view = DmaBusView {
                wram: &mut *self.wram,
                mapper: &mut *self.mapper,
                ppu: &mut *self.ppu,
                wm_addr: &mut *self.wm_addr,
                mdr: &mut *self.mdr,
                apu: &mut *self.apu_real,
                apu_stub: &mut *self.apu_stub_fallback,
                apu_panicked: *self.apu_panicked,
                dma_trace: trace.as_mut(),
                last_a_addr: 0,
                trace_frame: self.frame_count,
                trace_line: self.ppu_line,
                trace_blank: trace_blank_now,
                trace_hclock,
                dma_channel: 0,
                mem_trace: self.mem_trace_log.as_mut(),
                breakpoints: self.breakpoints.as_deref_mut(),
                cpu_pc: self.cpu_pc_full,
                trace_mclk: line_start_mclk,
            };
            hdma_stall += self
                .dma
                .hdma_run_line(&mut view, line_start_mclk, clock_count);
            self.dma.dma_trace = trace;
        }

        self.ppu_line += 1;
        if self.ppu_line == vblank_start {
            // Entering VBlank: latch the $4210 NMI flag + HVBJOY.7.
            self.cpu_regs.nmi_flag = true;
            self.cpu_regs.hvbjoy |= 0x80;
            if self.cpu_regs.nmitimen & 0x80 != 0 {
                *self.nmi = true;
                self.nmis_serviced = self.nmis_serviced.saturating_add(1);
                self.trace_irq_signal(MemEventKind::NmiSignal, self.cpu_regs.nmitimen);
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
            // P1 — faithful `nmiLine`: ares clears it when `nmiValid` falls
            // (vcounter < vdisp), i.e. at VBlank end, NOT only on a $4210 read
            // (irq.cpp `nmiLine = nmiValid`). Without this the flag stays
            // stale-true into the next frame — the latent bug a faithful
            // late-NMI-enable (P2) would turn into a spurious NMI (the SMRPG
            // black-screen tripwire). The actual NMI is still fired at VBlank
            // entry via `*self.nmi`; this only fixes what `$4210` reads outside
            // VBlank (now 0, matching hardware).
            self.cpu_regs.nmi_flag = false;
            self.frame_count = self.frame_count.saturating_add(1);
            self.mclk.on_frame_wrap();
            // Snapshot whether the frame that just completed showed any
            // visible content, paired with the frame counter bump so a
            // front-end polling at this boundary reads a consistent value.
            self.ppu.latch_frame_content();
            // Interlace field parity flips every frame at the V-counter wrap
            // (ares counter/inline.hpp:32), exposed at STAT78 bit 7. Phase A:
            // flag only — no vertical doubling yet.
            self.ppu.field = !self.ppu.field;
            let trace_hclock = self.hclock();
            let trace_blank_now = self.ppu_line >= self.vblank_start_line();
            let mut view = DmaBusView {
                wram: &mut *self.wram,
                mapper: &mut *self.mapper,
                ppu: &mut *self.ppu,
                wm_addr: &mut *self.wm_addr,
                mdr: &mut *self.mdr,
                apu: &mut *self.apu_real,
                apu_stub: &mut *self.apu_stub_fallback,
                apu_panicked: *self.apu_panicked,
                // hdma_init only reads table headers/pointers (A-bus); it makes
                // no B-bus register writes, so there is nothing to trace here.
                // The per-scanline transfers below are what the Event Viewer
                // captures.
                dma_trace: None,
                last_a_addr: 0,
                trace_frame: self.frame_count,
                trace_line: self.ppu_line,
                trace_blank: trace_blank_now,
                trace_hclock,
                dma_channel: 0,
                mem_trace: self.mem_trace_log.as_mut(),
                breakpoints: self.breakpoints.as_deref_mut(),
                cpu_pc: self.cpu_pc_full,
                trace_mclk: line_start_mclk,
            };
            hdma_stall += self.dma.hdma_init(&mut view, line_start_mclk, clock_count);
        }

        hdma_stall
    }
}

impl Bus for SnesBus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 {
        let value = self.read_inner(addr);
        // Latch the byte on the CPU data bus (ares' `r.mdr`): open-bus
        // reads below return this. An open-bus read returns the prior
        // MDR (read_inner hands back `*self.mdr`), so this is idempotent
        // for those — it only changes on a real fetch.
        //
        // The trace/watchpoint hook is not here: `read_inner` fires it at
        // the bus-sampling point, four clocks before the access ends.
        *self.mdr = value;
        value
    }
    fn write(&mut self, addr: Addr24, value: u8) {
        // A write drives the data bus too, so it updates the MDR.
        *self.mdr = value;
        self.trace_mem_access(addr, MemEventKind::Write, value);
        self.write_inner(addr, value);
    }
    fn io_cycle(&mut self, mcycles: MCycles) {
        self.advance_time(mcycles, true);
    }
    fn last_cycle(&mut self, i_flag: bool) -> InterruptSample {
        // ares `CPU::lastCycle()` (`sfc/cpu/irq.cpp:88-93`), which is
        // `nmiTest()` + `irqTest()`:
        //
        // - `nmiTest()` consumes `status.nmiTransition`. luna's edge is
        //   `Snes::nmi_pending`, raised by the scanline scheduler at
        //   VBlank entry when NMITIMEN.7 allows it.
        // - `irqTest()` consumes `status.irqTransition` but also sees the
        //   held `r.irq` pin, so a line still asserted re-qualifies at
        //   every poll. luna's held lines are `cpu_regs.irq_flag` (H/V
        //   timer, until `$4211` is read) and the coprocessor's own line.
        // - Both clear `r.wai` **before** the `I` check, so a masked IRQ
        //   still ends a `WAI` without entering the handler.
        //
        // ares' `status.irqLock` is deliberately not ported: it is set on
        // a `$4200` write and after a DMA burst, but `CPU::step()` clears
        // it at its top and every access and DMA step calls `step()`, so
        // it is always 0 by the time any `lastCycle()` runs. (Mesen2's
        // equivalent IS live — a one-cycle delay after DMA. That is a
        // real divergence between the references; we follow ares here and
        // track it as its own row.)
        let nmi = std::mem::replace(self.nmi, false);
        let edge = std::mem::replace(self.irq, false);
        let line = self.cpu_regs.irq_flag || self.mapper.coproc_main_irq_pending();
        let irq = edge || line;
        InterruptSample {
            nmi,
            irq: irq && !i_flag,
            wake: nmi || irq,
        }
    }
}

impl SnesBus<'_> {
    /// Master clocks in the scanline the cursor is currently on.
    #[inline]
    const fn line_period(&self) -> u32 {
        line_period(self.ppu_line, self.ppu.setini & 0x01 != 0, self.ppu.field)
    }

    /// The PPU's live (H, V): H in dots (a dot = 4 master clocks) for IRQ /
    /// HTIME comparison, V = the scanline. Read from the incremental counters,
    /// never derived from the master clock — see [`line_period`].
    #[inline]
    const fn hv(&self) -> (u16, u16) {
        ((self.mcycles_in_line / 4) as u16, self.ppu_line)
    }

    /// The exact horizontal master-clock position within the current scanline
    /// (Mesen2's `MemoryManager::GetHClock`), used verbatim as the Event
    /// Viewer's `Cycle` — full master-cycle precision, unlike [`Self::hv`].
    #[inline]
    const fn hclock(&self) -> u16 {
        self.mcycles_in_line as u16
    }

    /// Advance the master clock and the time-driven subsystems by
    /// `mcycles` (Phase 1 cycle-accuracy: per-bus-access synchronisation
    /// instead of one end-of-instruction lump).
    ///
    /// `advance_coproc` is `false` only on the DMA accounting path: the
    /// coprocessor already advanced per transferred byte inside
    /// [`DmaBusView::tick`], so charging it again with the lumped DMA
    /// cost here would double-count it.
    fn advance_time(&mut self, mcycles: MCycles, advance_coproc: bool) {
        // HDMA + DRAM refresh steal master cycles. `sched_advance` returns
        // the stall cost of anything crossed (per-line HDMA and the
        // once-per-line DRAM refresh); the CPU is halted for that long, so we
        // re-advance everything (master clock, APU, PPU, coproc, IRQ poll) by
        // the stall in a follow-up pass. The loop converges — each scanline's
        // HDMA + refresh run once and a stall rarely spans a full 1364-mclk
        // line.
        let mut step = mcycles;
        // False once we are re-advancing for a stall rather than for the
        // caller's own access — see the coproc note below.
        let mut caller_time = true;
        // The caller's own time goes to whoever owns this bus borrow (CPU
        // active / WAI / STP, or the DMA burst); a stall pass is credited to
        // its own buckets when it is discovered, below (issue #223).
        let mut kind = Some(self.mclk.current);
        loop {
            *self.mclk_total = self.mclk_total.saturating_add(step);
            if let Some(k) = kind {
                self.mclk.credit(k, step);
            }
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
            let (refresh, hdma) = self.sched_advance(step as u32);
            // `advance_coproc` is about the CALLER's time only: on the DMA
            // path the coprocessor was already stepped per transferred byte
            // in `DmaBusView::tick`, so charging it the lumped DMA cost again
            // would double-count. A stall is different — it is time nobody has
            // accounted for yet, and the coprocessor runs through it (the CPU
            // and the DMA are both halted), so always step it for one.
            if advance_coproc || !caller_time {
                self.mapper.step_coproc(step as u32, self.scpu_mar);
            }
            // The stall is charged on EVERY path, DMA included. ares checks
            // the DRAM refresh inside `CPU::step` (`cpu/timing.cpp:21-29`),
            // and a DMA runs on that same clock — `step(8)` per byte — so a
            // transfer is halted by the refresh once per scanline just like
            // ordinary CPU code. luna used to skip the charge here, which made
            // a large DMA finish ~40 mclk/scanline early: a 64 KiB VRAM clear
            // came out 15 840 master clocks short of Mesen2's, and every one
            // of those clocks is CPU-vs-scanline phase error that never comes
            // back.
            if refresh == 0 && hdma == 0 {
                return;
            }
            self.mclk.credit(MclkKind::Refresh, u64::from(refresh));
            self.mclk.credit(MclkKind::Hdma, u64::from(hdma));
            kind = None;
            caller_time = false;
            step = MCycles::from(refresh + hdma);
        }
    }

    /// ares' `dmaEdge()` (`cpu/timing.cpp:100-133`), the deferred half: a DMA
    /// armed by a `$420B` write runs at the start of the next bus access,
    /// after `clock_count` is known (the realignment step is computed against
    /// it) and before the access's own clocks are charged.
    fn dma_edge(&mut self) {
        if self.dma.pending_mdma == 0 {
            return;
        }
        // The burst's clocks are the DMA's, not the instruction's whose
        // access ran the edge (issue #223); stalls inside it still land in
        // their own buckets.
        let prev = self.mclk.current;
        self.mclk.current = MclkKind::Dma;
        self.dma_edge_inner();
        self.mclk.current = prev;
    }

    fn dma_edge_inner(&mut self) {
        let value = self.dma.pending_mdma;
        if value == 0 {
            return;
        }
        self.dma.pending_mdma = 0;
        // ares charges the burst against the DMA clock divider at the edge
        // and the cost of the access whose edge runs it — see `mdma_cost`.
        let mclk_at_edge = *self.mclk_total;
        let clock_count = self.clock_count;
        // Trigger sync DMA on every channel selected in `value`.
        // We splat the SnesBus borrows: `dma` is mutated by the DMA
        // call, the other refs flow into DmaBusView. This borrow-split
        // lets the DMA run without re-entering the Bus impl. The
        // DMA→VRAM trace is moved into the view for the burst (so its
        // $2118/9 writes are captured) and restored after.
        //
        // `advance_coproc = false`: coprocs already advanced per byte via
        // `DmaBusView::tick`, so the master-clock charge here must not
        // re-charge them.
        if self.dma.hdmaen == 0 {
            // Fast path — no HDMA armed, so nothing the DMA could
            // cross matters: run the whole burst as one lump (the
            // legacy behaviour, byte-identical). Covers virtually all
            // forced-blank / vblank uploads.
            let mut trace = self.dma.dma_trace.take();
            let trace_hclock = self.hclock();
            let bytes = {
                let trace_blank_now = self.ppu_line >= self.vblank_start_line();
                let mut view = DmaBusView {
                    wram: self.wram,
                    mapper: self.mapper,
                    ppu: self.ppu,
                    wm_addr: self.wm_addr,
                    mdr: self.mdr,
                    apu: &mut *self.apu_real,
                    apu_stub: &mut *self.apu_stub_fallback,
                    apu_panicked: *self.apu_panicked,
                    dma_trace: trace.as_mut(),
                    last_a_addr: 0,
                    trace_frame: self.frame_count,
                    trace_line: self.ppu_line,
                    trace_blank: trace_blank_now,
                    trace_hclock,
                    dma_channel: 0,
                    mem_trace: self.mem_trace_log.as_mut(),
                    breakpoints: self.breakpoints.as_deref_mut(),
                    cpu_pc: self.cpu_pc_full,
                    trace_mclk: mclk_at_edge,
                };
                self.dma.run_mdma(&mut view, value)
            };
            self.dma.dma_trace = trace;
            let cost = mdma_cost(
                mclk_at_edge,
                value.count_ones(),
                u64::from(bytes),
                clock_count,
            );
            self.advance_time(cost, false);
            return;
        }

        // Segmented path (Phase 5) — HDMA is armed, so a long burst
        // must yield to HDMA at scanline boundaries instead of landing
        // all at once. Drive the DMA in segments bounded by the next
        // line crossing; `advance_time` then runs that line's HDMA via
        // `sched_one_line`. A DMA yields to HDMA at most once per line,
        // so line-granular segmentation reproduces ares' per-byte
        // `dmaEdge` ordering for this case.
        self.advance_time(8, false); // one-shot start overhead
        let mut trace = self.dma.dma_trace.take();
        loop {
            let room_mclk = self.line_period().saturating_sub(self.mcycles_in_line);
            // Bytes that fit before the next boundary (≥1 to progress).
            // 8 mclk/byte, matching `DmaChannel::run_segment`.
            let seg_bytes = (room_mclk / 8).max(1);
            let trace_hclock = self.hclock();
            let done = {
                let trace_blank_now = self.ppu_line >= self.vblank_start_line();
                let mut view = DmaBusView {
                    wram: self.wram,
                    mapper: self.mapper,
                    ppu: self.ppu,
                    wm_addr: self.wm_addr,
                    mdr: self.mdr,
                    apu: &mut *self.apu_real,
                    apu_stub: &mut *self.apu_stub_fallback,
                    apu_panicked: *self.apu_panicked,
                    dma_trace: trace.as_mut(),
                    last_a_addr: 0,
                    trace_frame: self.frame_count,
                    trace_line: self.ppu_line,
                    trace_blank: trace_blank_now,
                    trace_hclock,
                    dma_channel: 0,
                    mem_trace: self.mem_trace_log.as_mut(),
                    breakpoints: self.breakpoints.as_deref_mut(),
                    cpu_pc: self.cpu_pc_full,
                    trace_mclk: *self.mclk_total,
                };
                self.dma.run_mdma_segment(&mut view, value, seg_bytes)
            };
            // Charge this segment's master clock; crosses the line
            // boundary and fires HDMA for any visible line crossed.
            self.advance_time(u64::from(done) * 8, false);
            if self.dma.mdma_cursor.is_none() {
                break; // burst complete
            }
        }
        self.dma.dma_trace = trace;
    }

    /// A CPU read, in ares' shape (`cpu/memory.cpp:8-19`): the DMA edge runs
    /// first, then all but the last four clocks are charged, the bus is
    /// sampled, and the tail is charged — see `READ_SAMPLE_TAIL`.
    fn read_inner(&mut self, addr: Addr24) -> u8 {
        // Hold this access as the S-CPU memory-address register (ares
        // `cpu.r.mar`) so the per-access coproc step can model SA-1
        // `conflict()` bus contention against it.
        self.scpu_mar = addr;
        let speed = address_speed(addr, *self.fast_rom);
        // ares `status.clockCount` — the cost of the access in flight, which
        // the DMA/HDMA realignment steps are computed against.
        self.clock_count = speed.mcycles() as u32;
        // ares runs `dmaEdge()` before the access's clocks: a DMA armed by
        // the previous instruction's `$420B` write executes HERE, charged to
        // this instruction — see `dma_edge`.
        self.dma_edge();
        // Every bus speed is >= 6 mclk, so the pre-sample step is >= 2.
        self.io_cycle(speed.mcycles() - READ_SAMPLE_TAIL);
        let data = self.read_sampled(addr);
        // Timestamp the trace / fire watchpoints at the sampling point — that
        // is where the byte was latched.
        self.trace_mem_access(addr, MemEventKind::Read, data);
        self.io_cycle(READ_SAMPLE_TAIL);
        data
    }

    /// Decode + perform the read itself, at the point in the access
    /// [`Self::read_inner`] has positioned the clock on. Charges no time.
    fn read_sampled(&mut self, addr: Addr24) -> u8 {
        if let Some(o) = Self::wram_offset(addr) {
            return self.wram[o];
        }
        if let Some(off) = Self::ppu_offset(addr) {
            // $2137 SLHV — reading latches the H/V counters into
            // OPHCT / OPVCT, but ONLY while the WRIO latch line is high
            // (ares ppu/io.cpp $2137 `if(cpu.pio().bit(7))`; Mesen2
            // SnesPpu.cpp agrees). The returned byte is the CPU's own
            // MDR (ares readIO `return data;`), which `Ppu::read`
            // reproduces for every register that is write-only on both
            // PPU chips.
            if off == luna_ppu::register::SLHV && self.cpu_regs.wrio & 0x80 != 0 {
                let (h, v) = self.hv();
                self.ppu.latch_counters(h, v);
            }
            // A CGRAM read during the picture returns the entry the PPU
            // is fetching (ares `io.cpp:47-53`): bring the line up to the
            // current dot so that entry is the pixel under the beam, and
            // refresh the picture gate the write path maintains.
            if off == luna_ppu::register::CGDATAREAD {
                let visible = self.ppu_line < self.vblank_start_line();
                self.ppu.active_display = visible && (self.ppu.inidisp & 0x80) == 0;
                self.ppu.beam_dot = self.hv().0;
                if visible {
                    let (h, _) = self.hv();
                    self.ppu.flush_partial_scanline(
                        self.ppu_line,
                        h.min(luna_ppu::FRAME_W as u16),
                        luna_ppu::RenderOptions::default(),
                    );
                }
            }
            return self.ppu.read(off, *self.mdr);
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
            if let Some(log) = self
                .mailbox_log
                .as_mut()
                .filter(|l| l.len() < DEBUG_LOG_MAX_EVENTS)
            {
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
            return *self.mdr;
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
            // A Mouse on this port answers the manual serial read with its own
            // 32-bit stream (device signature + signed dx/dy) instead of the
            // pad shift register. The $4016 strobe drives its latch (below).
            let port = usize::from(offset != 0x4016);
            if let Some(bit) = self.cpu_regs.port_serial_bit(port) {
                return bit;
            }
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
            return self.dma.read_register(offset).unwrap_or(*self.mdr);
        }
        if Self::is_mdmaen(addr) || Self::is_hdmaen(addr) {
            // MDMAEN / HDMAEN are write-only; reads return open bus.
            return *self.mdr;
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
                let (h, _) = self.hv();
                let in_hblank = h == 0 || h >= 274;
                let hblank_bit = if in_hblank { 0x40 } else { 0x00 };
                // Bits 1-5 are CPU open bus (ares io.cpp:33-37 only
                // drives bits 0, 6, 7 of the incoming MDR).
                return (self.cpu_regs.hvbjoy & 0x81) | hblank_bit | (*self.mdr & 0x3E);
            }
            if reg_off == 0x4210 {
                // RDNMI — see `RDNMI_RAISE_HCLOCK` / `RDNMI_HOLD_HCLOCK`. Both
                // windows are in master clocks, not dots: luna used to round
                // the hold up to a whole dot (< 2 dots = 8 mclk) and raise the
                // line at H=0, which let a `BIT $4210 / BPL` poll loop pass
                // twice in one VBlank (issue #107).
                let hclock = self.hclock();
                let on_nmi_line = self.ppu_line == self.vblank_start_line();
                let raised = !(on_nmi_line && hclock < RDNMI_RAISE_HCLOCK);
                let in_hold = on_nmi_line && hclock < RDNMI_HOLD_HCLOCK;
                // Bits 4-6 are CPU open bus (ares io.cpp:24-27 drives
                // only the version nibble and bit 7 of the MDR).
                return self.cpu_regs.read_rdnmi(raised, in_hold) | (*self.mdr & 0x70);
            }
            if reg_off == 0x4211 {
                // TIMEUP: bit 7 = held IRQ line, acknowledged by the read
                // UNLESS the bus sample lands inside the 4-clock hold
                // window after the raise (ares irq.cpp:60-66 `timeup()`
                // under `irqHold` — the mirror of the RDNMI hold). Bits
                // 0-6 are CPU open bus (ares io.cpp:29-31).
                let in_hold = *self.mclk_total < self.cpu_regs.irq_raise_mclk + 4;
                return self.cpu_regs.read_timeup(in_hold) | (*self.mdr & 0x7F);
            }
            if let Some(v) = self.cpu_regs.read(reg_off) {
                return v;
            }
            // Write-only registers fall through to open bus.
            return *self.mdr;
        }
        if let Some(v) = self.mapper.read(addr) {
            if let Some(reg) = Self::sa1_reg(addr)
                && let Some(log) = self
                    .sa1_log
                    .as_mut()
                    .filter(|l| l.len() < DEBUG_LOG_MAX_EVENTS)
            {
                log.push(Sa1LogEvent {
                    mclk_total: *self.mclk_total,
                    pc_full: self.cpu_pc_full,
                    kind: MailboxEventKind::Read,
                    reg,
                    value: v,
                });
            }
            return v;
        }
        // Unmapped → open bus: the last byte driven on the CPU bus (MDR).
        *self.mdr
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
        // S-CPU memory-address register (ares `cpu.r.mar`) — see `read_inner`.
        self.scpu_mar = addr;
        let speed = address_speed(addr, *self.fast_rom);
        self.clock_count = speed.mcycles() as u32;
        // ares `CPU::write` runs `dmaEdge()` before stepping (`memory.cpp:24`).
        self.dma_edge();
        self.io_cycle(speed.mcycles());

        // Nocash debug TTY: capture `$21FC` writes (no$/Mesen console port —
        // the SDK's `SNES_NOCASH`/`SNES_ASSERT`) when enabled, bounded so a
        // runaway never grows unbounded. Otherwise `$21FC` is open-bus.
        if let Some(buf) = self.nocash_log.as_mut() {
            let bank = (addr >> 16) as u8;
            if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && addr as u16 == 0x21FC {
                if buf.len() < 1 << 20 {
                    buf.push(value);
                }
                return;
            }
        }

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
                self.ppu_line < self.vblank_start_line() && (self.ppu.inidisp & 0x80) == 0;
            // The real dot of this access, for the CGRAM picture window
            // (the flush below clamps its cursor to the 256 picture dots).
            self.ppu.beam_dot = self.hv().0;

            // Phase 2 of gap G6 — intra-line partial flush. If the
            // CPU is writing a render-affecting PPU register ($2100..$2133)
            // mid-scanline, commit the in-progress dots with the OLD
            // state BEFORE applying the write so the partial line gets
            // the pre-write pixels. (Mesen2 SnesPpu.cpp:1884-1886
            // RenderScanline-before-write pattern.)
            if off < 0x34 && self.ppu_line < self.vblank_start_line() {
                let (h, _) = self.hv();
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
                if was_force_blank && !will_force_blank && self.ppu_line == self.vblank_start_line()
                {
                    self.ppu.oam.reload_address_from_latch();
                }
            }
            self.ppu.write(off, value);
            if off == 0x00 {
                // INIDISP just changed forced-blank: recompute `active_display`
                // LIVE so a MID-SCANLINE forced-blank gates VRAM/OAM writes
                // correctly. `active_display` is otherwise only refreshed per
                // scanline (advance_scheduler), so without this a game that
                // force-blanks mid-line then DMAs (Tales' attack-frame OBJ-tile
                // upload at line ~209) has its writes wrongly dropped — luna's
                // stale cache said active-display while forced-blank was set.
                // ares checks `displayDisable` live at the write (ppu io.cpp).
                self.ppu.active_display =
                    self.ppu_line < self.vblank_start_line() && (self.ppu.inidisp & 0x80) == 0;
            }
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
            if let Some(log) = self
                .mailbox_log
                .as_mut()
                .filter(|l| l.len() < DEBUG_LOG_MAX_EVENTS)
            {
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
                // Every connected peripheral latches off the $4016 strobe.
                self.cpu_regs.latch_devices(next_strobe);
            }
            // $4017 writes drive the expansion-port output pins —
            // ignored by an emulator that doesn't model the expansion.
            return;
        }
        if let Some(offset) = Self::dma_offset(addr) {
            self.dma.write_register(offset, value);
            // Let a DMA-observing coprocessor (S-DD1) capture the channel's
            // source address / length so it can set up on-the-fly graphics
            // decompression. Non-observing mappers ignore `$43xx` writes.
            let _ = self.mapper.write(addr, value);
            return;
        }
        if Self::is_mdmaen(addr) {
            // `$420B` only ARMS the transfer — ares `cpu/io.cpp`:
            // `if(data) status.dmaPending = 1;`. The burst itself runs at the
            // next `dmaEdge()`, i.e. at the start of the NEXT bus access, and
            // is charged to that instruction — which is where Mesen2's
            // per-instruction trace places it too. luna used to run it inline
            // here, charging it to the arming instruction: one instruction of
            // permanent CPU-vs-scanline phase error per DMA (issue #109).
            if value != 0 {
                self.dma.pending_mdma = value;
            }
            return;
        }
        if Self::is_hdmaen(addr) {
            self.dma.hdmaen = value;
            return;
        }
        if let Some(reg_off) = Self::cpu_reg_offset(addr) {
            // WRIO ($4201): the PPU H/V counters latch on the FALLING
            // edge of bit 7 (ares cpu/io.cpp:143 `io.pio.bit(7) &&
            // !data.bit(7)`; Mesen2 InternalRegisters.cpp:338 agrees) —
            // luna had the polarity inverted (0→1) until 2026-08-01.
            // Checked BEFORE CpuRegs::write so we see the previous value.
            // The PPU also mirrors the line level for the STAT78 bit-6
            // gate (held low ⇒ bit 6 reads 1, latch flag not cleared).
            if reg_off == 0x4201 {
                let prev = self.cpu_regs.wrio;
                if prev & 0x80 != 0 && value & 0x80 == 0 {
                    let (h, v) = self.hv();
                    self.ppu.latch_counters(h, v);
                }
                self.ppu.pio_bit7 = value & 0x80 != 0;
            }
            // P2 — late NMI enable: raising NMITIMEN.7 (0→1) while the NMI line
            // is still asserted ($4210 flag set, i.e. mid-VBlank, un-read) fires
            // the NMI now (ares irq.cpp `nmitimenUpdate`: `if nmiEnable.raise(7)
            // && nmiLine → nmiTransition`). Checked BEFORE the CpuRegs::write so
            // we see the previous NMITIMEN.7. P1's VBlank-end clear of `nmi_flag`
            // is what makes this safe: the line is no longer stale-true outside
            // VBlank, so this can't fire the spurious NMI that black-screened
            // SMRPG when the naive port was attempted.
            if reg_off == 0x4200
                && self.cpu_regs.nmitimen & 0x80 == 0
                && value & 0x80 != 0
                && self.cpu_regs.nmi_flag
            {
                *self.nmi = true;
                self.trace_irq_signal(MemEventKind::NmiSignal, value);
            }
            if self.cpu_regs.write(reg_off, value) {
                return;
            }
            // CpuRegs returned false → maybe a register that lives
            // elsewhere (e.g. $420D MEMSEL → fast_rom). Handle here.
            if reg_off == 0x420D {
                *self.fast_rom = value & 0x01 != 0;
            }
            return;
        }
        if let Some(reg) = Self::sa1_reg(addr)
            && let Some(log) = self
                .sa1_log
                .as_mut()
                .filter(|l| l.len() < DEBUG_LOG_MAX_EVENTS)
        {
            log.push(Sa1LogEvent {
                mclk_total: *self.mclk_total,
                pc_full: self.cpu_pc_full,
                kind: MailboxEventKind::Write,
                reg,
                value,
            });
        }
        // Mapper claims SRAM writes; anything not yet routed drops.
        let _ = self.mapper.write(addr, value);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests;
