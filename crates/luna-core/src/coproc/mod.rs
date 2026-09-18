//! SNES cartridge coprocessors.
//!
//! The coprocessor halves that need a CPU core `luna-bus` cannot depend
//! on: the SA-1's chip side ([`Sa1Chip`] — its bus-side shim is
//! `luna_bus::sa1::Sa1Mapper`) and the whole DSP-1 mapper ([`Dsp1Mapper`]).
//! Super FX and S-DD1 live entirely in `luna-bus`. Emulated today: SA-1,
//! Super FX, DSP-1, S-DD1. Not yet: DSP-2/3/4, Cx4, OBC1, S-RTC, ST-01x, SPC7110 —
//! the cartridge parser names those (`luna_cartridge::UnsupportedChip`) and
//! `Snes::try_from_cartridge` refuses them instead of booting them bare.

pub mod dsp1;
pub mod sa1;

pub use dsp1::Dsp1Mapper;
pub use sa1::Sa1Chip;
