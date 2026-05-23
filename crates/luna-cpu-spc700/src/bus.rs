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
pub trait SpcBus {
    /// Read one byte at the 16-bit address.
    fn read(&mut self, addr: u16) -> u8;

    /// Write one byte at the 16-bit address.
    fn write(&mut self, addr: u16, value: u8);
}
