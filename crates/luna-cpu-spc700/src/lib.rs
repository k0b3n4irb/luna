//! SPC700 CPU — the SNES audio coprocessor.
//!
//! Runs at 1.024 MHz (24/24 ratio from the SNES master clock).
//! Communicates with the main 65C816 through four mailbox ports
//! exposed at `$2140-$2143` on the main bus and `$F4-$F7` on the SPC
//! side. The DSP audio synth lives at `$F2-$F3` (index+data port).
//!
//! Implementation follows the same TDD pattern as `luna-cpu-65c816`:
//! a [`Spc700`] CPU struct + a [`SpcBus`] trait that downstream code
//! (in `luna-apu`) implements to plug in ARAM + the DSP + timers +
//! mailboxes.
//!
//! See `ARCHITECTURE.md` §6.3.

pub mod bus;
pub mod cpu;
pub mod flags;
pub mod iplrom;
pub mod opcodes;

#[cfg(any(test, feature = "test-utils"))]
pub mod testing;

pub use bus::SpcBus;
pub use cpu::Spc700;
pub use flags::Psw;
pub use iplrom::{IPL_ROM, IPL_ROM_BASE};
