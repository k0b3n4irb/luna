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
    /// Deepest native-mode stack reach over the profiled window, and the
    /// `--stack-floor` verdict if one was asked for.
    stack: StackReport,
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
    let mut step_frame = |em: &mut luna_api::Emulator| -> Result<bool, String> {
        let f = frame(em);
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
    if let Err(e) = em.enable_profile() {
        eprintln!("error: enable_profile: {e}");
        return ExitCode::from(1);
    }
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
            if let Err(e) = script.apply_due(&mut em, f) {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
            match em.step(left.min(FRAME_BUDGET)) {
                Ok(0) => break,
                Ok(_) => {}
                Err(luna_api::ApiError::Panic(msg)) => {
                    eprintln!("note: CPU panic: {msg}");
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
        let mut bytes = Vec::with_capacity(pcs.len() * 4);
        for pc in &pcs {
            bytes.extend_from_slice(&pc.to_le_bytes());
        }
        if let Err(e) = std::fs::write(path, bytes) {
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
        let json = serde_json::to_string_pretty(&Report {
            rom,
            from_frame: start_frame,
            end_frame,
            profile: report,
            budgets: verdicts,
            stack: StackReport {
                low,
                floor: o.stack_floor,
                ok: stack_ok,
            },
        })
        .expect("report serialises");
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
    if over || !stack_ok {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
