//! SNES DMA + HDMA controllers.
//!
//! Synchronous (general-purpose) DMA and HDMA, ported from ares
//! `cpu/dma.cpp`. Eight channels, all 8 transfer modes, A→B and B→A
//! directions, with the A-bus increment / decrement / fixed behaviours;
//! HDMA direct and indirect tables (`hdma_init` at frame start,
//! `hdma_run_line` per scanline).
//!
//! Bus abstraction: the DMA logic is decoupled from `luna-core`'s
//! `SnesBus` via the [`DmaBus`] trait, which exposes the minimum
//! primitives DMA needs (`read_a` / `write_a` on the 24-bit CPU bus,
//! `read_b` / `write_b` on the PPU's 8-bit `$2100 + offset` bus). The
//! production bus view is `DmaBusView` in `snes.rs`; this module is
//! testable in isolation with mock buses.
//!
//! See `ARCHITECTURE.md` §6.4.

mod bus;
mod channel;
mod controller;

pub use bus::DmaBus;
pub use channel::{Direction, DmaChannel, DmaParams, Increment, TransferMode};
pub(crate) use controller::HDMA_CHANNEL_FLAG;
pub use controller::{Dma, DmaTraceEvent, DmaTraceLog};
