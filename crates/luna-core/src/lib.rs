//! SNES emulator core — the system glue.
//!
//! Owns the top-level [`Snes`] struct, the CPU-driven master-clock
//! scheduler (per-access `io_cycle` synchronization of PPU / APU /
//! coprocessors, DRAM refresh, HV IRQs), and the DMA + coprocessor
//! subsystems (`dma`, `coproc`). Consumed exclusively through
//! `luna-api` — front-ends (CLI / GUI / MCP) never depend on this
//! crate directly (see `.claude/rules/api-first.md`).

pub mod apu_stub;
pub mod breakpoints;
pub mod controller;
pub mod coproc;
pub mod cpu_regs;
pub mod dma;
pub mod snes;

pub use apu_stub::{ApuStub, Phase as ApuPhase};
pub use breakpoints::{BreakHit, BreakKind, BreakpointInfo, BreakpointSet};
pub use cpu_regs::CpuRegs;
pub use dma::{DmaTraceEvent, DmaTraceLog};
pub use luna_apu::Spc700TraceEvent;
pub use luna_bus::{
    Dsp1TraceEvent, Dsp1TraceKind, Mapper, MapperKind, NullMapper, Sa1SideEvent, Sa1TraceEvent,
    SuperFxTraceEvent,
};
pub use snes::{
    CpuTraceEvent, CpuTraceLog, MailboxEvent, MailboxEventKind, MemEventKind, MemTraceEvent,
    MemTraceLog, Sa1LogEvent, Snes, UnsupportedMapper,
};

/// A placeholder [`Mapper`] trait object that owns no ROM and claims no
/// addresses. The save-state layer uses it to `mem::replace` the live
/// mapper out of a [`Snes`] (the trait object cannot derive `Deserialize`).
#[must_use]
pub fn null_mapper() -> Box<dyn Mapper + Send> {
    Box::new(NullMapper)
}
