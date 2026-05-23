//! SNES Picture Processing Unit.
//!
//! P1.1 scope: data-flow plumbing only — VRAM (64 KB), CGRAM (512 B)
//! and OAM (544 B) with the corresponding MMIO registers and auto-
//! increment behaviour. No rendering yet (lands in P1.4+).
//!
//! Reference: <https://problemkaputt.de/fullsnes.htm> §"PPU Registers".
//!
//! See `ARCHITECTURE.md` §6.2.

mod memory;
mod ppu;
mod tile;

pub use memory::{Cgram, Oam, Vram};
pub use ppu::{Ppu, register};
pub use tile::{apply_brightness, bgr555_to_rgb888, decode_2bpp_row, decode_4bpp_row, scale_5_to_8};
