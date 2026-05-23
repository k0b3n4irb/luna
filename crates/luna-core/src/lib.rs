//! SNES emulator core.
//!
//! Phase 0.6 scope: just enough to wire a `Cpu65816` against a cartridge
//! and step it. WRAM (128 KB) is exposed. PPU / APU / DMA registers are
//! still stubbed (reads return 0xFF / open-bus; writes are dropped) and
//! will land in Phase 1+.
//!
//! See `ARCHITECTURE.md` §6 and §6.6 for the target architecture.

pub mod snes;

pub use snes::Snes;
