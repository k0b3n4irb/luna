//! Scanline-based PPU renderer.
//!
//! P1.4b scope: BG1 only, Mode 0 (2bpp), at the layer's current
//! H/V scroll. Higher BGs and the other modes land in P1.4c+.
//!
//! Output: one row of `[u8; 3]` (RGB888) per call to [`render_bg1_scanline`].
//! The renderer is a free function that takes a `&Ppu` so it doesn't
//! depend on the PPU's internal mutability.

use crate::ppu::{Ppu, bg_state};
use crate::tile::{apply_brightness, bgr555_to_rgb888, decode_2bpp_row};

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

/// Same as [`render_bg1_scanline`] but with debug options.
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

        // 2bpp: 16 bytes per tile, 2 bytes per row.
        let tile_off = char_base + (tile_num as usize) * 16 + row_in_tile * 2;
        let lo = ppu.vram.peek(tile_off as u16);
        let hi = ppu.vram.peek(tile_off.wrapping_add(1) as u16);
        let row = decode_2bpp_row(lo, hi);
        let idx = row[col_in_tile];

        out[x as usize] = if idx == 0 {
            // Transparent — fall through to backdrop.
            backdrop
        } else {
            // Mode 0 BG1 palette range: CGRAM[0..32]. Palette offset
            // selects which 4-entry sub-palette (× 4 colors).
            let cgram_idx = palette_off * 4 + idx;
            decode_palette(ppu, cgram_idx, brightness)
        };
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
    let mut buf = vec![[0u8; 3]; FRAME_W * FRAME_H];
    for y in 0..FRAME_H {
        let line = render_bg1_scanline_with(ppu, y as u16, opts);
        let off = y * FRAME_W;
        buf[off..off + FRAME_W].copy_from_slice(&line);
    }
    buf
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
}
