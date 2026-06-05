//! SNES DMA + HDMA controllers.
//!
//! P1.2 scope: synchronous DMA only (HDMA in a later phase). Eight
//! channels, all 8 transfer modes, A→B and B→A directions, with the
//! A-bus increment / decrement / fixed behaviours.
//!
//! Bus abstraction: the DMA logic is decoupled from `luna-core`'s
//! `SnesBus` via the [`DmaBus`] trait, which exposes the minimum
//! primitives DMA needs (`read_a` / `write_a` on the 24-bit CPU bus,
//! `read_b` / `write_b` on the PPU's 8-bit `$2100 + offset` bus). The
//! production wiring lives in `luna-core`; this crate is testable in
//! isolation with mock buses.
//!
//! See `ARCHITECTURE.md` §6.4.

mod bus;
mod channel;
mod controller;

pub use bus::DmaBus;
pub use channel::{Direction, DmaChannel, DmaParams, Increment, TransferMode};
pub use controller::{Dma, DmaTraceEvent, DmaTraceLog};
