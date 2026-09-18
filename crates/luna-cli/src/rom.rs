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
    let state = luna_api::parse_power_on(power_on)?;
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
            em.load_rom_forced(rom, kind)
                .map_err(|e| format!("{}: {e}", rom.display()))?
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
    // WLA-DX symbol auto-detection (issue #67) is luna-api's: a `<rom>.sym`
    // next to the ROM is loaded by `load_rom*` for every front-end. Explicit
    // `--sym` on the subcommands overrides it afterwards.
    if let Some(n) = info.symbols_loaded {
        eprintln!(
            "loaded {n} symbols from {}",
            rom.with_extension("sym").display()
        );
    }
    if let Some(e) = &info.symbols_error {
        eprintln!("warning: could not parse {e}");
    }
    Ok(())
}
