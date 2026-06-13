//! The [`Mapper`] trait for cartridge mappings.

use crate::types::Addr24;

/// SNES cartridge mapping mode.
///
/// Determines how the cartridge ROM and SRAM are mapped into the 24-bit
/// CPU address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperKind {
    /// Mode 20 — 32 KB ROM pages at `$8000-$FFFF`, mirrored across many
    /// banks. Most common for small to medium games.
    LoRom,
    /// Mode 21 — 64 KB ROM pages at `$0000-$FFFF` of banks `$40-$7D`
    /// (and mirrors). Used for larger games.
    HiRom,
    /// Mode 25 — extended `HiROM` allowing > 32 Mbit ROMs.
    ExHiRom,
    /// SA-1 mapping (Super Mario RPG, Kirby Super Star, etc.).
    Sa1,
    /// Super FX mapping (Star Fox, Yoshi's Island, Doom).
    SuperFx,
    /// NEC DSP-1 coprocessor (Super Mario Kart, Pilotwings, …). The base
    /// ROM layout is `LoROM` or `HiROM` depending on the board; the chip is
    /// driven through a `Dsp1Mapper` shim in luna-core.
    Dsp1,
    /// S-DD1 (Star Ocean, Street Fighter Alpha 2).
    Sdd1,
    /// SPC7110 (Far East of Eden Zero).
    Spc7110,
}

impl MapperKind {
    /// Parse a `--force-mapper` CLI token (case-insensitive) into a
    /// [`MapperKind`]. This is the canonical name table; front-ends must
    /// not re-implement it. Returns `None` for an unknown token.
    ///
    /// `sdd1` / `spc7110` parse successfully even though the core cannot
    /// build them yet — that surfaces as a clean "unsupported mapper"
    /// error at load time rather than "unknown --force-mapper".
    #[must_use]
    pub fn from_cli_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "lorom" => Some(Self::LoRom),
            "hirom" => Some(Self::HiRom),
            "exhirom" => Some(Self::ExHiRom),
            "sa1" => Some(Self::Sa1),
            "superfx" => Some(Self::SuperFx),
            "dsp1" => Some(Self::Dsp1),
            "sdd1" => Some(Self::Sdd1),
            "spc7110" => Some(Self::Spc7110),
            _ => None,
        }
    }
}

/// A cartridge mapper: routes CPU bus accesses to ROM, SRAM, and
/// coprocessor regions as defined by the mapping mode.
///
/// The mapper does NOT charge memory-speed cycles — that's the bus's
/// responsibility (see [`crate::Bus::io_cycle`]).
pub trait Mapper {
    /// Identify the mapping mode.
    fn kind(&self) -> MapperKind;

    /// Returns `Some(byte)` if `addr` falls inside a region this mapper
    /// owns (ROM / SRAM / coprocessor MMIO), or `None` if it falls
    /// outside (in which case the bus will route to WRAM / PPU / etc.).
    fn read(&mut self, addr: Addr24) -> Option<u8>;

    /// Mirror of [`Mapper::read`] for writes. Returns `true` if the
    /// mapper accepted the write (so the bus knows not to fall through),
    /// `false` otherwise.
    fn write(&mut self, addr: Addr24, value: u8) -> bool;

    /// Size of the ROM in bytes.
    fn rom_size(&self) -> usize;

    /// Size of the SRAM in bytes (0 if none).
    fn sram_size(&self) -> usize;

    /// Re-power the cartridge coprocessor to its power-on state, as the
    /// SNES reset line does on real hardware (ares `SuperFX::power()` /
    /// `SA1::power()`). ROM and battery-backed SRAM persist; the
    /// coprocessor's registers, internal RAM, caches and its own CPU
    /// return to power-on. Default = no-op for plain `LoROM` / `HiROM`
    /// carts, which have no coprocessor state to clear.
    ///
    /// Without this, `Snes::reset` re-runs the main-CPU reset vector but
    /// leaves a coprocessor (Super FX / SA-1) mid-execution, so a reset
    /// of a Super FX title (e.g. Doom) freezes instead of rebooting.
    fn reset(&mut self) {}

    /// Step the cartridge coprocessor (SA-1 / Super FX / DSP-1 / …)
    /// forward by approximately `main_mclk` master cycles of main-CPU
    /// progress. Default = no-op for plain `LoROM` / `HiROM` carts.
    ///
    /// Coproc mappers translate that to the right number of coproc
    /// instructions internally (e.g. SA-1 is 10.74 MHz ≈ 2× the main
    /// CPU's max `FastROM` rate, so it consumes ~2 SA-1 instructions
    /// per main-CPU \`mclk\`). Implementations should be safely
    /// callable even when their coprocessor is reset / stopped.
    fn step_coproc(&mut self, _main_mclk: u32) {}

    /// `true` while the cartridge coprocessor is asserting an IRQ line
    /// onto the main CPU (SA-1 SCNT bit 7 latched + SIE bit 7 enabled,
    /// for instance). The main-CPU bus ORs this into its own
    /// `irq_pending` so the CPU services it through its normal IRQ path.
    /// Plain `LoROM` / `HiROM` carts return `false`.
    fn coproc_main_irq_pending(&self) -> bool {
        false
    }

    /// Diagnostic: the coprocessor's work RAM (e.g. Super FX Game Pak RAM)
    /// read directly, bypassing the SNES-side ownership gating that returns
    /// open-bus while the coprocessor owns the RAM. `None` if the mapper has
    /// no coprocessor RAM. Used to compare luna's CPU-prepared GSU inputs
    /// against a reference.
    fn coproc_ram(&self) -> Option<&[u8]> {
        None
    }

    /// Snapshot the SA-1 coprocessor's CPU state, if this mapper hosts
    /// one. Plain `LoROM` / `HiROM` / Super FX / DSP-N return `None`.
    /// Used by luna-api to expose SA-1 PC / running state to debug
    /// integrations diagnosing main↔SA-1 mailbox deadlocks.
    fn sa1_snapshot(&self) -> Option<Sa1Snapshot> {
        None
    }

    /// Enable SA-1-*side* execution logging: from now the coprocessor
    /// records its own accesses to the SA-1 MMIO window (`$2200-$23FF`)
    /// with its PC, for diagnosing why the SA-1 (re)asserts registers
    /// like SCNT. Complements the S-CPU-side `--sa1-log`. No-op for
    /// non-SA-1 mappers.
    fn enable_sa1_side_log(&mut self) {}

    /// Drain the SA-1-side execution log (empty if disabled / not SA-1).
    fn take_sa1_side_log(&mut self) -> Vec<Sa1SideEvent> {
        Vec::new()
    }

    /// Enable a full SA-1 instruction trace: a pre-instruction register
    /// snapshot per SA-1 opcode (up to `max_events`), for diffing luna's
    /// SA-1 PC/register stream against a reference emulator (bsnes/Mesen2)
    /// to localise the SMRPG deadlock. No-op for non-SA-1 mappers.
    fn enable_sa1_trace(&mut self, _max_events: usize) {}

    /// Drain the SA-1 instruction trace (empty if disabled / not SA-1).
    fn take_sa1_trace(&mut self) -> Vec<Sa1TraceEvent> {
        Vec::new()
    }

    /// Enable a full Super FX (GSU) instruction trace: a per-opcode
    /// snapshot of the GSU PC + register file (up to `max_events`), for
    /// diffing luna's GSU stream against a reference (bsnes / siena) to
    /// localise rendering divergences. No-op for non-Super-FX mappers.
    fn enable_superfx_trace(&mut self, _max_events: usize) {}

    /// Drain the Super FX instruction trace (empty if disabled / not GSU).
    fn take_superfx_trace(&mut self) -> Vec<SuperFxTraceEvent> {
        Vec::new()
    }
}

/// One per-opcode snapshot of the GSU register file — the Super FX
/// analogue of [`Sa1TraceEvent`]. Diffing this PC + register stream against
/// a reference GSU trace (bsnes / siena) pinpoints the first divergence.
#[derive(Debug, Clone, Copy)]
pub struct SuperFxTraceEvent {
    /// GSU PC (`pbr << 16 | r15`) at the fetch of this opcode.
    pub pc_full: u32,
    /// The opcode byte being executed.
    pub opcode: u8,
    /// Status flag register (raw 16-bit).
    pub sfr: u16,
    /// General-purpose registers R0–R15 (R15 = PC).
    pub r: [u16; 16],
    /// GSU clock position on the shared master-clock axis at this opcode
    /// (`cpu_mclk − clock_deficit`). Per-instruction deltas give each op's
    /// cycle cost; jumps across a GO/STOP boundary expose CPU↔GSU idle time —
    /// the timing dimension the per-op register stream alone cannot show.
    pub mclk: u64,
    /// First executed instruction of a GO task (the GSU had been stopped).
    /// Counting `go_start` per frame = number of GSU tasks that frame.
    pub go_start: bool,
    /// This instruction cleared `sfr.g` (STOP) — the end of a GO task.
    pub stop: bool,
}

/// One pre-instruction snapshot of the SA-1's 65C816 register file —
/// the SA-1 analogue of luna-core's `CpuTraceEvent`. Diffing this PC +
/// register stream against a reference SA-1 trace pinpoints the first
/// divergence.
#[derive(Debug, Clone, Copy)]
pub struct Sa1TraceEvent {
    /// 24-bit SA-1 PC (`pb << 16 | pc`) before the opcode runs.
    pub pc_full: u32,
    /// Accumulator (16-bit; low byte when M=1).
    pub a: u16,
    /// X index.
    pub x: u16,
    /// Y index.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Processor status (`P`).
    pub p: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page.
    pub dp: u16,
    /// Emulation flag.
    pub e: bool,
}

/// One SA-1-side access to an SA-1 MMIO register (`$2200-$23FF`), tagged
/// with the SA-1's pre-instruction `PB:PC`. Lets a trace show *which*
/// SA-1 code reads/writes a register (e.g. the SCNT=$87 re-raise loop).
#[derive(Debug, Clone, Copy)]
pub struct Sa1SideEvent {
    /// 24-bit SA-1 PC (`pb << 16 | pc`) at the start of the instruction
    /// performing the access.
    pub sa1_pc: u32,
    /// `true` = write, `false` = read.
    pub write: bool,
    /// Register address in `$2200..=$23FF`.
    pub reg: u16,
    /// The byte transferred.
    pub value: u8,
}

/// Minimal SA-1 CPU snapshot for diagnostics. Captures the just-fetched
/// `PB:PC`, the processor status flags, and whether the chip is
/// released from reset. Enough to tell at a glance whether the SA-1
/// is stuck in a polling loop, running random ROM bytes after a bad
/// bank mapping, or genuinely halted.
#[derive(Debug, Clone, Copy)]
pub struct Sa1Snapshot {
    /// Program counter within the program bank.
    pub pc: u16,
    /// Program bank.
    pub pb: u8,
    /// Processor status (N V M X D I Z C).
    pub p: u8,
    /// `true` while CCNT.5 is clear (chip released from reset).
    pub running: bool,
}
