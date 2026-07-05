//! `luna spc-dump` / `luna assets-dump` — raw-memory exporters.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::parsers::parse_input_script;
use crate::rom::load_rom_into;

/// `luna spc-dump` — run until the music driver is live, then write a
/// `.spc` file (via `Emulator::export_spc`). Scripted input is applied
/// during the warm-up, same semantics as `run_frames`.
pub(crate) fn run_spc_dump(
    rom: &std::path::Path,
    steps: u64,
    out: Option<&std::path::Path>,
    force_mapper: Option<&str>,
    dsp1_rom: Option<&std::path::Path>,
    input_script: Option<&str>,
) -> ExitCode {
    const FRAME_BUDGET: u64 = 200_000;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, dsp1_rom) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    let checkpoints: Vec<(u64, u16)> = match input_script {
        None => Vec::new(),
        Some(script) => match parse_input_script(script) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --input: {e}");
                return ExitCode::from(1);
            }
        },
    };
    for (frame, mask) in &checkpoints {
        while em.state().scheduler.frame_count < *frame {
            if em.step_until_frame(FRAME_BUDGET).unwrap_or(0) == 0 {
                break;
            }
        }
        if let Err(e) = em.set_joypad(0, *mask) {
            eprintln!("error: set_joypad: {e}");
            return ExitCode::from(1);
        }
    }
    if let Err(e) = em.step(steps) {
        eprintln!("step warning (warm-up): {e}");
    }
    let spc = match em.export_spc() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: export_spc: {e}");
            return ExitCode::from(1);
        }
    };
    // Default output path: the ROM's stem with a `.spc` extension, in CWD.
    let default_out =
        PathBuf::from(rom.file_stem().unwrap_or(rom.as_os_str())).with_extension("spc");
    let out_path = out.unwrap_or(&default_out);
    if let Err(e) = std::fs::write(out_path, &spc) {
        eprintln!("error: writing {}: {e}", out_path.display());
        return ExitCode::from(1);
    }
    println!(
        "SPC written to {} ({} bytes) — SPC700 pc=${:04X}",
        out_path.display(),
        spc.len(),
        em.spc700_state().map_or(0, |s| s.pc),
    );
    ExitCode::SUCCESS
}

/// `luna assets-dump` — run to a scene, then write every loaded graphics
/// asset as PNGs (+ raw VRAM/CGRAM + OAM JSON). Warm-up + scripted-input
/// handling mirrors `run_spc_dump`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_assets_dump(
    rom: &std::path::Path,
    steps: u64,
    out: &std::path::Path,
    bpp: Option<u8>,
    palette: u8,
    force_mapper: Option<&str>,
    dsp1_rom: Option<&std::path::Path>,
    input_script: Option<&str>,
) -> ExitCode {
    const FRAME_BUDGET: u64 = 200_000;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, dsp1_rom) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    let checkpoints: Vec<(u64, u16)> = match input_script {
        None => Vec::new(),
        Some(script) => match parse_input_script(script) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --input: {e}");
                return ExitCode::from(1);
            }
        },
    };
    for (frame, mask) in &checkpoints {
        while em.state().scheduler.frame_count < *frame {
            if em.step_until_frame(FRAME_BUDGET).unwrap_or(0) == 0 {
                break;
            }
        }
        if let Err(e) = em.set_joypad(0, *mask) {
            eprintln!("error: set_joypad: {e}");
            return ExitCode::from(1);
        }
    }
    if let Err(e) = em.step(steps) {
        eprintln!("step warning (warm-up): {e}");
    }
    if let Err(e) = std::fs::create_dir_all(out) {
        eprintln!("error: creating {}: {e}", out.display());
        return ExitCode::from(1);
    }

    // VRAM tile-sheet bpp: explicit, else auto from the current BG1 mode
    // (falling back to the common 4bpp).
    let bgmode = em.state().ppu.bgmode;
    let tile_bpp = bpp.unwrap_or_else(|| match em.bg_bpp(0) {
        Ok(b) if b != 0 => b,
        _ => 4,
    });

    // Helper: write a PNG (Result<Vec<u8>>) to `out/<name>`, logging.
    let mut wrote = 0u32;
    let mut put = |name: &str, png: Result<Vec<u8>, luna_api::ApiError>| match png {
        Ok(bytes) => {
            let path = out.join(name);
            if let Err(e) = std::fs::write(&path, &bytes) {
                eprintln!("error: writing {}: {e}", path.display());
            } else {
                println!("  {} ({} bytes)", path.display(), bytes.len());
                wrote += 1;
            }
        }
        Err(e) => eprintln!("skip {name}: {e}"),
    };

    println!(
        "assets @ frame {} (BGMODE ${bgmode:02X}, tile sheet {tile_bpp}bpp):",
        em.frame_count().unwrap_or(0)
    );
    put("screen.png", em.render_frame_png(true));
    put(
        "vram_tiles.png",
        em.render_vram_tiles_png(tile_bpp, palette),
    );
    put("palette.png", em.render_palette_png(16));
    put("sprites.png", em.render_sprite_sheet_png());
    // Mode 7 has a single 128×128 field (BG index is ignored); otherwise
    // dump each BG layer that is enabled in the current mode.
    if bgmode & 0x07 == 7 {
        put("bg1_tilemap_mode7.png", em.render_tilemap_png(0));
    } else {
        for bg in 0..4usize {
            if em.bg_bpp(bg).unwrap_or(0) != 0 {
                put(
                    &format!("bg{}_tilemap.png", bg + 1),
                    em.render_tilemap_png(bg),
                );
            }
        }
    }

    // Raw dumps + OAM metadata for tools that want the bytes.
    if let Ok(v) = em.vram_bytes() {
        let _ = std::fs::write(out.join("vram.bin"), v);
    }
    if let Ok(cg) = em.peek_cgram() {
        let mut bytes = Vec::with_capacity(cg.len() * 2);
        for c in cg {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let _ = std::fs::write(out.join("cgram.bin"), bytes);
    }
    if let Ok(sprites) = em.decode_sprites()
        && let Ok(json) = serde_json::to_string_pretty(&sprites)
    {
        let _ = std::fs::write(out.join("oam.json"), json);
    }

    println!(
        "wrote {wrote} PNG(s) + raw VRAM/CGRAM + oam.json to {}",
        out.display()
    );
    ExitCode::SUCCESS
}
