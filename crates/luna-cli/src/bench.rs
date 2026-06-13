//! `luna bench` — run every ROM in a directory headless, detect anomalies,
//! and write a compatibility report + one markdown "issue" per bug.
//!
//! Pure `luna-api` consumer (api-first). Panic-safe: `Emulator::step*` and
//! `load_rom` catch unwinds and return errors, so a single crashing ROM
//! becomes one ❌ row instead of aborting the whole run.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use luna_api::Emulator;

/// Instruction budget per frame for `step_until_frame` (matches `run_state`).
const PER_FRAME_BUDGET: u64 = 30_000;
/// Frame by which boot/intro should be underway; metrics measured after it.
const WARMUP_FRAME: u64 = 120;

/// Outcome bucket for a ROM, worst-first in `report.md`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Bug,
    Suspect,
    NeedsFirmware,
    Ok,
}

impl Status {
    const fn icon(self) -> &'static str {
        match self {
            Self::Bug => "❌ bug",
            Self::Suspect => "⚠️ suspect",
            Self::NeedsFirmware => "🔑 firmware",
            Self::Ok => "✅ ok",
        }
    }
}

struct RomResult {
    name: String,
    slug: String,
    title: String,
    mapper: String,
    status: Status,
    frames_run: u64,
    nmis: u64,
    instrs: u64,
    /// Peak absolute audio sample seen across the run + audio tail.
    audio_peak: i32,
    /// Whether the S-DSP ever keyed a voice with a non-zero envelope — the
    /// sound driver was alive and trying to play, even if the captured PCM
    /// peak stayed quiet. Distinguishes "engine never ran" from "just quiet".
    audio_voice_seen: bool,
    findings: Vec<String>,
    repro: String,
    screenshot: Option<String>,
}

/// A static/black screen with the CPU spinning over no more than this many
/// distinct addresses is treated as a confirmed hang (vs. an alive
/// render/forced-blank issue, which stays ⚠️ suspect).
const HANG_LOOP_MAX: usize = 64;
/// Instruction budget for the end-of-run liveness probe.
const LIVENESS_PROBE_STEPS: u64 = 200_000;

/// Peak `|sample|` above which audio counts as genuinely playing (not just
/// DC/dither). Below it the main window heard nothing audible.
const AUDIO_FLOOR: i32 = 256;
/// If the main window was silent, keep stepping up to this many extra frames
/// purely to sample audio. Many titles (esp. Konami) open with a silent
/// publisher logo / intro and only start the music well after frame 600, so a
/// 10 s window false-flags them as "silent". This is the audio twin of the
/// coproc-testing rule's "a black smoke screenshot ≠ a bug": a silent boot
/// window ≠ dead audio. ~30 s at 60 Hz is enough to reach their first track.
const AUDIO_TAIL_FRAMES: u64 = 1800;

/// Default scripted input: pulse Start a few times to clear title/menu screens
/// (a static title is NOT a bug — `coproc-testing.md`). `frame:mask`, Start=0x1000.
fn default_input() -> Vec<(u64, u16)> {
    vec![
        (90, 0x1000),
        (95, 0x0000),
        (240, 0x1000),
        (245, 0x0000),
        (420, 0x1000),
        (425, 0x0000),
    ]
}

/// `lowercase-kebab` slug of a ROM filename stem, for output paths.
fn slug(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let mut out = String::with_capacity(stem.len());
    let mut prev_dash = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Run one ROM and classify it.
fn bench_one(path: &Path, frames: u64, input: &[(u64, u16)], screens_dir: &Path) -> RomResult {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();
    let slug = slug(&name);
    let repro = format!(
        "luna state -n 40000000 --screenshot /tmp/{slug}.png \"{}\"",
        path.display()
    );

    let mut em = Emulator::new();
    let info = match em.load_rom(path) {
        Ok(info) => info,
        Err(e) => {
            return RomResult {
                name,
                slug,
                title: "?".into(),
                mapper: "?".into(),
                status: Status::Bug,
                frames_run: 0,
                nmis: 0,
                instrs: 0,
                audio_peak: 0,
                audio_voice_seen: false,
                findings: vec![format!("load failed: {e}")],
                repro,
                screenshot: None,
            };
        }
    };

    // Drive frames, applying input checkpoints; sample metrics along the way.
    let mut applied = 0usize;
    let mut crash: Option<String> = None;
    let mut nmis_at_warmup = 0u64;
    let mut audio_peak = 0i32;
    let mut audio_voice_seen = false;
    let mut fb_first: Option<u64> = None;
    let mut fb_changed = false;
    let mut last_frame = 0u64;
    for f in 0..frames {
        while applied < input.len() && input[applied].0 <= f {
            let _ = em.set_joypad(0, input[applied].1);
            applied += 1;
        }
        if let Err(e) = em.step_until_frame(PER_FRAME_BUDGET) {
            crash = Some(e.to_string());
            break;
        }
        if em.cpu_state().is_ok_and(|c| c.stopped) {
            break; // CPU halted (STP) — no point spinning
        }
        // Audio activity: drain this frame's samples, track peak amplitude.
        if let Ok(samples) = em.drain_audio(usize::MAX) {
            for (l, r) in samples {
                audio_peak = audio_peak.max(i32::from(l).abs()).max(i32::from(r).abs());
            }
        }
        let st = em.state();
        // The driver is alive and trying to play if any voice keyed on with a
        // live envelope — true even when the captured PCM happens to be quiet.
        audio_voice_seen |= st.apu.voice_envelope.iter().any(|&e| e > 0);
        last_frame = st.scheduler.frame_count;
        if f == WARMUP_FRAME {
            nmis_at_warmup = st.scheduler.nmis_serviced;
        }
        // Past warm-up, hash the framebuffer EVERY frame (exact, cheap — the
        // API hashes the native RGB buffer, no RGBA re-render); flag static if
        // it never changes. Per-frame beats sampling-every-Nth, which can
        // stride right over a brief change and false-flag a live screen.
        if f >= WARMUP_FRAME {
            if let Ok(h) = em.framebuffer_hash() {
                match fb_first {
                    None => fb_first = Some(h),
                    Some(h0) if h != h0 => fb_changed = true,
                    Some(_) => {}
                }
            }
        }
    }

    let st = em.state();
    let nmis = st.scheduler.nmis_serviced;
    let mut findings: Vec<String> = Vec::new();

    // ❌ confirmed-bug signals.
    if let Some(msg) = &crash {
        findings.push(format!("emulator error during run: {msg}"));
    }
    if em.cpu_state().is_ok_and(|c| c.stopped) {
        findings.push("65C816 halted (STP / runaway into data)".into());
    }
    if st.apu.spc_stopped {
        findings.push("SPC700 stopped".into());
    }
    let mut confirmed = !findings.is_empty();

    let static_fb = fb_first.is_some() && !fb_changed;
    let blank = !em.frame_showed_content().unwrap_or(true);
    let nmi_starved = nmis <= nmis_at_warmup;

    // Dead screen (static or forced-blank) + a CPU spinning over a tiny set of
    // addresses = a confirmed hang. If the CPU is alive (many distinct PCs),
    // the dead screen is a render/forced-blank issue → ⚠️ suspect, not a hang.
    // The liveness probe (`loop_probe`) is the new API signal that lets us tell
    // the two apart — NMI-starvation alone never could (Super FX/IRQ games are
    // alive without NMI).
    if !confirmed && (static_fb || blank) {
        let distinct = em
            .loop_probe(LIVENESS_PROBE_STEPS)
            .map_or(usize::MAX, |p| p.distinct_pcs);
        let where_ = if blank { "black" } else { "static" };
        let nmi = if nmi_starved {
            ", no NMIs after warm-up"
        } else {
            ""
        };
        if distinct <= HANG_LOOP_MAX {
            findings.push(format!(
                "CPU stuck in a {distinct}-address loop while the screen is {where_}{nmi} — hang"
            ));
            confirmed = true;
        } else {
            findings.push(format!(
                "screen {where_} for the whole run but CPU alive ({distinct} distinct PCs{nmi}) — render/forced-blank issue"
            ));
        }
    }

    // 🔑 firmware (informational alongside whatever else was found).
    if let Some(fw) = &info.missing_firmware {
        findings.push(format!("missing coprocessor firmware '{fw}' (inert)"));
    }
    // Audio peak is surfaced as its own report column (silence over a 10s
    // window is common for intros), not a status-driving finding.

    let status = if confirmed {
        Status::Bug
    } else if info.missing_firmware.is_some() {
        Status::NeedsFirmware
    } else if static_fb || blank {
        Status::Suspect
    } else {
        Status::Ok
    };

    let screenshot = em.render_frame_png(false).ok().and_then(|png| {
        std::fs::write(screens_dir.join(format!("{slug}.png")), png)
            .ok()
            .map(|()| format!("screenshots/{slug}.png"))
    });

    // Audio tail: if the main window heard nothing, keep stepping (pulsing
    // Start) purely to sample audio. A silent boot window is NOT dead audio —
    // it's usually a silent intro before the first track. Skipped for confirmed
    // bugs / halted CPUs (pointless) so it only costs time on otherwise-✅ ROMs.
    if !confirmed && crash.is_none() && audio_peak < AUDIO_FLOOR {
        for f in 0..AUDIO_TAIL_FRAMES {
            // Pulse Start every ~150 frames to nudge past attract/menu gates.
            let mask = if f % 150 < 4 { 0x1000 } else { 0x0000 };
            let _ = em.set_joypad(0, mask);
            if em.step_until_frame(PER_FRAME_BUDGET).is_err() {
                break;
            }
            if let Ok(samples) = em.drain_audio(usize::MAX) {
                for (l, r) in samples {
                    audio_peak = audio_peak.max(i32::from(l).abs()).max(i32::from(r).abs());
                }
            }
            audio_voice_seen |= em.state().apu.voice_envelope.iter().any(|&e| e > 0);
            if audio_peak >= AUDIO_FLOOR {
                break; // found the music — no need to run the full tail
            }
        }
    }

    RomResult {
        name,
        slug,
        title: info.title,
        mapper: info.mapper,
        status,
        frames_run: last_frame,
        nmis,
        instrs: st.stats.instructions_executed,
        audio_peak,
        audio_voice_seen,
        findings,
        repro,
        screenshot,
    }
}

/// `luna bench` entry point.
pub(crate) fn run_bench(
    dir: &Path,
    out: &Path,
    frames: u64,
    input: Option<Vec<(u64, u16)>>,
) -> ExitCode {
    let input = input.unwrap_or_else(default_input);

    let mut roms: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("sfc" | "smc")))
            .collect(),
        Err(e) => {
            eprintln!("error: reading {}: {e}", dir.display());
            return ExitCode::from(1);
        }
    };
    roms.sort();
    if roms.is_empty() {
        eprintln!("error: no .sfc/.smc ROMs in {}", dir.display());
        return ExitCode::from(1);
    }

    let screens_dir = out.join("screenshots");
    let bugs_dir = out.join("bugs");
    // Clear stale bug files so a now-fixed ROM doesn't keep a leftover report.
    let _ = std::fs::remove_dir_all(&bugs_dir);
    for d in [out, &screens_dir, &bugs_dir] {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("error: creating {}: {e}", d.display());
            return ExitCode::from(1);
        }
    }

    let mut results = Vec::with_capacity(roms.len());
    for (i, rom) in roms.iter().enumerate() {
        let r = bench_one(rom, frames, &input, &screens_dir);
        println!(
            "[{}/{}] {:<11} {}",
            i + 1,
            roms.len(),
            r.status.icon(),
            r.name
        );
        results.push(r);
    }

    write_reports(out, &bugs_dir, dir, frames, &results);
    let count = |s: Status| results.iter().filter(|r| r.status == s).count();
    println!(
        "\nReport: {}  ({} ok, {} bug, {} suspect, {} firmware)",
        out.join("report.md").display(),
        count(Status::Ok),
        count(Status::Bug),
        count(Status::Suspect),
        count(Status::NeedsFirmware),
    );
    ExitCode::SUCCESS
}

/// Write `report.md` + per-bug/suspect markdown files.
fn write_reports(out: &Path, bugs_dir: &Path, rom_dir: &Path, frames: u64, results: &[RomResult]) {
    let count = |s: Status| results.iter().filter(|r| r.status == s).count();
    let mut rep = String::new();
    rep.push_str("# luna ROM benchmark\n\n");
    let _ = writeln!(
        rep,
        "Corpus: `{}` · {} ROMs · {frames} frames/ROM · headless via `luna-api`.\n",
        rom_dir.display(),
        results.len()
    );
    let _ = writeln!(
        rep,
        "**Summary:** {} ✅ ok · {} ❌ bug · {} ⚠️ suspect · {} 🔑 firmware.\n",
        count(Status::Ok),
        count(Status::Bug),
        count(Status::Suspect),
        count(Status::NeedsFirmware),
    );
    rep.push_str("| ROM | Status | Mapper | Frames | NMIs | Audio | Notes |\n");
    rep.push_str("|---|---|---|--:|--:|--:|---|\n");
    for s in [
        Status::Bug,
        Status::Suspect,
        Status::NeedsFirmware,
        Status::Ok,
    ] {
        for r in results.iter().filter(|r| r.status == s) {
            let note = r.findings.first().map_or("—", String::as_str);
            let link = r
                .screenshot
                .as_ref()
                .map_or_else(|| r.name.clone(), |p| format!("[{}]({p})", r.name));
            // peak ≥ floor → real audio (show the number); below floor but a
            // voice keyed on → engine alive, just quiet; otherwise no audio
            // heard even after the tail.
            let audio = if r.audio_peak >= AUDIO_FLOOR {
                r.audio_peak.to_string()
            } else if r.audio_voice_seen {
                "quiet".to_string()
            } else {
                "silent".to_string()
            };
            let _ = writeln!(
                rep,
                "| {link} | {} | {} | {} | {} | {audio} | {note} |",
                r.status.icon(),
                r.mapper,
                r.frames_run,
                r.nmis,
            );
        }
    }

    rep.push_str(
        "\n*Audio column: a number is the peak `|sample|` once real audio was \
         heard; **quiet** = the sound engine keyed voices on but stayed below \
         the audible floor; **silent** = nothing heard even after the audio \
         tail. A silent *boot* window is not dead audio (intros are often \
         silent) — the tail keeps sampling ~30 s past the main window to reach \
         the first track before reporting.*\n",
    );

    rep.push_str("\n## API-coverage notes\n\n");
    rep.push_str(
        "Progress probing what `luna-api` lets the bench observe:\n\n\
         - ✅ **CPU infinite-loop / liveness detector** — `Emulator::loop_probe` \
           (distinct PB:PC count) separates a real hang from a wait-for-input \
           loop. Promoted a ⚠️ suspect to a confirmed ❌ hang.\n\
         - ✅ **Audio-activity metric** — peak PCM via `drain_audio` + a \
           voice-keyed-on signal from `EmulatorState::apu`, sampled across the \
           run and an audio tail. Caveat learned: a fixed boot window \
           false-flags silent intros, so the tail is required for an honest call.\n\
         - ⏳ **Per-frame `framebuffer_changed` flag** — cheaper + less \
           false-prone than hashing RGBA each sample.\n\
         - ⏳ **Reached-gameplay heuristic** — e.g. input-responsiveness or \
           sprite activity, to separate \"sitting at a menu\" from \"actually \
           playing\".\n",
    );

    if let Err(e) = std::fs::write(out.join("report.md"), &rep) {
        eprintln!("error: writing report.md: {e}");
    }

    for r in results
        .iter()
        .filter(|r| matches!(r.status, Status::Bug | Status::Suspect))
    {
        let mut b = String::new();
        let _ = writeln!(b, "# {} — {}\n", r.status.icon(), r.name);
        let _ = writeln!(b, "- **Title (header):** {}", r.title);
        let _ = writeln!(b, "- **Mapper:** {}", r.mapper);
        let _ = writeln!(
            b,
            "- **Frames run:** {} · **NMIs:** {} · **Instrs:** {}\n",
            r.frames_run, r.nmis, r.instrs
        );
        b.push_str("## Symptoms\n\n");
        for f in &r.findings {
            let _ = writeln!(b, "- {f}");
        }
        if let Some(p) = &r.screenshot {
            let _ = writeln!(b, "\n## Screenshot\n\n![screenshot](../{p})");
        }
        let _ = writeln!(b, "\n## Repro\n\n```\n{}\n```", r.repro);
        if r.status == Status::Suspect {
            b.push_str(
                "\n> ⚠️ Heuristic flag — **verify in the GUI** before treating as a real bug.\n",
            );
        }
        let _ = std::fs::write(bugs_dir.join(format!("{}.md", r.slug)), b);
    }
}
