//! `luna strict` — run a ROM and list what a real console would have
//! refused or corrupted (hardware strict mode, rules R1-R8).

use std::process::ExitCode;

use luna_api::StrictReport;

use crate::rom::load_rom_into;

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

/// Run `rom` for `frames` frames with the journal on and print it. Exit
/// `0` = no fault, `1` = at least one fault (or a core panic), `2` = the
/// ROM cannot be loaded.
pub(crate) fn run_strict(
    rom: &std::path::Path,
    frames: u64,
    json: bool,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
    power_on: Option<&str>,
) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, force_region, None, power_on) {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = em.enable_hw_strict() {
        eprintln!("error: enable_hw_strict: {e}");
        return ExitCode::from(2);
    }
    let mut panicked = false;
    while em.frame_count().unwrap_or(0) < frames {
        match em.step_until_frame(luna_api::FRAME_STEP_BUDGET) {
            Ok(0) => break, // STP: nothing more will happen
            Ok(_) => {}
            Err(luna_api::ApiError::Panic(msg)) => {
                eprintln!("core panic: {msg}");
                panicked = true;
                break;
            }
            Err(e) => {
                eprintln!("error: step_until_frame: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let ran = em.frame_count().unwrap_or(0);
    let report = match em.take_hw_faults() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: take_hw_faults: {e}");
            return ExitCode::from(2);
        }
    };
    let (text, code) = render(rom, &report, ran, json);
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
