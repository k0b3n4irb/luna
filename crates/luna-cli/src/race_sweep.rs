//! `luna race-sweep` — thin front of `luna_api::Emulator::race_sweep*`.

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use luna_api::{
    ApiError, Emulator, InterruptKind, RaceBaseline, RaceSweepRequest, RaceSweepSpec, RaceTrial,
    RaceWarm,
};

use crate::rom::{is_auto_sym, load_rom_into};

/// What the command line asked.
pub(crate) struct RaceSweepOpts<'a> {
    pub rom: &'a Path,
    pub sym: Option<&'a Path>,
    pub from: &'a str,
    pub to: &'a str,
    pub kind: &'a str,
    pub hit: u64,
    pub warm_until_pc: Option<&'a str>,
    pub warm_hit: u64,
    pub warm_frames: Option<u64>,
    pub until_pc: Option<&'a str>,
    pub until_hit: u64,
    pub until_frame: Option<u64>,
    pub compare: &'a [String],
    pub max_steps: u64,
    pub allow_baseline_interrupt: bool,
    pub jobs: usize,
    pub json: bool,
    pub out: Option<&'a Path>,
    pub fail_on_race: bool,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
}

/// A usage error is exit 2, a failure of the machine exit 1.
fn code(e: &ApiError) -> ExitCode {
    ExitCode::from(if matches!(e, ApiError::BadArg(_)) {
        2
    } else {
        1
    })
}

fn kinds(text: &str) -> Result<Vec<InterruptKind>, String> {
    match text {
        "nmi" => Ok(vec![InterruptKind::Nmi]),
        "irq" => Ok(vec![InterruptKind::Irq]),
        "both" => Ok(vec![InterruptKind::Nmi, InterruptKind::Irq]),
        other => Err(format!("--kind `{other}`: nmi, irq or both")),
    }
}

/// One more emulator on the same ROM, quiet (the first one said what the
/// load had to say). Its state comes from the baseline snapshot.
fn worker(o: &RaceSweepOpts) -> Result<Emulator, String> {
    let mut em = Emulator::new();
    match o.force_region {
        Some(r) => em.set_forced_region(Some(
            luna_api::parse_region(r).ok_or_else(|| format!("unknown --force-region '{r}'"))?,
        )),
        None => em.set_forced_region(None),
    }
    match o.force_mapper {
        Some(m) => {
            let kind = luna_api::MapperKind::from_cli_str(m)
                .ok_or_else(|| format!("unknown --force-mapper '{m}'"))?;
            em.load_rom_forced(o.rom, kind).map_err(|e| e.to_string())?;
        }
        None => {
            em.load_rom(o.rom).map_err(|e| e.to_string())?;
        }
    }
    Ok(em)
}

/// Run the trials on `jobs` emulators; the order of the result is free (the
/// report sorts it), the figures do not depend on `jobs`.
fn run_trials(
    em: &mut Emulator,
    o: &RaceSweepOpts,
    spec: &RaceSweepSpec,
    base: &RaceBaseline,
) -> Result<Vec<RaceTrial>, ApiError> {
    let tasks = base.tasks(spec);
    let jobs = if o.jobs == 0 {
        std::thread::available_parallelism().map_or(1, usize::from)
    } else {
        o.jobs
    }
    .min(tasks.len().max(1));
    if jobs <= 1 {
        let mut out = Vec::with_capacity(tasks.len());
        for &(a, k) in &tasks {
            out.push(em.race_sweep_trial(spec, base, a, k)?);
        }
        return Ok(out);
    }
    let next = AtomicUsize::new(0);
    let results: Vec<Result<Vec<RaceTrial>, ApiError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                scope.spawn(|| {
                    let mut w = worker(o).map_err(ApiError::BadArg)?;
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(a, k)) = tasks.get(i) else { break };
                        mine.push(w.race_sweep_trial(spec, base, a, k)?);
                    }
                    Ok(mine)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(ApiError::Panic("a sweep worker panicked".into())))
            })
            .collect()
    });
    let mut all = Vec::with_capacity(tasks.len());
    for r in results {
        all.extend(r?);
    }
    Ok(all)
}

/// Run the sweep and print the report. Exit `0` (or the sweep found
/// nothing), `1` under `--fail-on-race` when it found a race or tested
/// nothing, `2` a usage error.
pub(crate) fn run_race_sweep(o: &RaceSweepOpts) -> ExitCode {
    let mut em = Emulator::new();
    if let Err(e) = load_rom_into(
        &mut em,
        o.rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    ) {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
    if let Some(sym) = o.sym.filter(|s| !is_auto_sym(o.rom, s)) {
        match em.load_symbols(sym) {
            Ok(n) => eprintln!("loaded {n} symbols from {}", sym.display()),
            Err(e) => {
                eprintln!("error: --sym {}: {e}", sym.display());
                return ExitCode::from(2);
            }
        }
    }
    let usage = |m: String| {
        eprintln!("error: {m}");
        ExitCode::from(2)
    };
    let kinds = match kinds(o.kind) {
        Ok(k) => k,
        Err(m) => return usage(m),
    };
    let warm = match (o.warm_until_pc, o.warm_frames) {
        (Some(_), Some(_)) => {
            return usage("--warm-until-pc and --warm-frames are exclusive".into());
        }
        (Some(spec), None) => RaceWarm::Pc {
            spec: spec.to_string(),
            hit: o.warm_hit,
        },
        (None, Some(n)) => RaceWarm::Frame(n),
        (None, None) => RaceWarm::None,
    };
    let request = RaceSweepRequest {
        from: o.from.to_string(),
        to: o.to.to_string(),
        kinds,
        hit: o.hit,
        until_pc: o.until_pc.map(str::to_string),
        until_hit: o.until_hit,
        until_frame: o.until_frame,
        compare: o.compare.to_vec(),
        max_steps: o.max_steps,
        allow_baseline_interrupt: o.allow_baseline_interrupt,
    };
    let spec = match em.race_sweep_spec(&request) {
        Ok(s) => s,
        Err(e) => return usage(e.to_string()),
    };
    if let Err(e) = em.race_sweep_warm(&warm) {
        eprintln!("error: {e}");
        return code(&e);
    }
    let base = match em.race_sweep_baseline(&spec) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return code(&e);
        }
    };
    let trials = match run_trials(&mut em, o, &spec, &base) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return code(&e);
        }
    };
    let report = em.race_sweep_report(&spec, &base, trials);
    let text = if o.json {
        serde_json::to_string_pretty(&report).unwrap_or_default()
    } else {
        report.render_text()
    };
    match o.out {
        Some(path) => {
            if let Err(e) = std::fs::write(path, format!("{text}\n")) {
                eprintln!("error: {}: {e}", path.display());
                return ExitCode::from(2);
            }
        }
        None => println!("{text}"),
    }
    let failed = o.fail_on_race && (report.racy() || report.untested());
    ExitCode::from(u8::from(failed))
}
