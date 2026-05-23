//! The [`DmaBus`] trait — the minimum view of the system DMA needs.
//!
//! Splitting A-bus and B-bus access this way matches the hardware
//! topology: DMA shuffles bytes between two physical buses, the CPU's
//! 24-bit one (A) and the PPU's 8-bit `$2100 + offset` one (B). It also
//! lets the production [`luna_core::Snes`] do direct WRAM ↔ PPU memory
//! access without recursive [`luna_bus::Bus`] traffic (which would
//! otherwise re-enter the dispatch and re-tick `io_cycle`).

use luna_bus::Addr24;

/// View of the system exposed to a DMA channel during a transfer.
pub trait DmaBus {
    /// Read one byte from the CPU's 24-bit A-bus.
    fn read_a(&mut self, addr: Addr24) -> u8;

    /// Write one byte to the CPU's 24-bit A-bus.
    fn write_a(&mut self, addr: Addr24, value: u8);

    /// Read one byte from the PPU's B-bus at `$2100 + b_offset`.
    fn read_b(&mut self, b_offset: u8) -> u8;

    /// Write one byte to the PPU's B-bus at `$2100 + b_offset`.
    fn write_b(&mut self, b_offset: u8, value: u8);
}
