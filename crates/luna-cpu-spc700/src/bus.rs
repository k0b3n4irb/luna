//! The [`SpcBus`] trait — the view the SPC700 CPU has of the system.
//!
//! The SPC700 has a flat 16-bit address space, with 64 KB ARAM
//! occupying most of it. The "interesting" parts are:
//!
//! - `$F0-$FF` — DSP index/data, mailboxes, timers, control register.
//!   Implementations route these to the matching subsystem state.
//! - The rest is plain RAM.
//!
//! Unlike the 65C816 `Bus`, the SPC700 bus does **not** model cycle
//! costs explicitly — the SPC700 is a fixed 1.024 MHz CPU with a
//! simple per-opcode cycle table that we can apply post-execute.

/// View of the system exposed to the SPC700.
///
/// # Per-cycle clocking (the ares/Mesen2 grammar)
///
/// On real hardware the SPC700 clocks the S-DSP **and** the three timers
/// on **every** cycle — each memory access *and* each internal idle
/// cycle — in position (ares `smp/memory.cpp`→`wait()`; Mesen2
/// `Spc::IncCycleCount`). So a faithful consumer steps its timer/DSP
/// state one cycle per [`read`](Self::read) / [`write`](Self::write) /
/// [`idle`](Self::idle) call. Opcode implementations therefore call
/// [`idle`](Self::idle) at each internal cycle (matching ares'
/// `component/processor/spc700/instructions.cpp`), so the bus sees the
/// exact hardware cycle sequence rather than just a post-hoc total.
pub trait SpcBus {
    /// Read one byte at the 16-bit address (one bus cycle).
    fn read(&mut self, addr: u16) -> u8;

    /// Write one byte at the 16-bit address (one bus cycle).
    fn write(&mut self, addr: u16, value: u8);

    /// An internal/idle cycle: no memory access, but the SPC still
    /// burns a cycle and clocks the DSP + timers. Default is a no-op so
    /// flat-RAM consumers (tests) that don't model timing need not care;
    /// the timing-accurate consumer ([`luna_apu`]'s bus view) overrides
    /// it to clock one cycle.
    fn idle(&mut self) {}
}
