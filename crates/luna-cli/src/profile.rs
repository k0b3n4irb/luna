//! `luna profile` — real master cycles per symbol (issue #227).
//!
//! Replaces a static per-instruction-weight estimate with what the
//! machine actually paid: every step credits its master cycles — bus +
//! internal cycles and the DMA / HDMA / refresh stalls charged during it
//! — to the instruction's address; the report folds those onto the
//! nearest `.sym` label (or a 256-byte page without one), heaviest first.

use std::process::ExitCode;

use crate::parsers::parse_input_script;
use crate::rom::load_rom_into;

/// Instruction budget per frame (matches the other frame-stepping paths).
const FRAME_BUDGET: u64 = 200_000;

/// Options for [`run_profile`].
pub(crate) struct ProfileOptions<'a> {
    pub steps: u64,
    pub until_frame: Option<u64>,
    pub from_frame: u64,
    pub input_script: Option<&'a str>,
    pub sym: Option<&'a std::path::Path>,
    pub top: usize,
    pub out: Option<&'a std::path::Path>,
    /// Write every executed 24-bit PC, sorted, as little-endian `u32`s
    /// (`--pc-set`; `OpenSNES` R-C — a coverage tool folds them onto lines).
    pub pc_set: Option<&'a std::path::Path>,
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
    let checkpoints: Vec<(u64, u16)> = match o.input_script.map(parse_input_script) {
        None => Vec::new(),
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            eprintln!("error: --input: {e}");
            return ExitCode::from(2);
        }
    };
    // Warm-up to `--from-frame` with the profiler off, applying input on
    // the way; then profile to the end.
    let frame = |em: &luna_api::Emulator| em.frame_count().unwrap_or(0);
    let mut next_cp = 0usize;
    let mut apply_input = |em: &mut luna_api::Emulator, f: u64| -> Result<(), String> {
        while let Some(&(at, mask)) = checkpoints.get(next_cp) {
            if at > f {
                break;
            }
            em.set_joypad(0, mask).map_err(|e| e.to_string())?;
            next_cp += 1;
        }
        Ok(())
    };
    let mut step_frame = |em: &mut luna_api::Emulator| -> Result<bool, String> {
        let f = frame(em);
        apply_input(em, f)?;
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
            if let Err(e) = apply_input(&mut em, f) {
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
        "profile: frames {start_frame}..{end_frame}, {} instructions, {} master cycles, {} symbol(s)",
        report.instructions,
        report.total_mclk,
        report.entries.len()
    );
    println!(
        "{:>7}  {:>14}  {:>12}  {:>6}  {:>6}  symbol",
        "%", "mclk", "instr", "idle%", "pcs"
    );
    for e in report.entries.iter().take(o.top) {
        let idle = if e.mclk == 0 {
            0.0
        } else {
            e.idle_mclk as f64 * 100.0 / e.mclk as f64
        };
        println!(
            "{:>6.2}%  {:>14}  {:>12}  {:>5.1}%  {:>6}  {}",
            e.pct, e.mclk, e.instructions, idle, e.pcs, e.symbol
        );
    }
    if report.entries.len() > o.top {
        println!(
            "… {} more (raise --top or read --out)",
            report.entries.len() - o.top
        );
    }
    if let Some(path) = o.out {
        let json = serde_json::to_string_pretty(&Report {
            rom,
            from_frame: start_frame,
            end_frame,
            profile: report,
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
    ExitCode::SUCCESS
}
