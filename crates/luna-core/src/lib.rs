//! SNES emulator core.
//!
//! Phase 0.6 scope: just enough to wire a `Cpu65816` against a cartridge
//! and step it. WRAM (128 KB) is exposed. PPU / APU / DMA registers are
//! still stubbed (reads return 0xFF / open-bus; writes are dropped) and
//! will land in Phase 1+.
//!
//! See `ARCHITECTURE.md` §6 and §6.6 for the target architecture.

pub mod apu_stub;
pub mod coproc;
pub mod cpu_regs;
pub mod dma;
pub mod snes;

pub use apu_stub::{ApuStub, Phase as ApuPhase};
pub use cpu_regs::CpuRegs;
pub use dma::{DmaTraceEvent, DmaTraceLog};
pub use luna_apu::Spc700TraceEvent;
pub use luna_bus::{
    Mapper, MapperKind, NullMapper, Sa1SideEvent, Sa1TraceEvent, SuperFxTraceEvent,
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
