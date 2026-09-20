//! The [`Bus`] and [`BusDevice`] traits.

use crate::types::{Addr24, MCycles};

/// What [`Bus::last_cycle`] reports to the CPU: the state of the
/// interrupt lines as of the instruction's penultimate cycle.
///
/// `irq` is already filtered by the `I` mask, so the CPU only latches and
/// later consumes it — it never re-decides at the instruction boundary.
/// `wake` is "a transition was seen at all": ares' `nmiTest()`/`irqTest()`
/// clear `r.wai` before the `I` check, so a masked IRQ still ends a `WAI`
/// without entering the handler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InterruptSample {
    /// An NMI transition was consumed — service it at the next boundary.
    pub nmi: bool,
    /// An IRQ was seen **and** the `I` mask allows it.
    pub irq: bool,
    /// A transition was seen, masked or not: ends `WAI`.
    pub wake: bool,
}

impl InterruptSample {
    /// Nothing pending — what a bus with no interrupt sources reports.
    pub const NONE: Self = Self {
        nmi: false,
        irq: false,
        wake: false,
    };
}

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

    /// The interrupt sample the CPU takes **one cycle before an
    /// instruction's final bus access** — ares `CPU::lastCycle()`
    /// (`sfc/cpu/irq.cpp:88-93`), marked in the instruction tables by the
    /// `L` prefix (`#define L lastCycle();`, `registers.hpp:30`).
    ///
    /// Neither reference interrupts mid-instruction: both service at the
    /// instruction boundary. What this hook fixes is *when the decision is
    /// taken*. Sampling at the boundary instead lets an interrupt that
    /// arrives during the final access in — up to one whole instruction
    /// early — and lets `CLI` unmask its own pending IRQ. Mesen2 reaches
    /// the same result by recomputing `PrevIrqSource` on every cycle and
    /// reading it back at the boundary (`SnesCpu.Shared.h:336-338`).
    ///
    /// `i_flag` is the `I` mask **as it stands at the poll**, which is what
    /// makes the one-instruction `CLI`/`SEI` delay fall out for free (ares
    /// `irqTest()` returns `!r.p.i`). Implementations consume their edge
    /// latches here, exactly as `nmiTest()` / `irqTest()` do.
    ///
    /// The default samples nothing, which keeps standalone consumers of the
    /// CPU cores (the Tom Harte harness, unit-test buses) interrupt-free.
    fn last_cycle(&mut self, _i_flag: bool) -> InterruptSample {
        InterruptSample::NONE
    }
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
