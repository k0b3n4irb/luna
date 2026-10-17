//! Display filters and aspect ratios: Mesen2's Settings ▸ Video Filter and
//! Settings ▸ Aspect Ratio (`ScaleFilter.cpp`, `EmuSettings::GetAspectRatio`).
//!
//! Both act on the picture a front-end shows, after the emulation: the frame
//! `Emulator::render_frame_rgba` returns is untouched. The behaviour follows
//! Mesen2; the code is luna's own, written from the algorithms (for xBRZ
//! and the 2xSaI family, from a functional description of Mesen2's, checked
//! identical to its pictures). The filters luna does not implement yet (the
//! two NTSC filters, `HQx`) are listed so a menu can show Mesen2's
//! whole list, and report [`VideoFilter::is_available`] `false`.

/// One entry of Mesen2's Video Filter menu, in its order (`VideoFilterType`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum VideoFilter {
    /// The frame as rendered.
    #[default]
    None,
    /// NTSC composite signal (blargg's `snes_ntsc`). Not available.
    NtscBlargg,
    /// NTSC composite signal (Bisqwit), NES-only in Mesen2. Not available.
    NtscBisqwit,
    /// 2x, each pixel split in four cells of decreasing brightness.
    LcdGrid,
    /// xBRZ 2x (Zenju).
    Xbrz2x,
    /// xBRZ 3x (Zenju).
    Xbrz3x,
    /// xBRZ 4x (Zenju).
    Xbrz4x,
    /// xBRZ 5x (Zenju).
    Xbrz5x,
    /// xBRZ 6x (Zenju).
    Xbrz6x,
    /// HQ 2x. Not available.
    Hq2x,
    /// HQ 3x. Not available.
    Hq3x,
    /// HQ 4x. Not available.
    Hq4x,
    /// Scale2x (EPX).
    Scale2x,
    /// Scale3x.
    Scale3x,
    /// Scale4x: Scale2x applied twice.
    Scale4x,
    /// 2xSaI (Kreed).
    TwoXSai,
    /// `Super2xSaI` (Kreed).
    Super2xSai,
    /// `SuperEagle` (Kreed).
    SuperEagle,
    /// Nearest-neighbour 2x.
    Prescale2x,
    /// Nearest-neighbour 3x.
    Prescale3x,
    /// Nearest-neighbour 4x.
    Prescale4x,
    /// Nearest-neighbour 6x.
    Prescale6x,
    /// Nearest-neighbour 8x.
    Prescale8x,
    /// Nearest-neighbour 10x.
    Prescale10x,
}

/// LCD grid cell brightness in percent: top-left, top-right, bottom-left,
/// bottom-right (Mesen2's `VideoConfig` defaults).
pub const LCD_GRID_BRIGHTNESS: [u8; 4] = [100, 85, 85, 85];

impl VideoFilter {
    /// Every filter, in Mesen2's menu order.
    pub const ALL: [Self; 24] = [
        Self::None,
        Self::NtscBlargg,
        Self::NtscBisqwit,
        Self::LcdGrid,
        Self::Xbrz2x,
        Self::Xbrz3x,
        Self::Xbrz4x,
        Self::Xbrz5x,
        Self::Xbrz6x,
        Self::Hq2x,
        Self::Hq3x,
        Self::Hq4x,
        Self::Scale2x,
        Self::Scale3x,
        Self::Scale4x,
        Self::TwoXSai,
        Self::Super2xSai,
        Self::SuperEagle,
        Self::Prescale2x,
        Self::Prescale3x,
        Self::Prescale4x,
        Self::Prescale6x,
        Self::Prescale8x,
        Self::Prescale10x,
    ];

    /// The menu text (Mesen2's English resources).
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::NtscBlargg => "NTSC (blargg)",
            Self::NtscBisqwit => "NTSC (Bisqwit) (NES-only)",
            Self::LcdGrid => "LCD Grid",
            Self::Xbrz2x => "xBRZ 2x",
            Self::Xbrz3x => "xBRZ 3x",
            Self::Xbrz4x => "xBRZ 4x",
            Self::Xbrz5x => "xBRZ 5x",
            Self::Xbrz6x => "xBRZ 6x",
            Self::Hq2x => "HQ 2x",
            Self::Hq3x => "HQ 3x",
            Self::Hq4x => "HQ 4x",
            Self::Scale2x => "Scale 2x",
            Self::Scale3x => "Scale 3x",
            Self::Scale4x => "Scale 4x",
            Self::TwoXSai => "2xSai",
            Self::Super2xSai => "Super2xSai",
            Self::SuperEagle => "SuperEagle",
            Self::Prescale2x => "Prescale 2x",
            Self::Prescale3x => "Prescale 3x",
            Self::Prescale4x => "Prescale 4x",
            Self::Prescale6x => "Prescale 6x",
            Self::Prescale8x => "Prescale 8x",
            Self::Prescale10x => "Prescale 10x",
        }
    }

    /// The name stored in a settings file (Mesen2's enum identifier).
    pub const fn key(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::NtscBlargg => "NtscBlargg",
            Self::NtscBisqwit => "NtscBisqwit",
            Self::LcdGrid => "LcdGrid",
            Self::Xbrz2x => "xBRZ2x",
            Self::Xbrz3x => "xBRZ3x",
            Self::Xbrz4x => "xBRZ4x",
            Self::Xbrz5x => "xBRZ5x",
            Self::Xbrz6x => "xBRZ6x",
            Self::Hq2x => "HQ2x",
            Self::Hq3x => "HQ3x",
            Self::Hq4x => "HQ4x",
            Self::Scale2x => "Scale2x",
            Self::Scale3x => "Scale3x",
            Self::Scale4x => "Scale4x",
            Self::TwoXSai => "_2xSai",
            Self::Super2xSai => "Super2xSai",
            Self::SuperEagle => "SuperEagle",
            Self::Prescale2x => "Prescale2x",
            Self::Prescale3x => "Prescale3x",
            Self::Prescale4x => "Prescale4x",
            Self::Prescale6x => "Prescale6x",
            Self::Prescale8x => "Prescale8x",
            Self::Prescale10x => "Prescale10x",
        }
    }

    /// The filter a settings file names, if it names one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == key)
    }

    /// The menu section, Mesen2's separators falling between two sections.
    pub const fn group(self) -> u8 {
        match self {
            Self::None => 0,
            Self::NtscBlargg | Self::NtscBisqwit => 1,
            Self::LcdGrid => 2,
            Self::Xbrz2x | Self::Xbrz3x | Self::Xbrz4x | Self::Xbrz5x | Self::Xbrz6x => 3,
            Self::Hq2x | Self::Hq3x | Self::Hq4x => 4,
            Self::Scale2x | Self::Scale3x | Self::Scale4x => 5,
            Self::TwoXSai | Self::Super2xSai | Self::SuperEagle => 6,
            Self::Prescale2x
            | Self::Prescale3x
            | Self::Prescale4x
            | Self::Prescale6x
            | Self::Prescale8x
            | Self::Prescale10x => 7,
        }
    }

    /// Whether luna implements this filter.
    pub const fn is_available(self) -> bool {
        matches!(
            self,
            Self::None
                | Self::LcdGrid
                | Self::Xbrz2x
                | Self::Xbrz3x
                | Self::Xbrz4x
                | Self::Xbrz5x
                | Self::Xbrz6x
                | Self::Scale2x
                | Self::Scale3x
                | Self::Scale4x
                | Self::TwoXSai
                | Self::Super2xSai
                | Self::SuperEagle
                | Self::Prescale2x
                | Self::Prescale3x
                | Self::Prescale4x
                | Self::Prescale6x
                | Self::Prescale8x
                | Self::Prescale10x
        )
    }

    /// How many output pixels per input pixel, on each axis; `1` for a
    /// filter that is not available (it leaves the frame as rendered).
    pub const fn scale(self) -> usize {
        match self {
            Self::LcdGrid
            | Self::Scale2x
            | Self::TwoXSai
            | Self::Super2xSai
            | Self::SuperEagle
            | Self::Xbrz2x
            | Self::Prescale2x => 2,
            Self::Xbrz3x | Self::Scale3x | Self::Prescale3x => 3,
            Self::Xbrz4x | Self::Scale4x | Self::Prescale4x => 4,
            Self::Xbrz5x => 5,
            Self::Xbrz6x | Self::Prescale6x => 6,
            Self::Prescale8x => 8,
            Self::Prescale10x => 10,
            _ => 1,
        }
    }

    /// Filter a `width` × `height` RGBA frame into `out` (resized to fit),
    /// returning the output size. A filter that is not available copies the
    /// frame unchanged.
    pub fn apply(
        self,
        src: &[u8],
        width: usize,
        height: usize,
        out: &mut Vec<u8>,
    ) -> (usize, usize) {
        let s = self.scale();
        out.resize(width * s * height * s * 4, 0);
        let pixels = &src[..width * height * 4];
        match self {
            Self::LcdGrid => lcd_grid(pixels, width, height, LCD_GRID_BRIGHTNESS, out),
            Self::Xbrz2x | Self::Xbrz3x | Self::Xbrz4x | Self::Xbrz5x | Self::Xbrz6x => {
                xbrz(s, pixels, width, height, out);
            }
            Self::Scale2x => scale2x(pixels, width, height, out),
            Self::Scale3x => scale3x(pixels, width, height, out),
            Self::Scale4x => {
                let mut half = vec![0; width * height * 16];
                scale2x(pixels, width, height, &mut half);
                scale2x(&half, width * 2, height * 2, out);
            }
            Self::TwoXSai => sai(Sai::TwoX, pixels, width, height, out),
            Self::Super2xSai => sai(Sai::Super, pixels, width, height, out),
            Self::SuperEagle => sai(Sai::Eagle, pixels, width, height, out),
            _ if s > 1 => prescale(pixels, width, height, s, out),
            _ => out.copy_from_slice(pixels),
        }
        (width * s, height * s)
    }
}

/// One entry of Mesen2's Aspect Ratio menu (`VideoAspectRatio`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum AspectRatio {
    /// Square pixels: the frame's own width over height.
    #[default]
    NoStretching,
    /// The pixel aspect of the console's region (NTSC 8:7, PAL 11:8).
    Auto,
    /// NTSC pixel aspect, 8:7.
    Ntsc,
    /// PAL pixel aspect, 11:8.
    Pal,
    /// A 4:3 picture.
    Standard,
    /// A 16:9 picture.
    Widescreen,
    /// The picture ratio the user set ([`CUSTOM_ASPECT_RANGE`]).
    Custom,
}

/// Bounds of the custom picture ratio (Mesen2's `[MinMax(0.1, 5.0)]`).
pub const CUSTOM_ASPECT_RANGE: std::ops::RangeInclusive<f64> = 0.1..=5.0;

impl AspectRatio {
    /// Every aspect ratio, in Mesen2's menu order.
    pub const ALL: [Self; 7] = [
        Self::NoStretching,
        Self::Auto,
        Self::Ntsc,
        Self::Pal,
        Self::Standard,
        Self::Widescreen,
        Self::Custom,
    ];

    /// The menu text (Mesen2's English resources).
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoStretching => "No Stretching (default)",
            Self::Auto => "Auto (NTSC/PAL)",
            Self::Ntsc => "NTSC (8:7 PAR)",
            Self::Pal => "PAL (11:8 PAR)",
            Self::Standard => "Standard (4:3)",
            Self::Widescreen => "Widescreen (16:9)",
            Self::Custom => "Custom",
        }
    }

    /// The name stored in a settings file (Mesen2's enum identifier).
    pub const fn key(self) -> &'static str {
        match self {
            Self::NoStretching => "NoStretching",
            Self::Auto => "Auto",
            Self::Ntsc => "NTSC",
            Self::Pal => "PAL",
            Self::Standard => "Standard",
            Self::Widescreen => "Widescreen",
            Self::Custom => "Custom",
        }
    }

    /// The aspect ratio a settings file names, if it names one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    /// The menu section, Mesen2's separators falling between two sections.
    pub const fn group(self) -> u8 {
        match self {
            Self::NoStretching => 0,
            Self::Auto => 1,
            Self::Ntsc | Self::Pal | Self::Standard | Self::Widescreen => 2,
            Self::Custom => 3,
        }
    }

    /// Width over height of the shown picture, for a `width` × `height`
    /// frame of a PAL (`pal`) or NTSC console (`EmuSettings::GetAspectRatio`).
    pub fn ratio(self, custom: f64, pal: bool, width: usize, height: usize) -> f64 {
        let screen = width as f64 / height.max(1) as f64;
        match self {
            Self::NoStretching => screen,
            Self::Auto if pal => screen * 11.0 / 8.0,
            Self::Auto | Self::Ntsc => screen * 8.0 / 7.0,
            Self::Pal => screen * 11.0 / 8.0,
            Self::Standard => 4.0 / 3.0,
            Self::Widescreen => 16.0 / 9.0,
            Self::Custom => custom.clamp(*CUSTOM_ASPECT_RANGE.start(), *CUSTOM_ASPECT_RANGE.end()),
        }
    }
}

fn pixel(buf: &[u8], i: usize) -> [u8; 4] {
    let p = &buf[i * 4..i * 4 + 4];
    [p[0], p[1], p[2], p[3]]
}

fn put(buf: &mut [u8], i: usize, c: [u8; 4]) {
    buf[i * 4..i * 4 + 4].copy_from_slice(&c);
}

fn prescale(src: &[u8], width: usize, height: usize, s: usize, out: &mut [u8]) {
    let row = width * s * 4;
    for y in 0..height {
        let line = &mut out[y * s * row..(y * s + 1) * row];
        for x in 0..width {
            let c = &src[(y * width + x) * 4..(y * width + x) * 4 + 4];
            for cell in line[x * s * 4..(x + 1) * s * 4].chunks_exact_mut(4) {
                cell.copy_from_slice(c);
            }
        }
        let (first, rest) = out[y * s * row..(y + 1) * s * row].split_at_mut(row);
        for copy in rest.chunks_exact_mut(row) {
            copy.copy_from_slice(first);
        }
    }
}

fn lcd_grid(src: &[u8], width: usize, height: usize, pct: [u8; 4], out: &mut [u8]) {
    // Mesen2 stores the brightness as `(u8)(fraction * 255)`.
    let level = pct.map(|p| (f64::from(p.min(100)) / 100.0 * 255.0) as u32);
    let dim = |c: [u8; 4], b: u32| {
        [
            (u32::from(c[0]) * b / 255) as u8,
            (u32::from(c[1]) * b / 255) as u8,
            (u32::from(c[2]) * b / 255) as u8,
            0xFF,
        ]
    };
    let w2 = width * 2;
    for y in 0..height {
        for x in 0..width {
            let c = pixel(src, y * width + x);
            let tl = y * 2 * w2 + x * 2;
            put(out, tl, dim(c, level[0]));
            put(out, tl + 1, dim(c, level[1]));
            put(out, tl + w2, dim(c, level[2]));
            put(out, tl + w2 + 1, dim(c, level[3]));
        }
    }
}

/// The eight neighbours of `(x, y)`, the frame's edge repeated outward:
/// `[A, B, C, D, E, F, G, H, I]` row by row, `E` the pixel itself.
fn neighbourhood(src: &[u8], width: usize, height: usize, x: usize, y: usize) -> [[u8; 4]; 9] {
    let xs = [x.saturating_sub(1), x, (x + 1).min(width - 1)];
    let ys = [y.saturating_sub(1), y, (y + 1).min(height - 1)];
    let mut n = [[0; 4]; 9];
    for (r, &yy) in ys.iter().enumerate() {
        for (c, &xx) in xs.iter().enumerate() {
            n[r * 3 + c] = pixel(src, yy * width + xx);
        }
    }
    n
}

// Scale2x / Scale3x as published by their author (scale2x.it, "Algorithm"):
// a pixel takes a neighbour's colour only along an edge the two neighbours
// meeting at its corner agree on, and never where the cross is uniform.
fn scale2x(src: &[u8], width: usize, height: usize, out: &mut [u8]) {
    let w2 = width * 2;
    for y in 0..height {
        for x in 0..width {
            let [_, b, _, d, e, f, _, h, _] = neighbourhood(src, width, height, x, y);
            let cells = if b != h && d != f {
                [
                    if d == b { d } else { e },
                    if b == f { f } else { e },
                    if d == h { d } else { e },
                    if h == f { f } else { e },
                ]
            } else {
                [e; 4]
            };
            let tl = y * 2 * w2 + x * 2;
            put(out, tl, cells[0]);
            put(out, tl + 1, cells[1]);
            put(out, tl + w2, cells[2]);
            put(out, tl + w2 + 1, cells[3]);
        }
    }
}

fn scale3x(src: &[u8], width: usize, height: usize, out: &mut [u8]) {
    let w3 = width * 3;
    for y in 0..height {
        for x in 0..width {
            let [a, b, c, d, e, f, g, h, i] = neighbourhood(src, width, height, x, y);
            let cells = if b != h && d != f {
                [
                    if d == b { d } else { e },
                    if (d == b && e != c) || (b == f && e != a) {
                        b
                    } else {
                        e
                    },
                    if b == f { f } else { e },
                    if (d == b && e != g) || (d == h && e != a) {
                        d
                    } else {
                        e
                    },
                    e,
                    if (b == f && e != i) || (h == f && e != c) {
                        f
                    } else {
                        e
                    },
                    if d == h { d } else { e },
                    if (d == h && e != i) || (h == f && e != g) {
                        h
                    } else {
                        e
                    },
                    if h == f { f } else { e },
                ]
            } else {
                [e; 9]
            };
            for (k, cell) in cells.into_iter().enumerate() {
                put(out, (y * 3 + k / 3) * w3 + x * 3 + k % 3, cell);
            }
        }
    }
}

/// Kreed's 2x filters, after the behaviour of Mesen2's (written from a
/// functional description, not from its code).
#[derive(Copy, Clone, Eq, PartialEq)]
enum Sai {
    TwoX,
    Super,
    Eagle,
}

/// Per-byte `⌊(u + v) / 2⌋`, alpha included.
const fn half(u: u32, v: u32) -> u32 {
    ((u & 0xFEFE_FEFE) >> 1) + ((v & 0xFEFE_FEFE) >> 1) + (u & v & 0x0101_0101)
}

/// Per-byte `⌊(u + v + w + z) / 4⌋`, alpha included.
const fn quarter(u: u32, v: u32, w: u32, z: u32) -> u32 {
    let high = ((u & 0xFCFC_FCFC) >> 2)
        + ((v & 0xFCFC_FCFC) >> 2)
        + ((w & 0xFCFC_FCFC) >> 2)
        + ((z & 0xFCFC_FCFC) >> 2);
    let low = ((u & 0x0303_0303) + (v & 0x0303_0303) + (w & 0x0303_0303) + (z & 0x0303_0303)) >> 2
        & 0x0303_0303;
    high + low
}

/// Which of two candidates the probes `s`, `t` side with: a candidate both
/// probes equal scores 0, any other 1; the vote is `p`'s score minus `q`'s.
fn vote(p: u32, q: u32, s: u32, t: u32) -> i32 {
    let score = |z: u32| i32::from(!(z == s && z == t));
    score(p) - score(q)
}

fn sai(kind: Sai, src: &[u8], width: usize, height: usize, out: &mut [u8]) {
    let at = |x: usize, y: usize| {
        let i = (y * width + x) * 4;
        u32::from_le_bytes([src[i], src[i + 1], src[i + 2], src[i + 3]])
    };
    let w2 = width * 2;
    for y in 0..height {
        for x in 0..width {
            // The 4 × 4 window, offsets -1..=2, clamped to the frame per axis.
            let p = |dx: isize, dy: isize| {
                let cx = x.saturating_add_signed(dx).min(width - 1);
                let cy = y.saturating_add_signed(dy).min(height - 1);
                at(cx, cy)
            };
            let [tl, tr, bl, br] = match kind {
                Sai::TwoX => two_x_sai(&p),
                Sai::Super => super_2x_sai(&p),
                Sai::Eagle => super_eagle(&p),
            };
            let i = y * 2 * w2 + x * 2;
            for (k, c) in [(i, tl), (i + 1, tr), (i + w2, bl), (i + w2 + 1, br)] {
                out[k * 4..k * 4 + 4].copy_from_slice(&c.to_le_bytes());
            }
        }
    }
}

fn two_x_sai(p: &impl Fn(isize, isize) -> u32) -> [u32; 4] {
    let (a, b, c, d) = (p(0, 0), p(1, 0), p(0, 1), p(1, 1));
    if a == d && b != c {
        let tr = if (a == p(0, -1) && b == p(2, 1))
            || (a == c && a == p(1, -1) && b != p(0, -1) && b == p(2, -1))
        {
            a
        } else {
            half(a, b)
        };
        let bl = if (a == p(-1, 0) && c == p(1, 2))
            || (a == b && a == p(-1, 1) && p(-1, 0) != c && c == p(-1, 2))
        {
            a
        } else {
            half(a, c)
        };
        [a, tr, bl, a]
    } else if b == c && a != d {
        let tr = if (b == p(1, -1) && a == p(-1, 1))
            || (b == p(0, -1) && b == d && a != p(1, -1) && a == p(-1, -1))
        {
            b
        } else {
            half(a, b)
        };
        let bl = if (c == p(-1, 1) && a == p(1, -1))
            || (c == p(-1, 0) && c == d && a != p(-1, 1) && a == p(-1, -1))
        {
            c
        } else {
            half(a, c)
        };
        [a, tr, bl, b]
    } else if a == d && b == c {
        if a == b {
            return [a; 4];
        }
        let r = vote(a, b, p(-1, 0), p(0, -1))
            + vote(b, a, p(2, 0), p(1, -1))
            + vote(b, a, p(-1, 1), p(0, 2))
            + vote(a, b, p(2, 1), p(1, 2));
        let br = match r.signum() {
            1 => a,
            -1 => b,
            _ => quarter(a, b, c, d),
        };
        [a, half(a, b), half(a, c), br]
    } else {
        let tr = if a == c && a == p(1, -1) && b != p(0, -1) && b == p(2, -1) {
            a
        } else if b == p(0, -1) && b == d && a != p(1, -1) && a == p(-1, -1) {
            b
        } else {
            half(a, b)
        };
        let bl = if a == b && a == p(-1, 1) && p(-1, 0) != c && c == p(-1, 2) {
            a
        } else if c == p(-1, 0) && c == d && a != p(-1, 1) && a == p(-1, -1) {
            c
        } else {
            half(a, c)
        };
        [a, tr, bl, quarter(a, b, c, d)]
    }
}

/// The diagonal vote `Super2xSaI` and `SuperEagle` share: positive keeps `b`'s
/// diagonal, negative `a`'s.
fn diagonal_vote(p: &impl Fn(isize, isize) -> u32, a: u32, b: u32) -> i32 {
    vote(b, a, p(-1, 1), p(0, 2))
        + vote(b, a, p(-1, 0), p(0, -1))
        + vote(b, a, p(1, 2), p(2, 1))
        + vote(b, a, p(1, -1), p(2, 0))
}

fn super_2x_sai(p: &impl Fn(isize, isize) -> u32) -> [u32; 4] {
    let (a, b, c, d) = (p(0, 0), p(1, 0), p(0, 1), p(1, 1));
    let (tr, br) = if c == b && a != d {
        (c, c)
    } else if a == d && c != b {
        (a, a)
    } else if a == d && c == b {
        let v = match diagonal_vote(p, a, b).signum() {
            1 => b,
            -1 => a,
            _ => half(a, b),
        };
        (v, v)
    } else {
        let br = if b == d && d == p(0, 2) && c != p(1, 2) && d != p(-1, 2) {
            quarter(d, d, d, c)
        } else if a == c && c == p(1, 2) && p(0, 2) != d && c != p(2, 2) {
            quarter(c, c, c, d)
        } else {
            half(c, d)
        };
        let tr = if b == d && b == p(0, -1) && a != p(1, -1) && b != p(-1, -1) {
            quarter(b, b, b, a)
        } else if a == c && a == p(1, -1) && p(0, -1) != b && a != p(2, -1) {
            quarter(a, a, a, b)
        } else {
            half(a, b)
        };
        (tr, br)
    };
    let bl = if (a == d && c != b && p(-1, 0) == a && a != p(1, 2))
        || (a == p(-1, 1) && b == a && p(-1, 0) != c && a != p(-1, 2))
    {
        half(a, c)
    } else {
        c
    };
    let tl = if (c == b && a != d && p(-1, 1) == c && c != p(1, -1))
        || (p(-1, 0) == c && d == c && p(-1, 1) != a && c != p(-1, -1))
    {
        half(a, c)
    } else {
        a
    };
    [tl, tr, bl, br]
}

fn super_eagle(p: &impl Fn(isize, isize) -> u32) -> [u32; 4] {
    let (a, b, c, d) = (p(0, 0), p(1, 0), p(0, 1), p(1, 1));
    if c == b && a != d {
        let tl = if p(-1, 1) == c || b == p(1, -1) {
            half(c, half(c, a))
        } else {
            half(a, b)
        };
        let br = if b == p(2, 0) || c == p(0, 2) {
            half(c, half(c, d))
        } else {
            half(c, d)
        };
        [tl, c, c, br]
    } else if a == d && c != b {
        let tr = if p(0, -1) == a || d == p(2, 1) {
            half(a, half(a, b))
        } else {
            half(a, b)
        };
        // Mesen2 falls back to `c` blended with `d` here, not with `a`.
        let bl = if d == p(1, 2) || p(-1, 0) == a {
            half(a, half(a, c))
        } else {
            half(c, d)
        };
        [a, tr, bl, a]
    } else if a == d && c == b {
        match diagonal_vote(p, a, b).signum() {
            1 => [half(a, b), c, c, half(a, b)],
            -1 => [a, half(a, b), half(a, b), a],
            _ => [a, c, c, a],
        }
    } else {
        let t = half(c, b);
        let u = half(a, d);
        [
            quarter(a, a, a, t),
            quarter(b, b, b, u),
            quarter(c, c, c, u),
            quarter(d, d, d, t),
        ]
    }
}

// xBRZ (Zenju), after the behaviour of Mesen2's: alpha-aware, default
// settings, the whole frame at once. Written from a functional description,
// not from its code. Pixels are `0xAARRGGBB`.

const XBRZ_EQUAL: f64 = 30.0;
const XBRZ_DOMINANT: f64 = 3.6;
const XBRZ_STEEP: f64 = 2.2;

/// A corner's blend class: none, normal or dominant.
type Blend = u8;
const NORMAL: Blend = 1;
const DOMINANT: Blend = 2;

const fn channel(p: u32, shift: u32) -> u32 {
    (p >> shift) & 0xFF
}

/// The YCbCr (BT.2020) length of the difference, each channel difference
/// first quantised to 8 bits (an even difference loses one, zero becomes
/// −1), rounded to single precision as xBRZ's table holds it.
// Plain products and sums, in this order: a fused multiply-add would round
// differently from the reference.
#[allow(clippy::suboptimal_flops)]
fn xbrz_dist_rgb(p: u32, q: u32) -> f64 {
    let e = |shift| {
        let d = channel(p, shift) as i32 - channel(q, shift) as i32;
        f64::from(((d + 255) >> 1) * 2 - 255)
    };
    let (r, g, b) = (e(16), e(8), e(0));
    let (kb, kr) = (0.0593, 0.2627);
    let kg = 1.0 - kb - kr;
    let y = kr * r + kg * g + kb * b;
    let cb = 0.5 / (1.0 - kb) * (b - y);
    let cr = 0.5 / (1.0 - kr) * (r - y);
    f64::from((y * y + cb * cb + cr * cr).sqrt() as f32)
}

/// The distance with alpha: a transparent pixel is far from an opaque one.
// Same operation order as the reference (see `xbrz_dist_rgb`).
#[allow(clippy::suboptimal_flops)]
fn xbrz_dist(p: u32, q: u32) -> f64 {
    let a1 = f64::from(channel(p, 24)) / 255.0;
    let a2 = f64::from(channel(q, 24)) / 255.0;
    let d = xbrz_dist_rgb(p, q);
    if a1 < a2 {
        a1 * d + 255.0 * (a2 - a1)
    } else {
        a2 * d + 255.0 * (a1 - a2)
    }
}

fn xbrz_similar(p: u32, q: u32) -> bool {
    xbrz_dist(p, q) < XBRZ_EQUAL
}

/// `m/n` of `front` over `back`, the colours weighted by alpha too.
fn xbrz_mix(front: u32, back: u32, m: u32, n: u32) -> u32 {
    let wf = channel(front, 24) * m;
    let wb = channel(back, 24) * (n - m);
    let w = wf + wb;
    if w == 0 {
        return 0;
    }
    let mix = |shift| (channel(front, shift) * wf + channel(back, shift) * wb) / w;
    (w / n) << 24 | mix(16) << 16 | mix(8) << 8 | mix(0)
}

/// The five ways a corner is drawn.
#[derive(Copy, Clone)]
enum Line {
    Shallow,
    Steep,
    SteepShallow,
    Diagonal,
    Corner,
}

/// One write of a pattern: row, column, `m/n` of the blend colour; `m == n`
/// sets the cell.
type Cell = (u8, u8, u16, u16);

/// The bottom-right corner's writes at scale `s`; the steep line is the
/// shallow one transposed.
const fn xbrz_pattern(s: usize, line: Line) -> &'static [Cell] {
    const C: (u16, u16) = (1, 1);
    macro_rules! p {
        ($(($r:expr, $c:expr, $f:expr)),* $(,)?) => {
            &[$(($r, $c, $f.0, $f.1)),*]
        };
    }
    match (s, line) {
        (2, Line::Shallow | Line::Steep) => p![(1, 0, (1, 4)), (1, 1, (3, 4))],
        (2, Line::SteepShallow) => p![(0, 1, (1, 4)), (1, 0, (1, 4)), (1, 1, (5, 6))],
        (2, Line::Diagonal) => p![(1, 1, (1, 2))],
        (2, Line::Corner) => p![(1, 1, (21, 100))],
        (3, Line::Shallow | Line::Steep) => {
            p![(1, 2, (1, 4)), (2, 0, (1, 4)), (2, 1, (3, 4)), (2, 2, C)]
        }
        (3, Line::SteepShallow) => p![
            (0, 2, (1, 4)),
            (1, 2, (3, 4)),
            (2, 0, (1, 4)),
            (2, 1, (3, 4)),
            (2, 2, C),
        ],
        (3, Line::Diagonal) => p![(1, 2, (1, 8)), (2, 1, (1, 8)), (2, 2, (7, 8))],
        (3, Line::Corner) => p![(2, 2, (45, 100))],
        (4, Line::Shallow | Line::Steep) => p![
            (2, 2, (1, 4)),
            (2, 3, (3, 4)),
            (3, 0, (1, 4)),
            (3, 1, (3, 4)),
            (3, 2, C),
            (3, 3, C),
        ],
        (4, Line::SteepShallow) => p![
            (0, 3, (1, 4)),
            (1, 3, (3, 4)),
            (2, 2, (1, 3)),
            (2, 3, C),
            (3, 0, (1, 4)),
            (3, 1, (3, 4)),
            (3, 2, C),
            (3, 3, C),
        ],
        (4, Line::Diagonal) => p![(2, 3, (1, 2)), (3, 2, (1, 2)), (3, 3, C)],
        (4, Line::Corner) => p![(2, 3, (9, 100)), (3, 2, (9, 100)), (3, 3, (68, 100))],
        (5, Line::Shallow | Line::Steep) => p![
            (2, 4, (1, 4)),
            (3, 2, (1, 4)),
            (3, 3, (3, 4)),
            (3, 4, C),
            (4, 0, (1, 4)),
            (4, 1, (3, 4)),
            (4, 2, C),
            (4, 3, C),
            (4, 4, C),
        ],
        (5, Line::SteepShallow) => p![
            (0, 4, (1, 4)),
            (1, 4, (3, 4)),
            (2, 3, (1, 4)),
            (2, 4, C),
            (3, 2, (1, 4)),
            (3, 3, (2, 3)),
            (3, 4, C),
            (4, 0, (1, 4)),
            (4, 1, (3, 4)),
            (4, 2, C),
            (4, 3, C),
            (4, 4, C),
        ],
        (5, Line::Diagonal) => p![
            (2, 4, (1, 8)),
            (3, 3, (1, 8)),
            (3, 4, (7, 8)),
            (4, 2, (1, 8)),
            (4, 3, (7, 8)),
            (4, 4, C),
        ],
        (5, Line::Corner) => p![(3, 4, (23, 100)), (4, 3, (23, 100)), (4, 4, (86, 100))],
        (6, Line::Shallow | Line::Steep) => p![
            (3, 4, (1, 4)),
            (3, 5, (3, 4)),
            (4, 2, (1, 4)),
            (4, 3, (3, 4)),
            (4, 4, C),
            (4, 5, C),
            (5, 0, (1, 4)),
            (5, 1, (3, 4)),
            (5, 2, C),
            (5, 3, C),
            (5, 4, C),
            (5, 5, C),
        ],
        (6, Line::SteepShallow) => p![
            (0, 5, (1, 4)),
            (1, 5, (3, 4)),
            (2, 4, (1, 4)),
            (2, 5, C),
            (3, 4, (3, 4)),
            (3, 5, C),
            (4, 2, (1, 4)),
            (4, 3, (3, 4)),
            (4, 4, C),
            (4, 5, C),
            (5, 0, (1, 4)),
            (5, 1, (3, 4)),
            (5, 2, C),
            (5, 3, C),
            (5, 4, C),
            (5, 5, C),
        ],
        (6, Line::Diagonal) => p![
            (3, 5, (1, 2)),
            (4, 4, (1, 2)),
            (4, 5, C),
            (5, 3, (1, 2)),
            (5, 4, C),
            (5, 5, C),
        ],
        (6, Line::Corner) => p![
            (3, 5, (6, 100)),
            (4, 5, (42, 100)),
            (5, 3, (6, 100)),
            (5, 4, (42, 100)),
            (5, 5, (97, 100)),
        ],
        _ => &[],
    }
}

/// View cell `(i, j)` of an `n` × `n` grid turned `r` quarter-turns: the
/// bottom-right corner's view becomes the top-right one at `r = 1`, the
/// top-left at 2, the bottom-left at 3.
const fn rotate(r: usize, n: usize, i: usize, j: usize) -> (usize, usize) {
    match r {
        0 => (i, j),
        1 => (n - 1 - j, i),
        2 => (n - 1 - i, n - 1 - j),
        _ => (j, n - 1 - i),
    }
}

/// The classes of the corner between `f`, `g` (right of it), `j` (below)
/// and `k`, as seen from each: `[f, g, j, k]`.
// Plain products and sums, as in `xbrz_dist_rgb`.
#[allow(clippy::suboptimal_flops)]
fn xbrz_corner(at: &impl Fn(isize, isize) -> u32) -> [Blend; 4] {
    let (b, c) = (at(0, -1), at(1, -1));
    let (e, f, g, h) = (at(-1, 0), at(0, 0), at(1, 0), at(2, 0));
    let (i, j, k, l) = (at(-1, 1), at(0, 1), at(1, 1), at(2, 1));
    let (n, o) = (at(0, 2), at(1, 2));
    let mut out = [0; 4];
    if (f == g && j == k) || (f == j && g == k) {
        return out;
    }
    let jg = xbrz_dist(i, f)
        + xbrz_dist(f, c)
        + xbrz_dist(n, k)
        + xbrz_dist(k, h)
        + 4.0 * xbrz_dist(j, g);
    let fk = xbrz_dist(e, j)
        + xbrz_dist(j, o)
        + xbrz_dist(b, g)
        + xbrz_dist(g, l)
        + 4.0 * xbrz_dist(f, k);
    let strength = |small: f64, large: f64| {
        if XBRZ_DOMINANT * small < large {
            DOMINANT
        } else {
            NORMAL
        }
    };
    if jg < fk {
        let s = strength(jg, fk);
        if f != g && f != j {
            out[0] = s;
        }
        if k != j && k != g {
            out[3] = s;
        }
    } else if fk < jg {
        let s = strength(fk, jg);
        if j != f && j != k {
            out[2] = s;
        }
        if g != f && g != k {
            out[1] = s;
        }
    }
    out
}

fn xbrz(s: usize, src: &[u8], width: usize, height: usize, out: &mut [u8]) {
    let argb: Vec<u32> = src
        .chunks_exact(4)
        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
        .collect();
    let clamped = |x: usize, y: usize, dx: isize, dy: isize| {
        let cx = x.saturating_add_signed(dx).min(width - 1);
        let cy = y.saturating_add_signed(dy).min(height - 1);
        argb[cy * width + cx]
    };
    // Every corner whose top-left pixel is in the frame; the corners above
    // row 0 and left of column 0 are never classed.
    let mut corners = vec![[0; 4]; width * height];
    for y in 0..height {
        for x in 0..width {
            corners[y * width + x] = xbrz_corner(&|dx, dy| clamped(x, y, dx, dy));
        }
    }
    let ow = width * s;
    let mut block = vec![0u32; s * s];
    for y in 0..height {
        for x in 0..width {
            let centre = argb[y * width + x];
            // Bottom-right, top-right, top-left, bottom-left.
            let classes = [
                corners[y * width + x][0],
                if y > 0 {
                    corners[(y - 1) * width + x][2]
                } else {
                    0
                },
                if x > 0 && y > 0 {
                    corners[(y - 1) * width + x - 1][3]
                } else {
                    0
                },
                if x > 0 {
                    corners[y * width + x - 1][1]
                } else {
                    0
                },
            ];
            block.fill(centre);
            if classes != [0; 4] {
                for r in 0..4 {
                    xbrz_blend(
                        s,
                        r,
                        classes,
                        &|i, j| {
                            let (pi, pj) = rotate(r, 3, i, j);
                            clamped(x, y, pj as isize - 1, pi as isize - 1)
                        },
                        &mut block,
                    );
                }
            }
            for (i, row) in block.chunks_exact(s).enumerate() {
                let base = ((y * s + i) * ow + x * s) * 4;
                for (j, &p) in row.iter().enumerate() {
                    let [a, r, g, b] = p.to_be_bytes();
                    out[base + j * 4..base + j * 4 + 4].copy_from_slice(&[r, g, b, a]);
                }
            }
        }
    }
}

/// Draw one corner of a pixel into its `s` × `s` block, in the view turned
/// `r` quarter-turns (`view` reads its 3 × 3 neighbourhood in that view).
fn xbrz_blend(
    s: usize,
    r: usize,
    classes: [Blend; 4],
    view: &impl Fn(usize, usize) -> u32,
    block: &mut [u32],
) {
    let (br, tr, bl) = (classes[r], classes[(r + 1) % 4], classes[(r + 3) % 4]);
    if br == 0 {
        return;
    }
    let [b, c, d, e, f, g, h, i] = [
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (2, 2),
    ]
    .map(|(vi, vj)| view(vi, vj));
    let do_line = if br == DOMINANT {
        true
    } else if (tr != 0 && !xbrz_similar(e, g)) || (bl != 0 && !xbrz_similar(e, c)) {
        false
    } else {
        !(!xbrz_similar(e, i)
            && xbrz_similar(g, h)
            && xbrz_similar(h, i)
            && xbrz_similar(i, f)
            && xbrz_similar(f, c))
    };
    let col = if xbrz_dist(e, f) <= xbrz_dist(e, h) {
        f
    } else {
        h
    };
    let line = if do_line {
        let (fg, hc) = (xbrz_dist(f, g), xbrz_dist(h, c));
        let shallow = XBRZ_STEEP * fg <= hc && e != g && d != g;
        let steep = XBRZ_STEEP * hc <= fg && e != c && b != c;
        match (shallow, steep) {
            (true, true) => Line::SteepShallow,
            (true, false) => Line::Shallow,
            (false, true) => Line::Steep,
            (false, false) => Line::Diagonal,
        }
    } else {
        Line::Corner
    };
    for &(vi, vj, m, n) in xbrz_pattern(s, line) {
        let (vi, vj) = if matches!(line, Line::Steep) {
            (vj, vi)
        } else {
            (vi, vj)
        };
        let (pi, pj) = rotate(r, s, usize::from(vi), usize::from(vj));
        let t = &mut block[pi * s + pj];
        *t = if m == n {
            col
        } else {
            xbrz_mix(col, *t, u32::from(m), u32::from(n))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: [u8; 4] = [0, 0, 0, 255];
    const W: [u8; 4] = [255, 255, 255, 255];

    fn frame(cells: &[[u8; 4]]) -> Vec<u8> {
        cells.iter().flatten().copied().collect()
    }

    fn cells(buf: &[u8]) -> Vec<[u8; 4]> {
        buf.chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect()
    }

    #[test]
    fn keys_round_trip_and_menu_order_is_mesen2s() {
        for f in VideoFilter::ALL {
            assert_eq!(VideoFilter::from_key(f.key()), Some(f));
        }
        for a in AspectRatio::ALL {
            assert_eq!(AspectRatio::from_key(a.key()), Some(a));
        }
        assert_eq!(VideoFilter::from_key("nope"), None);
        let groups: Vec<u8> = VideoFilter::ALL.iter().map(|f| f.group()).collect();
        assert!(groups.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn unavailable_filters_leave_the_frame_alone() {
        let src = frame(&[K, W, W, K]);
        for f in VideoFilter::ALL.into_iter().filter(|f| !f.is_available()) {
            let mut out = Vec::new();
            assert_eq!(f.apply(&src, 2, 2, &mut out), (2, 2), "{f:?}");
            assert_eq!(out, src);
        }
    }

    #[test]
    fn prescale_repeats_each_pixel() {
        let src = frame(&[K, W]);
        let mut out = Vec::new();
        assert_eq!(VideoFilter::Prescale3x.apply(&src, 2, 1, &mut out), (6, 3));
        let row = [K, K, K, W, W, W];
        assert_eq!(cells(&out), [row, row, row].concat());
    }

    #[test]
    fn lcd_grid_dims_three_cells_of_four() {
        let src = frame(&[[200, 100, 50, 255]]);
        let mut out = Vec::new();
        assert_eq!(VideoFilter::LcdGrid.apply(&src, 1, 1, &mut out), (2, 2));
        // 85 % = 216 / 255: 200 → 169, 100 → 84, 50 → 42.
        let dim = [169, 84, 42, 255];
        assert_eq!(cells(&out), [[200, 100, 50, 255], dim, dim, dim]);
    }

    #[test]
    fn scale2x_rounds_a_diagonal_and_keeps_flat_areas() {
        // A white diagonal on black: the corner between two white neighbours
        // fills in, a uniform frame stays uniform.
        let src = frame(&[W, K, K, W]);
        let mut out = Vec::new();
        assert_eq!(VideoFilter::Scale2x.apply(&src, 2, 2, &mut out), (4, 4));
        let px = cells(&out);
        // Top-left source pixel (white): its bottom-right cell sits between
        // the black right and black below neighbours, which agree → black.
        assert_eq!(px[4 + 1], K);
        assert_eq!(px[0], W);
        let flat = frame(&[W; 4]);
        VideoFilter::Scale2x.apply(&flat, 2, 2, &mut out);
        assert!(cells(&out).iter().all(|&c| c == W));
    }

    #[test]
    fn scale3x_and_scale4x_have_their_sizes_and_keep_flat_areas() {
        let flat = frame(&[K; 6]);
        let mut out = Vec::new();
        assert_eq!(VideoFilter::Scale3x.apply(&flat, 3, 2, &mut out), (9, 6));
        assert!(cells(&out).iter().all(|&c| c == K));
        assert_eq!(VideoFilter::Scale4x.apply(&flat, 3, 2, &mut out), (12, 8));
        assert!(cells(&out).iter().all(|&c| c == K));
    }

    /// A `width` × `height` frame of `0xAARRGGBB` pixels through `filter`,
    /// back as `0xAARRGGBB` rows.
    fn sai_rows(filter: VideoFilter, width: usize, argb: &[u32]) -> Vec<Vec<u32>> {
        let src: Vec<u8> = argb.iter().flat_map(|p| p.to_le_bytes()).collect();
        let mut out = Vec::new();
        let (w, _) = filter.apply(&src, width, argb.len() / width, &mut out);
        out.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect::<Vec<_>>()
            .chunks(w)
            .map(<[u32]>::to_vec)
            .collect()
    }

    #[test]
    fn the_2xsai_family_matches_its_worked_examples() {
        // Small frames whose every pixel meets the edge clamp. The filters
        // gave Mesen2's pictures to the pixel on three commercial scenes.
        const X: u32 = 0xFF11_3355;
        const Y: u32 = 0xFFEE_AA01;
        const Z: u32 = 0xFF00_FF7F;
        let checker = [X, Y, Y, X];
        assert_eq!(
            sai_rows(VideoFilter::TwoXSai, 2, &checker),
            [
                [X, 0xFF7F_6E2B, Y, Y],
                [0xFF7F_6E2B, Y, 0xFF7F_6E2B, 0xFF7F_6E2B],
                [Y, 0xFF7F_6E2B, X, X],
                [Y, 0xFF7F_6E2B, X, X],
            ]
        );
        assert_eq!(
            sai_rows(VideoFilter::Super2xSai, 2, &checker),
            [
                [X, 0xFF7F_6E2B, Y, Y],
                [Y, 0xFF7F_6E2B, X, X],
                [Y, 0xFF7F_6E2B, X, X],
                [Y, 0xFF7F_6E2B, X, X],
            ]
        );
        assert_eq!(
            sai_rows(VideoFilter::SuperEagle, 2, &checker),
            [
                [X, Y, 0xFFD2_9B0B, 0xFFD2_9B0B],
                [Y, X, 0xFF2C_414A, 0xFF2C_414A],
                [0xFFD2_9B0B, 0xFF2C_414A, X, X],
                [0xFFD2_9B0B, 0xFF2C_414A, X, X],
            ]
        );
        let strip = [X, Y, Z, Z, X, Y];
        assert_eq!(
            sai_rows(VideoFilter::TwoXSai, 3, &strip),
            [
                [X, X, Y, 0xFF77_D440, Z, Z],
                [0xFF08_996A, X, 0xFF7F_6E2B, Y, 0xFF77_D440, 0xFF77_D440],
                [Z, 0xFF08_996A, X, Y, Y, Y],
                [Z, 0xFF08_996A, X, 0xFF7F_6E2B, Y, Y],
            ]
        );
        assert_eq!(
            sai_rows(VideoFilter::Super2xSai, 3, &strip),
            [
                [X, X, Y, Y, Z, Z],
                [Z, X, X, Y, Y, Y],
                [Z, 0xFF08_996A, X, 0xFFB6_8C16, Y, Y],
                [Z, 0xFF08_996A, X, 0xFF7F_6E2B, Y, Y],
            ]
        );
        assert_eq!(
            sai_rows(VideoFilter::SuperEagle, 3, &strip),
            [
                [X, 0xFF48_5040, Y, 0xFFB2_BF20, 0xFF1D_F46F, 0xFF1D_F46F],
                [0xFF0C_665F, X, 0xFFB6_8C16, Y, 0xFFD0_B410, 0xFFD0_B410],
                [0xFF02_E579, 0xFF0E_4C5A, 0xFF2C_414A, 0xFFD2_9B0B, Y, Y],
                [0xFF02_E579, 0xFF0E_4C5A, 0xFF2C_414A, 0xFFD2_9B0B, Y, Y],
            ]
        );
    }

    #[test]
    fn aspect_ratios_follow_emusettings() {
        let base = 256.0 / 224.0;
        let r = |a: AspectRatio, pal| a.ratio(1.5, pal, 256, 224);
        assert!((r(AspectRatio::NoStretching, false) - base).abs() < 1e-12);
        assert!((r(AspectRatio::Auto, false) - base * 8.0 / 7.0).abs() < 1e-12);
        assert!((r(AspectRatio::Auto, true) - base * 11.0 / 8.0).abs() < 1e-12);
        assert!((r(AspectRatio::Pal, false) - base * 11.0 / 8.0).abs() < 1e-12);
        assert!((r(AspectRatio::Standard, true) - 4.0 / 3.0).abs() < 1e-12);
        assert!((r(AspectRatio::Widescreen, true) - 16.0 / 9.0).abs() < 1e-12);
        assert!((r(AspectRatio::Custom, false) - 1.5).abs() < 1e-12);
        assert!((AspectRatio::Custom.ratio(9.0, false, 256, 224) - 5.0).abs() < 1e-12);
    }

    /// `0xAARRGGBB` cells, as frame bytes.
    fn argb(cells: &[u32]) -> Vec<u8> {
        cells
            .iter()
            .flat_map(|&p| {
                let [a, r, g, b] = p.to_be_bytes();
                [r, g, b, a]
            })
            .collect()
    }

    #[test]
    fn xbrz_matches_its_worked_examples() {
        let x2 = |cells: &[u32], w: usize, h: usize| {
            let mut out = Vec::new();
            assert_eq!(
                VideoFilter::Xbrz2x.apply(&argb(cells), w, h, &mut out),
                (w * 2, h * 2)
            );
            out
        };
        // A dark pixel on white: four corner blends of 21/100.
        let (wh, bk) = (0xFFFF_FFFF, 0xFF00_0000);
        let mut want = vec![wh; 36];
        for i in [14, 15, 20, 21] {
            want[i] = 0xFF35_3535;
        }
        assert_eq!(x2(&[wh, wh, wh, wh, bk, wh, wh, wh, wh], 3, 3), argb(&want));
        // A 45° red/blue edge: 1/2 diagonals inside, 1/4-3/4 lines at the
        // clamped top and left borders.
        let (rd, bl) = (0xFFFF_0000, 0xFF00_00FF);
        let (q1, q3, h) = (0xFFBF_003F, 0xFF3F_00BF, 0xFF7F_007F);
        let src = [
            rd, rd, rd, bl, rd, rd, bl, bl, rd, bl, bl, bl, bl, bl, bl, bl,
        ];
        let want = [
            [rd, rd, rd, rd, rd, q1, bl, bl],
            [rd, rd, rd, rd, rd, q3, bl, bl],
            [rd, rd, rd, rd, h, bl, bl, bl],
            [rd, rd, rd, h, bl, bl, bl, bl],
            [rd, rd, h, bl, bl, bl, bl, bl],
            [q1, q3, bl, bl, bl, bl, bl, bl],
            [bl; 8],
            [bl; 8],
        ];
        assert_eq!(x2(&src, 4, 4), argb(want.as_flattened()));
        // A ramp of near colours, which the distance's quantisation decides.
        let (a, b, c, d) = (0xFF10_2030, 0xFF40_5060, 0xFF70_8090, 0xFFA0_B0C0);
        let src = [a, b, c, d, a, a, b, c, a, a, a, b];
        let want: [[u32; 8]; 6] = [
            [a, a, 0xFF34_4454, b, 0xFF64_7484, c, d, d],
            [a, a, 0xFF1C_2C3C, b, 0xFF4C_5C6C, c, 0xFF88_98A8, d],
            [a, a, a, 0xFF28_3848, b, 0xFF58_6878, c, c],
            [a, a, a, a, 0xFF28_3848, b, 0xFF4C_5C6C, 0xFF64_7484],
            [a, a, a, a, a, 0xFF34_4454, b, b],
            [a, a, a, a, a, 0xFF1C_2C3C, b, b],
        ];
        assert_eq!(x2(&src, 4, 3), argb(want.as_flattened()));
    }

    #[test]
    fn xbrz_distance_quantises_and_is_not_symmetric() {
        assert!((xbrz_dist_rgb(0xFF12_3456, 0xFF12_3456) - 1.0).abs() < 1e-12);
        assert!((xbrz_dist_rgb(0xFF00_0000, 0xFFFF_FFFF) - 255.0).abs() < 1e-12);
        assert!((xbrz_dist_rgb(0, 0x0002_0000) - 1.845_218_54).abs() < 1e-6);
        assert!((xbrz_dist_rgb(0x0002_0000, 0) - 1.141_591_55).abs() < 1e-6);
    }
}
