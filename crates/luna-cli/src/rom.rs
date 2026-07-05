//! ROM loading shared by every headless subcommand.

/// Load `rom` into `em`, honouring an optional `--force-mapper` override.
/// Centralises the force-mapper parse + file read shared by the `state`
/// and `frames` subcommands. Returns a human-facing error string.
pub(crate) fn load_rom_into(
    em: &mut luna_api::Emulator,
    rom: &std::path::Path,
    force_mapper: Option<&str>,
    dsp1_rom: Option<&std::path::Path>,
) -> Result<(), String> {
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
    Ok(())
}
