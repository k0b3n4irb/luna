//! `luna bench` — run every ROM in a directory headless, detect anomalies,
//! and write a compatibility report + one markdown "issue" per bug.
//!
//! Pure `luna-api` consumer (api-first). Panic-safe: `Emulator::step*` and
//! `load_rom` catch unwinds and return errors, so a single crashing ROM
//! becomes one ❌ row instead of aborting the whole run.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
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
    findings: Vec<String>,
    repro: String,
    screenshot: Option<String>,
}

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

/// Hash the published framebuffer to detect a wholly static screen.
fn framebuffer_hash(em: &Emulator) -> u64 {
    let mut h = DefaultHasher::new();
    em.render_frame_rgba(false).unwrap_or_default().hash(&mut h);
    h.finish()
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
    let mut hashes: Vec<u64> = Vec::new();
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
        let st = em.state();
        last_frame = st.scheduler.frame_count;
        if f == WARMUP_FRAME {
            nmis_at_warmup = st.scheduler.nmis_serviced;
        }
        if f == WARMUP_FRAME || f == frames / 2 || f + 1 == frames {
            hashes.push(framebuffer_hash(&em));
        }
    }

    let st = em.state();
    let nmis = st.scheduler.nmis_serviced;
    let mut findings: Vec<String> = Vec::new();

    // ❌ confirmed
    if let Some(msg) = &crash {
        findings.push(format!("emulator error during run: {msg}"));
    }
    if em.cpu_state().is_ok_and(|c| c.stopped) {
        findings.push("65C816 halted (STP / runaway into data)".into());
    }
    if st.apu.spc_stopped {
        findings.push("SPC700 stopped".into());
    }
    if let Some(op) = &st.apu.unimplemented_opcode {
        findings.push(format!(
            "SPC700 unimplemented opcode ${:02X} @ ${:04X}",
            op.opcode, op.pc
        ));
    }
    let confirmed = !findings.is_empty();

    // 🔑 firmware
    if let Some(fw) = &info.missing_firmware {
        findings.push(format!("missing coprocessor firmware '{fw}' (inert)"));
    }

    // ⚠️ suspect (only if not already confirmed). A live game animates its
    // framebuffer even without servicing NMI (Super FX rendering, IRQ-driven
    // attract demos) — so NMI-starvation ALONE is not a freeze; we only flag
    // it as corroborating evidence when the framebuffer is also static.
    if !confirmed {
        let static_fb = hashes.len() >= 2 && hashes.iter().all(|h| *h == hashes[0]);
        let nmi_starved = nmis <= nmis_at_warmup;
        let blank = !em.frame_showed_content().unwrap_or(true);
        if static_fb {
            let extra = if nmi_starved {
                format!(" and no NMIs after warm-up (frame {WARMUP_FRAME}: {nmis_at_warmup})")
            } else {
                String::new()
            };
            findings.push(format!(
                "framebuffer never changed across {frames} frames{extra} — possible freeze/hang"
            ));
        }
        if blank {
            findings.push("screen forced-blank / no visible content for the whole run".into());
        }
    }

    let status = if confirmed {
        Status::Bug
    } else if info.missing_firmware.is_some() {
        Status::NeedsFirmware
    } else if findings.is_empty() {
        Status::Ok
    } else {
        Status::Suspect
    };

    let screenshot = em.render_frame_png(false).ok().and_then(|png| {
        std::fs::write(screens_dir.join(format!("{slug}.png")), png)
            .ok()
            .map(|()| format!("screenshots/{slug}.png"))
    });

    RomResult {
        name,
        slug,
        title: info.title,
        mapper: info.mapper,
        status,
        frames_run: last_frame,
        nmis,
        instrs: st.stats.instructions_executed,
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
    rep.push_str("| ROM | Status | Mapper | Frames | NMIs | Instrs | Notes |\n");
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
            let _ = writeln!(
                rep,
                "| {link} | {} | {} | {} | {} | {} | {note} |",
                r.status.icon(),
                r.mapper,
                r.frames_run,
                r.nmis,
                r.instrs,
            );
        }
    }

    rep.push_str("\n## API-coverage notes\n\n");
    rep.push_str(
        "Signals the benchmark wanted but `luna-api` does not expose yet \
         (each would turn a ⚠️ suspect into a confirmed verdict):\n\n\
         - **CPU infinite-loop / liveness detector** — distinguish a real hang \
           from a legitimate wait-for-input loop (today only STP is observable).\n\
         - **Audio-activity metric** — whether the DSP is producing non-silent \
           samples (to flag dead audio without ears).\n\
         - **Per-frame `framebuffer_changed` flag** — cheaper + less false-prone \
           than hashing RGBA each sample.\n\
         - **Reached-gameplay heuristic** — e.g. input-responsiveness or sprite \
           activity, to separate \"sitting at a menu\" from \"actually playing\".\n",
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
