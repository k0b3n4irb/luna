//! SPC700 CPU — the SNES audio coprocessor.
//!
//! Runs at ~3.072 MHz independently from the main 65C816, communicates
//! via four mailbox ports ($2140–$2143).
//!
//! See `ARCHITECTURE.md` §6.3.
