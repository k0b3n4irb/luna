//! The one parser for power-on memory specs (issue #224), shared by the
//! CLI, `luna test` manifests and the MCP server.

use crate::PowerOnState;

/// Parse a power-on spec — the CLI `--power-on`, a `luna test` manifest
/// `power_on`, the MCP `load_rom {power_on}`: `zero` (default), `ones`,
/// `random` (seed derived from the clock + pid — report it so the run can be
/// replayed) or `random=<seed>` (decimal or `0x` hex).
pub fn parse_power_on(spec: Option<&str>) -> Result<PowerOnState, String> {
    let Some(spec) = spec else {
        return Ok(PowerOnState::Zero);
    };
    let lower = spec.trim().to_ascii_lowercase();
    match lower.as_str() {
        "zero" | "zeros" => Ok(PowerOnState::Zero),
        "ones" => Ok(PowerOnState::Ones),
        "random" => Ok(PowerOnState::Random {
            seed: derived_seed(),
        }),
        s => {
            let Some(seed) = s.strip_prefix("random=") else {
                return Err(format!(
                    "unknown power-on state '{spec}' (zero, ones, random, random=<seed>)"
                ));
            };
            let seed = seed.strip_prefix("0x").map_or_else(
                || seed.parse::<u64>().ok(),
                |hex| u64::from_str_radix(hex, 16).ok(),
            );
            seed.map(|seed| PowerOnState::Random { seed })
                .ok_or_else(|| format!("bad power-on seed in '{spec}' (decimal or 0x hex)"))
        }
    }
}

/// A fresh seed for `random` without one: wall clock mixed with the pid,
/// never zero.
fn derived_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let seed = nanos ^ (u64::from(std::process::id()).rotate_left(32));
    if seed == 0 { 0x5EED } else { seed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_on_spec_parses_every_form() {
        assert_eq!(parse_power_on(None).unwrap(), PowerOnState::Zero);
        assert_eq!(parse_power_on(Some("zero")).unwrap(), PowerOnState::Zero);
        assert_eq!(parse_power_on(Some("ONES")).unwrap(), PowerOnState::Ones);
        assert_eq!(
            parse_power_on(Some("random=42")).unwrap(),
            PowerOnState::Random { seed: 42 }
        );
        assert_eq!(
            parse_power_on(Some("random=0xdead")).unwrap(),
            PowerOnState::Random { seed: 0xDEAD }
        );
        assert!(matches!(
            parse_power_on(Some("random")).unwrap(),
            PowerOnState::Random { seed } if seed != 0
        ));
        assert!(parse_power_on(Some("random=zz")).is_err());
        assert!(parse_power_on(Some("garbage")).is_err());
    }
}
