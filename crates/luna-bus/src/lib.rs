//! SNES memory map, cartridge mappers and the [`Bus`] trait.
//!
//! The [`Bus`] trait exposes [`Bus::io_cycle`] — the key primitive that
//! makes mid-instruction PPU/HDMA catch-up possible (and thus correct
//! Mario Kart, F-Zero, and every other HDMA-heavy SNES game).
//!
//! # Crate layout
//!
//! - [`mod@types`]: time / address aliases (`MCycles`, address helpers).
//! - [`mod@speed`]: SNES memory access speed lookup
//!   (`FAST` / `SLOW` / `XSLOW`).
//! - [`mod@bus`]: [`Bus`] and [`BusDevice`] traits.
//! - [`mod@mapper`]: [`Mapper`] trait for cartridge mappings.
//! - [`mod@lorom`]: [`lorom::LoRomMapper`] — Mode 20 cartridge mapping.
//! - [`mod@hirom`]: [`hirom::HiRomMapper`] — Mode 21 `HiROM` and Mode 25
//!   `ExHiROM` cartridge mapping.
//! - [`mod@sa1`]: [`sa1::Sa1Mapper`] — SA-1 shared cartridge memory + MMIO
//!   register file (the SA-1's own 65C816 is driven from `luna-core`).
//! - [`mod@superfx`]: [`superfx::SuperFxMapper`] — Super FX / GSU memory
//!   map **and** the GSU core itself.
//! - [`mod@sdd1`]: [`sdd1::Sdd1Mapper`] + [`sdd1::Sdd1Decompressor`] —
//!   S-DD1 graphics-decompression chip.
//! - [`mod@testing`]: [`testing::RamBus`] — a flat-RAM `Bus` for unit
//!   tests in downstream crates (gated behind the `test-utils` feature
//!   or `#[cfg(test)]`).
//!
//! The DSP-1 mapper is the exception: it needs the `luna-cpu-upd96050`
//! core, so it lives in `luna-core` (`coproc::dsp1`), not here.
//!
//! See `ARCHITECTURE.md` §5.

pub mod bus;
pub mod hirom;
pub mod lorom;
pub mod mapper;
pub mod sa1;
pub mod sdd1;
pub mod speed;
pub mod superfx;
pub mod types;

#[cfg(any(test, feature = "test-utils"))]
pub mod testing;

pub use bus::{Bus, BusDevice, InterruptSample};
pub use mapper::{
    Dsp1Snapshot, Dsp1TraceEvent, Dsp1TraceKind, Mapper, MapperKind, MapperStateError, NullMapper,
    Sa1SideEvent, Sa1Snapshot, Sa1TraceEvent, SuperFxJob, SuperFxTraceEvent, check_state_len,
    decode_state,
};
pub use speed::{MemorySpeed, address_speed};
pub use types::{Addr24, MCycles, bank_of, make_addr, offset_of};
