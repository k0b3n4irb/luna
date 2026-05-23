//! SNES DMA + HDMA controllers.
//!
//! HDMA is required for virtually every SNES visual effect (parallax,
//! dynamic Mode 7, scanline color math) — incorrect emulation breaks
//! Final Fantasy VI, Chrono Trigger and most cinematic games.
//!
//! See `ARCHITECTURE.md` §6.4.
