//! SNES memory map, cartridge mappers and the `Bus` trait.
//!
//! The `Bus` trait exposes `io_cycle()` — the key primitive that makes
//! mid-instruction PPU/HDMA catch-up possible (and thus correct Mario Kart,
//! F-Zero, and every other HDMA-heavy SNES game).
//!
//! See `ARCHITECTURE.md` §5.
