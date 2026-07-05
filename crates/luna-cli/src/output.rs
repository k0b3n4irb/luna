//! Human-readable printers, screenshot and WAV writers shared by
//! the `state` / `run` subcommands.

use crate::fmt::flag_string;

/// Print a 16-bytes-per-row hex dump to stderr.
pub(crate) fn print_hex_dump(bank: u8, base: u16, bytes: &[u8]) {
    for (row_idx, chunk) in bytes.chunks(16).enumerate() {
        let addr = (u32::from(bank) << 16) | (u32::from(base) + (row_idx as u32 * 16));
        let hex: String = chunk
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("  ${addr:06X}  {hex}");
    }
}

/// Minimal RIFF/WAVE writer for 16-bit signed PCM stereo at 32 kHz.
/// We hand-roll instead of pulling a `hound` dependency just for
/// one diagnostic path.
pub(crate) fn write_wav(path: &std::path::Path, samples: &[(i16, i16)]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let sample_rate: u32 = 32_000;
    let channels: u16 = 2;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_size =
        (samples.len() * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
    let riff_size = 36 + data_size;
    f.write_all(b"RIFF")?;
    f.write_all(&riff_size.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM format
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for (l, r) in samples {
        f.write_all(&l.to_le_bytes())?;
        f.write_all(&r.to_le_bytes())?;
    }
    Ok(())
}

pub(crate) fn print_header(info: &luna_api::RomInfo) {
    println!("=== ROM ===");
    println!("Title:       {:?}", info.title);
    println!(
        "Mapper:      {}{}",
        info.mapper,
        if info.fast_rom { " (FastROM)" } else { "" }
    );
    println!(
        "ROM size:    {} KB ({} bytes on disk)",
        info.header_rom_size_kb, info.rom_bytes
    );
    println!("SRAM size:   {} KB", info.sram_kb);
    println!("Region:      {}", info.region);
    println!("Version:     v{}", info.version);
    println!(
        "Checksum:    ${:04X} / complement ${:04X} (valid: {})",
        info.checksum, info.checksum_complement, info.checksum_valid
    );
}

pub(crate) fn print_cpu_state(cpu: &luna_api::CpuState) {
    println!(
        "A=${:04X}  X=${:04X}  Y=${:04X}  SP=${:04X}  DP=${:04X}",
        cpu.a, cpu.x, cpu.y, cpu.sp, cpu.dp
    );
    println!(
        "PC=${:02X}:{:04X}  DB=${:02X}  P=${:02X}  E={}",
        cpu.pb,
        cpu.pc,
        cpu.db,
        cpu.p,
        u8::from(cpu.e)
    );
    println!("flags: {}", flag_string(cpu.p, cpu.e));
}

pub(crate) fn print_diag_state(em: &mut luna_api::Emulator) {
    let st = em.state();
    let p = &st.ppu;
    println!(
        "PPU:  INIDISP=${:02X} (blanked={}, brightness={})  BGMODE=${:02X}  VRAM_addr=${:04X}",
        p.inidisp,
        p.inidisp & 0x80 != 0,
        p.inidisp & 0x0F,
        p.bgmode,
        p.vram_addr_words
    );
    println!(
        "PPU:  INIDISP_writes={}  Backdrop=${:04X}",
        p.inidisp_write_count, p.backdrop
    );
    // Tilemap occupancy per BG, scanned from a one-shot VRAM dump.
    let vram = em.vram_bytes().unwrap_or_default();
    for (i, bg) in p.bgs.iter().enumerate() {
        let base = (bg.tilemap_addr_words as usize) << 1;
        let mut nonzero = 0usize;
        for off in 0..(32 * 32 * 2) {
            let a = (base + off) & 0xFFFF;
            if vram.get(a).copied().unwrap_or(0) != 0 {
                nonzero += 1;
            }
        }
        println!(
            "BG{}:  tile=${:04X} (byte ${:04X})  char=${:04X} (byte ${:04X})  hscroll={} vscroll={}  tilemap_nonzero={}/{}",
            i + 1,
            bg.tilemap_addr_words,
            base,
            bg.char_addr_words,
            (bg.char_addr_words as usize) << 1,
            bg.h_scroll,
            bg.v_scroll,
            nonzero,
            32 * 32 * 2,
        );
    }
    println!(
        "CPU regs:  NMITIMEN=${:02X}  HVBJOY=${:02X}  frames={}  NMIs_served={}  ppu_line={}",
        st.cpu_regs.nmitimen,
        st.cpu_regs.hvbjoy,
        st.scheduler.frame_count,
        st.scheduler.nmis_serviced,
        st.scheduler.ppu_line,
    );
    let ports = &st.apu.to_cpu_ports;
    println!(
        "APU:  SPC PC=${:04X}  stopped={}  past_ipl={}  $2140=${:02X} $2141=${:02X} $2142=${:02X} $2143=${:02X}",
        st.apu.spc_pc,
        st.apu.spc_stopped,
        st.apu.past_iplrom,
        ports[0],
        ports[1],
        ports[2],
        ports[3]
    );
    // Audio pipeline diagnostic — show whether the music driver is
    // actually producing audio. If MVOL or active voices stay at 0,
    // we *can't* hear anything regardless of the audio backend.
    let mvol_l = st.apu.mvol_l;
    let mvol_r = st.apu.mvol_r;
    let kon = st.apu.kon;
    let endx = st.apu.endx;
    let active_count = st.apu.active_voices;
    let any_envelope = st.apu.voice_envelope.iter().any(|&e| e != 0);
    let queue_len = st.apu.audio_queue_len;
    let (last_l, last_r) = st.apu.last_audio_sample;
    println!(
        "Audio:  MVOL_L={mvol_l} MVOL_R={mvol_r}  KON=${kon:02X} ENDX=${endx:02X}  \
         active_voices={active_count}  any_env_nonzero={any_envelope}  \
         queue_len={queue_len}  last_sample=({last_l},{last_r})"
    );
    // Echo subsystem state — useful for verifying the music driver
    // actually configured echo (most SNES tracks use it heavily).
    let dsp = &st.apu.dsp_regs;
    let flg = dsp[0x6C];
    let esa = dsp[0x6D];
    let edl = dsp[0x7D] & 0x0F;
    let efb = dsp[0x0D] as i8;
    let evol_l = dsp[0x2C] as i8;
    let evol_r = dsp[0x3C] as i8;
    let eon = dsp[0x4D];
    let pmon = dsp[0x2D];
    let non = dsp[0x3D];
    println!(
        "Echo:   FLG=${flg:02X} (reset={} mute={} ECEN={}) \
         ESA=${esa:02X} (=${esa:02X}00) EDL=${edl:X} ({} samples) \
         EFB={efb} EVOL=({evol_l},{evol_r}) EON=${eon:02X} PMON=${pmon:02X} NON=${non:02X}",
        flg >> 7 & 1,
        flg >> 6 & 1,
        flg >> 5 & 1,
        if edl == 0 { 1 } else { (edl as u16) * 512 }
    );
    // OAM occupancy + first few sprite entries.
    let oam = &p.oam_full;
    println!(
        "OAM:   {}/544 non-zero  |  OBSEL=${:02X}",
        p.oam_non_zero, p.obsel
    );
    // What's actually been written into OAM that *isn't* the hide
    // value? Helps distinguish "game uploaded N sprites" from
    // "game wrote the hide marker over everything".
    print!("OAM non-$F0/non-zero bytes: ");
    let mut shown = 0;
    for off in 0..0x220usize {
        let b = oam.get(off).copied().unwrap_or(0);
        if b != 0 && b != 0xF0 {
            print!("[${off:03X}=${b:02X}] ");
            shown += 1;
            if shown >= 20 {
                print!("...");
                break;
            }
        }
    }
    println!();
    let all_sprites = em.decode_sprites().unwrap_or_default();
    let visible_count = all_sprites.iter().filter(|sp| sp.y < 224).count();
    println!("  visible sprites (y<224): {visible_count}");
    let mut shown = 0;
    for sp in &all_sprites {
        if sp.y >= 224 {
            continue;
        }
        if shown >= 12 {
            break;
        }
        shown += 1;
        println!(
            "  sprite #{:>3}: x={:>4} y={:>3} tile=${:03X} pal={} pri={} {}x{} {}{}",
            sp.index,
            sp.x,
            sp.y,
            sp.tile,
            sp.palette,
            sp.priority,
            sp.w,
            sp.h,
            if sp.h_flip { "H" } else { "-" },
            if sp.v_flip { "V" } else { "-" },
        );
    }

    // VRAM / CGRAM occupancy digest: how many non-zero bytes in each.
    // Lets us tell "the game has uploaded graphics" from "VRAM is
    // empty" — important for diagnosing why the screen stays black.
    println!(
        "VRAM:  {}/65536 non-zero bytes  |  CGRAM: {}/256 non-zero colours",
        p.vram_non_zero, p.cgram_non_zero
    );

    // @PC bytes need mutable bus access — run after all immutable
    // PPU diagnostics are done.
    let pc_bytes = em.peek_pc_bytes(8).unwrap_or_default();
    print!("@PC bytes:");
    for b in &pc_bytes {
        print!(" {b:02X}");
    }
    println!();
}

pub(crate) fn save_screenshot(
    em: &luna_api::Emulator,
    path: &std::path::Path,
    force_display: bool,
    bg: Option<u8>,
) -> Result<(), luna_api::ApiError> {
    // Default path (no --bg, no --force-display) copies the persistent
    // framebuffer; debug paths (`--force-display` or single-BG render)
    // go through the one-shot renderer. All routed through luna-api so
    // the CLI and GUI render the exact same pixels.
    let png = match bg {
        Some(n) => {
            let idx = (n.saturating_sub(1).min(3)) as usize;
            em.render_frame_bg_png(idx, force_display)?
        }
        None => em.render_frame_png(force_display)?,
    };
    std::fs::write(path, png)?;
    Ok(())
}
