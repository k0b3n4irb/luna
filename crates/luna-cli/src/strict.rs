//! `luna strict` — run a ROM and list what a real console would have
//! refused or corrupted (hardware strict mode, rules R1-R8).

use std::process::ExitCode;

use luna_api::StrictReport;

use crate::parsers::{InputFlags, apply_input_flags};
use crate::rom::load_rom_into;
use crate::state::{PokeAt, PokeGroup};

/// The fault lines, one per distinct (rule, pc, address), and a summary.
pub(crate) fn render_text(report: &StrictReport, frames: u64) -> String {
    let mut lines: Vec<String> = report
        .faults
        .iter()
        .map(|f| {
            format!(
                "{} pc={} {} {} x{} (first: frame {} line {} h {})  {}",
                f.rule, f.pc, f.addr, f.origin, f.count, f.frame, f.line, f.hclock, f.what
            )
        })
        .collect();
    if report.overflow > 0 {
        lines.push(format!(
            "... {} more distinct fault(s) not kept (journal full)",
            report.overflow
        ));
    }
    lines.push(if report.faults.is_empty() {
        format!("strict: no hardware fault in {frames} frame(s)")
    } else {
        format!(
            "strict: {} fault site(s), {} occurrence(s) in {frames} frame(s)",
            report.faults.len(),
            report.occurrences()
        )
    });
    lines.join("\n")
}

/// Pure rendering, kept apart from the run so it is testable: the output
/// text and the exit code (`0` clean, `1` at least one fault).
pub(crate) fn render(
    rom: &std::path::Path,
    report: &StrictReport,
    frames: u64,
    json: bool,
) -> (String, u8) {
    let code = u8::from(!report.faults.is_empty() || report.overflow > 0);
    let text = if json {
        let v = serde_json::json!({
            "rom": rom.display().to_string(),
            "frames": frames,
            "ok": code == 0,
            "occurrences": report.occurrences(),
            "overflow": report.overflow,
            "faults": report.faults,
        });
        serde_json::to_string_pretty(&v).unwrap_or_default()
    } else {
        render_text(report, frames)
    };
    (text, code)
}

/// What `luna strict` is told besides the ROM: the scripted input and the
/// pokes of `state`, so a run can play a menu, a match, a GAME OVER.
pub(crate) struct StrictOpts<'a> {
    pub rom: &'a std::path::Path,
    pub frames: u64,
    pub json: bool,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
    pub input: Option<&'a str>,
    pub input2: Option<&'a str>,
    pub input_at: Option<&'a str>,
    pub pokes: &'a [PokeGroup],
    pub sym: Option<&'a std::path::Path>,
}

/// Run the ROM for `frames` frames with the journal on and print it. Exit
/// `0` = no fault, `1` = at least one fault (or a core panic), `2` = the
/// ROM cannot be loaded or a flag is malformed.
pub(crate) fn run_strict(o: &StrictOpts<'_>) -> ExitCode {
    use luna_api::{ScriptBound, WatchEvent};
    let mut em = luna_api::Emulator::new();
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
    if let Some(sym) = o.sym.filter(|s| !crate::rom::is_auto_sym(o.rom, s)) {
        match em.load_symbols(sym) {
            Ok(n) => eprintln!("loaded {n} symbols from {}", sym.display()),
            Err(e) => {
                eprintln!("error: --sym {}: {e}", sym.display());
                return ExitCode::from(2);
            }
        }
    }
    let extra_pads = [(1u8, o.input2)];
    let flags = InputFlags {
        extra_pads: &extra_pads,
        clocked: o.input_at.is_some(),
        ..InputFlags::pad1(o.input)
    };
    let mut script = match apply_input_flags(&mut em, &flags) {
        Ok(s) => s,
        Err(code) => return ExitCode::from(code),
    };
    if let Some(spec) = o.input_at {
        match crate::test_cmd::resolve_key(&em, spec) {
            Ok(pc) => script.set_clock_pc(pc),
            Err(e) => {
                eprintln!("error: --input-at `{spec}`: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let mut pokes = match PokeAt::resolve(&em, o.pokes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut watched: Vec<u32> = Vec::new();
    for p in &pokes {
        if !watched.contains(&p.addr) {
            watched.push(p.addr);
        }
    }
    if let Err(e) = em.enable_hw_strict() {
        eprintln!("error: enable_hw_strict: {e}");
        return ExitCode::from(2);
    }
    let mut panicked = false;
    let run = em.run_script_watching(
        &mut script,
        ScriptBound::Frame(o.frames),
        &watched,
        |em, ev| {
            if let WatchEvent::Pc(i) = ev {
                for p in pokes.iter_mut().filter(|p| p.addr == watched[i]) {
                    p.on_arrival(em);
                }
            }
            false
        },
    );
    match run {
        Ok(_) => {}
        Err(luna_api::ApiError::Panic(msg)) => {
            eprintln!("core panic: {msg}");
            panicked = true;
        }
        Err(e) => {
            eprintln!("error: scripted input: {e}");
            return ExitCode::from(2);
        }
    }
    // A poke the run never reached wrote nothing: the scenario was not played.
    if let Some(p) = pokes.iter().find(|p| !p.applied) {
        eprintln!(
            "error: --poke-at {} never reached arrival {} (seen {})",
            p.spec, p.hit, p.hits_seen
        );
        return ExitCode::from(2);
    }
    let ran = em.frame_count().unwrap_or(0);
    let report = match em.take_hw_faults() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: take_hw_faults: {e}");
            return ExitCode::from(2);
        }
    };
    let (text, code) = render(o.rom, &report, ran, o.json);
    println!("{text}");
    ExitCode::from(code.max(u8::from(panicked)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_api::StrictFault;

    fn fault() -> StrictFault {
        StrictFault {
            rule: "R1",
            what: "VRAM write",
            pc: "$00:8017".into(),
            addr: "$2118".into(),
            origin: "cpu".into(),
            frame: 1,
            line: 40,
            hclock: 312,
            count: 7,
        }
    }

    #[test]
    fn a_clean_run_exits_zero_and_a_fault_exits_one() {
        let p = std::path::Path::new("a.sfc");
        let clean = StrictReport {
            faults: vec![],
            overflow: 0,
        };
        let (text, code) = render(p, &clean, 10, false);
        assert_eq!(
            (text.as_str(), code),
            ("strict: no hardware fault in 10 frame(s)", 0)
        );
        let dirty = StrictReport {
            faults: vec![fault()],
            overflow: 0,
        };
        let (text, code) = render(p, &dirty, 10, false);
        assert_eq!(code, 1);
        assert!(text.starts_with("R1 pc=$00:8017 $2118 cpu x7 (first: frame 1 line 40 h 312)"));
        assert!(text.ends_with("strict: 1 fault site(s), 7 occurrence(s) in 10 frame(s)"));
        let (json, _) = render(p, &dirty, 10, true);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["occurrences"], 7);
        assert_eq!(v["faults"][0]["rule"], "R1");
    }

    #[test]
    fn a_full_journal_is_never_reported_clean() {
        let full = StrictReport {
            faults: vec![],
            overflow: 3,
        };
        assert_eq!(render(std::path::Path::new("a"), &full, 1, false).1, 1);
    }
}
