//! SNES emulator core.
//!
//! Assembles `luna-cpu-65c816`, `luna-ppu`, `luna-apu`, `luna-dma`,
//! `luna-coproc` and `luna-cartridge` behind a single `Snes` struct, and
//! runs the CPU-driven master-clock catch-up scheduler.
//!
//! See `ARCHITECTURE.md` §6 and §6.6 in particular.
