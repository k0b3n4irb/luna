//! `luna diff` — per-frame MATCH/DIFF between two ROMs (issue #225).
//!
//! The "compare at equal PPU frame" protocol that validates a compiler or
//! library change: build A and build B of the same program are run side by
//! side in one process, the displayed frame is hashed at every PPU frame,
//! and each requested frame is MATCH when A's hash at `F` equals B's hash
//! at some frame within `F ± tolerance` (the boot-length offset a codegen
//! change can introduce), DIFF otherwise. Exit 0 = every frame matched,
//! 1 = at least one DIFF, 2 = usage error — the same CI contract as
//! `luna test`.

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::parsers::parse_input_script;
use crate::rom::load_rom_into;

/// Instruction budget per frame (matches the other frame-stepping paths).
const FRAME_BUDGET: u64 = 200_000;

/// Options shared by both machines.
pub(crate) struct DiffOptions<'a> {
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
    pub input_script: Option<&'a str>,
    pub force_display: bool,
    pub native_res: bool,
    pub tolerance: u64,
    pub screenshot_dir: Option<&'a std::path::Path>,
    pub out: Option<&'a std::path::Path>,
}

/// One machine being stepped frame by frame: its hash at every frame
/// passed, plus the PNG at each requested frame (kept until the verdict
/// decides whether it is written).
struct Machine {
    em: luna_api::Emulator,
    hashes: BTreeMap<u64, u64>,
    pngs: BTreeMap<u64, Vec<u8>>,
    halted: bool,
}

impl Machine {
    fn load(rom: &std::path::Path, o: &DiffOptions<'_>) -> Result<Self, String> {
        let mut em = luna_api::Emulator::new();
        load_rom_into(
            &mut em,
            rom,
            o.force_mapper,
            o.force_region,
            None,
            o.power_on,
        )?;
        if o.native_res {
            em.set_native_capture(true).map_err(|e| e.to_string())?;
        }
        Ok(Self {
            em,
            hashes: BTreeMap::new(),
            pngs: BTreeMap::new(),
            halted: false,
        })
    }

    fn frame(&self) -> u64 {
        self.em.frame_count().unwrap_or(0)
    }

    /// Record the displayed frame's hash (and PNG when wanted) for the
    /// frame the machine is currently on.
    fn record(&mut self, o: &DiffOptions<'_>, want_png: bool) -> Result<(), String> {
        let f = self.frame();
        let hash = if o.native_res {
            self.em.frame_hash_native()
        } else {
            self.em.frame_hash(o.force_display)
        }
        .map_err(|e| e.to_string())?;
        self.hashes.insert(f, hash);
        if want_png {
            let png = if o.native_res {
                self.em.render_frame_png_native()
            } else {
                self.em.render_frame_png(o.force_display)
            }
            .map_err(|e| e.to_string())?;
            self.pngs.insert(f, png);
        }
        Ok(())
    }

    /// Advance to the next PPU frame, applying any input checkpoint due
    /// on the frame being left. `false` once the machine has halted.
    fn step_frame(&mut self, checkpoints: &[(u64, u16)]) -> Result<bool, String> {
        if self.halted {
            return Ok(false);
        }
        let f = self.frame();
        for &(_, mask) in checkpoints.iter().filter(|&&(at, _)| at == f) {
            self.em.set_joypad(0, mask).map_err(|e| e.to_string())?;
        }
        let ran = match self.em.step_until_frame(FRAME_BUDGET) {
            Ok(n) => n,
            Err(luna_api::ApiError::Panic(msg)) => {
                eprintln!("note: machine panicked at frame {f}: {msg}");
                0
            }
            Err(e) => return Err(e.to_string()),
        };
        if ran == 0 || self.frame() == f {
            self.halted = true;
            return Ok(false);
        }
        Ok(true)
    }
}

/// Verdict for one requested frame.
#[derive(serde::Serialize)]
struct FrameVerdict {
    frame: u64,
    /// `"match"` or `"diff"`.
    status: &'static str,
    /// For a match: the frame offset `b - a` that produced it (0 when the
    /// two machines agree at the same frame).
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    a_hash: String,
    b_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot_b: Option<String>,
}

/// The `--out` JSON report.
#[derive(serde::Serialize)]
struct Report<'a> {
    a: &'a std::path::Path,
    b: &'a std::path::Path,
    tolerance: u64,
    frames: Vec<FrameVerdict>,
    diff_count: usize,
}

/// `luna diff` entry point.
pub(crate) fn run_diff(
    rom_a: &std::path::Path,
    rom_b: &std::path::Path,
    frames: &[u64],
    o: &DiffOptions<'_>,
) -> ExitCode {
    let mut frames: Vec<u64> = frames.to_vec();
    frames.sort_unstable();
    frames.dedup();
    if frames.is_empty() {
        eprintln!("error: --frames needs at least one PPU frame number");
        return ExitCode::from(2);
    }
    let checkpoints: Vec<(u64, u16)> = match o.input_script.map(parse_input_script) {
        None => Vec::new(),
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            eprintln!("error: --input: {e}");
            return ExitCode::from(2);
        }
    };
    let (mut a, mut b) = match (Machine::load(rom_a, o), Machine::load(rom_b, o)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let last = *frames.last().expect("non-empty");
    let horizon = last + o.tolerance;
    let want_png = o.screenshot_dir.is_some();

    // Run both machines frame by frame to the horizon. A only needs its
    // hash at the requested frames; B needs one at every frame inside a
    // tolerance window, i.e. simply every frame (one u64 each).
    if let Err(e) = drive(&mut a, o, &frames, want_png, last, &checkpoints, false) {
        eprintln!("error: rom A: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = drive(&mut b, o, &frames, want_png, horizon, &checkpoints, true) {
        eprintln!("error: rom B: {e}");
        return ExitCode::from(1);
    }

    let mut verdicts = Vec::with_capacity(frames.len());
    let mut diff_count = 0usize;
    for &f in &frames {
        let (Some(&ha), Some(&hb_same)) = (a.hashes.get(&f), b.hashes.get(&f)) else {
            eprintln!("frame {f}: DIFF (a machine halted before reaching it)");
            verdicts.push(FrameVerdict {
                frame: f,
                status: "diff",
                offset: None,
                a_hash: a
                    .hashes
                    .get(&f)
                    .map_or_else(String::new, |h| format!("{h:016x}")),
                b_hash: b
                    .hashes
                    .get(&f)
                    .map_or_else(String::new, |h| format!("{h:016x}")),
                screenshot_a: None,
                screenshot_b: None,
            });
            diff_count += 1;
            continue;
        };
        // Nearest offset first: 0, -1, +1, -2, +2, …
        let tol = i64::try_from(o.tolerance).unwrap_or(i64::MAX);
        let offset = (0..=tol)
            .flat_map(|d| if d == 0 { vec![0] } else { vec![-d, d] })
            .find(|&d| {
                f.checked_add_signed(d)
                    .and_then(|fb| b.hashes.get(&fb))
                    .is_some_and(|&hb| hb == ha)
            });
        let (status, shots) = if let Some(d) = offset {
            println!("frame {f}: MATCH (offset {d:+}) a={ha:016x} b={hb_same:016x}");
            ("match", (None, None))
        } else {
            println!("frame {f}: DIFF a={ha:016x} b={hb_same:016x}");
            diff_count += 1;
            let shots = match o.screenshot_dir {
                Some(dir) => match write_shots(dir, f, &a.pngs, &b.pngs) {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(1);
                    }
                },
                None => (None, None),
            };
            ("diff", shots)
        };
        verdicts.push(FrameVerdict {
            frame: f,
            status,
            offset,
            a_hash: format!("{ha:016x}"),
            b_hash: format!("{hb_same:016x}"),
            screenshot_a: shots.0,
            screenshot_b: shots.1,
        });
    }
    println!(
        "{} frame(s): {} match, {} diff (tolerance ±{})",
        frames.len(),
        frames.len() - diff_count,
        diff_count,
        o.tolerance
    );
    if let Some(path) = o.out {
        let report = Report {
            a: rom_a,
            b: rom_b,
            tolerance: o.tolerance,
            frames: verdicts,
            diff_count,
        };
        let json = serde_json::to_string_pretty(&report).expect("report serialises");
        let res = if path.as_os_str() == "-" {
            println!("{json}");
            Ok(())
        } else {
            std::fs::write(path, json)
        };
        if let Err(e) = res {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }
    if diff_count == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Step `m` to `horizon`, recording its hash at every frame (`every`) or
/// only at the requested `frames`, with PNGs at the requested frames when
/// `want_png`.
fn drive(
    m: &mut Machine,
    o: &DiffOptions<'_>,
    frames: &[u64],
    want_png: bool,
    horizon: u64,
    checkpoints: &[(u64, u16)],
    every: bool,
) -> Result<(), String> {
    loop {
        let f = m.frame();
        let requested = frames.binary_search(&f).is_ok();
        if every || requested {
            m.record(o, want_png && requested)?;
        }
        if f >= horizon || !m.step_frame(checkpoints)? {
            return Ok(());
        }
    }
}

/// Write `f_a.png` / `f_b.png` for a DIFF frame; returns their paths.
fn write_shots(
    dir: &std::path::Path,
    f: u64,
    a: &BTreeMap<u64, Vec<u8>>,
    b: &BTreeMap<u64, Vec<u8>>,
) -> Result<(Option<String>, Option<String>), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let mut out = (None, None);
    for (png, tag, slot) in [(a.get(&f), "a", 0), (b.get(&f), "b", 1)] {
        let Some(png) = png else { continue };
        let path = dir.join(format!("frame_{f}_{tag}.png"));
        std::fs::write(&path, png).map_err(|e| format!("writing {}: {e}", path.display()))?;
        let s = Some(path.display().to_string());
        if slot == 0 {
            out.0 = s;
        } else {
            out.1 = s;
        }
    }
    Ok(out)
}
