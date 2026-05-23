//! SNES APU — orchestrates the `luna-cpu-spc700` core, the audio DSP and
//! the four mailbox ports facing the main CPU.
//!
//! Catch-up sync uses rational arithmetic on `u64` (no float drift). See
//! `ARCHITECTURE.md` §6.3 and §6.6.
