//! Display filters and aspect ratios: Mesen2's Settings ▸ Video Filter and
//! Settings ▸ Aspect Ratio (`ScaleFilter.cpp`, `EmuSettings::GetAspectRatio`).
//!
//! Both act on the picture a front-end shows, after the emulation: the frame
//! `Emulator::render_frame_rgba` returns is untouched. The behaviour follows
//! Mesen2; the code is luna's own, written from the algorithms (for NTSC
//! (blargg), xBRZ, `HQx` and the 2xSaI family, from a functional
//! description of Mesen2's, checked identical to its pictures). NTSC
//! (Bisqwit), NES-only in Mesen2, is listed so a menu can show Mesen2's
//! whole list, and reports [`VideoFilter::is_available`] `false`.

/// One entry of Mesen2's Video Filter menu, in its order (`VideoFilterType`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum VideoFilter {
    /// The frame as rendered.
    #[default]
    None,
    /// NTSC composite signal (blargg's `snes_ntsc`), 602 pixels wide.
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
    /// HQ 2x (Maxim Stepin).
    Hq2x,
    /// HQ 3x (Maxim Stepin).
    Hq3x,
    /// HQ 4x (Maxim Stepin).
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
                | Self::NtscBlargg
                | Self::LcdGrid
                | Self::Hq2x
                | Self::Hq3x
                | Self::Hq4x
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
    /// filter that is not available (it leaves the frame as rendered) and
    /// for NTSC, whose width is not a whole multiple (see
    /// [`Self::output_size`]).
    pub const fn scale(self) -> usize {
        match self {
            Self::LcdGrid
            | Self::Scale2x
            | Self::TwoXSai
            | Self::Super2xSai
            | Self::SuperEagle
            | Self::Xbrz2x
            | Self::Hq2x
            | Self::Prescale2x => 2,
            Self::Xbrz3x | Self::Hq3x | Self::Scale3x | Self::Prescale3x => 3,
            Self::Xbrz4x | Self::Hq4x | Self::Scale4x | Self::Prescale4x => 4,
            Self::Xbrz5x => 5,
            Self::Xbrz6x | Self::Prescale6x => 6,
            Self::Prescale8x => 8,
            Self::Prescale10x => 10,
            _ => 1,
        }
    }

    /// The size of the filtered picture of a `width` × `height` frame.
    /// NTSC makes every row [`NTSC_WIDTH`] wide and doubles the rows of a
    /// low-res (256-wide) frame.
    pub const fn output_size(self, width: usize, height: usize) -> (usize, usize) {
        match self {
            Self::NtscBlargg if width >= 512 => (NTSC_WIDTH, height),
            Self::NtscBlargg => (NTSC_WIDTH, height * 2),
            _ => (width * self.scale(), height * self.scale()),
        }
    }

    /// Filter a `width` × `height` RGBA frame into `out` (resized to fit),
    /// returning the output size. A filter that is not available copies the
    /// frame unchanged. [`Self::apply_at`] with frame 0.
    pub fn apply(
        self,
        src: &[u8],
        width: usize,
        height: usize,
        out: &mut Vec<u8>,
    ) -> (usize, usize) {
        self.apply_at(src, width, height, 0, out)
    }

    /// [`Self::apply`] for PPU frame `frame`, whose parity sets the NTSC
    /// filter's burst phase (the other filters ignore it).
    pub fn apply_at(
        self,
        src: &[u8],
        width: usize,
        height: usize,
        frame: u64,
        out: &mut Vec<u8>,
    ) -> (usize, usize) {
        let s = self.scale();
        let (ow, oh) = self.output_size(width, height);
        out.resize(ow * oh * 4, 0);
        let pixels = &src[..width * height * 4];
        match self {
            Self::NtscBlargg => ntsc(pixels, width, height, frame, out),
            Self::LcdGrid => lcd_grid(pixels, width, height, LCD_GRID_BRIGHTNESS, out),
            Self::Xbrz2x | Self::Xbrz3x | Self::Xbrz4x | Self::Xbrz5x | Self::Xbrz6x => {
                xbrz(s, pixels, width, height, out);
            }
            Self::Hq2x | Self::Hq3x | Self::Hq4x => {
                hqx(s, HqxYuv::default(), pixels, width, height, out);
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
        (ow, oh)
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

// --- NTSC (blargg) ---------------------------------------------------------
//
// The composite signal of a SNES on an NTSC set, as Mesen2's `snes_ntsc`
// gives it: every reduced SNES colour is turned once into the share it
// lends to its neighbours (for each burst phase and each column alignment),
// and a row is then the sum of those shares. All the float work happens
// when the table is built, in single precision, in the reference's order
// of operations: the table must come out bit for bit.

/// Mesen2's NTSC filter settings, each in −1 … +1 (the UI's −100 … 100
/// divided by 100); all zero is Mesen2's default and its Composite preset.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct NtscSettings {
    /// Hue: −1 = −180°, +1 = +180°.
    pub hue: f32,
    /// Saturation: −1 = grey, +1 = twice.
    pub saturation: f32,
    /// Contrast: −1 = half, +1 = one and a half.
    pub contrast: f32,
    /// Brightness.
    pub brightness: f32,
    /// Edge enhancement (+) or blur (−).
    pub sharpness: f32,
    /// Gamma: −1 = 1.5 (dark), +1 = 0.5 (light).
    pub gamma: f32,
    /// Luma bandwidth.
    pub resolution: f32,
    /// Colour artefacts from colour changes.
    pub artifacts: f32,
    /// Colour artefacts from brightness changes.
    pub fringing: f32,
    /// Chroma bandwidth reduction.
    pub bleed: f32,
    /// Average the three burst phases (forced on when both `artifacts`
    /// and `fringing` are −1).
    pub merge_fields: bool,
}

/// Width of an NTSC row, low-res or hi-res: 86 chunks of 7 pixels.
pub const NTSC_WIDTH: usize = 602;
/// Packed words per table entry (126 used).
const NTSC_ENTRY: usize = 128;
/// Words of one burst phase in an entry.
const NTSC_KERNEL: usize = 42;
/// A field's lowest bit, in each of the three packed channels.
const NTSC_BUILDER: u32 = 0x0020_0802;
/// `pack(256, 256, 256)`.
const NTSC_BIAS: u32 = NTSC_BUILDER * 256;
/// Per column alignment: start in the kernel bank, sign, composite weights.
const NTSC_ALIGN: [(usize, f32, [f32; 4]); 3] = [
    (353, 1.0, [1.0, 1.0, 0.6667, 0.0]),
    (22, -1.0, [0.3333, 1.0, 1.0, 0.3333]),
    (154, 1.0, [0.0, 0.6667, 1.0, 1.0]),
];

/// The YIQ decoder: the (I, Q) weights of red, green and blue.
const NTSC_DECODER: [f32; 6] = [0.956, 0.621, -0.272, -0.647, -1.105, 1.702];

/// Three channels in one word: red in bits 21–30, green 11–20, blue 1–10.
const fn ntsc_pack([r, g, b]: [i32; 3]) -> u32 {
    ((r as u32) << 21) | ((g as u32) << 11) | ((b as u32) << 1)
}

/// YIQ to truncated RGB through one burst's decoder matrix.
// Plain products and sums, as the reference rounds them (no fused
// multiply-add).
#[allow(clippy::suboptimal_flops)]
fn ntsc_rgb(y: f32, i: f32, q: f32, m: &[f32; 6]) -> [i32; 3] {
    [
        (y + m[0] * i + m[1] * q) as i32,
        (y + m[2] * i + m[3] * q) as i32,
        (y + m[4] * i + m[5] * q) as i32,
    ]
}

/// The precomputed shares of every reduced colour (8192 × 128 words).
#[derive(Clone)]
pub struct NtscTable {
    words: Box<[u32]>,
}

impl std::fmt::Debug for NtscTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NtscTable")
    }
}

// The reference rounds every product and sum on its own: a fused
// multiply-add gives another table.
#[allow(clippy::suboptimal_flops)]
impl NtscTable {
    /// Build the table for `s`.
    #[must_use]
    pub fn new(s: &NtscSettings) -> Self {
        use std::f32::consts::PI;
        // Luma: a windowed sinc with roll-off, in t[33..66]; chroma: a
        // Gaussian, in t[0..33].
        let mut t = [0f32; 66];
        let rolloff = 1.0 + s.sharpness * 0.032;
        let n = 32.0f32;
        let pow_a_n = rolloff.powf(n);
        let r1 = s.resolution + 1.0;
        let to_angle = ((PI / n) * 0.20) * (r1 * r1 + 1.0);
        t[49] = 32.0;
        for i in 0..33 {
            let x = i as i32 - 16;
            let angle = x as f32 * to_angle;
            if x != 0 || !(0.981..=1.056).contains(&pow_a_n) {
                let rc = rolloff * angle.cos();
                let num = ((1.0 - rc) - (pow_a_n * (n * angle).cos()))
                    + ((pow_a_n * rolloff) * (31.0 * angle).cos());
                let den = ((1.0 - rc) - rc) + (rolloff * rolloff);
                t[33 + i] = (num / den) - 0.5;
            }
        }
        let mut sum = 0f32;
        for i in 0..33 {
            let u = ((PI * 2.0) / 32.0) * i as f32;
            let w = (0.42 - (0.5 * u.cos())) + (0.08 * (u * 2.0).cos());
            t[33 + i] *= w;
            sum += t[33 + i];
        }
        let scale = 1.0 / sum;
        for v in &mut t[33..] {
            *v *= scale;
        }
        let mut c = s.bleed;
        if c < 0.0 {
            c *= c;
            c *= c;
            c *= c;
            c *= -30.0f32 / 0.65;
        }
        c = (-0.03125) - ((0.65 * (-0.03125)) * c);
        for i in -16i32..=16 {
            t[(16 + i) as usize] = ((i * i) as f32 * c).exp();
        }
        for parity in 0..2 {
            let sum: f32 = t[..33].iter().skip(parity).step_by(2).sum();
            let scale = 1.0 / sum;
            for v in t[..33].iter_mut().skip(parity).step_by(2) {
                *v *= scale;
            }
        }
        // The 8 → 7 resampling bank: seven phases of both kernels. The
        // remainder runs on from the chroma half into the luma half.
        let mut bank = [0f32; 7 * 66];
        let mut weight = 1.0f32;
        for block in bank.chunks_exact_mut(66) {
            weight -= 0.125;
            let mut rest = 0f32;
            for (k, &cur) in block.iter_mut().zip(&t) {
                let m = cur * weight;
                *k = m + rest;
                rest = cur - m;
            }
        }

        let step = 1.0f32 / 31.0;
        let gamma = 1.1333 - s.gamma * 0.5;
        let contrast = s.contrast * 64.0 + 128.0;
        let brightness = s.brightness * 64.0 + 256.5;
        let level: [f32; 32] =
            std::array::from_fn(|i| (i as f32 * step).powf(gamma) * contrast + brightness);

        let mut hue = s.hue * PI + (PI / 180.0) * 15.0;
        hue += (PI / 180.0) * -15.0;
        let sat = s.saturation + 1.0;
        let (mut sn, mut cs) = (hue.sin() * sat, hue.cos() * sat);
        let mut matrix = [[0f32; 6]; 3];
        for m in &mut matrix {
            for (pair, d) in m.chunks_exact_mut(2).zip(NTSC_DECODER.chunks_exact(2)) {
                pair[0] = d[0] * cs - d[1] * sn;
                pair[1] = d[0] * sn + d[1] * cs;
            }
            let t = sn * -0.5 - cs * 0.866_025;
            cs = sn * 0.866_025 + cs * -0.5;
            sn = t;
        }

        let mut a = s.artifacts;
        if a > 0.0 {
            a *= 0.5;
        }
        let artifacts = a * 1.0 + 1.0;
        let mut f = s.fringing;
        if f > 0.0 {
            f *= 1.0;
        }
        let fringing = f * 1.0 + 1.0;
        let merge = s.merge_fields || (s.artifacts <= -1.0 && s.fringing <= -1.0);

        let mut words = vec![0u32; 8192 * NTSC_ENTRY].into_boxed_slice();
        for (e, w) in words.chunks_exact_mut(NTSC_ENTRY).enumerate() {
            let rr = level[(e >> 8) & 0x1E];
            let gg = level[(e >> 4) & 0x1F];
            let bb = level[(e << 1) & 0x1E];
            let y = rr * 0.299 + gg * 0.587 + bb * 0.114;
            let (mut i, mut q) = (
                rr * 0.596 - gg * 0.275 - bb * 0.321,
                rr * 0.212 - gg * 0.523 + bb * 0.311,
            );
            let color = ntsc_pack(ntsc_rgb(y, i, q, &matrix[0]));
            let y = y - 256.5;
            for (burst, m) in matrix.iter().enumerate() {
                for (align, &(start, ng, k)) in NTSC_ALIGN.iter().enumerate() {
                    let yy = (y * fringing) * ng;
                    let (ic0, qc1) = ((i + yy) * k[0], (q + yy) * k[1]);
                    let (ic2, qc3) = ((i - yy) * k[2], (q - yy) * k[3]);
                    let fac = artifacts * ng;
                    let ii = i * fac;
                    let (yc0, yc2) = ((y + ii) * k[0], (y - ii) * k[2]);
                    let qq = q * fac;
                    let (yc1, yc3) = ((y + qq) * k[1], (y - qq) * k[3]);
                    let mut p = start;
                    for n in 0..14 {
                        let k = &bank[p..p + 37];
                        let ic = k[0] * ic0 + k[2] * ic2;
                        let qc = k[1] * qc1 + k[3] * qc3;
                        let yc = k[33] * yc0 + k[34] * yc1 + k[35] * yc2 + k[36] * yc3 + 256.5;
                        p = if p < 396 { p + 65 } else { p - 398 };
                        w[burst * NTSC_KERNEL + align * 14 + n] =
                            ntsc_pack(ntsc_rgb(yc, ic, qc, m)).wrapping_sub(NTSC_BIAS);
                    }
                }
                let t = i * -0.5 - q * -0.866_025;
                q = i * -0.866_025 + q * -0.5;
                i = t;
            }
            if merge {
                let avg =
                    |x: u32, y: u32| (x.wrapping_add(y).wrapping_sub((x ^ y) & NTSC_BUILDER)) >> 1;
                for n in 0..NTSC_KERNEL {
                    let p0 = w[n].wrapping_add(NTSC_BIAS);
                    let p1 = w[42 + n].wrapping_add(NTSC_BIAS);
                    let p2 = w[84 + n].wrapping_add(NTSC_BIAS);
                    w[n] = avg(p0, p1).wrapping_sub(NTSC_BIAS);
                    w[42 + n] = avg(p1, p2).wrapping_sub(NTSC_BIAS);
                    w[84 + n] = avg(p2, p0).wrapping_sub(NTSC_BIAS);
                }
            }
            // A flat area must come out exactly `color`: the six words of
            // one output pixel are corrected to sum to it.
            for k in w[..3 * NTSC_KERNEL].chunks_exact_mut(NTSC_KERNEL) {
                for i in 0..7 {
                    let (t1, t2) = ((i + 12) % 14 + 14, (i + 10) % 14 + 28);
                    let e = [i, t1, t2, i + 7, i + 19, i + 31]
                        .iter()
                        .fold(color, |e, &j| e.wrapping_sub(k[j]));
                    let f = ((e.wrapping_add(0x0040_1004) >> 2) & 0x0FE3_F8FE)
                        .wrapping_sub(0x0802_0080);
                    for j in [i + 31, i + 19, i + 7] {
                        k[j] = k[j].wrapping_add(f);
                    }
                    k[i] = k[i].wrapping_add(e.wrapping_sub(f.wrapping_mul(3)));
                }
            }
        }
        Self { words }
    }

    /// The table for Mesen2's default settings, built once.
    pub fn default_table() -> &'static Self {
        static TABLE: std::sync::OnceLock<NtscTable> = std::sync::OnceLock::new();
        TABLE.get_or_init(|| Self::new(&NtscSettings::default()))
    }

    /// The 42 words of colour `c` (15-bit BGR) at burst `phase`.
    fn kernel(&self, c: u16, phase: usize) -> &[u32] {
        let (r, g, b) = (c & 31, (c >> 5) & 31, (c >> 10) & 31);
        let e = usize::from(((r >> 1) << 9) | (g << 4) | (b >> 1));
        let at = e * NTSC_ENTRY + phase * NTSC_KERNEL;
        &self.words[at..at + NTSC_KERNEL]
    }

    /// One row of 256 (low-res) or 512 (hi-res) SNES colours into
    /// [`NTSC_WIDTH`] pixels `0xAARRGGBB`, at burst `phase` (0 … 2).
    pub fn blit_row(&self, row: &[u16], phase: usize, out: &mut [u32]) {
        if row.len() >= 512 {
            self.blit_hires(&row[..512], phase, out);
        } else {
            self.blit_lores(&row[..256], phase, out);
        }
    }

    fn blit_lores(&self, row: &[u16], phase: usize, out: &mut [u32]) {
        const TAPS: [[usize; 6]; 7] = [
            [0, 19, 31, 7, 26, 38],
            [1, 20, 32, 8, 27, 39],
            [2, 14, 33, 9, 21, 40],
            [3, 15, 34, 10, 22, 41],
            [4, 16, 28, 11, 23, 35],
            [5, 17, 29, 12, 24, 36],
            [6, 18, 30, 13, 25, 37],
        ];
        let black = self.kernel(0, phase);
        let (mut cur, mut prev) = ([black, black, self.kernel(row[0], phase)], [black; 3]);
        let emit = |cur: &[&[u32]; 3], prev: &[&[u32]; 3], x: usize| {
            let t = TAPS[x];
            let s = cur[0][t[0]]
                .wrapping_add(cur[1][t[1]])
                .wrapping_add(cur[2][t[2]])
                .wrapping_add(prev[0][t[3]])
                .wrapping_add(prev[1][t[4]])
                .wrapping_add(prev[2][t[5]]);
            ntsc_extract(s, 1)
        };
        let colours = row[1..]
            .chunks_exact(3)
            .chain(std::iter::once(&[0u16; 3][..]));
        for (chunk, px) in colours.zip(out.chunks_exact_mut(7)) {
            for (slot, xs) in [0..2, 2..4, 4..7].into_iter().enumerate() {
                prev[slot] = cur[slot];
                cur[slot] = self.kernel(chunk[slot], phase);
                for x in xs {
                    px[x] = emit(&cur, &prev, x);
                }
            }
        }
    }

    fn blit_hires(&self, row: &[u16], phase: usize, out: &mut [u32]) {
        const TAPS: [[usize; 12]; 7] = [
            [0, 6, 19, 18, 31, 30, 7, 13, 26, 25, 38, 37],
            [1, 0, 20, 19, 32, 31, 8, 7, 27, 26, 39, 38],
            [2, 1, 14, 20, 33, 32, 9, 8, 21, 27, 40, 39],
            [3, 2, 15, 14, 34, 33, 10, 9, 22, 21, 41, 40],
            [4, 3, 16, 15, 28, 34, 11, 10, 23, 22, 35, 41],
            [5, 4, 17, 16, 29, 28, 12, 11, 24, 23, 36, 35],
            [6, 5, 18, 17, 30, 29, 13, 12, 25, 24, 37, 36],
        ];
        let black = self.kernel(0, phase);
        let mut cur = [black; 6];
        cur[4] = self.kernel(row[0], phase);
        cur[5] = self.kernel(row[1], phase);
        let mut prev = [black; 6];
        let emit = |cur: &[&[u32]; 6], prev: &[&[u32]; 6], x: usize| {
            let t = TAPS[x];
            let s = (0..6).fold(0u32, |s, k| {
                s.wrapping_add(cur[k][t[k]]).wrapping_add(prev[k][t[6 + k]])
            });
            ntsc_extract(s, 0)
        };
        let colours = row[2..]
            .chunks_exact(6)
            .chain(std::iter::once(&[0u16; 6][..]));
        for (chunk, px) in colours.zip(out.chunks_exact_mut(7)) {
            for slot in 0..6 {
                prev[slot] = cur[slot];
                cur[slot] = self.kernel(chunk[slot], phase);
                px[slot] = emit(&cur, &prev, slot);
            }
            px[6] = emit(&cur, &prev, 6);
        }
    }
}

/// Clamp the three packed channels of a summed word and extract them as
/// `0xFFRRGGBB`; `shift` is 1 for a low-res sum, 0 for a hi-res one.
const fn ntsc_extract(mut s: u32, shift: u32) -> u32 {
    let u = (s >> (9 - shift)) & 0x0030_0C03;
    let mut c = 0x2028_0A02u32.wrapping_sub(u);
    s |= c;
    c = c.wrapping_sub(u);
    s &= c;
    let s = s >> (1 - shift);
    0xFF00_0000 | (((s >> 20) & 0xFF) << 16) | (((s >> 10) & 0xFF) << 8) | (s & 0xFF)
}

/// The NTSC filter on a `width` × `height` RGBA frame (256 wide low-res or
/// 512 wide hi-res), as Mesen2 shows it with its default overscan: low-res
/// rows are output twice. The burst phase starts at 0 on an even `frame`
/// and 1 on an odd one, then follows the row's place in Mesen2's 239-row
/// picture (a 224-row frame sits 7 rows down).
fn ntsc(src: &[u8], width: usize, height: usize, frame: u64, out: &mut [u8]) {
    let table = NtscTable::default_table();
    let hires = width >= 512;
    let lines = if hires { height / 2 } else { height };
    let pad = if lines == 239 { 0 } else { 7 } * if hires { 2 } else { 1 };
    let start = (frame % 2) as usize;
    let mut row = vec![0u16; width];
    let mut line = vec![0u32; NTSC_WIDTH];
    let copies = if hires { 1 } else { 2 };
    for (y, (src, dst)) in src
        .chunks_exact(width * 4)
        .zip(out.chunks_exact_mut(NTSC_WIDTH * 4 * copies))
        .enumerate()
    {
        for (c, p) in row.iter_mut().zip(src.chunks_exact(4)) {
            *c = u16::from(p[0] >> 3) | u16::from(p[1] >> 3) << 5 | u16::from(p[2] >> 3) << 10;
        }
        table.blit_row(&row, (start + pad + y) % 3, &mut line);
        for (d, &px) in dst.chunks_exact_mut(4).zip(line.iter().cycle()) {
            d.copy_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, px as u8, 0xFF]);
        }
    }
}

// --- HQx -------------------------------------------------------------------
//
// HQ2x / HQ3x / HQ4x (Maxim Stepin): each source pixel becomes an s × s
// block chosen by which of its eight neighbours differ from it in YUV. The
// 256 patterns per scale are data (`hqx_tables.txt`): each sub-pixel copies
// the centre or blends it with neighbours, some only when two neighbours
// differ.

/// How `HQx` turns a colour into YUV for its similarity test. Mesen2's
/// picture depends on the machine it was built for: the two differ on a
/// few percent of the pixels, near the thresholds. luna gives the same one
/// everywhere (`f64::mul_add` is exact on every target).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum HqxYuv {
    /// Fused multiply-adds, negative U / V clamped to 0: what Mesen2's
    /// aarch64 builds compute, checked identical to their pictures.
    #[default]
    Fused,
    /// Each product rounded on its own, negative U / V kept: the
    /// algorithm as written, as an x86-64 build computes it.
    Separate,
}

/// `0x00YYUUVV` of an `0xAARRGGBB` colour. Pure white gives 0, as in the
/// reference, whose table never fills that entry.
// Plain products and sums in the separate variant, as the reference
// rounds them.
#[allow(clippy::suboptimal_flops)]
fn hqx_yuv(c: u32, variant: HqxYuv) -> u32 {
    let c = c & 0x00FF_FFFF;
    if c == 0x00FF_FFFF {
        return 0;
    }
    let (r, g, b) = (
        f64::from(c >> 16),
        f64::from((c >> 8) & 0xFF),
        f64::from(c & 0xFF),
    );
    let (y, u, v) = match variant {
        HqxYuv::Separate => {
            let y = 0.299 * r + 0.587 * g + 0.114 * b;
            let u = -0.169 * r - 0.331 * g + 0.5 * b;
            let v = 0.5 * r - 0.419 * g - 0.081 * b;
            (y as i32, u as i32 + 128, v as i32 + 128)
        }
        HqxYuv::Fused => {
            let y = 0.114f64.mul_add(b, 0.299f64.mul_add(r, 0.587 * g));
            let u = 0.5f64.mul_add(b, (-0.169f64).mul_add(r, -0.331 * g));
            let v = (-0.081f64).mul_add(b, 0.5f64.mul_add(r, -0.419 * g));
            (
                y as u32 as i32,
                u as u32 as i32 + 128,
                v as u32 as i32 + 128,
            )
        }
    };
    ((y as u32) << 16) | ((u as u32) << 8) | v as u32
}

/// Whether two YUV values are far enough apart to count as an edge.
fn hqx_differ(a: u32, b: u32) -> bool {
    let d = |shift: u32| (((a >> shift) & 0xFF) as i32 - ((b >> shift) & 0xFF) as i32).abs();
    d(16) > 48 || d(8) > 7 || d(0) > 6
}

/// The weights and the divisor's shift of blend kind `k` (1 … 10).
const HQX_BLEND: [([u32; 3], u32); 11] = [
    ([1, 0, 0], 0),
    ([3, 1, 0], 2),
    ([2, 1, 1], 2),
    ([7, 1, 0], 3),
    ([2, 7, 7], 4),
    ([1, 1, 0], 1),
    ([5, 2, 1], 3),
    ([6, 1, 1], 3),
    ([5, 3, 0], 3),
    ([2, 3, 3], 3),
    ([14, 1, 1], 4),
];

/// One way to fill a sub-pixel: blend kind (0 = copy) of up to three
/// neighbours (1 … 9).
#[derive(Copy, Clone, Debug, Default)]
struct HqxPick {
    kind: u8,
    of: [u8; 3],
}

/// A sub-pixel: `then`, or `other` when `test` names two neighbours that do
/// not differ.
#[derive(Copy, Clone, Debug, Default)]
struct HqxCell {
    test: Option<(u8, u8)>,
    then: HqxPick,
    other: HqxPick,
}

/// The blocks of every pattern for scale 2, 3 and 4.
fn hqx_table(s: usize) -> &'static [Vec<HqxCell>] {
    static TABLES: std::sync::OnceLock<[Vec<Vec<HqxCell>>; 3]> = std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        let pick = |t: &str| -> HqxPick {
            if let Some(n) = t.strip_prefix('w') {
                return HqxPick {
                    kind: 0,
                    of: [n.parse().expect("hqx table"), 0, 0],
                };
            }
            let (kind, args) = t[1..]
                .trim_end_matches(')')
                .split_once('(')
                .expect("hqx table");
            let mut of = [0u8; 3];
            for (o, a) in of.iter_mut().zip(args.split(',')) {
                *o = a.parse().expect("hqx table");
            }
            HqxPick {
                kind: kind.parse().expect("hqx table"),
                of,
            }
        };
        let mut tables: [Vec<Vec<HqxCell>>; 3] = Default::default();
        let mut current = 0;
        for line in include_str!("hqx_tables.txt").lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix("[hq") {
                current = usize::from(name.as_bytes()[0] - b'2');
                tables[current] = vec![Vec::new(); 256];
                continue;
            }
            let (patterns, cells) = line.split_once(" :: ").expect("hqx table");
            let block: Vec<HqxCell> = cells
                .split([' ', ';'])
                .filter(|t| !t.is_empty())
                .map(|t| match t.strip_prefix('D') {
                    Some(rest) => {
                        let (pair, choice) = rest.split_once(':').expect("hqx table");
                        let (a, b) = choice.split_once('|').expect("hqx table");
                        let p = pair.as_bytes();
                        HqxCell {
                            test: Some((p[0] - b'0', p[1] - b'0')),
                            then: pick(a),
                            other: pick(b),
                        }
                    }
                    None => HqxCell {
                        test: None,
                        then: pick(t),
                        other: HqxPick::default(),
                    },
                })
                .collect();
            for p in patterns.split(',') {
                tables[current][p.parse::<usize>().expect("hqx table")].clone_from(&block);
            }
        }
        tables
    });
    &tables[s - 2]
}

/// `HQx` at scale `s` (2 … 4) of a `width` × `height` RGBA frame into `out`
/// (`s·width` × `s·height` RGBA), testing similarity with `yuv`.
pub fn hqx(s: usize, yuv: HqxYuv, src: &[u8], width: usize, height: usize, out: &mut [u8]) {
    let table = hqx_table(s);
    let argb: Vec<u32> = src
        .chunks_exact(4)
        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
        .collect();
    let key: Vec<u32> = argb.iter().map(|&c| hqx_yuv(c, yuv)).collect();
    let row = width * s * 4;
    for y in 0..height {
        let rows = [y.saturating_sub(1), y, (y + 1).min(height - 1)];
        for x in 0..width {
            let cols = [x.saturating_sub(1), x, (x + 1).min(width - 1)];
            // Neighbours 1 … 9, keypad order; index 0 unused.
            let mut at = [0usize; 10];
            for (k, slot) in at[1..].iter_mut().enumerate() {
                *slot = rows[k / 3] * width + cols[k % 3];
            }
            let differ =
                |a: u8, b: u8| hqx_differ(key[at[usize::from(a)]], key[at[usize::from(b)]]);
            let pattern = [1, 2, 3, 4, 6, 7, 8, 9]
                .iter()
                .enumerate()
                .fold(0, |p, (bit, &n)| p | (usize::from(differ(5, n)) << bit));
            let eval = |pick: HqxPick| -> u32 {
                let (weights, shift) = HQX_BLEND[usize::from(pick.kind)];
                (0..4).fold(0, |acc, ch| {
                    let sum: u32 = weights
                        .iter()
                        .zip(pick.of)
                        .filter(|(w, _)| **w != 0)
                        .map(|(w, n)| w * ((argb[at[usize::from(n)]] >> (8 * ch)) & 0xFF))
                        .sum();
                    acc | ((sum >> shift) << (8 * ch))
                })
            };
            for (i, cell) in table[pattern].iter().enumerate() {
                let pick = match cell.test {
                    Some((a, b)) if !differ(a, b) => cell.other,
                    _ => cell.then,
                };
                let c = eval(pick);
                let o = (y * s + i / s) * row + (x * s + i % s) * 4;
                out[o..o + 4].copy_from_slice(&[
                    (c >> 16) as u8,
                    (c >> 8) as u8,
                    c as u8,
                    (c >> 24) as u8,
                ]);
            }
        }
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

    /// FNV-1a 64 over little-endian words, from the basis the reference
    /// hashes were taken with (the standard one with a digit missing).
    fn fnv(words: impl IntoIterator<Item = u32>) -> u64 {
        words
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .fold(0x1465_0FB0_739D_0383, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    #[test]
    fn ntsc_tables_match_the_reference_hashes() {
        let table = |hue, sat, con, bri, sharp, gamma, res, art, fringe, bleed, merge| {
            let s = NtscSettings {
                hue,
                saturation: sat,
                contrast: con,
                brightness: bri,
                sharpness: sharp,
                gamma,
                resolution: res,
                artifacts: art,
                fringing: fringe,
                bleed,
                merge_fields: merge,
            };
            fnv(NtscTable::new(&s).words.iter().copied())
        };
        let z = 0.0;
        assert_eq!(
            table(z, z, z, z, z, z, z, z, z, z, false),
            0xb45d_294e_040d_5056
        );
        assert_eq!(
            table(z, z, z, z, z, z, z, z, z, z, true),
            0xd57c_4a81_536d_39ea
        );
        // S-Video, RGB, Mesen2's Monochrome.
        assert_eq!(
            table(z, z, z, z, 0.2, z, 0.2, -1.0, -1.0, z, false),
            0x45bb_d88a_51bd_2217
        );
        assert_eq!(
            table(z, z, z, z, 0.2, z, 0.7, -1.0, -1.0, -1.0, false),
            0xa2f7_a544_ad30_987a
        );
        assert_eq!(
            table(z, -1.0, z, z, 0.2, z, 0.7, -0.2, -0.2, -0.1, false),
            0xff9e_2cd9_a64c_8134
        );
        assert_eq!(
            table(
                0.13, 0.27, -0.31, 0.11, -0.45, 0.33, -0.6, 0.5, 0.4, 0.7, false
            ),
            0x05eb_5c53_ce94_1886
        );
        assert_eq!(
            table(-1.0, 1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, false),
            0x0b87_e9af_9fbe_9462
        );
        assert_eq!(
            table(
                0.5, -0.5, 0.5, 0.5, -1.0, 1.0, -1.0, -1.0, -0.5, -0.37, true
            ),
            0xd68c_cbfe_c31d_b186
        );
        let t = NtscTable::default_table();
        let white = &t.words[0x1FFF * NTSC_ENTRY..][..4];
        assert_eq!(white, [0x2167_ea0a, 0x21c7_e212, 0x2127_6a02, 0x2107_4206]);
    }

    #[test]
    fn ntsc_rows_match_the_reference_blits() {
        let t = NtscTable::default_table();
        let blit = |w: usize, h: usize, start: usize, px: fn(usize, usize) -> u16| {
            let mut line = vec![0u32; NTSC_WIDTH];
            let mut all = Vec::new();
            for y in 0..h {
                let row: Vec<u16> = (0..w).map(|x| px(x, y)).collect();
                t.blit_row(&row, (start + y) % 3, &mut line);
                all.extend_from_slice(&line);
            }
            all
        };
        let lores = |x: usize, y: usize| ((37 * x + 101 * y + (x * y) % 7) & 0x7FFF) as u16;
        let hires = |x: usize, y: usize| ((19 * x + 53 * y + (x ^ y)) & 0x7FFF) as u16;
        for (start, hash, spot) in [
            (0, 0xec42_e5d8_a543_d5ef, 0xff7e_6266),
            (1, 0x7c16_24a5_5812_d4eb, 0xff80_666a),
            (2, 0x66ae_ad95_ac6f_11e3, 0xff78_6662),
        ] {
            let out = blit(256, 239, start, lores);
            assert_eq!(out[100 * NTSC_WIDTH + 300], spot, "phase {start}");
            assert_eq!(fnv(out), hash, "phase {start}");
        }
        let out = blit(256, 239, 0, lores);
        assert_eq!(
            out[..4],
            [0xff04_0002, 0xff02_0000, 0xff06_0000, 0xff0a_0000]
        );
        for (start, hash, spot) in [
            (0, 0xd577_25d8_543b_e7af, 0xff4a_1e3b),
            (1, 0x2bd3_05b7_a684_97df, 0xff2a_2207),
            (2, 0xe04f_1f9e_6ffd_235c, 0xff5f_250b),
        ] {
            let out = blit(512, 478, start, hires);
            assert_eq!(out[100 * NTSC_WIDTH + 300], spot, "hi-res phase {start}");
            assert_eq!(fnv(out), hash, "hi-res phase {start}");
        }
        // Flat white settles on F6FCF6: the low-res clamp stops green at 252.
        let out = blit(256, 1, 0, |_, _| 0x7FFF);
        assert!(
            out[9..595].iter().all(|&p| p == 0xFFF6_FCF6),
            "{:08x}",
            out[300]
        );
    }

    /// The reference frames' hash helper.
    const fn hqx_h(x: u32, y: u32) -> u32 {
        let mut h = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5BD1_E995);
        h ^ (h >> 15)
    }

    /// The reference frames, as `0xAARRGGBB` words.
    fn hqx_frame(name: &str) -> (usize, usize, Vec<u32>) {
        const PALETTE: [u32; 8] = [
            0xFF00_0000,
            0xFFFF_FFFF,
            0xFFF8_F8F8,
            0xFF20_40C0,
            0xFF20_C040,
            0xFFC0_2020,
            0x80FF_FFFF,
            0xFF80_8080,
        ];
        let (w, h) = match name {
            "F1s" => (5, 3),
            "F4t" => (1, 1),
            "F4n" => (3, 1),
            "F4v" => (1, 4),
            _ => (256, 224),
        };
        let e = |v: u32| v * 8 + v / 4;
        let px = |x: u32, y: u32| match name {
            "F1" | "F1s" => hqx_h(x, y),
            "F2" => {
                let n = hqx_h(x, y);
                let i = if n.is_multiple_of(16) {
                    (n >> 8) % 8
                } else {
                    hqx_h(x / 8, y / 8) % 8
                };
                PALETTE[i as usize]
            }
            "F3" => {
                let (dx, dy) = (x as i32 - 128, y as i32 - 112);
                if x == y {
                    0xFFFF_FFFF
                } else if dx * dx + dy * dy < 3600 {
                    0xFFE0_C000
                } else if ((x + y) / 16) % 2 == 1 {
                    0xFF00_30A0
                } else {
                    0xFF00_0000 + (x & 0xF8) * 65536 + (y & 0xF8) * 256 + 0x40
                }
            }
            "F5" => {
                let c = hqx_h(x / 4, y / 4) & 0x7FFF;
                0xFF00_0000 + e(c & 31) * 65536 + e((c >> 5) & 31) * 256 + e((c >> 10) & 31)
            }
            _ => hqx_h(x + 7, y + 3),
        };
        let v = (0..h as u32)
            .flat_map(|y| (0..w as u32).map(move |x| (x, y)))
            .map(|(x, y)| px(x, y))
            .collect();
        (w, h, v)
    }

    fn fnv_std(words: impl IntoIterator<Item = u32>) -> u64 {
        words
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .fold(0xcbf2_9ce4_8422_2325, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    #[test]
    fn hqx_matches_the_reference_hashes_in_both_yuv_variants() {
        let run = |name: &str, s: usize, yuv: HqxYuv| {
            let (w, h, words) = hqx_frame(name);
            let rgba: Vec<u8> = words
                .iter()
                .flat_map(|&c| [(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8])
                .collect();
            let mut out = vec![0u8; w * h * s * s * 4];
            hqx(s, yuv, &rgba, w, h, &mut out);
            fnv_std(
                out.chunks_exact(4)
                    .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])),
            )
        };
        assert_eq!(fnv_std(hqx_frame("F1").2), 0x69d1_f1ba_5752_3b4a);
        assert_eq!(fnv_std(hqx_frame("F2").2), 0x1833_3c10_879c_42ed);
        assert_eq!(fnv_std(hqx_frame("F3").2), 0xb498_44b0_a991_01d5);
        assert_eq!(fnv_std(hqx_frame("F5").2), 0x7182_b1a2_89aa_7525);
        let fused = [
            (
                "F1",
                [
                    0x89f4_97fe_5686_f3e5,
                    0x135c_590f_fcb8_2a3a,
                    0x9eaf_715c_15a5_a7b6,
                ],
            ),
            (
                "F2",
                [
                    0x1cfc_7f05_0b12_afa4,
                    0x38bd_5c6f_96a4_79d8,
                    0x3cf0_9f9c_10b9_52d5,
                ],
            ),
            (
                "F3",
                [
                    0x0b38_925b_25ee_65e1,
                    0x00c2_900b_aa16_2856,
                    0xf3e3_40e1_f2b2_0021,
                ],
            ),
            (
                "F5",
                [
                    0x651b_f2b9_22df_682f,
                    0x62d7_b50e_467d_5fc9,
                    0xc521_1e8c_2eaf_a83d,
                ],
            ),
            (
                "F1s",
                [
                    0x68f8_5a07_2da2_502f,
                    0x12c0_64ea_a0a3_bc7f,
                    0x2f17_6415_f090_a6c4,
                ],
            ),
        ];
        let separate = [
            (
                "F1",
                [
                    0x5de2_2cb3_50d3_8cd5,
                    0xcc8f_9097_dc6a_fa57,
                    0xb01b_645c_c6cf_d1ce,
                ],
            ),
            (
                "F2",
                [
                    0x9b61_3a3c_a0b7_54de,
                    0xbd4f_04d4_e29b_2f18,
                    0xf2a0_6bc3_dd4a_12d9,
                ],
            ),
            (
                "F3",
                [
                    0xcca9_688e_dbf0_7153,
                    0x7f83_15b3_de82_681d,
                    0x295f_ca08_7d9c_574a,
                ],
            ),
            (
                "F5",
                [
                    0x401f_408d_d6e0_61ac,
                    0x43b8_ff10_7307_d89e,
                    0x28ee_6828_a886_986e,
                ],
            ),
            (
                "F1s",
                [
                    0x1b3d_53a2_afb4_3f1d,
                    0x9a16_8f65_5e1f_f7c5,
                    0xee7a_6e52_6425_e245,
                ],
            ),
            (
                "F4t",
                [
                    0x2a47_b6b5_8e70_70ad,
                    0x0a2e_68fb_58a3_5bce,
                    0x1779_38f5_eec8_7085,
                ],
            ),
            (
                "F4n",
                [
                    0xecd6_56b0_9a0c_d365,
                    0xf23c_ceff_7e6c_0ecf,
                    0xedb6_fc70_2232_39e5,
                ],
            ),
            (
                "F4v",
                [
                    0x39b9_a87b_1e12_bc5d,
                    0xae64_ff2e_48e7_4e3f,
                    0xd316_d3f7_1570_4345,
                ],
            ),
        ];
        for (yuv, table) in [
            (HqxYuv::Fused, &fused[..]),
            (HqxYuv::Separate, &separate[..]),
        ] {
            for (name, hashes) in table {
                for (s, &hash) in (2..=4).zip(hashes) {
                    assert_eq!(run(name, s, yuv), hash, "{name} {s}x {yuv:?}");
                }
            }
        }
        // The white quirk and the two conversions.
        assert_eq!(hqx_yuv(0xFFFF_FFFF, HqxYuv::Separate), 0);
        assert_eq!(hqx_yuv(0x00F8_F8F8, HqxYuv::Fused), 0xF7_8080);
        assert_eq!(hqx_yuv(0x00F8_F8F8, HqxYuv::Separate), 0xF8_8080);
        assert_eq!(hqx_yuv(0x0000_FF00, HqxYuv::Separate), 0x95_2C16);
    }
}
