//! SNES cartridge coprocessors.
//!
//! Each chip is gated behind a Cargo feature so a minimal build can target a
//! specific game without pulling in unused coprocessors.
//!
//! V1 priorities: SA-1 (Super Mario RPG), Super FX (Star Fox), DSP-1
//! (Super Mario Kart). V2: DSP-2/3/4, Cx4, S-DD1. V3: SPC7110, OBC1,
//! ST010+.
//!
//! See `ARCHITECTURE.md` §6.5.

pub mod sa1;

pub use sa1::Sa1Chip;
