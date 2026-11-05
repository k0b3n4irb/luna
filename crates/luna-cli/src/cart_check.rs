//! `luna cart-check` — is this ROM's header ready for a physical cartridge?

use std::process::ExitCode;

use luna_api::{Finding, Severity};

/// Pure rendering, kept apart from the file read so it is testable: the
/// output text and the exit code (`0` clean or Info only, `1` at least one
/// Error/Warn).
pub(crate) fn render(rom: &std::path::Path, findings: &[Finding], json: bool) -> (String, u8) {
    let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count();
    let (errors, warnings, infos) = (
        count(Severity::Error),
        count(Severity::Warn),
        count(Severity::Info),
    );
    let code = u8::from(errors + warnings > 0);
    let text = if json {
        let v = serde_json::json!({
            "rom": rom.display().to_string(),
            "errors": errors,
            "warnings": warnings,
            "infos": infos,
            "ok": code == 0,
            "findings": findings,
        });
        serde_json::to_string_pretty(&v).unwrap_or_default()
    } else {
        findings
            .iter()
            .map(|f| format!("{} {}: {}", f.severity.label(), f.code, f.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    (text, code)
}

/// Read `rom`, audit it, print the findings. Exit `2` when the file cannot
/// be read.
pub(crate) fn run_cart_check(rom: &std::path::Path, json: bool) -> ExitCode {
    let bytes = match std::fs::read(rom) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {}: {e}", rom.display());
            return ExitCode::from(2);
        }
    };
    let findings = luna_api::audit_cartridge(&bytes);
    let (text, code) = render(rom, &findings, json);
    println!("{text}");
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity) -> Finding {
        Finding {
            severity,
            code: "X",
            message: "m".into(),
        }
    }

    #[test]
    fn info_only_exits_zero_and_a_warning_exits_one() {
        let p = std::path::Path::new("a.sfc");
        assert_eq!(render(p, &[finding(Severity::Info)], false).0, "INFO X: m");
        assert_eq!(render(p, &[finding(Severity::Info)], false).1, 0);
        assert_eq!(render(p, &[finding(Severity::Warn)], false).1, 1);
        assert_eq!(render(p, &[finding(Severity::Error)], true).1, 1);
        let (json, _) = render(p, &[finding(Severity::Warn)], true);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["warnings"], 1);
        assert_eq!(v["findings"][0]["severity"], "warn");
        assert_eq!(v["ok"], false);
    }
}
