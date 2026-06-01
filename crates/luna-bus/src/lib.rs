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
//! - [`mod@testing`]: [`testing::RamBus`] — a flat-RAM `Bus` for unit
//!   tests in downstream crates (gated behind the `test-utils` feature
//!   or `#[cfg(test)]`).
//!
//! See `ARCHITECTURE.md` §5.

pub mod bus;
pub mod hirom;
pub mod lorom;
pub mod mapper;
pub mod sa1;
pub mod speed;
pub mod types;

#[cfg(any(test, feature = "test-utils"))]
pub mod testing;

pub use bus::{Bus, BusDevice};
pub use mapper::{Mapper, MapperKind, Sa1SideEvent, Sa1Snapshot};
pub use speed::{MemorySpeed, address_speed};
pub use types::{Addr24, MCycles, bank_of, make_addr, offset_of};
