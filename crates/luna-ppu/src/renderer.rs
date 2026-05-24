//! Scanline-based PPU renderer.
//!
//! P1.4b scope: BG1 only, Mode 0 (2bpp), at the layer's current
//! H/V scroll. Higher BGs and the other modes land in P1.4c+.
//!
//! Output: one row of `[u8; 3]` (RGB888) per call to [`render_bg1_scanline`].
//! The renderer is a free function that takes a `&Ppu` so it doesn't
//! depend on the PPU's internal mutability.

use crate::ppu::{Ppu, bg_state};
use crate::tile::{apply_brightness, bgr555_to_rgb888, decode_2bpp_row, decode_4bpp_row};

/// One scanline of pixels (256 wide, RGB888).
pub type Scanline = [[u8; 3]; 256];

/// One frame at SNES native resolution — pixel width.
pub const FRAME_W: usize = 256;
/// One frame at SNES native resolution — visible scanline count
/// (NTSC: 224 lines; PAL adds 15 more, modelled later).
pub const FRAME_H: usize = 224;

/// Renderer options — feature-flag-style switches for debugging.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions {
    /// When `true`, the renderer ignores `INIDISP` bit 7 (forced blank)
    /// and forces full master brightness ($0F). Used by the GUI's
    /// **Force display** debug toggle so the user can peek at VRAM/CGRAM
    /// state even when a game keeps the screen blanked during boot.
    pub bypass_forced_blank: bool,
}

/// Render one scanline of BG1 in Mode 0 (2bpp).
///
/// Pixels are produced by:
/// 1. adding the BG1 H/V scroll to `(x, y)`
/// 2. looking up the tilemap entry at the resulting (tile-row, tile-col)
/// 3. decoding the 2bpp tile row addressed by the entry
/// 4. mapping the 0..=3 palette index through the entry's palette
///    offset into CGRAM and converting BGR555 → RGB888
///
/// Color index 0 in any BG palette is **transparent**; this rendering
/// pass replaces it with CGRAM index 0 (the backdrop / "color 0"
/// global), which matches the behaviour of a single-BG composite.
#[must_use]
pub fn render_bg1_scanline(ppu: &Ppu, y: u16) -> Scanline {
    render_bg1_scanline_with(ppu, y, RenderOptions::default())
}

/// BG1's bit-depth derived from `BGMODE` (the low 3 bits select the
/// PPU mode). Returns 2, 4 or 8 — matching the SNES per-mode mapping
/// from <https://problemkaputt.de/fullsnes.htm> §"PPU Background
/// Modes":
///
/// | Mode | BG1 | BG2 | BG3 | BG4 |
/// |:----:|----:|----:|----:|----:|
/// |  0   | 2   | 2   | 2   | 2   |
/// |  1   | 4   | 4   | 2   | —   |
/// |  2   | 4   | 4   | —   | —   |
/// |  3   | 8   | 4   | —   | —   |
/// |  4   | 8   | 2   | —   | —   |
/// |  5   | 4   | 2   | —   | —   |
/// |  6   | 4   | —   | —   | —   |
/// |  7   | 8   | —   | —   | —   |
///
/// Modes 5/6 are high-res (512px); Mode 7 is affine. We render either
/// the way Mode 1/2/3 would (planar tiles + tilemap) for now.
#[must_use]
pub fn bg1_bpp(bgmode: u8) -> u8 {
    match bgmode & 0x07 {
        0 => 2,
        1 | 2 | 5 | 6 => 4,
        3 | 4 | 7 => 8,
        _ => unreachable!(),
    }
}

/// Same as [`render_bg1_scanline`] but with debug options.
///
/// Honours `BGMODE` for BG1 bit-depth:
///
/// - **2bpp** (Mode 0): 16 bytes/tile, 4-color sub-palettes at
///   `CGRAM[palette_off * 4 + idx]`.
/// - **4bpp** (Mode 1/2/5/6): 32 bytes/tile, 16-color sub-palettes
///   at `CGRAM[palette_off * 16 + idx]`.
/// - **8bpp** (Mode 3/4/7): 64 bytes/tile, full 256-colour palette
///   indexed straight by `idx` (tilemap palette offset ignored).
///
/// Mode 7 affine is still handled the planar way for now — close
/// enough to show *something* until the real Mode-7 path lands.
#[must_use]
pub fn render_bg1_scanline_with(ppu: &Ppu, y: u16, opts: RenderOptions) -> Scanline {
    let mut out = [[0u8; 3]; 256];

    // INIDISP bit 7: forced blank — entire scanline is black, ignoring
    // master brightness (which is 0 in forced blank anyway per spec).
    if ppu.inidisp & 0x80 != 0 && !opts.bypass_forced_blank {
        return out;
    }
    let brightness = if opts.bypass_forced_blank {
        0x0F
    } else {
        ppu.inidisp & 0x0F
    };

    let bg = bg_state(ppu, 0);
    let bpp = bg1_bpp(ppu.bgmode);
    let bytes_per_tile = match bpp {
        2 => 16,
        4 => 32,
        8 => 64,
        _ => 16,
    };
    // 32x32 tilemap layout only for now. (SC bits in $2107 not modelled.)
    let tilemap_words = 32u16;
    // VRAM byte address of the tilemap base.
    let tilemap_base = (bg.tilemap_addr_words as usize) << 1;
    // VRAM byte address of the BG1 character base.
    let char_base = (bg.char_addr_words as usize) << 1;

    // Backdrop colour (CGRAM index 0).
    let backdrop = decode_palette(ppu, 0, brightness);

    for x in 0..256u16 {
        let src_x = x.wrapping_add(bg.h_scroll);
        let src_y = y.wrapping_add(bg.v_scroll);

        let tile_col = (src_x / 8) & 0x1F; // 32-tile wrap
        let tile_row = (src_y / 8) & 0x1F;
        let entry_off = tilemap_base + ((tile_row * tilemap_words + tile_col) as usize) * 2;
        let entry_lo = ppu.vram.peek(entry_off as u16);
        let entry_hi = ppu.vram.peek(entry_off.wrapping_add(1) as u16);
        let entry = u16::from(entry_lo) | (u16::from(entry_hi) << 8);

        let tile_num = entry & 0x03FF;
        let palette_off = ((entry >> 10) & 0x07) as u8;
        let h_flip = entry & 0x4000 != 0;
        let v_flip = entry & 0x8000 != 0;

        // Pixel within the tile (with flip).
        let mut row_in_tile = (src_y & 7) as usize;
        let mut col_in_tile = (src_x & 7) as usize;
        if v_flip {
            row_in_tile = 7 - row_in_tile;
        }
        if h_flip {
            col_in_tile = 7 - col_in_tile;
        }

        let tile_base = char_base + (tile_num as usize) * bytes_per_tile;
        let idx = decode_tile_pixel(ppu, tile_base, row_in_tile, col_in_tile, bpp);

        out[x as usize] = if idx == 0 {
            // Transparent — fall through to backdrop.
            backdrop
        } else {
            let cgram_idx = match bpp {
                // 2bpp: 4-color sub-palettes at CGRAM[palette_off*4].
                2 => palette_off * 4 + idx,
                // 4bpp: 16-color sub-palettes at CGRAM[palette_off*16].
                4 => palette_off.wrapping_mul(16).wrapping_add(idx),
                // 8bpp: full 256-color palette; palette offset is
                // ignored by hardware.
                _ => idx,
            };
            decode_palette(ppu, cgram_idx, brightness)
        };
    }
    out
}

/// Decode a single pixel's palette index out of a planar tile in VRAM.
///
/// SNES tile layout is *planar bitplane-pair* in VRAM:
///
/// - 2bpp: rows 0..7 each have planes (0, 1) packed as two bytes →
///   16 bytes per tile, addressed `tile_base + row*2`.
/// - 4bpp: planes (0, 1) for rows 0..7 fill the first 16 bytes; planes
///   (2, 3) for rows 0..7 fill the next 16 bytes. So a row's four
///   plane bytes live at `tile_base + row*2`, `+1`, `+16+row*2`, `+17`.
/// - 8bpp: same as 4bpp but with planes (4, 5, 6, 7) in the next 32
///   bytes.
fn decode_tile_pixel(
    ppu: &Ppu,
    tile_base: usize,
    row_in_tile: usize,
    col_in_tile: usize,
    bpp: u8,
) -> u8 {
    let row_off = tile_base + row_in_tile * 2;
    let p0 = ppu.vram.peek(row_off as u16);
    let p1 = ppu.vram.peek(row_off.wrapping_add(1) as u16);
    if bpp == 2 {
        return decode_2bpp_row(p0, p1)[col_in_tile];
    }
    let p2 = ppu.vram.peek(row_off.wrapping_add(16) as u16);
    let p3 = ppu.vram.peek(row_off.wrapping_add(17) as u16);
    if bpp == 4 {
        return decode_4bpp_row(p0, p1, p2, p3)[col_in_tile];
    }
    // 8bpp: fold in the upper four planes too.
    let p4 = ppu.vram.peek(row_off.wrapping_add(32) as u16);
    let p5 = ppu.vram.peek(row_off.wrapping_add(33) as u16);
    let p6 = ppu.vram.peek(row_off.wrapping_add(48) as u16);
    let p7 = ppu.vram.peek(row_off.wrapping_add(49) as u16);
    let bit = 7 - col_in_tile;
    let mask = 1u8 << bit;
    let lo = decode_4bpp_row(p0, p1, p2, p3)[col_in_tile];
    let hi_p4 = (p4 & mask) >> bit;
    let hi_p5 = (p5 & mask) >> bit;
    let hi_p6 = (p6 & mask) >> bit;
    let hi_p7 = (p7 & mask) >> bit;
    lo | (hi_p4 << 4) | (hi_p5 << 5) | (hi_p6 << 6) | (hi_p7 << 7)
}

/// Sprite-table decoded entry — what we need to draw one sprite.
#[derive(Debug, Clone, Copy)]
pub struct SpriteEntry {
    /// Signed X position (-256..=255 after high-table sign extension).
    pub x: i16,
    /// 8-bit Y position (compared to scanline modulo 256).
    pub y: u8,
    /// 9-bit tile number (low byte from low table, bit 8 from attrs).
    pub tile: u16,
    /// 3-bit palette index (sprites use the upper half of CGRAM, so
    /// final CGRAM index = `128 + palette * 16 + pixel_idx`).
    pub palette: u8,
    /// 2-bit priority (0-3). Compared against BG priorities.
    pub priority: u8,
    /// Horizontal flip flag.
    pub h_flip: bool,
    /// Vertical flip flag.
    pub v_flip: bool,
    /// Sprite width in pixels (from OBSEL size pair).
    pub w: u16,
    /// Sprite height in pixels (from OBSEL size pair).
    pub h: u16,
}

/// Sprite size pair selected by `OBSEL.bits[7:5]` — small vs large.
#[must_use]
pub fn sprite_size_pair(obsel: u8) -> ((u16, u16), (u16, u16)) {
    match (obsel >> 5) & 0x07 {
        0 => ((8, 8), (16, 16)),
        1 => ((8, 8), (32, 32)),
        2 => ((8, 8), (64, 64)),
        3 => ((16, 16), (32, 32)),
        4 => ((16, 16), (64, 64)),
        5 => ((32, 32), (64, 64)),
        6 | 7 => ((16, 32), (32, 64)),
        _ => ((8, 8), (16, 16)),
    }
}

/// Decode all 128 OAM entries into [`SpriteEntry`].
#[must_use]
pub fn decode_all_sprites(ppu: &Ppu) -> [SpriteEntry; 128] {
    let (small, large) = sprite_size_pair(ppu.obsel);
    let mut out = [SpriteEntry {
        x: 0,
        y: 0,
        tile: 0,
        palette: 0,
        priority: 0,
        h_flip: false,
        v_flip: false,
        w: 8,
        h: 8,
    }; 128];
    for (idx, slot) in out.iter_mut().enumerate() {
        let base = (idx * 4) as u16;
        let x_low = ppu.oam.peek(base);
        let y_pos = ppu.oam.peek(base + 1);
        let tile_low = ppu.oam.peek(base + 2);
        let attrs = ppu.oam.peek(base + 3);
        // High table: 32 bytes at OAM[$200..$220], 2 bits per sprite.
        let high_byte_idx = 0x200 + (idx / 4);
        let high_bit_off = (idx % 4) * 2;
        let high = (ppu.oam.peek(high_byte_idx as u16) >> high_bit_off) & 0x03;
        let x_high_bit = (high & 0x01) as i16;
        let is_large = (high & 0x02) != 0;
        // 9-bit X with sign extension: bit 8 set => x is negative.
        let mut x = (x_high_bit << 8) | (x_low as i16);
        if x >= 256 {
            x -= 512;
        }
        let (w, h) = if is_large { large } else { small };
        *slot = SpriteEntry {
            x,
            y: y_pos,
            tile: (tile_low as u16) | ((attrs as u16 & 0x01) << 8),
            palette: (attrs >> 1) & 0x07,
            priority: (attrs >> 4) & 0x03,
            h_flip: attrs & 0x40 != 0,
            v_flip: attrs & 0x80 != 0,
            w,
            h,
        };
    }
    out
}

/// Render one scanline of sprite output.
///
/// Returns, for each of the 256 visible columns, either `Some(rgb)`
/// when a non-transparent sprite covers that pixel, or `None` when
/// no sprite contributes. The caller composites this on top of the
/// BG layers.
///
/// Per-sprite priority bits *are* decoded but not yet used to slot
/// sprites between BG layers — that lands once the per-pixel priority
/// engine is in place. For now the simple "sprites on top of all
/// BGs" rule is enough for title-screen Mario/Yoshi visibility.
#[must_use]
pub fn render_sprites_scanline(ppu: &Ppu, y: u16, opts: RenderOptions) -> [Option<[u8; 3]>; 256] {
    let mut out: [Option<[u8; 3]>; 256] = [None; 256];
    if ppu.inidisp & 0x80 != 0 && !opts.bypass_forced_blank {
        return out;
    }
    let brightness = if opts.bypass_forced_blank {
        0x0F
    } else {
        ppu.inidisp & 0x0F
    };
    // OBSEL bits 0-2: name select base in 8K-byte chunks (= 16 KB
    // word steps). 0..7 → byte base 0 / $2000 / ... / $E000.
    let sprite_tile_base_bytes = ((ppu.obsel as usize) & 0x07) << 13;
    // Iterate sprites highest-OAM-index first so lower indices win
    // (real PPU draws sprite 0 last; we emulate by drawing 127 first
    // and overwriting with lower indices).
    let sprites = decode_all_sprites(ppu);
    for sp in sprites.iter().rev() {
        // Does this scanline cross the sprite?
        let row_in_sprite = y.wrapping_sub(sp.y as u16);
        if usize::from(row_in_sprite) >= sp.h as usize {
            continue;
        }
        for col in 0..sp.w {
            let screen_x = sp.x + col as i16;
            if !(0..256).contains(&screen_x) {
                continue;
            }
            // Apply flip to find tile-local coordinates.
            let mut sc = col as usize;
            let mut sr = row_in_sprite as usize;
            if sp.h_flip {
                sc = (sp.w - 1) as usize - sc;
            }
            if sp.v_flip {
                sr = (sp.h - 1) as usize - sr;
            }
            // SNES sprites use a 16-tile-wide "sprite plane" — each 8×8
            // tile-block of a big sprite is at `(base_tile + row*16 + col)`
            // with col/row in tile units within the sprite, all mod 256.
            let tile_x = sc / 8;
            let tile_y = sr / 8;
            let pix_x = sc % 8;
            let pix_y = sr % 8;
            let tile_id = (sp.tile.wrapping_add(((tile_y * 16) + tile_x) as u16)) & 0x01FF;
            let tile_off = sprite_tile_base_bytes + (tile_id as usize) * 32;
            let idx = decode_tile_pixel(ppu, tile_off, pix_y, pix_x, 4);
            if idx == 0 {
                continue; // transparent
            }
            // Sprite palette: upper half of CGRAM.
            let cgram_idx = 128u16 + (sp.palette as u16) * 16 + (idx as u16);
            let rgb = decode_palette(ppu, cgram_idx as u8, brightness);
            out[screen_x as usize] = Some(rgb);
        }
    }
    out
}

/// Render the full visible frame for BG1-only Mode 0.
///
/// 224 scanlines (NTSC native). For 239-line PAL we'll extend later.
#[must_use]
pub fn render_frame_bg1(ppu: &Ppu) -> Vec<[u8; 3]> {
    render_frame_bg1_with(ppu, RenderOptions::default())
}

/// Same as [`render_frame_bg1`] but with debug options.
#[must_use]
pub fn render_frame_bg1_with(ppu: &Ppu, opts: RenderOptions) -> Vec<[u8; 3]> {
    render_frame_with(ppu, opts)
}

/// Render the full visible frame composited from BG3 (top) over BG2
/// over BG1 over backdrop. This is the right priority order for the
/// common Mode 1 title-screen pattern (BG3 = text overlay, BG2 =
/// background, BG1 = (sprite-substitute) foreground) and is good
/// enough to display Super Mario World's, Tetris 2's and similar
/// title screens cleanly without a full per-pixel priority engine.
///
/// Internally we render each layer's scanline, then for each pixel
/// take the top-most non-backdrop value.
#[must_use]
pub fn render_frame_with(ppu: &Ppu, opts: RenderOptions) -> Vec<[u8; 3]> {
    let mut buf = vec![[0u8; 3]; FRAME_W * FRAME_H];
    for y in 0..FRAME_H {
        let bg1 = render_bg_scanline_with(ppu, 0, y as u16, opts);
        let bg2 = render_bg_scanline_with(ppu, 1, y as u16, opts);
        let bg3 = render_bg_scanline_with(ppu, 2, y as u16, opts);
        let sprites = render_sprites_scanline(ppu, y as u16, opts);
        let backdrop = bg1[0]; // each scanline writes backdrop where idx==0
        let off = y * FRAME_W;
        for x in 0..FRAME_W {
            // Priority order (top to bottom): sprites → BG3 → BG1 → BG2 → backdrop.
            buf[off + x] = if let Some(rgb) = sprites[x] {
                rgb
            } else if bg3[x] != backdrop {
                bg3[x]
            } else if bg1[x] != backdrop {
                bg1[x]
            } else if bg2[x] != backdrop {
                bg2[x]
            } else {
                backdrop
            };
        }
    }
    buf
}

/// Render a specific BG layer (`bg_idx` = 0..=3 → BG1..=BG4) into a
/// full frame. Useful for debugging which layer of a multi-BG game
/// actually carries the visible content. Honours that BG's per-bgmode
/// bit depth, its scroll, tile-map and char-base addresses.
#[must_use]
pub fn render_frame_bg_with(ppu: &Ppu, bg_idx: usize, opts: RenderOptions) -> Vec<[u8; 3]> {
    let mut buf = vec![[0u8; 3]; FRAME_W * FRAME_H];
    for y in 0..FRAME_H {
        let line = render_bg_scanline_with(ppu, bg_idx, y as u16, opts);
        let off = y * FRAME_W;
        buf[off..off + FRAME_W].copy_from_slice(&line);
    }
    buf
}

/// Bits-per-pixel for any BG in any mode (cf. [`bg1_bpp`]).
#[must_use]
pub fn bg_bpp(bgmode: u8, bg_idx: usize) -> u8 {
    let m = bgmode & 0x07;
    match (m, bg_idx) {
        (0, _) => 2,
        (1, 0) | (1, 1) => 4,
        (1, 2) => 2,
        (2, 0) | (2, 1) => 4,
        (3, 0) => 8,
        (3, 1) => 4,
        (4, 0) => 8,
        (4, 1) => 2,
        (5, 0) => 4,
        (5, 1) => 2,
        (6, 0) => 4,
        (7, 0) => 8,
        _ => 0, // BG disabled in this mode
    }
}

/// Render one scanline for the requested BG layer.
#[must_use]
pub fn render_bg_scanline_with(ppu: &Ppu, bg_idx: usize, y: u16, opts: RenderOptions) -> Scanline {
    let mut out = [[0u8; 3]; 256];
    if ppu.inidisp & 0x80 != 0 && !opts.bypass_forced_blank {
        return out;
    }
    let brightness = if opts.bypass_forced_blank {
        0x0F
    } else {
        ppu.inidisp & 0x0F
    };
    let bpp = bg_bpp(ppu.bgmode, bg_idx);
    if bpp == 0 {
        // BG disabled in this mode → fill with backdrop.
        let backdrop = decode_palette(ppu, 0, brightness);
        for px in out.iter_mut() {
            *px = backdrop;
        }
        return out;
    }
    let bytes_per_tile = match bpp {
        2 => 16,
        4 => 32,
        8 => 64,
        _ => 16,
    };
    let bg = bg_state(ppu, bg_idx);
    let tilemap_base = (bg.tilemap_addr_words as usize) << 1;
    let char_base = (bg.char_addr_words as usize) << 1;
    let backdrop = decode_palette(ppu, 0, brightness);

    // SC bits 0-1 from BG*SC: 0 = 32×32, 1 = 64×32 (extra screen to
    // the right), 2 = 32×64 (extra screen below), 3 = 64×64 (extra
    // screens right + below + diagonal). Each "extra screen" is a
    // full 32×32 sub-tilemap stored at `base + N*0x800` bytes.
    let (cols, rows) = match bg.tilemap_size & 0x03 {
        0 => (32u16, 32u16),
        1 => (64u16, 32u16),
        2 => (32u16, 64u16),
        3 => (64u16, 64u16),
        _ => (32u16, 32u16),
    };

    for x in 0..256u16 {
        let src_x = x.wrapping_add(bg.h_scroll);
        let src_y = y.wrapping_add(bg.v_scroll);
        let tile_col_full = (src_x / 8) & (cols - 1);
        let tile_row_full = (src_y / 8) & (rows - 1);
        // Within which 32×32 sub-screen does this (col, row) live?
        let sub_x = (tile_col_full >> 5) as usize; // 0 or 1
        let sub_y = (tile_row_full >> 5) as usize; // 0 or 1
        let sub_index = match bg.tilemap_size & 0x03 {
            0 => 0,
            1 => sub_x,             // 64x32: right sub-screen offset
            2 => sub_y,             // 32x64: bottom sub-screen offset
            3 => sub_y * 2 + sub_x, // 64x64: TL/TR/BL/BR
            _ => 0,
        };
        let tile_col = tile_col_full & 0x1F;
        let tile_row = tile_row_full & 0x1F;
        let entry_off =
            tilemap_base + sub_index * 0x0800 + ((tile_row * 32 + tile_col) as usize) * 2;
        let entry_lo = ppu.vram.peek(entry_off as u16);
        let entry_hi = ppu.vram.peek(entry_off.wrapping_add(1) as u16);
        let entry = u16::from(entry_lo) | (u16::from(entry_hi) << 8);
        let tile_num = entry & 0x03FF;
        let palette_off = ((entry >> 10) & 0x07) as u8;
        let h_flip = entry & 0x4000 != 0;
        let v_flip = entry & 0x8000 != 0;
        let mut row_in_tile = (src_y & 7) as usize;
        let mut col_in_tile = (src_x & 7) as usize;
        if v_flip {
            row_in_tile = 7 - row_in_tile;
        }
        if h_flip {
            col_in_tile = 7 - col_in_tile;
        }
        let tile_base = char_base + (tile_num as usize) * bytes_per_tile;
        let idx = decode_tile_pixel(ppu, tile_base, row_in_tile, col_in_tile, bpp);

        out[x as usize] = if idx == 0 {
            backdrop
        } else {
            let cgram_idx = match bpp {
                2 => palette_off * 4 + idx,
                4 => palette_off.wrapping_mul(16).wrapping_add(idx),
                _ => idx,
            };
            decode_palette(ppu, cgram_idx, brightness)
        };
    }
    out
}

/// Look up a CGRAM index and apply master brightness.
fn decode_palette(ppu: &Ppu, cgram_index: u8, brightness: u8) -> [u8; 3] {
    let color = ppu.cgram.color(cgram_index);
    apply_brightness(bgr555_to_rgb888(color), brightness)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::register;

    /// Build a PPU that has a single 2bpp tile pre-seeded in VRAM,
    /// a known 4-entry palette in CGRAM, and the tilemap pointing at
    /// our test tile in the top-left corner.
    fn setup_demo_tile() -> Ppu {
        let mut p = Ppu::new();

        // Disable forced blank, full brightness.
        p.write(register::INIDISP, 0x0F);
        // BG1 tilemap at VRAM word $0000, character base at word $1000
        // (= byte $2000). The default after-reset values for BG1SC
        // and BG12NBA give exactly these, but we set them explicitly:
        p.write(0x07, 0x00); // BG1SC: 32x32, tilemap word base = 0
        p.write(0x0B, 0x01); // BG12NBA: BG1 char addr low nibble = 1
        //  → BG1 char base = 1 << 12 (words) = byte $2000
        // No scroll yet.
        // (Scroll registers $210D-$2114 land in P1.4c; default 0.)

        // Tile #0 lives at VRAM byte $2000-$200F.
        // Make it a checkerboard:
        //   row 0: pixels {1,2,3,1,2,3,1,2} — exercises all 4 indices.
        // 2bpp row 0: bits per pixel (left→right). lo holds bit-0,
        // hi holds bit-1.
        //   pix:  1 2 3 1 2 3 1 2
        //   bit0: 1 0 1 1 0 1 1 0  → lo = 0b1011_0110 = 0xB6
        //   bit1: 0 1 1 0 1 1 0 1  → hi = 0b0110_1101 = 0x6D
        p.vram.poke(0x2000, 0xB6);
        p.vram.poke(0x2001, 0x6D);
        // Rest of the tile (rows 1-7) stays zero → transparent.

        // Tilemap entry at byte $0000-$0001: tile 0, palette offset 0.
        p.vram.poke(0x0000, 0x00);
        p.vram.poke(0x0001, 0x00);

        // CGRAM: backdrop (idx 0) = magenta ($7C1F), color 1 = red,
        // color 2 = green, color 3 = blue.
        p.cgram.poke(0, 0x1F);
        p.cgram.poke(1, 0x7C); // 0x7C1F
        p.cgram.poke(2, 0x1F);
        p.cgram.poke(3, 0x00); // 0x001F red
        p.cgram.poke(4, 0xE0);
        p.cgram.poke(5, 0x03); // 0x03E0 green
        p.cgram.poke(6, 0x00);
        p.cgram.poke(7, 0x7C); // 0x7C00 blue
        p
    }

    #[test]
    fn forced_blank_returns_all_black() {
        let mut p = setup_demo_tile();
        p.write(register::INIDISP, 0x80); // forced blank
        let line = render_bg1_scanline(&p, 0);
        for px in &line {
            assert_eq!(*px, [0, 0, 0]);
        }
    }

    #[test]
    fn bypass_forced_blank_ignores_inidisp_bit_7() {
        let mut p = setup_demo_tile();
        p.write(register::INIDISP, 0x80); // forced blank ON
        let opts = RenderOptions {
            bypass_forced_blank: true,
        };
        let line = render_bg1_scanline_with(&p, 0, opts);
        // First pixel uses index 1 → red ($001F) at full brightness.
        assert_ne!(line[0], [0, 0, 0]);
    }

    #[test]
    fn bypass_forced_blank_forces_full_brightness() {
        let mut p = setup_demo_tile();
        // Forced blank ON, but ALSO brightness 0 (which would be black even
        // without bit 7). Bypass should override brightness too.
        p.write(register::INIDISP, 0x80);
        let opts = RenderOptions {
            bypass_forced_blank: true,
        };
        let line = render_bg1_scanline_with(&p, 0, opts);
        // Color 1 in our setup is $001F → red at brightness 15.
        // scale_5_to_8(0x1F) = 0xFF; brightness 15 keeps full value.
        assert_eq!(line[0], [0xFF, 0, 0]);
    }

    #[test]
    fn bg1_bpp_table_matches_snes_modes() {
        assert_eq!(bg1_bpp(0), 2);
        assert_eq!(bg1_bpp(1), 4);
        assert_eq!(bg1_bpp(2), 4);
        assert_eq!(bg1_bpp(3), 8);
        assert_eq!(bg1_bpp(4), 8);
        assert_eq!(bg1_bpp(5), 4);
        assert_eq!(bg1_bpp(6), 4);
        assert_eq!(bg1_bpp(7), 8);
        // Mode 1 with high bit set (BG3 priority) still mode 1 = 4bpp.
        assert_eq!(bg1_bpp(0x09), 4);
    }

    #[test]
    fn mode1_4bpp_decodes_correctly_with_16_color_palette() {
        // Set up a 4bpp BG1 with one tile that uses an index in the
        // upper 4-bit range (e.g. index 10), which can ONLY be reached
        // when planes 2-3 are non-zero. With a 2bpp decoder this pixel
        // would only see planes 0-1 and read the wrong value.
        let mut p = Ppu::new();
        p.write(register::INIDISP, 0x0F); // no forced blank, full bright
        p.write(register::BGMODE, 0x01); // Mode 1
        p.write(0x07, 0x00); // BG1SC: tilemap at word 0
        p.write(0x0B, 0x01); // BG12NBA: BG1 char base = word $1000 (= byte $2000)

        // 4bpp tile #0 at VRAM byte $2000-$201F (32 bytes).
        // Row 0, pixel 0 = palette index 10 = 0b1010.
        //   plane bits, left-to-right pixel 0:
        //     p0=0 (bit 0), p1=1 (bit 1), p2=0 (bit 2), p3=1 (bit 3)
        //   So the MSB of each plane byte is the matching bit value.
        // p0=0 → byte $00, p1=$80, p2=$00, p3=$80.
        p.vram.poke(0x2000, 0x00); // plane 0, row 0
        p.vram.poke(0x2001, 0x80); // plane 1, row 0
        p.vram.poke(0x2010, 0x00); // plane 2, row 0
        p.vram.poke(0x2011, 0x80); // plane 3, row 0

        // Tilemap entry at byte $0000-$0001: tile 0, palette offset 0.
        p.vram.poke(0x0000, 0x00);
        p.vram.poke(0x0001, 0x00);

        // CGRAM[10] = colour we expect to see ($7FFF = white).
        p.cgram.poke(20, 0xFF);
        p.cgram.poke(21, 0x7F);
        // CGRAM[0] = black (backdrop default).

        let line = render_bg1_scanline(&p, 0);
        // First pixel should be white (index 10 → CGRAM[10] = $7FFF).
        assert_eq!(line[0], [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn first_scanline_has_expected_palette_pattern() {
        let p = setup_demo_tile();
        let line = render_bg1_scanline(&p, 0);
        // Pixels 0..=7 of row 0 should match {1,2,3,1,2,3,1,2} mapped
        // through the palette → red, green, blue, red, green, blue, red, green.
        let red = [0xFF, 0, 0];
        let green = [0, 0xFF, 0];
        let blue = [0, 0, 0xFF];
        assert_eq!(line[0], red);
        assert_eq!(line[1], green);
        assert_eq!(line[2], blue);
        assert_eq!(line[3], red);
        assert_eq!(line[4], green);
        assert_eq!(line[5], blue);
        assert_eq!(line[6], red);
        assert_eq!(line[7], green);
    }

    #[test]
    fn rows_past_tile0_pull_from_other_tilemap_entries() {
        // Tile (0,1) — tilemap entry at byte $0040 (32 words × 2 bytes
        // = $0040). We leave it pointing at tile 0 with palette 0 so
        // row 0 of column 1 should still be tile 0 row 0.
        let p = setup_demo_tile();
        let line = render_bg1_scanline(&p, 0);
        // x=8 is the first pixel of column 1 → same as pixel 0 (red).
        assert_eq!(line[8], [0xFF, 0, 0]);
    }

    #[test]
    fn transparent_pixels_show_the_backdrop_color() {
        // Row 1 of tile 0 is all zero → all pixels transparent →
        // backdrop ($7C1F = magenta = R=31 B=31).
        let p = setup_demo_tile();
        let line = render_bg1_scanline(&p, 1);
        // Magenta in RGB: R=255, G=0, B=255.
        for px in &line[..256] {
            assert_eq!(*px, [0xFF, 0, 0xFF]);
        }
    }

    #[test]
    fn brightness_scales_the_output_linearly() {
        // Brightness 7 → half intensity. Color 3 (blue) = $7C00 → RGB
        // [0, 0, 255] → at brightness 7 → [0, 0, 127] (255 * 8 / 16).
        let mut p = setup_demo_tile();
        p.write(register::INIDISP, 0x07);
        let line = render_bg1_scanline(&p, 0);
        assert_eq!(line[2], [0, 0, 127]);
    }

    #[test]
    fn full_frame_returns_correct_pixel_count() {
        let p = setup_demo_tile();
        let frame = render_frame_bg1(&p);
        assert_eq!(frame.len(), FRAME_W * FRAME_H);
    }

    #[test]
    fn palette_offset_selects_subpalette() {
        // Point the tilemap entry's palette bits to palette 1
        // (offset = 1 in bits 10-12 of the entry), so color 1 → CGRAM
        // index 5 (1 × 4 + 1). Seed CGRAM[5] = $7C00 (blue). Row 0
        // pixel 0 should now be blue.
        let mut p = setup_demo_tile();
        // Set entry palette offset to 1 → entry word = (1 << 10) = $0400.
        p.vram.poke(0x0000, 0x00);
        p.vram.poke(0x0001, 0x04);
        // CGRAM index 5 — bytes $0A/$0B.
        p.cgram.poke(0x0A, 0x00);
        p.cgram.poke(0x0B, 0x7C);
        let line = render_bg1_scanline(&p, 0);
        assert_eq!(line[0], [0, 0, 255]);
    }

    #[test]
    fn sprite_size_pair_table() {
        assert_eq!(sprite_size_pair(0x00), ((8, 8), (16, 16)));
        assert_eq!(sprite_size_pair(0x20), ((8, 8), (32, 32)));
        assert_eq!(sprite_size_pair(0x60), ((16, 16), (32, 32)));
        assert_eq!(sprite_size_pair(0x80), ((16, 16), (64, 64)));
    }

    #[test]
    fn sprite_renders_at_known_position_with_known_palette() {
        // Seed OAM #0: x=16, y=20, tile=0, palette=0, small (8x8).
        let mut p = Ppu::new();
        p.write(register::INIDISP, 0x0F); // visible, full brightness
        // OBSEL = $00 → sprite tile base = $0000, sizes (8x8, 16x16).
        // Default small = 8x8.
        p.oam.poke(0, 16); // x.low
        p.oam.poke(1, 20); // y
        p.oam.poke(2, 0); // tile.low
        p.oam.poke(3, 0); // attrs: palette 0, priority 0, no flip
        // 4bpp tile #0 at VRAM byte $0000-$001F.
        // Pixel (0, 0): planes (0,1,2,3) bits = (1,0,0,0) → palette idx 1.
        p.vram.poke(0x0000, 0x80); // plane 0 row 0
        p.vram.poke(0x0001, 0x00); // plane 1 row 0
        p.vram.poke(0x0010, 0x00); // plane 2 row 0
        p.vram.poke(0x0011, 0x00); // plane 3 row 0
        // Sprite palette starts at CGRAM[128]. Palette offset 0,
        // index 1 → CGRAM[129] = $001F (red).
        p.cgram.poke(258, 0x1F);
        p.cgram.poke(259, 0x00);
        let line = render_sprites_scanline(&p, 20, RenderOptions::default());
        // At screen X=16 (sprite's x), pixel 0 should be red.
        assert_eq!(line[16], Some([0xFF, 0, 0]));
        // Pixels outside the sprite are None.
        assert_eq!(line[0], None);
    }

    #[test]
    fn hidden_sprite_at_y_240_does_not_appear_on_visible_lines() {
        // OAM #0 hidden at Y = 240, which is past the visible 224
        // scanlines — should produce nothing on any screen row. To
        // make this test isolating, we hide ALL 128 sprites at Y=240
        // (else the default zeroed OAM has 127 sprites at (0,0) with
        // tile 0, which would draw all over scanline 0).
        let mut p = Ppu::new();
        p.write(register::INIDISP, 0x0F);
        for i in 0..128u16 {
            p.oam.poke(i * 4 + 1, 240); // every sprite's Y = 240
        }
        p.oam.poke(0, 16); // sprite #0 keeps its x=16
        p.vram.poke(0x0000, 0xFF);
        p.cgram.poke(258, 0x1F);
        p.cgram.poke(259, 0x00);
        for y in 0..224u16 {
            let line = render_sprites_scanline(&p, y, RenderOptions::default());
            for (xi, px) in line.iter().enumerate() {
                assert!(px.is_none(), "y={y} x={xi} had visible sprite pixel");
            }
        }
    }
}
