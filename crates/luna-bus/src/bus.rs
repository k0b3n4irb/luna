//! The [`Bus`] and [`BusDevice`] traits.

use crate::types::{Addr24, MCycles};

/// View of the SNES system exposed to the main CPU during one of its ticks.
///
/// Mid-instruction PPU/HDMA accuracy comes from [`Bus::io_cycle`]: every
/// byte access — and every internal CPU cycle that doesn't touch the bus —
/// pays its master-cycle cost through this method, which gives the bus the
/// opportunity to immediately catch up the PPU and other subsystems.
///
/// See `ARCHITECTURE.md` §5 and §6.6.
pub trait Bus {
    /// Read one byte at a 24-bit address.
    ///
    /// Implementations MUST call [`Bus::io_cycle`] internally with the
    /// access cost (see [`crate::address_speed`]).
    fn read(&mut self, addr: Addr24) -> u8;

    /// Write one byte at a 24-bit address.
    ///
    /// Implementations MUST call [`Bus::io_cycle`] internally with the
    /// access cost.
    fn write(&mut self, addr: Addr24, value: u8);

    /// Pay `mcycles` master cycles of bus time.
    ///
    /// This is the **key primitive for mid-instruction accuracy**. It is
    /// called by [`Bus::read`] / [`Bus::write`] with the access cost, and
    /// can also be called directly by the CPU for internal cycles that
    /// do not touch the bus (e.g. branch penalty, page-cross penalty).
    ///
    /// The implementation typically advances the PPU, HDMA controllers,
    /// and re-evaluates the IRQ / NMI lines.
    fn io_cycle(&mut self, mcycles: MCycles);

    /// Returns whether an NMI is currently latched as pending.
    fn nmi_pending(&self) -> bool;

    /// Returns whether an IRQ is currently asserted (and unmasked).
    fn irq_pending(&self) -> bool;
}

/// A component that responds to a memory-mapped region (PPU, DMA, APU
/// ports, etc.).
///
/// Unlike [`Bus`], a `BusDevice` does not pay its own access cost — that's
/// the parent bus's job. It just reads / writes its own state.
pub trait BusDevice {
    /// Read one byte from the device.
    fn read(&mut self, addr: Addr24) -> u8;

    /// Write one byte to the device.
    fn write(&mut self, addr: Addr24, value: u8);
}
