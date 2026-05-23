//! SNES ROM parsing.
//!
//! Detects the internal header (offset `0x7FC0` or `0xFFC0` depending on
//! mapping mode), infers the cartridge mapper (LoROM / HiROM / ExHiROM /
//! SA-1 / SuperFX / S-DD1 / SPC7110), and exposes the SRAM size.
//!
//! See `ARCHITECTURE.md` §5.
