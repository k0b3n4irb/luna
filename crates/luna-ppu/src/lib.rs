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
mod renderer;
mod tile;

pub use memory::{Cgram, Oam, Vram};
pub use ppu::{BgState, Ppu, bg_state, register};
pub use renderer::{
    FRAME_H, FRAME_W, IndexedPixel, IndexedScanline, RenderOptions, Scanline, SpriteEntry, bg_bpp,
    bg1_bpp, decode_all_sprites, render_bg_scanline_indexed_with, render_bg_scanline_with,
    render_bg1_scanline, render_bg1_scanline_with, render_frame_bg_with, render_frame_bg1,
    render_frame_bg1_with, render_frame_with, render_sprites_scanline,
    render_sprites_scanline_indexed_with, sprite_size_pair,
};
pub use tile::{
    apply_brightness, bgr555_to_rgb888, decode_2bpp_row, decode_4bpp_row, scale_5_to_8,
};
