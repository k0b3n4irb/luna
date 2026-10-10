//! `luna profile` — real master cycles per symbol (issue #227).
//!
//! Replaces a static per-instruction-weight estimate with what the
//! machine actually paid: every step credits its master cycles — bus +
//! internal cycles and the DMA / HDMA / refresh stalls charged during it
//! — to the instruction's address; the report folds those onto the
//! nearest `.sym` label (or a 256-byte page without one), heaviest first.

use std::process::ExitCode;

use crate::rom::load_rom_into;

/// Instruction budget per frame (matches the other frame-stepping paths).
use luna_api::FRAME_STEP_BUDGET as FRAME_BUDGET;

/// Options for [`run_profile`].
pub(crate) struct ProfileOptions<'a> {
    pub steps: u64,
    pub until_frame: Option<u64>,
    pub from_frame: u64,
    /// Controller flags — the same set `state` takes, so a coverage run
    /// can replay a manifest that plugs a mouse or a Super Scope.
    pub input: crate::parsers::InputFlags<'a>,
    pub sym: Option<&'a std::path::Path>,
    pub top: usize,
    pub out: Option<&'a std::path::Path>,
    /// Write every executed 24-bit PC, sorted, as little-endian `u32`s
    /// (`--pc-set`; `OpenSNES` R-C — a coverage tool folds them onto lines).
    pub pc_set: Option<&'a std::path::Path>,
    /// `--budget SYMBOL=MCLK` gates (`OpenSNES` R-B): the symbol's worst
    /// completed frame must not exceed `MCLK`, else exit 1.
    pub budgets: &'a [String],
    /// `--stack-floor N`: exit 1 if `S` ever went below N (`OpenSNES` R3).
    pub stack_floor: Option<u16>,
    /// `--gsu-pc-set`: write the distinct GSU PCs executed (`OpenSNES` R3).
    pub gsu_pc_set: Option<&'a std::path::Path>,
    /// `--input-at`: the routine whose arrivals index the input scripts.
    pub input_at: Option<&'a str>,
    /// `--frames-out`: the frame series as CSV (issue #270).
    pub frames_out: Option<&'a std::path::Path>,
    /// `--worst N`: per-symbol tables of the N heaviest frames.
    pub worst: usize,
    /// `--max-frame-mclk` / `--max-lag-frames` / `--max-lag-run`.
    pub frame_gates: crate::profile_frames::FrameGates,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
}

/// The `--out` JSON: the folded report plus what was profiled.
#[derive(serde::Serialize)]
struct Report<'a> {
    rom: &'a std::path::Path,
    /// First PPU frame the profile covers (`--from-frame`).
    from_frame: u64,
    /// PPU frame reached at the end of the run.
    end_frame: u64,
    #[serde(flatten)]
    profile: luna_api::ProfileReport,
    /// One verdict per `--budget`, in command-line order.
    budgets: Vec<BudgetVerdict>,
    /// The frame series in a few numbers, and the frame gates' verdicts.
    frame_summary: crate::profile_frames::FrameSummary,
    /// Deepest native-mode stack reach over the profiled window, and the
    /// `--stack-floor` verdict if one was asked for.
    stack: StackReport,
    /// Super FX jobs completed in the window, or `None` without a GSU.
    #[serde(skip_serializing_if = "Option::is_none")]
    gsu: Option<GsuReport>,
    /// SA-1 accounting for the window, or `None` without an SA-1.
    #[serde(skip_serializing_if = "Option::is_none")]
    sa1: Option<Sa1Report>,
}

/// SA-1 accounting over the profiled window (`OpenSNES` R5): the
/// difference between two cumulative readings.
#[derive(serde::Serialize)]
struct Sa1Report {
    #[serde(flatten)]
    window: luna_api::Sa1Stats,
    /// Share of SA-1 clocks spent parked in `WAI`.
    idle_share: f64,
    /// Share of the SA-1's *busy* clocks lost to bus contention.
    conflict_share: f64,
    /// How fast the SA-1 runs while it runs: the nominal 10.74 MHz scaled
    /// by the busy clocks not lost to contention or DMA. Idle time is left
    /// out on purpose — it says whether the chip is used, not how fast it
    /// goes — and reported as `idle_share`. `None` if it never ran.
    effective_mhz: Option<f64>,
}

/// Super FX accounting over the profiled window (`OpenSNES` R3).
#[derive(serde::Serialize)]
struct GsuReport {
    /// Jobs (GO→STOP) completed in the window.
    jobs: usize,
    /// GSU clocks spent executing, summed.
    gsu_cycles: u64,
    /// GSU instructions retired, summed.
    instructions: u64,
    /// Opcode fetches served from the 512-byte cache.
    cache_hits: u64,
    /// Fetches that missed and refilled a cache line.
    cache_misses: u64,
    /// Clocks the GSU was running but parked on a bus it did not own.
    stall_cycles: u64,
    /// The worst job by GSU clocks, which is what a frame budget is set by.
    worst: Option<luna_api::SuperFxJob>,
    /// Every job, in order.
    per_job: Vec<luna_api::SuperFxJob>,
}

/// The stack low-water mark plus its optional gate (`OpenSNES` R3).
#[derive(serde::Serialize)]
struct StackReport {
    /// `None` when the run never left emulation mode.
    low: Option<luna_api::StackLow>,
    /// The `--stack-floor` value, if given.
    floor: Option<u16>,
    /// `false` only when a floor was given and the stack went below it.
    ok: bool,
}

/// The outcome of one `--budget SYMBOL=MCLK` gate.
#[derive(serde::Serialize)]
struct BudgetVerdict {
    symbol: String,
    limit: u64,
    /// The symbol's worst completed frame; `None` when it never ran.
    max: Option<u64>,
    max_frame: Option<u64>,
    ok: bool,
}

/// Parse one `--budget SYMBOL=MCLK` argument.
fn parse_budget(spec: &str) -> Result<(String, u64), String> {
    let (sym, limit) = spec
        .rsplit_once('=')
        .ok_or_else(|| format!("`{spec}`: expected SYMBOL=MCLK"))?;
    let sym = sym.trim();
    if sym.is_empty() {
        return Err(format!("`{spec}`: empty symbol"));
    }
    let limit = limit
        .trim()
        .replace('_', "")
        .parse::<u64>()
        .map_err(|e| format!("`{spec}`: bad master-cycle count: {e}"))?;
    Ok((sym.to_string(), limit))
}

/// Judge the `--budget` gates against the folded report. A symbol the
/// loaded table does not know is a usage error (a typo must not pass);
/// a known symbol that never ran costs 0 and passes.
fn judge_budgets(
    em: &luna_api::Emulator,
    report: &luna_api::ProfileReport,
    budgets: &[(String, u64)],
) -> Result<Vec<BudgetVerdict>, String> {
    let mut out = Vec::with_capacity(budgets.len());
    for (symbol, limit) in budgets {
        let entry = report.entries.iter().find(|e| &e.symbol == symbol);
        if entry.is_none() && em.resolve_symbol(symbol).is_none() {
            return Err(format!(
                "--budget {symbol}: unknown symbol (not in the loaded .sym, and it never ran)"
            ));
        }
        let per_frame = entry.and_then(|e| e.per_frame);
        let max = per_frame.map(|p| p.max);
        out.push(BudgetVerdict {
            symbol: symbol.clone(),
            limit: *limit,
            max,
            max_frame: per_frame.map(|p| p.max_frame),
            ok: max.unwrap_or(0) <= *limit,
        });
    }
    Ok(out)
}

/// Write a sorted PC set as little-endian `u32`s — the encoding a
/// coverage tool reads. Shared by the 65816 and GSU sets so they cannot
/// drift apart in format.
fn write_pc_set(path: &std::path::Path, pcs: &[u32]) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(pcs.len() * 4);
    for pc in pcs {
        bytes.extend_from_slice(&pc.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

/// The SA-1's nominal clock: master / 2 (ares `SA1::step()` = `step(2)`).
const SA1_NOMINAL_MHZ: f64 = 21.477_272 / 2.0;

/// `luna profile` entry point.
pub(crate) fn run_profile(rom: &std::path::Path, o: &ProfileOptions<'_>) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(
        &mut em,
        rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    ) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if let Some(sym) = o.sym {
        match em.load_symbols(sym) {
            Ok(n) => eprintln!("loaded {n} symbols from {}", sym.display()),
            Err(e) => {
                eprintln!("error: --sym {}: {e}", sym.display());
                return ExitCode::from(1);
            }
        }
    }
    let budgets: Vec<(String, u64)> = match o.budgets.iter().map(|b| parse_budget(b)).collect() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: --budget {e}");
            return ExitCode::from(2);
        }
    };
    let mut script = match crate::parsers::apply_input_flags(&mut em, &o.input) {
        Ok(s) => s,
        Err(code) => return ExitCode::from(code),
    };
    // Warm-up to `--from-frame` with the profiler off, applying input on
    // the way; then profile to the end.
    let frame = |em: &luna_api::Emulator| em.frame_count().unwrap_or(0);
    if let Some(spec) = o.input_at {
        match crate::test_cmd::resolve_key(&em, spec) {
            Ok(pc) => script.set_clock_pc(pc),
            Err(e) => {
                eprintln!("error: --input-at `{spec}`: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let clocked = script.clock_pc().is_some();
    let mut step_frame = |em: &mut luna_api::Emulator| -> Result<bool, String> {
        let f = frame(em);
        if clocked {
            // The script fires on arrivals: the watching run applies it.
            let before = em.instructions_executed();
            em.run_script_watching(
                &mut script,
                luna_api::ScriptBound::Frame(f + 1),
                &[],
                |_, _| false,
            )
            .map_err(|e| e.to_string())?;
            return Ok(em.instructions_executed() > before && frame(em) > f);
        }
        script.apply_due(em, f).map_err(|e| e.to_string())?;
        let ran = em
            .step_until_frame(FRAME_BUDGET)
            .map_err(|e| e.to_string())?;
        Ok(ran > 0 && frame(em) > f)
    };
    while frame(&em) < o.from_frame {
        match step_frame(&mut em) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }
    let start_frame = frame(&em);
    // The warm-up to `--from-frame` is not part of what is being measured,
    // and boot pushes deeper than a game loop does. Start the stack
    // watermark here, with the profile.
    em.clear_stack_low();
    // SA-1 counters are cumulative since power-on; read them here so the
    // report covers the profiled window, not the boot.
    let sa1_start = em.sa1_stats().ok().flatten();
    if o.gsu_pc_set.is_some()
        && let Err(e) = em.enable_gsu_pc_set()
    {
        eprintln!("error: enable_gsu_pc_set: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = em.enable_profile_worst(o.worst) {
        eprintln!("error: enable_profile: {e}");
        return ExitCode::from(1);
    }
    let mut core_panicked = false;
    if let Some(target) = o.until_frame {
        while frame(&em) < target {
            match step_frame(&mut em) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        }
    } else {
        let start = em.instructions_executed();
        while em.instructions_executed().saturating_sub(start) < o.steps {
            let left = o.steps - em.instructions_executed().saturating_sub(start);
            let f = frame(&em);
            let stepped = if clocked {
                let before = em.instructions_executed();
                em.run_script_watching(
                    &mut script,
                    luna_api::ScriptBound::Steps(left.min(FRAME_BUDGET)),
                    &[],
                    |_, _| false,
                )
                .map(|_| em.instructions_executed() - before)
            } else {
                if let Err(e) = script.apply_due(&mut em, f) {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
                em.step(left.min(FRAME_BUDGET))
            };
            match stepped {
                Ok(0) => break,
                Ok(_) => {}
                Err(luna_api::ApiError::Panic(msg)) => {
                    // The report of what ran is still written; the
                    // command then exits 1, as the frame-bounded runs do.
                    eprintln!("error: CPU panic: {msg}");
                    core_panicked = true;
                    break;
                }
                Err(e) => {
                    eprintln!("error: step: {e}");
                    return ExitCode::from(1);
                }
            }
        }
    }
    if let Some(path) = o.pc_set {
        let pcs = match em.profile_pcs() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: profile_pcs: {e}");
                return ExitCode::from(1);
            }
        };
        if let Err(e) = write_pc_set(path, &pcs) {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
        eprintln!("pc-set: {} distinct PCs -> {}", pcs.len(), path.display());
    }
    let report = match em.take_profile() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: take_profile: {e}");
            return ExitCode::from(1);
        }
    };
    let end_frame = frame(&em);

    println!(
        "profile: frames {start_frame}..{end_frame} ({} completed), {} instructions, {} master cycles, {} symbol(s)",
        report.frames,
        report.instructions,
        report.total_mclk,
        report.entries.len()
    );
    println!(
        "{:>7}  {:>14}  {:>12}  {:>6}  {:>6}  {:>10}  symbol",
        "%", "mclk", "instr", "idle%", "pcs", "max/frame"
    );
    for e in report.entries.iter().take(o.top) {
        let idle = if e.mclk == 0 {
            0.0
        } else {
            e.idle_mclk as f64 * 100.0 / e.mclk as f64
        };
        let max_frame = e
            .per_frame
            .map_or_else(|| "-".to_string(), |p| p.max.to_string());
        println!(
            "{:>6.2}%  {:>14}  {:>12}  {:>5.1}%  {:>6}  {:>10}  {}",
            e.pct, e.mclk, e.instructions, idle, e.pcs, max_frame, e.symbol
        );
    }
    if report.entries.len() > o.top {
        println!(
            "… {} more (raise --top or read --out)",
            report.entries.len() - o.top
        );
    }
    let verdicts = match judge_budgets(&em, &report, &budgets) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut over = false;
    for v in &verdicts {
        match (v.max, v.max_frame) {
            (Some(max), Some(frame)) => println!(
                "budget: {} max {} mclk (frame {}) {} {} — {}",
                v.symbol,
                max,
                frame,
                if v.ok { "<=" } else { ">" },
                v.limit,
                if v.ok { "ok" } else { "OVER" }
            ),
            _ => println!("budget: {} never ran in a completed frame — ok", v.symbol),
        }
        over |= !v.ok;
    }
    // The window frame by frame (issue #270): what the per-symbol maxima
    // cannot say, since they do not fall on the same frame.
    let frame_summary = crate::profile_frames::summarize(&report.frame_series, &o.frame_gates);
    crate::profile_frames::print_summary(&frame_summary);
    crate::profile_frames::print_worst(&report.worst_frames, o.top);
    over |= !frame_summary.ok();
    if let Some(path) = o.frames_out {
        if let Err(e) = std::fs::write(path, crate::profile_frames::csv(&report.frame_series)) {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
        eprintln!(
            "frame series: {} frame(s) -> {}",
            report.frame_series.len(),
            path.display()
        );
    }
    // Super FX accounting (`OpenSNES` R3). Drained after the run so the
    // window matches the profile's.
    let gsu = match em.take_gsu_jobs() {
        Ok(jobs) if !jobs.is_empty() => {
            let worst = jobs.iter().copied().max_by_key(|j| j.gsu_cycles);
            let rep = GsuReport {
                jobs: jobs.len(),
                gsu_cycles: jobs.iter().map(|j| j.gsu_cycles).sum(),
                instructions: jobs.iter().map(|j| j.instructions).sum(),
                cache_hits: jobs.iter().map(|j| j.cache_hits).sum(),
                cache_misses: jobs.iter().map(|j| j.cache_misses).sum(),
                stall_cycles: jobs.iter().map(|j| j.stall_cycles).sum(),
                worst,
                per_job: jobs,
            };
            let fetches = rep.cache_hits + rep.cache_misses;
            // The ratio is what a renderer tunes its code layout against;
            // print it only when there were fetches to divide by.
            let hit_pct = if fetches > 0 {
                format!(
                    "{:.1}% cache hits",
                    100.0 * rep.cache_hits as f64 / fetches as f64
                )
            } else {
                "no fetches".to_string()
            };
            println!(
                "gsu: {} job(s), {} instr, {} clocks ({hit_pct}, {} stalled)",
                rep.jobs, rep.instructions, rep.gsu_cycles, rep.stall_cycles
            );
            if let Some(w) = rep.worst {
                println!(
                    "gsu: worst job #{} — {} clocks, {} instr, {} stalled",
                    w.seq, w.gsu_cycles, w.instructions, w.stall_cycles
                );
            }
            Some(rep)
        }
        _ => None,
    };
    if let Some(path) = o.gsu_pc_set {
        match em.gsu_pc_set() {
            Ok(pcs) => match write_pc_set(path, &pcs) {
                Ok(()) => eprintln!(
                    "gsu pc-set: {} distinct GSU PC(s) -> {}",
                    pcs.len(),
                    path.display()
                ),
                Err(e) => eprintln!("error: writing {}: {e}", path.display()),
            },
            Err(e) => eprintln!("error: gsu_pc_set: {e}"),
        }
    }
    let sa1 = match (sa1_start, em.sa1_stats().ok().flatten()) {
        (Some(a), Some(b)) if b.steps > a.steps => {
            let w = luna_api::Sa1Stats {
                instructions: b.instructions - a.instructions,
                steps: b.steps - a.steps,
                conflict_steps: b.conflict_steps - a.conflict_steps,
                rom_conflict_steps: b.rom_conflict_steps - a.rom_conflict_steps,
                bwram_conflict_steps: b.bwram_conflict_steps - a.bwram_conflict_steps,
                iram_conflict_steps: b.iram_conflict_steps - a.iram_conflict_steps,
                dma_steps: b.dma_steps - a.dma_steps,
                idle_steps: b.idle_steps - a.idle_steps,
                rom_accesses: b.rom_accesses - a.rom_accesses,
                iram_accesses: b.iram_accesses - a.iram_accesses,
                bwram_accesses: b.bwram_accesses - a.bwram_accesses,
                other_accesses: b.other_accesses - a.other_accesses,
            };
            let busy = w.steps.saturating_sub(w.idle_steps);
            let idle_share = w.idle_steps as f64 / w.steps.max(1) as f64;
            let conflict_share = w.conflict_steps as f64 / busy.max(1) as f64;
            let effective_mhz = (busy > 0).then(|| {
                SA1_NOMINAL_MHZ * busy.saturating_sub(w.conflict_steps + w.dma_steps) as f64
                    / busy as f64
            });
            let acc = (w.rom_accesses + w.iram_accesses + w.bwram_accesses + w.other_accesses)
                .max(1) as f64;
            let pct = |n: u64| 100.0 * n as f64 / acc;
            let speed = effective_mhz.map_or_else(
                || "never ran".to_string(),
                |m| format!("~{m:.2} MHz while running"),
            );
            println!(
                "sa1: {} instr, {} clocks ({:.1}% idle in WAI), {:.1}% of busy clocks lost to \
                 bus conflicts (rom {}, bwram {}, iram {}) — {speed}",
                w.instructions,
                w.steps,
                100.0 * idle_share,
                100.0 * conflict_share,
                w.rom_conflict_steps,
                w.bwram_conflict_steps,
                w.iram_conflict_steps,
            );
            println!(
                "sa1: accesses {:.1}% rom, {:.1}% iram, {:.1}% bwram, {:.1}% other",
                pct(w.rom_accesses),
                pct(w.iram_accesses),
                pct(w.bwram_accesses),
                pct(w.other_accesses),
            );
            Some(Sa1Report {
                window: w,
                idle_share,
                conflict_share,
                effective_mhz,
            })
        }
        _ => None,
    };
    let low = em.stack_low();
    let stack_ok = match (o.stack_floor, low.as_ref()) {
        (Some(floor), Some(l)) => {
            let ok = l.sp >= floor;
            let sym = l.symbol.as_deref().unwrap_or("?");
            println!(
                "stack: deepest S ${:04X} at ${:06X} ({sym}, frame {}) {} ${floor:04X} — {}",
                l.sp,
                l.pc,
                l.frame,
                if ok { ">=" } else { "<" },
                if ok { "ok" } else { "UNDER" }
            );
            ok
        }
        (Some(floor), None) => {
            // Nothing to compare: `S` is hardware-confined to page 1 until
            // the program goes native, so there is no native low to judge.
            println!(
                "stack: never left emulation mode, no native low to check against ${floor:04X} — ok"
            );
            true
        }
        (None, Some(l)) => {
            let sym = l.symbol.as_deref().unwrap_or("?");
            println!(
                "stack: deepest S ${:04X} at ${:06X} ({sym}, frame {})",
                l.sp, l.pc, l.frame
            );
            true
        }
        (None, None) => true,
    };
    if let Some(path) = o.out {
        let report = Report {
            rom,
            from_frame: start_frame,
            end_frame,
            profile: report,
            budgets: verdicts,
            frame_summary,
            stack: StackReport {
                low,
                floor: o.stack_floor,
                ok: stack_ok,
            },
            gsu,
            sa1,
        };
        if let Err(code) = crate::output::write_json_report(path, &report) {
            return code;
        }
    }
    if over || !stack_ok || core_panicked {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
