//! SNES Picture Processing Unit.
//!
//! VRAM (64 KB), CGRAM (512 B) and OAM (544 B), the `$21xx` register
//! file with its auto-increment ports, the scanline renderer (BG modes
//! 0-7, hi-res, Mode 7, OBJ, interlace) and the compositor (priorities,
//! windows, colour math).
//!
//! Reference: <https://problemkaputt.de/fullsnes.htm> §"PPU Registers".
//!
//! See `ARCHITECTURE.md` §6.2.

mod memory;
mod ppu;
mod renderer;
mod tile;

pub use memory::{Cgram, Oam, Vram};
pub use ppu::{BgState, Ppu, register};
pub use renderer::{
    FRAME_H, FRAME_H_MAX, FRAME_W, RenderOptions, Scanline, SpriteEntry, TilemapImage, bg_bpp,
    decode_all_sprites, render_bg_tilemap, render_cgram_palette, render_frame_bg_with,
    render_frame_with, render_obj_sheet, render_vram_tiles,
};
