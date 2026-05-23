//! Cycle-accurate 65C816 CPU — the SNES main processor.
//!
//! Dispatch is implemented as a large `match opcode { ... }` in
//! [`Cpu::step`]; LLVM lowers it to a jump-table in release mode, which is
//! what gives us the "static dispatch zero-alloc" hot loop required by
//! `ARCHITECTURE.md` §6.6.
//!
//! Mid-instruction accuracy comes from [`luna_bus::Bus::io_cycle`]: each
//! memory access pays its cost through the bus, which catches up the PPU
//! and HDMA immediately. The CPU itself doesn't track master cycles —
//! that's the bus's responsibility.
//!
//! See `ARCHITECTURE.md` §6.1.

pub mod addressing;
pub mod cpu;
pub mod flags;
pub mod opcodes;

pub use cpu::Cpu;
pub use flags::StatusFlags;
