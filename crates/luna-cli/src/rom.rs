//! ROM loading shared by every headless subcommand.

/// Load `rom` into `em`, honouring the optional `--force-mapper` and
/// `--force-region` overrides. Centralises the override parsing + file read
/// shared by every ROM-loading subcommand. Returns a human-facing error
/// string.
pub(crate) fn load_rom_into(
    em: &mut luna_api::Emulator,
    rom: &std::path::Path,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
    dsp1_rom: Option<&std::path::Path>,
    power_on: Option<&str>,
) -> Result<(), String> {
    // `--power-on` (issue #224): what RAM holds before the ROM boots.
    // Applied by the `load_rom*` below; a derived seed is printed so a
    // failure under `random` can be replayed with `random=<seed>`.
    let state = parse_power_on(power_on)?;
    if let luna_api::PowerOnState::Random { seed } = state {
        eprintln!("power-on: random (seed=0x{seed:016x})");
    }
    em.set_power_on(state);
    match force_region {
        Some(r) => {
            let region = match r.to_ascii_lowercase().as_str() {
                "ntsc" => luna_api::Region::Ntsc,
                "pal" => luna_api::Region::Pal,
                _ => return Err(format!("unknown --force-region '{r}' (ntsc, pal)")),
            };
            em.set_forced_region(Some(region));
        }
        None => em.set_forced_region(None),
    }
    // `--dsp1-rom` installs the firmware into luna's firmware folder so it
    // is found now and on every future run.
    if let Some(fw) = dsp1_rom {
        match luna_api::Emulator::install_firmware(fw, "dsp1b.rom") {
            Ok(dest) => eprintln!("installed DSP firmware → {}", dest.display()),
            Err(e) => eprintln!("warning: could not install {}: {e}", fw.display()),
        }
    }
    let info = match force_mapper {
        Some(kind_str) => {
            let kind = luna_api::MapperKind::from_cli_str(kind_str)
                .ok_or_else(|| format!("unknown --force-mapper '{kind_str}'"))?;
            let bytes =
                std::fs::read(rom).map_err(|e| format!("reading {}: {e}", rom.display()))?;
            em.load_rom_bytes_forced(bytes, kind)
                .map_err(|e| e.to_string())?
        }
        None => em.load_rom(rom).map_err(|e| e.to_string())?,
    };
    if let Some(name) = &info.missing_firmware {
        let dir = luna_api::Emulator::firmware_dir().map_or_else(
            || "<config>/luna/firmware".to_string(),
            |d| d.display().to_string(),
        );
        eprintln!(
            "warning: '{}' needs coprocessor firmware '{name}' which was not found — \
             the coprocessor stays inert (e.g. Mode 7 graphics will be wrong). \
             Supply it with `--dsp1-rom <path>` or place '{name}' in {dir}.",
            info.title.trim()
        );
    }
    // WLA-DX symbol auto-detection (issue #67): a `<rom>.sym` next to the
    // ROM (the wlalink convention) is loaded automatically so disassembly
    // and symbol resolution work with zero flags. Explicit `--sym` on the
    // subcommands overrides this afterwards.
    let sym = rom.with_extension("sym");
    if sym.is_file() {
        match em.load_symbols(&sym) {
            Ok(n) => eprintln!("loaded {n} symbols from {}", sym.display()),
            Err(e) => eprintln!("warning: could not parse {}: {e}", sym.display()),
        }
    }
    Ok(())
}

/// Parse a `--power-on` / manifest `power_on` value: `zero` (default),
/// `ones`, `random` (seed derived from the clock + pid, printed by the
/// caller) or `random=<seed>` (decimal or `0x` hex).
pub(crate) fn parse_power_on(spec: Option<&str>) -> Result<luna_api::PowerOnState, String> {
    use luna_api::PowerOnState;
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
                    "unknown --power-on '{spec}' (zero, ones, random, random=<seed>)"
                ));
            };
            let seed = seed.strip_prefix("0x").map_or_else(
                || seed.parse::<u64>().ok(),
                |hex| u64::from_str_radix(hex, 16).ok(),
            );
            seed.map(|seed| PowerOnState::Random { seed })
                .ok_or_else(|| format!("bad --power-on seed in '{spec}' (decimal or 0x hex)"))
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
    use super::parse_power_on;
    use luna_api::PowerOnState;

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
