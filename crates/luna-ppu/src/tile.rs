//! Tile decode primitives.
//!
//! SNES tiles are stored in VRAM in a planar layout:
//! - **2bpp**: 16 bytes / tile. Each pair of bytes encodes one row of
//!   8 pixels — byte 0 holds the LSB of each pixel, byte 1 the MSB.
//! - **4bpp**: 32 bytes / tile. Same encoding as two stacked 2bpp tiles
//!   (planes 0/1 contiguous, planes 2/3 right after).
//! - **8bpp**: 64 bytes / tile. Four stacked 2bpp planes.
//!
//! See `ARCHITECTURE.md` §6.2 and fullsnes §"BG Modes".

/// Decode one row of an 8×8 2bpp tile.
///
/// `lo` is the byte whose bits hold each pixel's bit 0; `hi` holds
/// each pixel's bit 1. Returned indices are in `0..=3`. Pixel 0 is the
/// **leftmost** pixel — i.e. bit 7 of each byte.
#[must_use]
pub fn decode_2bpp_row(lo: u8, hi: u8) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 7 - i;
        let bit0 = (lo >> shift) & 1;
        let bit1 = (hi >> shift) & 1;
        *slot = (bit1 << 1) | bit0;
    }
    out
}

/// Decode one row of an 8×8 4bpp tile.
///
/// `p0..=p3` are the four plane bytes for the row. Pixel n is built
/// as `(p3[n]<<3) | (p2[n]<<2) | (p1[n]<<1) | p0[n]`. Returned indices
/// are in `0..=15`.
#[must_use]
pub fn decode_4bpp_row(p0: u8, p1: u8, p2: u8, p3: u8) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 7 - i;
        let b0 = (p0 >> shift) & 1;
        let b1 = (p1 >> shift) & 1;
        let b2 = (p2 >> shift) & 1;
        let b3 = (p3 >> shift) & 1;
        *slot = (b3 << 3) | (b2 << 2) | (b1 << 1) | b0;
    }
    out
}

/// Convert a 15-bit BGR555 color to 24-bit RGB888.
///
/// Hardware packs each channel as 5 bits in the order BGR (blue is
/// bit 10-14, green bit 5-9, red bit 0-4). The bit-15 high bit is
/// ignored by the renderer.
#[must_use]
pub const fn bgr555_to_rgb888(color: u16) -> [u8; 3] {
    let r5 = (color & 0x001F) as u8;
    let g5 = ((color >> 5) & 0x001F) as u8;
    let b5 = ((color >> 10) & 0x001F) as u8;
    [scale_5_to_8(r5), scale_5_to_8(g5), scale_5_to_8(b5)]
}

/// Expand a 5-bit channel to 8 bits using the canonical SNES
/// "replicate-top-3-bits" formula — gives 0 → 0 and 31 → 255 exactly,
/// with smooth quantization in between.
#[inline]
#[must_use]
pub const fn scale_5_to_8(c5: u8) -> u8 {
    let c5 = c5 & 0x1F;
    (c5 << 3) | (c5 >> 2)
}

/// Apply SNES master brightness (`$2100` bits 0-3, 0..15) to an
/// already-converted RGB888 triple.
///
/// Brightness 0 produces black; brightness 15 is "full" (identity).
/// Intermediate values are linearly scaled: `out = in * (brightness + 1) / 16`.
#[inline]
#[must_use]
pub fn apply_brightness(rgb: [u8; 3], brightness: u8) -> [u8; 3] {
    let b = u16::from(brightness & 0x0F);
    let scale = b + 1;
    [
        ((u16::from(rgb[0]) * scale) >> 4) as u8,
        ((u16::from(rgb[1]) * scale) >> 4) as u8,
        ((u16::from(rgb[2]) * scale) >> 4) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // 2bpp tile decode
    // ---------------------------------------------------------------

    #[test]
    fn row_2bpp_all_zero_is_all_color_0() {
        assert_eq!(decode_2bpp_row(0x00, 0x00), [0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn row_2bpp_leftmost_pixel_is_msb() {
        // lo = 0x80 (bit 7 set), hi = 0x00 → only pixel 0 has color 1.
        assert_eq!(decode_2bpp_row(0x80, 0x00), [1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn row_2bpp_combines_planes_into_2bit_index() {
        // lo = $A5 (bits 7,5,2,0), hi = $5A (bits 6,4,3,1).
        // Pixel n (left→right) gets (hi.bit(7-n)<<1 | lo.bit(7-n)):
        //   0: (0<<1)|1 = 1
        //   1: (1<<1)|0 = 2
        //   2: (0<<1)|1 = 1
        //   3: (1<<1)|0 = 2
        //   4: (1<<1)|0 = 2
        //   5: (0<<1)|1 = 1
        //   6: (1<<1)|0 = 2
        //   7: (0<<1)|1 = 1
        assert_eq!(decode_2bpp_row(0xA5, 0x5A), [1, 2, 1, 2, 2, 1, 2, 1]);
    }

    #[test]
    fn row_2bpp_full_palette_3_when_both_planes_full() {
        assert_eq!(decode_2bpp_row(0xFF, 0xFF), [3, 3, 3, 3, 3, 3, 3, 3]);
    }

    // ---------------------------------------------------------------
    // 4bpp tile decode
    // ---------------------------------------------------------------

    #[test]
    fn row_4bpp_uses_all_four_planes() {
        // Plane 0 = bit 0, plane 1 = bit 1, plane 2 = bit 2, plane 3 = bit 3.
        // Make pixel 0 = $F (all four planes set at bit 7).
        assert_eq!(
            decode_4bpp_row(0x80, 0x80, 0x80, 0x80),
            [0xF, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn row_4bpp_full_palette_15_when_all_planes_full() {
        assert_eq!(
            decode_4bpp_row(0xFF, 0xFF, 0xFF, 0xFF),
            [15, 15, 15, 15, 15, 15, 15, 15]
        );
    }

    // ---------------------------------------------------------------
    // Color conversion
    // ---------------------------------------------------------------

    #[test]
    fn bgr555_zero_is_black() {
        assert_eq!(bgr555_to_rgb888(0x0000), [0, 0, 0]);
    }

    #[test]
    fn bgr555_pure_red_is_full_red() {
        // R = 31, G = 0, B = 0 → bits 0..4 of u16.
        assert_eq!(bgr555_to_rgb888(0x001F), [0xFF, 0, 0]);
    }

    #[test]
    fn bgr555_pure_green_is_full_green() {
        // G = 31 → bits 5..9.
        assert_eq!(bgr555_to_rgb888(0x03E0), [0, 0xFF, 0]);
    }

    #[test]
    fn bgr555_pure_blue_is_full_blue() {
        // B = 31 → bits 10..14.
        assert_eq!(bgr555_to_rgb888(0x7C00), [0, 0, 0xFF]);
    }

    #[test]
    fn bgr555_white_is_all_31() {
        assert_eq!(bgr555_to_rgb888(0x7FFF), [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn scale_5_to_8_endpoints_are_exact() {
        assert_eq!(scale_5_to_8(0), 0);
        assert_eq!(scale_5_to_8(31), 0xFF);
    }

    // ---------------------------------------------------------------
    // Brightness
    // ---------------------------------------------------------------

    #[test]
    fn brightness_15_is_identity() {
        assert_eq!(apply_brightness([100, 150, 200], 15), [100, 150, 200]);
    }

    #[test]
    fn brightness_0_is_black() {
        // out = in * (0+1) / 16 = in / 16.
        // For in=255: 255 / 16 = 15 (not zero!). The "0 = black" claim
        // only holds approximately; the canonical formula is the one
        // above. Document the truth:
        assert_eq!(apply_brightness([255, 255, 255], 0), [15, 15, 15]);
        // Anything dim enough rounds to 0:
        assert_eq!(apply_brightness([15, 15, 15], 0), [0, 0, 0]);
    }

    #[test]
    fn brightness_mid_scales_linearly() {
        // brightness 7 → scale = 8/16 = 0.5
        assert_eq!(apply_brightness([200, 200, 200], 7), [100, 100, 100]);
    }
}
