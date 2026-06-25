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

    /// Per-byte cooperative tick — called by the DMA channel after
    /// every transferred byte so cartridge coprocessors (SA-1, Super
    /// FX, DSP-N, …) get a chance to run *during* the burst instead
    /// of being frozen until the main CPU's instruction step
    /// completes ~4 kHz of mclks later.
    ///
    /// Matches ares (`coprocessor/sa1/sa1.cpp:63-94` — `Thread::step(2) + Thread::synchronize(cpu)` interleaved by the cooperative scheduler) and Mesen2 (`SnesDmaController::CopyDmaByte` → `IncMasterClock4` → `_memoryManager->Exec()` → `cart->SyncCoprocessors()` → `Sa1::Run()` every 2 mclks during DMA).
    ///
    /// Default `no-op` for the test mock buses; production buses
    /// override to forward to the coprocessor's `step_coproc`.
    fn tick(&mut self, _mcycles: u32) {}

    /// Set the DMA channel (0-7) currently driving the bus. The controller
    /// calls this before each channel's transfer so the bus view can tag
    /// captured B-bus writes with their source channel — mirrors Mesen2's
    /// `dma->GetActiveChannel()` (`SnesEventManager.cpp:40-42`). Default
    /// no-op for the test mock buses.
    fn set_active_channel(&mut self, _channel: u8) {}
}
