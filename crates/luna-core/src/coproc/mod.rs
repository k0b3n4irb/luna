//! SNES cartridge coprocessors.
//!
//! The chip-side state of the coprocessors whose bus-side shim lives in
//! `luna-bus` (`Sa1Mapper`, `Dsp1Mapper`). Emulated today: SA-1, Super FX,
//! DSP-1, S-DD1. Not yet: DSP-2/3/4, Cx4, OBC1, S-RTC, ST-01x, SPC7110 —
//! the cartridge parser names those (`luna_cartridge::UnsupportedChip`) and
//! `Snes::try_from_cartridge` refuses them instead of booting them bare.

pub mod dsp1;
pub mod sa1;

pub use dsp1::Dsp1Mapper;
pub use sa1::Sa1Chip;
