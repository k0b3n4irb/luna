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
) -> Result<(), String> {
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
