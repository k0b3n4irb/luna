//! `luna audio-check` ([`audio_check`]): an audio fault analyzer for a WAV
//! the core produced (`state --audio-out`, the per-voice files of
//! `--voice-wav-dir`).
//!
//! The question it answers is "did the sound break?", on a long run, with
//! numbers instead of an ear: a hole in a sound that was playing (a **gap**),
//! a jump out of the slope of the signal (a **click**), samples pinned at the
//! rails (**saturation**), a voice that vanished with no release decay (a
//! **voice cut**).
//!
//! Limits, stated once:
//!
//! - It reads the core's stream. The host's resampling, pacing and underruns
//!   (the GUI's audio thread) are not in that stream.
//! - A WAV carries no frame marks. A "frame" here is the nominal length of
//!   one PPU frame at the WAV's rate (from the region), and the sound is
//!   cut on that grid; a real frame drifts from it by the rounding of the
//!   audio rate (32 040 Hz is not a whole number of samples per frame).
//! - A click is told from a legitimate sharp attack by one criterion only:
//!   the signal before it was already playing (a **fault**) or came from
//!   near silence (a **note**, an onset). A loud percussive hit over a
//!   playing sound is a fault by that rule; the ratio is in the line.
//! - A voice that ends on a BRR block flagged "end, no loop" is cut by the
//!   hardware itself, with no release: the analyzer reports it like any
//!   other voice cut.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

/// Master clock and master cycles per frame (262 / 312 lines of 1364, the
/// short line of every other NTSC frame averaged out).
const NTSC: (f64, f64) = (21_477_272.0, 357_366.0);
const PAL: (f64, f64) = (21_281_370.0, 425_568.0);

/// The version of the JSON report; it changes when a field is renamed or
/// removed, not when one is added.
pub const AUDIO_CHECK_SCHEMA: u32 = 1;

/// Sub-windows per nominal frame: the resolution at which silence is
/// looked for. A hole is not aligned to the frame grid, so a one-frame hole
/// seldom empties a whole nominal frame; it always empties several of these.
const BLOCKS_PER_FRAME: usize = 8;
/// A voice's silence must last this many exact-zero samples to be a cut
/// (a tone crossing zero holds one or two).
const VOICE_SILENT_RUN: usize = 16;
/// How far back a voice cut looks for the level it fell from.
const VOICE_LOOKBACK: usize = 256;
/// Samples each side of a click used for the level before and after.
const CLICK_SIDE: usize = 32;
/// A click is an onset (a note) when the level after is this many times the
/// level before.
const ONSET_FACTOR: f64 = 8.0;
/// Detections closer than this many samples are one click.
const CLICK_MERGE: usize = 4;

/// The video region: sets the nominal frame length in samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AudioRegion {
    /// 60.0988 frames per second.
    Ntsc,
    /// 50.0070 frames per second.
    Pal,
}

impl AudioRegion {
    /// `ntsc` or `pal`, any case.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.to_ascii_lowercase().as_str() {
            "ntsc" => Ok(Self::Ntsc),
            "pal" => Ok(Self::Pal),
            other => Err(format!("region `{other}`: expected ntsc or pal")),
        }
    }

    /// Frames per second.
    #[must_use]
    pub fn frame_hz(self) -> f64 {
        match self {
            Self::Ntsc => NTSC.0 / NTSC.1,
            Self::Pal => PAL.0 / PAL.1,
        }
    }
}

/// A kind of fault.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    /// A hole in a sound that was playing.
    Gap,
    /// A jump out of the slope of the signal.
    Click,
    /// A run of samples at the rails.
    Saturation,
    /// A voice that went silent without a release decay.
    VoiceCut,
}

impl FaultKind {
    /// Every kind, in report order.
    pub const ALL: [Self; 4] = [Self::Gap, Self::Click, Self::Saturation, Self::VoiceCut];

    /// The kinds that fail a check by default. A voice cut is listed but
    /// not failing: the hardware ends a voice the same way at the end of a
    /// BRR sample that is not looped (`ENV` to 0, no release), so the audio
    /// alone cannot tell it from a stopped voice. Ask for it with
    /// `--fail-on gap,click,saturation,voice-cut`.
    pub const DEFAULT_FAIL_ON: [Self; 3] = [Self::Gap, Self::Click, Self::Saturation];

    /// The name used on the command line and in the JSON.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Click => "click",
            Self::Saturation => "saturation",
            Self::VoiceCut => "voice-cut",
        }
    }

    /// A comma-separated list (`gap,click`); `voice-cut` or `voice_cut`.
    pub fn parse_list(list: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let kind = match item.replace('_', "-").to_ascii_lowercase().as_str() {
                "gap" => Self::Gap,
                "click" => Self::Click,
                "saturation" => Self::Saturation,
                "voice-cut" => Self::VoiceCut,
                other => {
                    return Err(format!(
                        "fault kind `{other}`: expected gap, click, saturation or voice-cut"
                    ));
                }
            };
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        if out.is_empty() {
            return Err("fault list is empty".into());
        }
        Ok(out)
    }
}

/// Whether a detection counts for the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultSeverity {
    /// Counts for the verdict.
    Fault,
    /// Worth knowing, does not fail the check (a click that is an onset).
    Note,
}

/// The tunable thresholds, with the names the command line uses
/// (`--gap-floor`, ...). The defaults were tuned on the census described in
/// the guide.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AudioThresholds {
    /// A stretch is silent when its RMS is below this (16-bit units).
    pub gap_floor: f64,
    /// The sound before a gap must average at least this RMS.
    pub gap_level: f64,
    /// How many frames of sound before the hole make it a gap.
    pub gap_before: usize,
    /// A silence of at least this many frames is a gap (a hole must be
    /// seen as one even when it falls across two nominal frames, so this
    /// is below 1).
    pub gap_min_frames: f64,
    /// A silence longer than this many frames is an absence, not a gap
    /// (a deliberate pause).
    pub gap_max_frames: f64,
    /// A gap starts abruptly: the RMS of the 32 samples just before the
    /// hole is at least this fraction of the sound's level (`gap_level`'s
    /// measure over `gap_before` frames). A musical rest fades into its
    /// silence (release) and fails this; a dropped chunk of the stream
    /// does not. (Sound that begins again after the hole is not asked to
    /// be abrupt: a note attacks.)
    pub gap_edge_ratio: f64,
    /// A click: the second difference is this many times the local mean.
    pub click_ratio: f64,
    /// A click: the second difference is at least this (absolute, so quiet
    /// noise does not trigger).
    pub click_floor: f64,
    /// A click: the local scale is measured over this many samples each side.
    pub click_span: usize,
    /// Saturation: this many consecutive samples at the rails.
    pub sat_run: usize,
    /// Voice cut: the voice must have been above this level (peak).
    pub voice_level: i32,
    /// Voice cut: the fall to silence takes fewer samples than this.
    pub voice_cut_samples: usize,
    /// Voice cut: the last `voice_cut_samples` must still hold at least this
    /// fraction of the preceding 256-sample peak (a release decay does not).
    pub voice_decay_ratio: f64,
}

impl Default for AudioThresholds {
    fn default() -> Self {
        Self {
            gap_floor: 16.0,
            gap_level: 256.0,
            gap_before: 3,
            gap_min_frames: 0.75,
            gap_max_frames: 60.0,
            gap_edge_ratio: 0.35,
            click_ratio: 40.0,
            click_floor: 4000.0,
            click_span: 32,
            sat_run: 4,
            voice_level: 2000,
            voice_cut_samples: 32,
            voice_decay_ratio: 0.25,
        }
    }
}

/// What to check.
#[derive(Debug, Clone)]
pub struct AudioCheckOptions {
    /// Sets the nominal frame length.
    pub region: AudioRegion,
    /// The thresholds.
    pub thresholds: AudioThresholds,
    /// The kinds whose faults fail the verdict (the others are listed).
    pub fail_on: Vec<FaultKind>,
}

impl Default for AudioCheckOptions {
    fn default() -> Self {
        Self {
            region: AudioRegion::Ntsc,
            thresholds: AudioThresholds::default(),
            fail_on: FaultKind::DEFAULT_FAIL_ON.to_vec(),
        }
    }
}

/// Decoded PCM, interleaved.
#[derive(Debug, Clone)]
pub struct AudioClip {
    /// Samples per second.
    pub rate: u32,
    /// 1 or 2.
    pub channels: usize,
    /// Interleaved 16-bit samples.
    pub samples: Vec<i16>,
}

impl AudioClip {
    /// Sample frames (one per channel group).
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// No samples.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    fn channel(&self, c: usize) -> Vec<i32> {
        self.samples
            .iter()
            .skip(c)
            .step_by(self.channels)
            .map(|&s| i32::from(s))
            .collect()
    }
}

/// Read a RIFF/WAVE file: 16-bit PCM, mono or stereo.
pub fn parse_wav(bytes: &[u8]) -> Result<AudioClip, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if body + 16 > bytes.len() {
                return Err("truncated fmt chunk".into());
            }
            let b = &bytes[body..body + 16];
            fmt = Some((
                u16::from_le_bytes([b[0], b[1]]),
                u16::from_le_bytes([b[2], b[3]]),
                u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                u16::from_le_bytes([b[14], b[15]]),
            ));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt.ok_or("data chunk before the fmt chunk")?;
            if format != 1 || bits != 16 || !(1..=2).contains(&channels) {
                return Err(format!(
                    "unsupported WAV (format {format}, {channels} channel(s), {bits} bits): \
                     16-bit PCM, mono or stereo, is read"
                ));
            }
            if rate == 0 {
                return Err("WAV with a zero sample rate".into());
            }
            // A streaming writer may leave the size at 0 or too large: take
            // what is there.
            let end = body.saturating_add(size).min(bytes.len());
            let mut data = &bytes[body..end];
            let frame = usize::from(channels) * 2;
            data = &data[..data.len() / frame * frame];
            let samples = data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Ok(AudioClip {
                rate,
                channels: usize::from(channels),
                samples,
            });
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("no data chunk".into())
}

/// One detection.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AudioFault {
    /// What it is.
    pub kind: FaultKind,
    /// Whether it counts for the verdict.
    pub severity: FaultSeverity,
    /// The nominal frame it starts in.
    pub frame: u64,
    /// The sample (per channel) it starts at.
    pub sample: u64,
    /// `sample` in seconds.
    pub time_s: f64,
    /// `L` or `R` (click, saturation) when it is one channel's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'static str>,
    /// The voice (0-7) of a voice cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<u8>,
    /// Gap: its length in nominal frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_frames: Option<f64>,
    /// Gap and saturation: its length in samples.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_samples: Option<u64>,
    /// Gap: RMS of the sound before it. Click: RMS of the 32 samples
    /// before. Voice cut: the peak it fell from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_before: Option<f64>,
    /// Gap: RMS of the first frame after. Click: RMS of the 32 samples after.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_after: Option<f64>,
    /// Gap: the RMS of the 32 samples just before the hole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_rms: Option<f64>,
    /// Click: the size of the second difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnitude: Option<f64>,
    /// Click: `magnitude` over the local mean second difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    /// Saturation: consecutive samples at the rail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<u64>,
    /// Voice cut: samples the fall took.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fall_samples: Option<u64>,
    /// The thresholds that decided it.
    pub threshold: String,
}

/// Counts per kind (faults only) and the notes.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct AudioSummary {
    /// Gaps.
    pub gap: u64,
    /// Click faults.
    pub click: u64,
    /// Saturation runs.
    pub saturation: u64,
    /// Voice cuts.
    pub voice_cut: u64,
    /// Detections kept as notes (not counted above).
    pub notes: u64,
}

/// The `--json` report.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AudioReport {
    /// [`AUDIO_CHECK_SCHEMA`].
    pub schema: u32,
    /// The WAV analysed.
    pub wav: String,
    /// Its sample rate.
    pub rate: u32,
    /// Its channels.
    pub channels: usize,
    /// Samples per channel.
    pub samples: u64,
    /// Its length in seconds.
    pub duration_s: f64,
    /// The region the frame length comes from.
    pub region: AudioRegion,
    /// The nominal frame length, in samples (not a whole number).
    pub frame_samples: f64,
    /// Complete nominal frames analysed.
    pub frames: u64,
    /// Voice files analysed (their numbers).
    pub voices: Vec<u8>,
    /// The thresholds used.
    pub thresholds: AudioThresholds,
    /// The kinds that fail the verdict.
    pub fail_on: Vec<FaultKind>,
    /// Every detection, in position order.
    pub faults: Vec<AudioFault>,
    /// Counts.
    pub summary: AudioSummary,
    /// `ok` or `faults`.
    pub verdict: &'static str,
}

impl AudioReport {
    /// One line per detection, then the verdict.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut s = String::new();
        for f in &self.faults {
            let tag = match f.severity {
                FaultSeverity::Fault => f.kind.name().to_ascii_uppercase(),
                FaultSeverity::Note => format!("{} (note)", f.kind.name()),
            };
            let _ = write!(
                s,
                "{tag} frame {} t={:.3}s sample {}",
                f.frame, f.time_s, f.sample
            );
            if let Some(c) = f.channel {
                let _ = write!(s, " ch {c}");
            }
            if let Some(v) = f.voice {
                let _ = write!(s, " voice {v}");
            }
            if let Some(d) = f.duration_frames {
                let _ = write!(s, " duration {d:.2} frames");
            }
            if let Some(d) = f.duration_samples {
                let _ = write!(s, " ({d} samples)");
            }
            if let (Some(b), Some(a)) = (f.level_before, f.level_after) {
                let _ = write!(s, " level {b:.1} -> {a:.1}");
            } else if let Some(b) = f.level_before {
                let _ = write!(s, " level before {b:.1}");
            }
            if let Some(e) = f.edge_rms {
                let _ = write!(s, " edge rms {e:.0}");
            }
            if let Some(m) = f.magnitude {
                let _ = write!(s, " jump {m:.0}");
            }
            if let Some(r) = f.ratio {
                let _ = write!(s, " ratio {r:.1}");
            }
            if let Some(r) = f.run {
                let _ = write!(s, " run {r}");
            }
            if let Some(n) = f.fall_samples {
                let _ = write!(s, " fell in {n} samples");
            }
            let _ = writeln!(s, " [{}]", f.threshold);
        }
        let _ = write!(
            s,
            "{}: {} frame(s) of {:.1} samples at {} Hz ({:?}), gap {} click {} saturation {} \
             voice-cut {} (notes {})",
            self.verdict,
            self.frames,
            self.frame_samples,
            self.rate,
            self.region,
            self.summary.gap,
            self.summary.click,
            self.summary.saturation,
            self.summary.voice_cut,
            self.summary.notes,
        );
        s
    }
}

/// Read `wav` (and the `voice0..7.wav` of `voices_dir`) and check them.
pub fn audio_check_files(
    wav: &Path,
    voices_dir: Option<&Path>,
    opts: &AudioCheckOptions,
) -> Result<AudioReport, String> {
    let read = |p: &Path| -> Result<AudioClip, String> {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        parse_wav(&bytes).map_err(|e| format!("{}: {e}", p.display()))
    };
    let clip = read(wav)?;
    let mut voices = Vec::new();
    if let Some(dir) = voices_dir {
        for v in 0..8u8 {
            let p = dir.join(format!("voice{v}.wav"));
            if p.exists() {
                voices.push((v, read(&p)?));
            }
        }
        if voices.is_empty() {
            return Err(format!("{}: no voice0.wav .. voice7.wav", dir.display()));
        }
    }
    Ok(audio_check(
        &wav.display().to_string(),
        &clip,
        &voices,
        opts,
    ))
}

/// Check a clip (and its voices): the analysis, as a pure function.
#[must_use]
pub fn audio_check(
    name: &str,
    clip: &AudioClip,
    voices: &[(u8, AudioClip)],
    opts: &AudioCheckOptions,
) -> AudioReport {
    let th = &opts.thresholds;
    let rate = f64::from(clip.rate);
    let frame_samples = rate / opts.region.frame_hz();
    let n = clip.len();
    let chans: Vec<Vec<i32>> = (0..clip.channels).map(|c| clip.channel(c)).collect();
    let ctx = Ctx {
        rate,
        frame_samples,
    };

    let mut faults = Vec::new();
    faults.extend(find_gaps(&ctx, &chans, th));
    for (c, x) in chans.iter().enumerate() {
        let label = if clip.channels == 1 {
            "M"
        } else if c == 0 {
            "L"
        } else {
            "R"
        };
        faults.extend(find_clicks(&ctx, x, label, th));
        faults.extend(find_saturation(&ctx, x, label, th));
    }
    // The two channels of one click are one click.
    faults = merge_clicks(faults);
    for (v, vc) in voices {
        faults.extend(find_voice_cuts(&ctx, *v, vc, th));
    }
    faults.sort_by(|a, b| a.sample.cmp(&b.sample).then(a.kind.cmp(&b.kind)));

    let mut summary = AudioSummary::default();
    for f in &faults {
        match (f.severity, f.kind) {
            (FaultSeverity::Note, _) => summary.notes += 1,
            (_, FaultKind::Gap) => summary.gap += 1,
            (_, FaultKind::Click) => summary.click += 1,
            (_, FaultKind::Saturation) => summary.saturation += 1,
            (_, FaultKind::VoiceCut) => summary.voice_cut += 1,
        }
    }
    let failing = faults
        .iter()
        .any(|f| f.severity == FaultSeverity::Fault && opts.fail_on.contains(&f.kind));
    AudioReport {
        schema: AUDIO_CHECK_SCHEMA,
        wav: name.to_string(),
        rate: clip.rate,
        channels: clip.channels,
        samples: n as u64,
        duration_s: n as f64 / rate,
        region: opts.region,
        frame_samples,
        frames: (n as f64 / frame_samples).floor() as u64,
        voices: voices.iter().map(|(v, _)| *v).collect(),
        thresholds: th.clone(),
        fail_on: opts.fail_on.clone(),
        faults,
        summary,
        verdict: if failing { "faults" } else { "ok" },
    }
}

struct Ctx {
    rate: f64,
    frame_samples: f64,
}

impl Ctx {
    fn fault(&self, kind: FaultKind, severity: FaultSeverity, sample: usize) -> AudioFault {
        AudioFault {
            kind,
            severity,
            frame: (sample as f64 / self.frame_samples).floor() as u64,
            sample: sample as u64,
            time_s: sample as f64 / self.rate,
            channel: None,
            voice: None,
            duration_frames: None,
            duration_samples: None,
            level_before: None,
            level_after: None,
            edge_rms: None,
            magnitude: None,
            ratio: None,
            run: None,
            fall_samples: None,
            threshold: String::new(),
        }
    }
}

/// RMS of `lo..hi` over every channel together.
fn rms_of(chans: &[Vec<i32>], lo: usize, hi: usize) -> f64 {
    let hi = hi.min(chans[0].len());
    if lo >= hi {
        return 0.0;
    }
    let mut sum = 0.0;
    for x in chans {
        sum += x[lo..hi]
            .iter()
            .map(|&v| f64::from(v) * f64::from(v))
            .sum::<f64>();
    }
    (sum / ((hi - lo) * chans.len()) as f64).sqrt()
}

fn find_gaps(ctx: &Ctx, chans: &[Vec<i32>], th: &AudioThresholds) -> Vec<AudioFault> {
    let n = chans[0].len();
    let block = (ctx.frame_samples / BLOCKS_PER_FRAME as f64).max(1.0);
    let nblocks = (n as f64 / block).floor() as usize;
    let bounds = |i: usize| (i as f64 * block).round() as usize;
    let silent: Vec<bool> = (0..nblocks)
        .map(|i| rms_of(chans, bounds(i), bounds(i + 1)) < th.gap_floor)
        .collect();
    let before_len = (th.gap_before as f64 * ctx.frame_samples).round() as usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i < nblocks {
        if !silent[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < nblocks && silent[j] {
            j += 1;
        }
        // Silent blocks [i, j). The silence is the samples at the floor
        // around them too: widen to the exact edges.
        let quiet = |s: usize| {
            chans
                .iter()
                .all(|x| f64::from(x[s].abs()) < th.gap_floor.max(1.0))
        };
        let mut start = bounds(i);
        while start > 0 && quiet(start - 1) {
            start -= 1;
        }
        let mut end = bounds(j);
        while end < n && quiet(end) {
            end += 1;
        }
        let dur_samples = end - start;
        let dur_frames = dur_samples as f64 / ctx.frame_samples;
        // Sound before (enough of it), and sound after (not the end).
        let level_before = if start >= before_len && before_len > 0 {
            rms_of(chans, start - before_len, start)
        } else {
            0.0
        };
        let has_after = end + 1 < n && j < nblocks;
        let level_after = rms_of(chans, end, end + ctx.frame_samples.round() as usize);
        let edge_rms = rms_of(chans, start.saturating_sub(CLICK_SIDE), start);
        let abrupt = edge_rms >= th.gap_edge_ratio * level_before;
        if has_after
            && abrupt
            && start >= before_len
            && level_before >= th.gap_level
            && dur_frames >= th.gap_min_frames
            && dur_frames <= th.gap_max_frames
        {
            let mut f = ctx.fault(FaultKind::Gap, FaultSeverity::Fault, start);
            f.duration_frames = Some(dur_frames);
            f.duration_samples = Some(dur_samples as u64);
            f.level_before = Some(level_before);
            f.level_after = Some(level_after);
            f.edge_rms = Some(edge_rms);
            f.threshold =
                "gap_floor, gap_level, gap_before, gap_min_frames, gap_max_frames, gap_edge_ratio"
                    .into();
            out.push(f);
        }
        i = j;
    }
    out
}

struct Hit {
    pos: usize,
    mag: f64,
    ratio: f64,
}

fn find_clicks(ctx: &Ctx, x: &[i32], label: &'static str, th: &AudioThresholds) -> Vec<AudioFault> {
    let n = x.len();
    let w = th.click_span.max(2);
    if n < 2 * w + 4 {
        return Vec::new();
    }
    let mut d2 = vec![0.0f64; n];
    for i in 1..n - 1 {
        d2[i] = f64::from(x[i + 1] - 2 * x[i] + x[i - 1]);
    }
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + d2[i].abs();
    }
    let mean = |lo: usize, hi: usize| {
        if hi <= lo {
            None
        } else {
            Some((prefix[hi] - prefix[lo]) / (hi - lo) as f64)
        }
    };
    let mut hits: Vec<Hit> = Vec::new();
    for (i, &d) in d2.iter().enumerate().take(n - 1).skip(1) {
        let m = d.abs();
        if m < th.click_floor {
            continue;
        }
        let left = mean(i.saturating_sub(w), i.saturating_sub(1));
        let right = mean((i + 2).min(n), (i + 2 + w).min(n));
        let scale = match (left, right) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => continue,
        };
        let ratio = m / scale.max(1.0);
        if ratio < th.click_ratio {
            continue;
        }
        match hits.last_mut() {
            Some(h) if i - h.pos <= CLICK_MERGE => {
                if ratio > h.ratio {
                    *h = Hit {
                        pos: i,
                        mag: m,
                        ratio,
                    };
                }
            }
            _ => hits.push(Hit {
                pos: i,
                mag: m,
                ratio,
            }),
        }
    }
    let one = [x.to_vec()];
    hits.into_iter()
        .map(|h| {
            let pre = rms_of(&one, h.pos.saturating_sub(CLICK_SIDE), h.pos);
            let post = rms_of(&one, h.pos + 1, h.pos + 1 + CLICK_SIDE);
            let severity = if pre * ONSET_FACTOR < post {
                FaultSeverity::Note
            } else {
                FaultSeverity::Fault
            };
            let mut f = ctx.fault(FaultKind::Click, severity, h.pos);
            f.channel = Some(label);
            f.magnitude = Some(h.mag);
            f.ratio = Some(h.ratio);
            f.level_before = Some(pre);
            f.level_after = Some(post);
            f.threshold = "click_ratio, click_floor, click_span".into();
            f
        })
        .collect()
}

/// A click seen on both channels at one place is one detection (the larger
/// ratio wins).
fn merge_clicks(faults: Vec<AudioFault>) -> Vec<AudioFault> {
    let mut out: Vec<AudioFault> = Vec::new();
    for f in faults {
        if f.kind == FaultKind::Click
            && let Some(prev) = out.iter_mut().find(|p| {
                p.kind == FaultKind::Click && p.sample.abs_diff(f.sample) <= CLICK_MERGE as u64
            })
        {
            if f.ratio > prev.ratio {
                *prev = f;
            }
            continue;
        }
        out.push(f);
    }
    out
}

fn find_saturation(
    ctx: &Ctx,
    x: &[i32],
    label: &'static str,
    th: &AudioThresholds,
) -> Vec<AudioFault> {
    let rail = |v: i32| v >= 32767 || v <= -32768;
    let mut out = Vec::new();
    let mut i = 0;
    while i < x.len() {
        if !rail(x[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < x.len() && rail(x[j]) {
            j += 1;
        }
        if j - i >= th.sat_run {
            let mut f = ctx.fault(FaultKind::Saturation, FaultSeverity::Fault, i);
            f.channel = Some(label);
            f.run = Some((j - i) as u64);
            f.duration_samples = Some((j - i) as u64);
            f.threshold = "sat_run".into();
            out.push(f);
        }
        i = j;
    }
    out
}

fn find_voice_cuts(
    ctx: &Ctx,
    voice: u8,
    clip: &AudioClip,
    th: &AudioThresholds,
) -> Vec<AudioFault> {
    let chans: Vec<Vec<i32>> = (0..clip.channels).map(|c| clip.channel(c)).collect();
    let n = clip.len();
    let env: Vec<i32> = (0..n)
        .map(|i| chans.iter().map(|x| x[i].abs()).max().unwrap_or(0))
        .collect();
    let mut out = Vec::new();
    let mut s = 1;
    while s < n {
        if env[s] != 0 || env[s - 1] == 0 {
            s += 1;
            continue;
        }
        let mut e = s;
        while e < n && env[e] == 0 {
            e += 1;
        }
        if e - s >= VOICE_SILENT_RUN {
            let last_lo = s.saturating_sub(th.voice_cut_samples);
            let peak_last = *env[last_lo..s].iter().max().unwrap_or(&0);
            let peak_prior = *env[s.saturating_sub(VOICE_LOOKBACK)..s]
                .iter()
                .max()
                .unwrap_or(&0);
            if peak_last > th.voice_level
                && f64::from(peak_last) >= th.voice_decay_ratio * f64::from(peak_prior)
            {
                let mut f = ctx.fault(FaultKind::VoiceCut, FaultSeverity::Fault, s);
                f.voice = Some(voice);
                f.level_before = Some(f64::from(peak_last));
                f.fall_samples = Some(th.voice_cut_samples as u64);
                f.threshold = "voice_level, voice_cut_samples, voice_decay_ratio".into();
                out.push(f);
            }
        }
        s = e;
    }
    out
}

/// A sorted summary map of the thresholds, for a caller that wants to print
/// them by name.
#[must_use]
pub fn threshold_map(th: &AudioThresholds) -> BTreeMap<&'static str, f64> {
    BTreeMap::from([
        ("gap_floor", th.gap_floor),
        ("gap_level", th.gap_level),
        ("gap_before", th.gap_before as f64),
        ("gap_min_frames", th.gap_min_frames),
        ("gap_max_frames", th.gap_max_frames),
        ("gap_edge_ratio", th.gap_edge_ratio),
        ("click_ratio", th.click_ratio),
        ("click_floor", th.click_floor),
        ("click_span", th.click_span as f64),
        ("sat_run", th.sat_run as f64),
        ("voice_level", f64::from(th.voice_level)),
        ("voice_cut_samples", th.voice_cut_samples as f64),
        ("voice_decay_ratio", th.voice_decay_ratio),
    ])
}

#[cfg(test)]
mod tests {
    // Test signals are written as plain sums of sines, as the maths reads.
    #![allow(clippy::suboptimal_flops)]

    use super::*;

    const RATE: u32 = 32_040;

    /// A tone chord, three partials, stereo (R a little different).
    fn chord(n: usize) -> Vec<(i32, i32)> {
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(RATE);
                let tau = std::f64::consts::TAU;
                let l = 3000.0 * (tau * 220.0 * t).sin()
                    + 2000.0 * (tau * 330.0 * t).sin()
                    + 1000.0 * (tau * 495.0 * t).sin();
                let r = 0.8 * l + 500.0 * (tau * 110.0 * t).sin();
                (l as i32, r as i32)
            })
            .collect()
    }

    /// The chord with a slowly varying amplitude and a noise floor.
    fn noisy(n: usize) -> Vec<(i32, i32)> {
        let mut seed = 12345u32;
        chord(n)
            .into_iter()
            .enumerate()
            .map(|(i, (l, r))| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = i32::from((seed >> 24) as u8) - 128;
                let env = 0.6 + 0.4 * (i as f64 / 9000.0).sin();
                (
                    (f64::from(l) * env) as i32 + noise,
                    (f64::from(r) * env) as i32 - noise,
                )
            })
            .collect()
    }

    fn clip(v: &[(i32, i32)]) -> AudioClip {
        AudioClip {
            rate: RATE,
            channels: 2,
            samples: v
                .iter()
                .flat_map(|&(l, r)| [l.clamp(-32768, 32767) as i16, r.clamp(-32768, 32767) as i16])
                .collect(),
        }
    }

    fn check(v: &[(i32, i32)]) -> AudioReport {
        audio_check("t.wav", &clip(v), &[], &AudioCheckOptions::default())
    }

    fn kinds(r: &AudioReport, k: FaultKind) -> Vec<&AudioFault> {
        r.faults
            .iter()
            .filter(|f| f.kind == k && f.severity == FaultSeverity::Fault)
            .collect()
    }

    #[test]
    fn clean_signals_are_not_flagged() {
        for v in [chord(32_040 * 3), noisy(32_040 * 3)] {
            let r = check(&v);
            assert_eq!(r.verdict, "ok", "{}", r.render_text());
            assert!(r.faults.is_empty(), "{}", r.render_text());
        }
    }

    #[test]
    fn a_one_frame_hole_is_a_gap_wherever_it_falls() {
        // Unaligned on purpose: the hole straddles two nominal frames.
        for hole_at in [40_000usize, 40_000 + 137, 40_000 + 300] {
            let mut v = chord(32_040 * 3);
            for s in &mut v[hole_at..hole_at + 533] {
                *s = (0, 0);
            }
            let r = check(&v);
            let gaps = kinds(&r, FaultKind::Gap);
            assert_eq!(gaps.len(), 1, "{}", r.render_text());
            let g = gaps[0];
            assert!(g.sample.abs_diff(hole_at as u64) < 40, "{g:?}");
            assert!(g.duration_frames.unwrap() > 0.9, "{g:?}");
            assert!(g.level_before.unwrap() > 1000.0);
            assert_eq!(r.verdict, "faults");
        }
    }

    #[test]
    fn a_rest_that_fades_out_is_not_a_gap() {
        // The same hole, but the tone releases over 400 samples before it
        // and attacks after it: a rest in the music, not a dropped chunk.
        let mut v = chord(32_040 * 3);
        for (k, s) in v[40_000 - 400..40_000].iter_mut().enumerate() {
            let g = 1.0 - k as f64 / 400.0;
            *s = ((f64::from(s.0) * g) as i32, (f64::from(s.1) * g) as i32);
        }
        for s in &mut v[40_000..40_000 + 700] {
            *s = (0, 0);
        }
        let r = check(&v);
        assert!(kinds(&r, FaultKind::Gap).is_empty(), "{}", r.render_text());
    }

    #[test]
    fn silence_at_the_start_or_the_end_is_not_a_gap() {
        let mut v = chord(32_040 * 3);
        for s in &mut v[..2000] {
            *s = (0, 0);
        }
        let end = v.len();
        for s in &mut v[end - 2000..] {
            *s = (0, 0);
        }
        let r = check(&v);
        assert!(kinds(&r, FaultKind::Gap).is_empty(), "{}", r.render_text());
    }

    #[test]
    fn a_step_in_a_playing_tone_is_a_click_fault() {
        let mut v = chord(32_040 * 2);
        // The signal jumps by 9000 and stays there for 64 samples, then back.
        for s in &mut v[30_000..30_064] {
            s.0 += 9000;
        }
        let r = check(&v);
        let clicks = kinds(&r, FaultKind::Click);
        assert!(!clicks.is_empty(), "{}", r.render_text());
        assert!(clicks.iter().any(|c| c.sample.abs_diff(30_000) <= 2));
        assert!(clicks[0].ratio.unwrap() >= 40.0);
    }

    #[test]
    fn an_attack_from_silence_is_a_note_not_a_fault() {
        let mut v = vec![(0, 0); 5000];
        v.extend(chord(5000).into_iter().map(|(l, r)| (l * 3, r * 3)));
        let r = check(&v);
        assert!(
            kinds(&r, FaultKind::Click).is_empty(),
            "{}",
            r.render_text()
        );
        assert_eq!(r.verdict, "ok");
    }

    #[test]
    fn a_clipped_stretch_is_saturation() {
        let mut v = chord(32_040);
        for s in &mut v[5000..5010] {
            *s = (32_767, -32_768);
        }
        let r = check(&v);
        let sat = kinds(&r, FaultKind::Saturation);
        assert_eq!(sat.len(), 2, "{}", r.render_text());
        assert_eq!(sat[0].run, Some(10));
        // A shorter touch of the rail is not.
        let mut v = chord(32_040);
        for s in &mut v[5000..5002] {
            *s = (32_767, 0);
        }
        assert!(kinds(&check(&v), FaultKind::Saturation).is_empty());
    }

    fn voice(decay: bool) -> AudioClip {
        let mut v: Vec<(i32, i32)> = (0..4000)
            .map(|i| {
                let x = (8000.0 * (i as f64 * 0.2).sin()) as i32;
                (x, x)
            })
            .collect();
        if decay {
            // A hardware release: 1/256 of the envelope per sample.
            for (k, s) in v[2000..2256].iter_mut().enumerate() {
                let g = 1.0 - k as f64 / 256.0;
                *s = ((f64::from(s.0) * g) as i32, (f64::from(s.1) * g) as i32);
            }
            for s in &mut v[2256..] {
                *s = (0, 0);
            }
        } else {
            for s in &mut v[2000..] {
                *s = (0, 0);
            }
        }
        clip(&v)
    }

    #[test]
    fn an_abrupt_voice_is_a_cut_and_a_decaying_one_is_not() {
        let main = clip(&chord(8000));
        let opts = AudioCheckOptions::default();
        let r = audio_check("t.wav", &main, &[(3, voice(false))], &opts);
        let cuts = kinds(&r, FaultKind::VoiceCut);
        assert_eq!(cuts.len(), 1, "{}", r.render_text());
        assert_eq!(cuts[0].voice, Some(3));
        assert_eq!(cuts[0].sample, 2000);
        let r = audio_check("t.wav", &main, &[(3, voice(true))], &opts);
        assert!(
            kinds(&r, FaultKind::VoiceCut).is_empty(),
            "{}",
            r.render_text()
        );
    }

    #[test]
    fn a_voice_cut_is_listed_but_does_not_fail_by_default() {
        let main = clip(&chord(8000));
        let r = audio_check(
            "t.wav",
            &main,
            &[(3, voice(false))],
            &AudioCheckOptions::default(),
        );
        assert_eq!(r.summary.voice_cut, 1);
        assert_eq!(r.verdict, "ok");
        let opts = AudioCheckOptions {
            fail_on: FaultKind::ALL.to_vec(),
            ..AudioCheckOptions::default()
        };
        let r = audio_check("t.wav", &main, &[(3, voice(false))], &opts);
        assert_eq!(r.verdict, "faults");
    }

    #[test]
    fn fail_on_picks_which_kinds_fail_the_verdict() {
        let mut v = chord(32_040);
        for s in &mut v[5000..5010] {
            *s = (32_767, 32_767);
        }
        let opts = AudioCheckOptions {
            fail_on: vec![FaultKind::Gap],
            ..AudioCheckOptions::default()
        };
        let r = audio_check("t.wav", &clip(&v), &[], &opts);
        assert_eq!(r.summary.saturation, 2);
        assert_eq!(r.verdict, "ok");
    }

    #[test]
    fn json_schema_one() {
        let r = check(&chord(32_040));
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["schema"], 1);
        for key in [
            "wav",
            "rate",
            "region",
            "frames",
            "frame_samples",
            "thresholds",
            "faults",
            "summary",
            "verdict",
            "fail_on",
        ] {
            assert!(j.get(key).is_some(), "missing {key}");
        }
        assert_eq!(j["region"], "ntsc");
        assert_eq!(j["rate"], 32_040);
        assert!((j["frame_samples"].as_f64().unwrap() - 533.1).abs() < 0.1);
        assert_eq!(j["thresholds"]["sat_run"], 4);
        let pal = audio_check(
            "t",
            &clip(&chord(32_040)),
            &[],
            &AudioCheckOptions {
                region: AudioRegion::Pal,
                ..AudioCheckOptions::default()
            },
        );
        assert!((pal.frame_samples - 640.7).abs() < 0.1);
    }

    #[test]
    fn wav_round_trip_and_refusals() {
        let c = clip(&chord(100));
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + c.samples.len() as u32 * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&RATE.to_le_bytes());
        b.extend_from_slice(&(RATE * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(c.samples.len() as u32 * 2).to_le_bytes());
        for s in &c.samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        let back = parse_wav(&b).unwrap();
        assert_eq!(
            (back.rate, back.channels, back.samples.len()),
            (RATE, 2, 200)
        );
        assert!(parse_wav(b"not a wav at all").is_err());
        b[34] = 8; // 8 bits
        assert!(parse_wav(&b).unwrap_err().contains("unsupported"));
        assert!(FaultKind::parse_list("gap,bogus").is_err());
        assert_eq!(
            FaultKind::parse_list("voice_cut, gap").unwrap(),
            vec![FaultKind::VoiceCut, FaultKind::Gap]
        );
    }
}
