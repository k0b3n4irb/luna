//! `luna record` — a run as a small video, a text timeline and contact
//! sheets: the video for eyes and for the models that take one, the text
//! and the sheets for the ones that do not.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};

use crate::parsers::{InputFlags, apply_input_flags};
use crate::rom::load_rom_into;
use crate::state::{PokeAt, PokeGroup};

/// Width of the video picture: the native capture, two dots per pixel.
const VIDEO_W: usize = 512;
/// Rows of the strip under the video picture.
const STRIP_H: usize = 24;
/// Width of a contact-sheet tile: the displayed frame.
const TILE_W: usize = 256;
/// Rows of the label under a contact-sheet tile.
const LABEL_H: usize = 10;
/// Tiles per row of a contact sheet.
const SHEET_COLS: usize = 6;
/// Rows of tiles per contact sheet.
const SHEET_ROWS: usize = 6;
/// Frames a picture must hold to be reported as still.
const STILL_MIN: usize = 30;
/// Frames the screen must stay off to be reported as off.
const BLANK_MIN: usize = 4;
/// The S-DSP key-on register.
const DSP_KON: u8 = 0x4C;
/// Bitrate the picture is held to: a busy minute stays near 4 MB.
pub(crate) const DEFAULT_VIDEO_KBPS: u32 = 500;

/// What `luna record` was asked for.
pub(crate) struct RecordOptions<'a> {
    pub rom: &'a Path,
    pub out: &'a Path,
    pub from_frame: u64,
    pub count: u64,
    pub sheet_every: Option<u64>,
    pub video_kbps: u32,
    pub ffmpeg: &'a str,
    pub no_video: bool,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
    pub input: Option<&'a str>,
    pub input2: Option<&'a str>,
    pub input_at: Option<&'a str>,
    pub pokes: &'a [PokeGroup],
    pub sym: Option<&'a Path>,
}

/// A failure with the exit code it maps to; an empty message was already
/// printed by whoever found it.
struct Fail(u8, String);

impl Fail {
    fn run(e: impl std::fmt::Display) -> Self {
        Self(1, e.to_string())
    }
}

/// `luna record` — exit 0 when every file was written, 1 when the run or
/// the encoder failed, 2 on a malformed option.
pub(crate) fn run_record(o: &RecordOptions<'_>) -> ExitCode {
    match record(o) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail(code, msg)) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            ExitCode::from(code)
        }
    }
}

// =============================================================================
// The picture a frame or a sheet is composed on
// =============================================================================

/// The 5×7 glyph of `c`, one row per byte, bit 4 leftmost. Only what the
/// labels print; anything else is a blank.
const fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '^' => [0x04, 0x0E, 0x15, 0x04, 0x04, 0x04, 0x04],
        'v' => [0x04, 0x04, 0x04, 0x04, 0x15, 0x0E, 0x04],
        '<' => [0x00, 0x04, 0x08, 0x1F, 0x08, 0x04, 0x00],
        '>' => [0x00, 0x04, 0x02, 0x1F, 0x02, 0x04, 0x00],
        _ => [0; 7],
    }
}

/// A row-major RGB picture.
struct Canvas {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            rgb: vec![0; w * h * 3],
        }
    }

    /// Fill rows `y..y + rows` with one grey level.
    fn fill_rows(&mut self, y: usize, rows: usize, level: u8) {
        let end = ((y + rows) * self.w * 3).min(self.rgb.len());
        self.rgb[(y * self.w * 3).min(end)..end].fill(level);
    }

    /// Copy an RGB picture `src_w` wide to `(x, y)`, clipped to the canvas.
    fn blit(&mut self, x: usize, y: usize, src: &[u8], src_w: usize) {
        let width = src_w.min(self.w.saturating_sub(x)) * 3;
        for (row, line) in src.chunks_exact(src_w * 3).enumerate() {
            if y + row >= self.h {
                break;
            }
            let at = ((y + row) * self.w + x) * 3;
            self.rgb[at..at + width].copy_from_slice(&line[..width]);
        }
    }

    /// Print `text` in white from `(x, y)`, each font dot `scale` wide.
    fn text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            let left = x + i * 6 * scale;
            for (row, bits) in glyph(c).into_iter().enumerate() {
                for col in (0..5).filter(|col| bits & (0x10 >> col) != 0) {
                    self.dot(left + col * scale, y + row * scale, scale);
                }
            }
        }
    }

    fn dot(&mut self, x: usize, y: usize, scale: usize) {
        for py in y..(y + scale).min(self.h) {
            for px in x..(x + scale).min(self.w) {
                let at = (py * self.w + px) * 3;
                self.rgb[at..at + 3].fill(0xFF);
            }
        }
    }
}

// =============================================================================
// The pad
// =============================================================================

/// The twelve buttons from bit 15 down: the glyph of the strip and the
/// name of the timeline.
const BUTTONS: [(char, &str); 12] = [
    ('B', "B"),
    ('Y', "Y"),
    ('E', "Select"),
    ('S', "Start"),
    ('^', "Up"),
    ('v', "Down"),
    ('<', "Left"),
    ('>', "Right"),
    ('A', "A"),
    ('X', "X"),
    ('L', "L"),
    ('R', "R"),
];

fn held(mask: u16) -> impl Iterator<Item = (char, &'static str)> {
    BUTTONS
        .into_iter()
        .enumerate()
        .filter(move |(bit, _)| mask & (0x8000 >> bit) != 0)
        .map(|(_, b)| b)
}

/// The strip's twelve slots: the glyph of a held button, a dot otherwise.
fn pad_slots(mask: u16) -> String {
    BUTTONS
        .into_iter()
        .enumerate()
        .map(|(bit, (c, _))| if mask & (0x8000 >> bit) == 0 { '.' } else { c })
        .collect()
}

fn pad_names(port: usize, mask: u16) -> String {
    let names: Vec<&str> = held(mask).map(|(_, name)| name).collect();
    if names.is_empty() {
        format!("pad {}: nothing held", port + 1)
    } else {
        format!("pad {}: {} held", port + 1, names.join(" + "))
    }
}

// =============================================================================
// The encoder
// =============================================================================

/// An `ffmpeg` fed raw frames on its standard input.
struct Encoder {
    child: Child,
    stdin: ChildStdin,
}

/// The H.264 encoder this `ffmpeg` was built with, held to `kbps`.
fn h264_args(ffmpeg: &str, kbps: u32) -> Result<Vec<String>, Fail> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| {
            Fail::run(format!(
                "cannot run `{ffmpeg}` ({e}): `luna record` needs ffmpeg to write the video; \
                 `--no-video` writes the timeline and the sheets without it"
            ))
        })?;
    let list = String::from_utf8_lossy(&out.stdout);
    let has = |name: &str| list.split_whitespace().any(|word| word == name);
    let rate = format!("{kbps}k");
    let buffer = format!("{}k", kbps * 2);
    let args: [&str; 10] = if has("libx264") {
        // Quality first, the bitrate as a ceiling.
        [
            "-c:v", "libx264", "-preset", "medium", "-crf", "23", "-maxrate", &rate, "-bufsize",
            &buffer,
        ]
    } else if has("libopenh264") {
        // It takes the bitrate as a hint, and a busy scene twice it, unless
        // the mode is `bitrate` and the quantiser range is given whole.
        [
            "-c:v",
            "libopenh264",
            "-b:v",
            &rate,
            "-rc_mode",
            "bitrate",
            "-qmin",
            "20",
            "-qmax",
            "51",
        ]
    } else {
        return Err(Fail::run(format!(
            "`{ffmpeg}` has no H.264 encoder (libx264 or libopenh264)"
        )));
    };
    Ok(args.map(str::to_owned).to_vec())
}

impl Encoder {
    fn start(
        ffmpeg: &str,
        codec: &[String],
        (w, h): (usize, usize),
        fps: f64,
        to: &Path,
    ) -> Result<Self, Fail> {
        let mut child = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pixel_format", "rgb24"])
            .args(["-video_size", &format!("{w}x{h}")])
            .args(["-framerate", &format!("{fps}"), "-i", "pipe:0", "-an"])
            .args(codec)
            .args(["-pix_fmt", "yuv420p"])
            .arg(to)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| Fail::run(format!("cannot run `{ffmpeg}`: {e}")))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        Ok(Self { child, stdin })
    }

    fn frame(&mut self, rgb: &[u8]) -> Result<(), Fail> {
        self.stdin
            .write_all(rgb)
            .map_err(|e| Fail::run(format!("ffmpeg stopped reading the frames: {e}")))
    }

    fn finish(self) -> Result<(), Fail> {
        let Self { mut child, stdin } = self;
        drop(stdin);
        let status = child.wait().map_err(Fail::run)?;
        if status.success() {
            Ok(())
        } else {
            Err(Fail::run(format!(
                "ffmpeg failed encoding the video ({status})"
            )))
        }
    }
}

/// Join the encoded picture and the run's sound into the final MP4. The
/// sound is raw 16-bit stereo at `hz`, the rate that makes it last as long
/// as the picture.
fn mux(ffmpeg: &str, video: &Path, pcm: Option<(&Path, u32)>, to: &Path) -> Result<(), Fail> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video);
    if let Some((pcm, hz)) = pcm {
        cmd.args(["-f", "s16le", "-ar", &hz.to_string(), "-ac", "2", "-i"])
            .arg(pcm)
            .args(["-c:a", "aac", "-b:a", "64k", "-ar", "32000"]);
    }
    let status = cmd
        .args(["-c:v", "copy", "-movflags", "+faststart"])
        .arg(to)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| Fail::run(format!("cannot run `{ffmpeg}`: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Fail::run(format!(
            "ffmpeg failed writing {} ({status})",
            to.display()
        )))
    }
}

// =============================================================================
// What the run is reduced to
// =============================================================================

/// One recorded frame, as the timeline sees it.
struct Shot {
    frame: u64,
    hash: u64,
    showed: bool,
    /// What pads 1 and 2 held while the frame ran.
    masks: [u16; 2],
}

/// One second of sound.
#[derive(Default, Clone)]
struct Second {
    frame: u64,
    samples: u64,
    squares: f64,
    key_ons: [u32; 8],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picture {
    Moving,
    Still,
    Off,
}

/// Label every frame: off once the screen stays off [`BLANK_MIN`] frames,
/// still once a picture holds [`STILL_MIN`], moving otherwise.
fn classify(shots: &[Shot]) -> Vec<Picture> {
    let mut labels = vec![Picture::Moving; shots.len()];
    let same = |i: usize| i > 0 && shots[i].showed && shots[i].hash == shots[i - 1].hash;
    let mut i = 0;
    while i < shots.len() {
        let off = !shots[i].showed;
        let repeat = same(i);
        let mut end = i + 1;
        while end < shots.len() && shots[end].showed != off && (off || same(end) == repeat) {
            end += 1;
        }
        if off && end - i >= BLANK_MIN {
            labels[i..end].fill(Picture::Off);
        } else if repeat && end - i >= STILL_MIN {
            // The frame before the run is the one that first showed it.
            let first = if shots[i - 1].showed { i - 1 } else { i };
            labels[first..end].fill(Picture::Still);
        }
        i = end;
    }
    labels
}

/// The timeline rows `(frame, event)`: a row when the picture changes
/// kind, a row when the pad changes.
fn timeline(shots: &[Shot]) -> Vec<(u64, String)> {
    let labels = classify(shots);
    let mut rows = Vec::new();
    let mut start = 0;
    while start < shots.len() {
        let kind = labels[start];
        let end = (start..shots.len())
            .find(|&i| labels[i] != kind)
            .unwrap_or(shots.len());
        let frames = end - start;
        let event = match kind {
            Picture::Off => format!("screen off, {frames} frames"),
            Picture::Still => format!(
                "still picture, {frames} frames (fbhash `{:016x}`)",
                shots[start].hash
            ),
            Picture::Moving => {
                let distinct = 1
                    + (start + 1..end)
                        .filter(|&i| shots[i].hash != shots[i - 1].hash)
                        .count();
                format!("moving, {distinct} distinct pictures in {frames} frames")
            }
        };
        rows.push((shots[start].frame, event));
        start = end;
    }
    for port in 0..2 {
        let mut mask = None;
        for shot in shots {
            let now = shot.masks[port];
            if mask != Some(now) && (mask.is_some() || now != 0) {
                rows.push((shot.frame, pad_names(port, now)));
            }
            mask = Some(now);
        }
    }
    rows.sort_by_key(|&(frame, _)| frame);
    rows
}

/// `base` with `suffix` appended to its file name (`demo` + `.mp4`).
fn beside(base: &Path, suffix: &str) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The contact sheets: one tile every `every` frames, written as they fill.
struct Sheets<'a> {
    base: &'a Path,
    every: u64,
    tile_h: usize,
    canvas: Canvas,
    tiles: usize,
    written: Vec<PathBuf>,
}

impl<'a> Sheets<'a> {
    fn new(base: &'a Path, every: u64, tile_h: usize) -> Self {
        Self {
            base,
            every,
            tile_h,
            canvas: Canvas::new(TILE_W * SHEET_COLS, (tile_h + LABEL_H) * SHEET_ROWS),
            tiles: 0,
            written: Vec::new(),
        }
    }

    fn add(&mut self, rgb: &[u8], label: &str) -> Result<(), Fail> {
        let x = self.tiles % SHEET_COLS * TILE_W;
        let y = self.tiles / SHEET_COLS * (self.tile_h + LABEL_H);
        let rows = (rgb.len() / (TILE_W * 3)).min(self.tile_h);
        self.canvas.blit(x, y, &rgb[..rows * TILE_W * 3], TILE_W);
        self.canvas
            .fill_rows_at(x, y + self.tile_h, TILE_W, LABEL_H);
        self.canvas.text(x + 2, y + self.tile_h + 1, 1, label);
        self.tiles += 1;
        if self.tiles == SHEET_COLS * SHEET_ROWS {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Fail> {
        if self.tiles == 0 {
            return Ok(());
        }
        let rows = self.tiles.div_ceil(SHEET_COLS) * (self.tile_h + LABEL_H);
        let full = Canvas::new(self.canvas.w, self.canvas.h);
        let mut sheet = std::mem::replace(&mut self.canvas, full);
        sheet.rgb.truncate(sheet.w * rows * 3);
        let png =
            luna_api::encode_png_rgb(sheet.w as u32, rows as u32, sheet.rgb).map_err(Fail::run)?;
        let path = beside(self.base, &format!("-{}.png", self.written.len() + 1));
        std::fs::write(&path, png)
            .map_err(|e| Fail::run(format!("writing {}: {e}", path.display())))?;
        self.written.push(path);
        self.tiles = 0;
        Ok(())
    }
}

impl Canvas {
    /// The dark band a tile's label is printed on.
    fn fill_rows_at(&mut self, x: usize, y: usize, w: usize, rows: usize) {
        for row in y..(y + rows).min(self.h) {
            let at = (row * self.w + x) * 3;
            self.rgb[at..at + w * 3].fill(0x20);
        }
    }
}

// =============================================================================
// The run
// =============================================================================

/// What the capture keeps from one frame to the next.
struct Capture<'a> {
    fps: f64,
    canvas: Canvas,
    encoder: Option<Encoder>,
    sheets: Sheets<'a>,
    shots: Vec<Shot>,
    audio: Vec<(i16, i16)>,
    seconds: Vec<Second>,
}

impl Capture<'_> {
    /// A frame just completed: keep its picture, its sound and what the
    /// timeline says of it.
    fn frame(&mut self, em: &mut luna_api::Emulator) -> Result<(), Fail> {
        let i = self.shots.len() as u64;
        let shot = Shot {
            frame: em.frame_count().map_err(Fail::run)?,
            hash: em.frame_hash(false).map_err(Fail::run)?,
            showed: em.frame_showed_content().unwrap_or(true),
            masks: [
                em.joypad(0).map_err(Fail::run)?,
                em.joypad(1).map_err(Fail::run)?,
            ],
        };
        let time = i as f64 / self.fps;
        let stamp = format!("F{:06} T{time:07.2}", shot.frame);

        if let Some(encoder) = self.encoder.as_mut() {
            let native = em.render_frame_rgb_native().map_err(Fail::run)?;
            let picture = self.canvas.h - STRIP_H;
            self.canvas.fill_rows(0, picture, 0);
            let rows = (native.len() / (VIDEO_W * 3)).min(picture);
            self.canvas
                .blit(0, 0, &native[..rows * VIDEO_W * 3], VIDEO_W);
            // Black under large white dots: what a starved encoder keeps.
            self.canvas.fill_rows(picture, STRIP_H, 0);
            let strip = format!(
                "F{:06} T{time:06.2} {}",
                shot.frame,
                pad_slots(shot.masks[0])
            );
            self.canvas.text(4, picture + 2, 3, &strip);
            encoder.frame(&self.canvas.rgb)?;
        }
        if i.is_multiple_of(self.sheets.every) {
            let rgba = em.render_frame_rgba(false).map_err(Fail::run)?;
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|px| [px[0], px[1], px[2]])
                .collect();
            self.sheets.add(&rgb, &stamp)?;
        }

        let second = time as usize;
        if self.seconds.len() <= second {
            self.seconds.resize(second + 1, Second::default());
            self.seconds[second].frame = shot.frame;
        }
        let samples = em.drain_audio(usize::MAX).map_err(Fail::run)?;
        self.seconds[second].samples += 2 * samples.len() as u64;
        self.seconds[second].squares += samples
            .iter()
            .map(|&(l, r)| i64::from(l).pow(2) + i64::from(r).pow(2))
            .sum::<i64>() as f64;
        for write in em.take_dsp_trace().map_err(Fail::run)? {
            if write.reg == DSP_KON {
                for voice in (0..8).filter(|v| write.value & (1 << v) != 0) {
                    self.seconds[second].key_ons[voice] += 1;
                }
            }
        }
        self.audio.extend(samples);
        self.shots.push(shot);
        Ok(())
    }
}

fn record(o: &RecordOptions<'_>) -> Result<(), Fail> {
    use luna_api::{ScriptBound, WatchEvent};
    if o.count == 0 {
        return Err(Fail(2, "--count must be at least 1".into()));
    }
    if o.sheet_every == Some(0) {
        return Err(Fail(2, "--sheet-every must be at least 1".into()));
    }
    let base = if o.out.extension().is_some_and(|e| e == "mp4") {
        o.out.with_extension("")
    } else {
        o.out.to_path_buf()
    };
    // Know the encoder before the run, not after it.
    let codec = if o.no_video {
        None
    } else {
        Some(h264_args(o.ffmpeg, o.video_kbps)?)
    };

    let mut em = luna_api::Emulator::new();
    load_rom_into(
        &mut em,
        o.rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    )
    .map_err(Fail::run)?;
    if let Some(sym) = o.sym.filter(|s| !crate::rom::is_auto_sym(o.rom, s)) {
        let n = em
            .load_symbols(sym)
            .map_err(|e| Fail::run(format!("--sym {}: {e}", sym.display())))?;
        eprintln!("loaded {n} symbols from {}", sym.display());
    }
    if codec.is_some() {
        em.set_native_capture(true).map_err(Fail::run)?;
    }
    let extra_pads = [(1u8, o.input2)];
    let flags = InputFlags {
        extra_pads: &extra_pads,
        clocked: o.input_at.is_some(),
        ..InputFlags::pad1(o.input)
    };
    let mut script =
        apply_input_flags(&mut em, &flags).map_err(|code| Fail(code, String::new()))?;
    if let Some(spec) = o.input_at {
        let pc = crate::test_cmd::resolve_key(&em, spec)
            .map_err(|e| Fail(2, format!("--input-at `{spec}`: {e}")))?;
        script.set_clock_pc(pc);
    }
    let mut pokes = PokeAt::resolve(&em, o.pokes).map_err(|e| Fail(2, e))?;
    let mut watched: Vec<u32> = Vec::new();
    for p in &pokes {
        if !watched.contains(&p.addr) {
            watched.push(p.addr);
        }
    }
    // Execution reached `watched[i]`: the bytes of its `--poke-at`.
    let arrive = |em: &mut luna_api::Emulator, i: usize, pokes: &mut [PokeAt]| {
        for p in pokes.iter_mut().filter(|p| p.addr == watched[i]) {
            p.on_arrival(em);
        }
    };
    let script_failed = |e: luna_api::ApiError| Fail::run(format!("scripted input: {e}"));

    // The warm-up: everything up to the frame before the first one kept.
    let first = o.from_frame.max(1);
    em.run_script_watching(
        &mut script,
        ScriptBound::Frame(first - 1),
        &watched,
        |em, ev| {
            if let WatchEvent::Pc(i) = ev {
                arrive(em, i, &mut pokes);
            }
            false
        },
    )
    .map_err(script_failed)?;
    if em.frame_count().unwrap_or(0) + 1 < first {
        return Err(Fail::run(format!(
            "the emulator halted before frame {first}"
        )));
    }
    // What the warm-up left in the queue is sound from before the capture.
    em.drain_audio(usize::MAX).map_err(Fail::run)?;
    em.enable_dsp_trace(1 << 16).map_err(Fail::run)?;

    let fps = em.frame_rate_hz();
    let every = o.sheet_every.unwrap_or_else(|| fps.round() as u64);
    let tile_h = em.frame_height().map_err(Fail::run)?;
    let video_tmp = beside(&base, ".video.tmp.mp4");
    let pcm_tmp = beside(&base, ".audio.tmp.pcm");
    let canvas = Canvas::new(VIDEO_W, tile_h * 2 + STRIP_H);
    let encoder = match &codec {
        Some(codec) => Some(Encoder::start(
            o.ffmpeg,
            codec,
            (canvas.w, canvas.h),
            fps,
            &video_tmp,
        )?),
        None => None,
    };
    let mut capture = Capture {
        fps,
        canvas,
        encoder,
        sheets: Sheets::new(&base, every, tile_h),
        shots: Vec::new(),
        audio: Vec::new(),
        seconds: Vec::new(),
    };
    let mut failure = None;
    em.run_script_watching(
        &mut script,
        ScriptBound::Frame(first - 1 + o.count),
        &watched,
        |em, ev| match ev {
            WatchEvent::Pc(i) => {
                arrive(em, i, &mut pokes);
                false
            }
            WatchEvent::Frame => capture.frame(em).is_err_and(|e| {
                failure = Some(e);
                true
            }),
        },
    )
    .map_err(script_failed)?;
    if let Some(e) = failure {
        return Err(e);
    }
    let Capture {
        canvas: frame_canvas,
        mut encoder,
        mut sheets,
        shots,
        audio,
        seconds,
        ..
    } = capture;
    if (shots.len() as u64) < o.count {
        eprintln!(
            "note: the emulator halted after {} frames; the record stops there",
            shots.len()
        );
    }
    sheets.flush()?;
    let Some(last) = shots.last() else {
        return Err(Fail::run("no frame was recorded"));
    };
    let first_frame = shots[0].frame;

    // The console's sound clock is its own: the samples of the run are
    // played at the rate that makes them last as long as its pictures.
    let picture_s = shots.len() as f64 / fps;
    let sound_hz = (audio.len() as f64 / picture_s).round() as u32;
    let mut written = Vec::new();
    if let Some(encoder) = encoder.take() {
        encoder.finish()?;
        let pcm = if audio.is_empty() {
            None
        } else {
            let bytes: Vec<u8> = audio
                .iter()
                .flat_map(|&(l, r)| [l.to_le_bytes(), r.to_le_bytes()])
                .flatten()
                .collect();
            std::fs::write(&pcm_tmp, bytes)
                .map_err(|e| Fail::run(format!("writing {}: {e}", pcm_tmp.display())))?;
            Some((pcm_tmp.as_path(), sound_hz))
        };
        let mp4 = beside(&base, ".mp4");
        let muxed = mux(o.ffmpeg, &video_tmp, pcm, &mp4);
        let _ = std::fs::remove_file(&video_tmp);
        let _ = std::fs::remove_file(&pcm_tmp);
        muxed?;
        written.push(mp4);
    }

    let md = beside(&base, ".md");
    let text = Report {
        o,
        shots: &shots,
        seconds: &seconds,
        fps,
        every,
        video: written
            .first()
            .map(|p| (p.as_path(), frame_canvas.w, frame_canvas.h)),
        sheets: &sheets.written,
    }
    .render();
    std::fs::write(&md, text).map_err(|e| Fail::run(format!("writing {}: {e}", md.display())))?;
    written.push(md);
    written.extend(sheets.written);

    println!(
        "recorded frames {first_frame}..{} ({} frames, {picture_s:.2} s, sound at {sound_hz} Hz)",
        last.frame,
        shots.len(),
    );
    for path in &written {
        let bytes = std::fs::metadata(path).map_or(0, |m| m.len());
        println!("  {:>9} bytes  {}", bytes, path.display());
    }
    // A `--poke-at` the run never reached wrote nothing: what was
    // recorded is not the scene that was asked for.
    match pokes.iter().find(|p| !p.applied) {
        Some(p) => Err(Fail::run(p.missed(last.frame))),
        None => Ok(()),
    }
}

// =============================================================================
// The text
// =============================================================================

/// Everything the timeline file says.
struct Report<'a> {
    o: &'a RecordOptions<'a>,
    shots: &'a [Shot],
    seconds: &'a [Second],
    fps: f64,
    every: u64,
    video: Option<(&'a Path, usize, usize)>,
    sheets: &'a [PathBuf],
}

impl Report<'_> {
    fn render(&self) -> String {
        let (first, last) = (self.shots[0].frame, self.shots[self.shots.len() - 1].frame);
        let standard = if self.fps < 55.0 { "PAL" } else { "NTSC" };
        let mut s = String::new();
        let _ = writeln!(
            s,
            "# {} — frames {first} to {last}\n",
            file_name(self.o.rom)
        );
        let _ = writeln!(
            s,
            "Recorded by `luna record` (luna {}): {} frames, {:.2} s at {} Hz ({standard}).\n",
            env!("CARGO_PKG_VERSION"),
            self.shots.len(),
            self.shots.len() as f64 / self.fps,
            self.fps,
        );
        for (pad, script) in [(1, self.o.input), (2, self.o.input2)] {
            if let Some(script) = script {
                let _ = writeln!(s, "Input script, pad {pad}: `{script}`\n");
            }
        }
        if let Some(at) = self.o.input_at {
            let _ = writeln!(
                s,
                "The input scripts are numbered by arrivals on `{at}`, not by frame.\n"
            );
        }
        for group in self.o.pokes {
            let _ = writeln!(
                s,
                "Written at arrival {} on `{}`: `{}`\n",
                group.hit,
                group.at,
                group.pokes.join("`, `")
            );
        }
        let _ = writeln!(s, "| File | What it holds |\n|---|---|");
        if let Some((path, w, h)) = self.video {
            let _ = writeln!(
                s,
                "| `{}` | the run, {w}x{h}, H.264 + AAC; a strip under each picture names it |",
                file_name(path)
            );
        }
        for path in self.sheets {
            let _ = writeln!(
                s,
                "| `{}` | one picture every {} frames, {SHEET_COLS} per row, each labelled |",
                file_name(path),
                self.every
            );
        }
        let _ = writeln!(
            s,
            "\nA label reads `F<frame> T<seconds from the first frame>`. The video strip adds \
             twelve slots, a letter for a held button of pad 1 and a dot for a released one, \
             in the order B, Y, E (Select), S (Start), up, down, left, right, A, X, L, R.\n"
        );

        let _ = writeln!(s, "## Timeline\n");
        let _ = writeln!(
            s,
            "A picture is *still* once it holds {STILL_MIN} frames, the screen is *off* once \
             it stays off {BLANK_MIN}. A pad row names the first picture drawn with those \
             buttons{}.\n",
            if self.o.input_at.is_some() {
                ""
            } else {
                "; the frame of the input script is one less"
            }
        );
        let _ = writeln!(s, "| frame | time (s) | event |\n|---|---|---|");
        for (frame, event) in timeline(self.shots) {
            let time = (frame - first) as f64 / self.fps;
            let _ = writeln!(s, "| {frame} | {time:.2} | {event} |");
        }

        let _ = writeln!(s, "\n## Sound, second by second\n");
        let _ = writeln!(
            s,
            "The level is the RMS of the second, in dB below full scale. A note started is a \
             key-on written to the S-DSP, counted per voice (`v0` to `v7`).\n"
        );
        let _ = writeln!(
            s,
            "| second | frame | level (dBFS) | notes started |\n|---|---|---|---|"
        );
        for (i, sec) in self.seconds.iter().enumerate() {
            let rms = if sec.samples == 0 {
                0.0
            } else {
                (sec.squares / sec.samples as f64).sqrt()
            };
            let level = if rms < 1.0 {
                "silent".to_owned()
            } else {
                format!("{:.1}", 20.0 * (rms / 32768.0).log10())
            };
            let notes: Vec<String> = sec
                .key_ons
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n > 0)
                .map(|(v, n)| format!("v{v}×{n}"))
                .collect();
            let notes = if notes.is_empty() {
                "—".to_owned()
            } else {
                notes.join(" ")
            };
            let _ = writeln!(s, "| {i} | {} | {level} | {notes} |", sec.frame);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(frame: u64, hash: u64, showed: bool, pad1: u16) -> Shot {
        Shot {
            frame,
            hash,
            showed,
            masks: [pad1, 0],
        }
    }

    #[test]
    fn a_picture_is_still_once_it_holds_and_off_once_the_screen_stays_off() {
        let mut shots = Vec::new();
        // 10 moving, 40 holding the last of them, 5 off, 3 moving.
        shots.extend((0..10).map(|i| shot(100 + i, i, true, 0)));
        shots.extend((10..50).map(|i| shot(100 + i, 9, true, 0)));
        shots.extend((50..55).map(|i| shot(100 + i, 0, false, 0)));
        shots.extend((55..58).map(|i| shot(100 + i, 1000 + i, true, 0)));
        let rows = timeline(&shots);
        assert_eq!(
            rows,
            vec![
                (100, "moving, 9 distinct pictures in 9 frames".to_owned()),
                (
                    109,
                    "still picture, 41 frames (fbhash `0000000000000009`)".to_owned()
                ),
                (150, "screen off, 5 frames".to_owned()),
                (155, "moving, 3 distinct pictures in 3 frames".to_owned()),
            ]
        );
    }

    #[test]
    fn a_game_drawing_every_other_frame_is_moving_not_still() {
        let shots: Vec<Shot> = (0..60).map(|i| shot(i, i / 2, true, 0)).collect();
        assert_eq!(
            timeline(&shots),
            vec![(0, "moving, 30 distinct pictures in 60 frames".to_owned())]
        );
    }

    #[test]
    fn the_pad_gets_a_row_when_it_changes() {
        let shots: Vec<Shot> = (0..6)
            .map(|i| shot(i, i, true, if (2..4).contains(&i) { 0x9000 } else { 0 }))
            .collect();
        let rows = timeline(&shots);
        assert!(rows.contains(&(2, "pad 1: B + Start held".to_owned())));
        assert!(rows.contains(&(4, "pad 1: nothing held".to_owned())));
        assert_eq!(pad_slots(0x9000), "B..S........");
        assert_eq!(pad_names(1, 0x0080), "pad 2: A held");
    }
}
