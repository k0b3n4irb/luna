//! Luna SNES emulator — command-line entry point.
//!
//! Dispatches between execution modes (run / mcp / replay).
//! See `ARCHITECTURE.md` §3.2.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use luna_cartridge::Cartridge;
use luna_core::Snes;

#[derive(Parser, Debug)]
#[command(
    name = "luna",
    version,
    about = "SNES emulator with introspection API",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Load a ROM, step the CPU N instructions, optionally dump a
    /// screenshot of the resulting PPU state.
    ///
    /// Phase 1.7: no APU yet, so real ROMs that handshake with the
    /// SPC700 will eventually hang. We still render whatever the PPU
    /// produced via direct CPU writes / DMA. unimplemented opcodes
    /// panic and are caught — partial state is still dumped.
    Run {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// Maximum number of CPU instructions to execute before dumping.
        #[arg(short = 'n', long, default_value_t = 64)]
        steps: u64,
        /// If set, render a 256×224 PNG of the framebuffer (BG1 Mode 0
        /// only at this point) and write it to the given path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Bypass INIDISP forced-blank when rendering. Lets you see
        /// whatever the game has uploaded to VRAM/CGRAM even if its
        /// init left the screen blanked (typical when waiting on the
        /// SPC700 we don't fully emulate yet).
        #[arg(long)]
        force_display: bool,
        /// If set (1..=4), render ONLY that BG layer. Default is the
        /// composited BG3-over-BG1-over-BG2 frame (right for most
        /// Mode-1 title screens).
        #[arg(long)]
        bg: Option<u8>,
        /// If set, capture the APU's stereo 32 kHz output and write
        /// it to a WAV file at the end of the run. Lets the
        /// emulator be audio-verified without a GUI / sound card.
        #[arg(long)]
        audio_out: Option<PathBuf>,
    },
    /// Serve the Luna MCP server on stdio.
    ///
    /// Once started, Luna exposes a tool catalogue (load_rom, reset,
    /// step, state, screenshot, drain_audio, peek_memory, peek_aram)
    /// to any connected MCP client (Claude Desktop, Claude Code,
    /// custom clients). The process stays alive until the client
    /// closes the stream.
    Mcp,
    /// Run the emulator through `luna-api` and emit a JSON state
    /// snapshot — the same data the MCP `get_state` tool returns.
    ///
    /// This is the dogfood path: the API surface that the CLI, GUI
    /// and (eventually) MCP server all share. Use it to test the
    /// API directly without going through any transport.
    State {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// CPU instructions to execute before snapshotting.
        #[arg(short = 'n', long, default_value_t = 1000)]
        steps: u64,
        /// Where to write the JSON state. Use `-` for stdout.
        #[arg(long, default_value = "-")]
        out: PathBuf,
        /// Optional screenshot output path (PNG).
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Optional audio dump path (32 kHz stereo WAV).
        #[arg(long)]
        audio_out: Option<PathBuf>,
        /// Scripted joypad-1 input. Format: comma-separated
        /// `frame:hex` checkpoints (frame number in decimal, hex
        /// mask with optional `0x` prefix). The mask is latched at
        /// the start of the named PPU frame and held until the next
        /// checkpoint overrides it.
        ///
        /// Example: `--input "100:0x1000,110:0"` holds Start
        /// (`$1000`) for frames 100..=109 then releases.
        ///
        /// JOY1 bit layout: B(15) Y(14) Sel(13) Start(12)
        /// Up/Down/Left/Right(11..8) A(7) X(6) L(5) R(4).
        #[arg(long)]
        input: Option<String>,
        /// Optional memory peek(s) after snapshot.  Format:
        /// `BANK:OFFSET:COUNT` (all hex, no `0x` prefix).  Can be
        /// specified multiple times.  Output goes to stderr as a
        /// labelled hex dump.  Example: `--peek 7E:0200:220` reads
        /// 544 bytes of SMW shadow-OAM.
        #[arg(long = "peek")]
        peek: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            rom,
            steps,
            screenshot,
            force_display,
            bg,
            audio_out,
        } => run(
            &rom,
            steps,
            screenshot.as_deref(),
            force_display,
            bg,
            audio_out.as_deref(),
        ),
        Command::Mcp => serve_mcp(),
        Command::State {
            rom,
            steps,
            out,
            screenshot,
            audio_out,
            input,
            peek,
        } => run_state(
            &rom,
            steps,
            &out,
            screenshot.as_deref(),
            audio_out.as_deref(),
            input.as_deref(),
            &peek,
        ),
    }
}

/// `luna mcp` — serve the Luna MCP server on stdio until the client
/// disconnects.
fn serve_mcp() -> ExitCode {
    // Build a fresh tokio runtime here rather than `#[tokio::main]` so
    // the rest of the CLI (which doesn't need async) stays sync.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: building tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match rt.block_on(luna_mcp_server::serve_stdio()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: MCP server: {e}");
            ExitCode::from(1)
        }
    }
}

/// Parse a `--input` script: comma-separated `frame:hex` entries.
/// Returns the checkpoints sorted by ascending frame. The frame
/// number is decimal; the hex mask accepts an optional `0x` prefix
/// and is read as a 16-bit value.
fn parse_input_script(script: &str) -> Result<Vec<(u64, u16)>, String> {
    let mut out: Vec<(u64, u16)> = Vec::new();
    for entry in script.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (frame_str, mask_str) = entry
            .split_once(':')
            .ok_or_else(|| format!("missing ':' in entry `{entry}`"))?;
        let frame: u64 = frame_str
            .trim()
            .parse()
            .map_err(|e| format!("bad frame `{frame_str}`: {e}"))?;
        let mask_str = mask_str
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let mask: u16 = u16::from_str_radix(mask_str, 16)
            .map_err(|e| format!("bad hex mask `{mask_str}`: {e}"))?;
        out.push((frame, mask));
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

/// Parse a `BANK:OFFSET:COUNT` peek spec (all hex, no `0x` prefix).
fn parse_peek_spec(spec: &str) -> Result<(u8, u16, u16), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected BANK:OFFSET:COUNT, got `{spec}`"));
    }
    let bank = u8::from_str_radix(parts[0].trim(), 16)
        .map_err(|e| format!("bad bank `{}`: {e}", parts[0]))?;
    let offset = u16::from_str_radix(parts[1].trim(), 16)
        .map_err(|e| format!("bad offset `{}`: {e}", parts[1]))?;
    let count = u16::from_str_radix(parts[2].trim(), 16)
        .map_err(|e| format!("bad count `{}`: {e}", parts[2]))?;
    Ok((bank, offset, count))
}

/// Print a 16-bytes-per-row hex dump to stderr.
fn print_hex_dump(bank: u8, base: u16, bytes: &[u8]) {
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

/// `luna state` — exercise the public `luna-api` surface end-to-end.
fn run_state(
    rom: &std::path::Path,
    steps: u64,
    out: &std::path::Path,
    screenshot: Option<&std::path::Path>,
    audio_out: Option<&std::path::Path>,
    input_script: Option<&str>,
    peek_specs: &[String],
) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    if let Err(e) = em.load_rom(rom) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    // Parse `frame:hex` checkpoints into a sorted vector. We apply
    // them by stepping `step_until_frame` between checkpoints — a
    // ~30k-instruction budget per frame is enough for any real ROM,
    // including SA-1 carts.
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
    if !checkpoints.is_empty() {
        const PER_FRAME_BUDGET: u64 = 60_000;
        for (frame, mask) in &checkpoints {
            // Step until we reach `frame` (no-op if we already crossed it).
            while em.state().scheduler.frame_count < *frame {
                let executed = em.step_until_frame(PER_FRAME_BUDGET).unwrap_or(0);
                if executed == 0 {
                    break;
                }
            }
            if let Err(e) = em.set_joypad(0, *mask) {
                eprintln!("error: set_joypad: {e}");
                return ExitCode::from(1);
            }
        }
    }
    match em.step(steps) {
        Ok(_) => {}
        Err(e) => {
            // Step errors are informational — we still want a state
            // snapshot.
            eprintln!("step warning: {e}");
        }
    }

    for spec in peek_specs {
        match parse_peek_spec(spec) {
            Ok((bank, offset, count)) => match em.peek_memory(bank, offset, count) {
                Ok(bytes) => {
                    eprintln!("peek ${:02X}:{:04X} +{:04X}:", bank, offset, bytes.len());
                    print_hex_dump(bank, offset, &bytes);
                }
                Err(e) => eprintln!("error: peek_memory `{spec}`: {e}"),
            },
            Err(e) => eprintln!("error: --peek `{spec}`: {e}"),
        }
    }

    let state = em.state();
    let json = match serde_json::to_string_pretty(&state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: serialising state: {e}");
            return ExitCode::from(1);
        }
    };
    if out.as_os_str() == "-" {
        println!("{json}");
    } else if let Err(e) = std::fs::write(out, &json) {
        eprintln!("error: writing state JSON: {e}");
        return ExitCode::from(1);
    } else {
        eprintln!("State JSON written to {}", out.display());
    }

    if let Some(p) = screenshot {
        match em.render_frame_png(false) {
            Ok(png) => {
                if let Err(e) = std::fs::write(p, &png) {
                    eprintln!("error: writing screenshot: {e}");
                    return ExitCode::from(1);
                }
                eprintln!("Screenshot written to {}", p.display());
            }
            Err(e) => {
                eprintln!("error: render_frame_png: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(p) = audio_out {
        match em.drain_audio(usize::MAX) {
            Ok(samples) => {
                if let Err(e) = write_wav(p, &samples) {
                    eprintln!("error: writing WAV: {e}");
                    return ExitCode::from(1);
                }
                eprintln!(
                    "Audio WAV written to {}  ({} samples @ 32 kHz stereo)",
                    p.display(),
                    samples.len()
                );
            }
            Err(e) => {
                eprintln!("error: drain_audio: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn run(
    rom_path: &std::path::Path,
    steps: u64,
    screenshot: Option<&std::path::Path>,
    force_display: bool,
    bg: Option<u8>,
    audio_out: Option<&std::path::Path>,
) -> ExitCode {
    let cart = match Cartridge::load(rom_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    print_header(&cart);

    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    println!("After reset: PC=${:02X}:{:04X}", snes.cpu.pb, snes.cpu.pc);
    println!();

    // Silence the default panic printer so we own the panic message
    // output (catch_unwind doesn't suppress the hook by itself).
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut executed: u64 = 0;
    let mut panic_msg: Option<String> = None;
    // If we're recording audio, drain the APU queue every batch of
    // steps so it doesn't overflow (capped at 16 384 samples).
    let mut audio_samples: Vec<(i16, i16)> = if audio_out.is_some() {
        Vec::with_capacity(1 << 20)
    } else {
        Vec::new()
    };
    while executed < steps {
        if snes.cpu.stopped {
            println!("CPU halted by STP after {executed} instructions.");
            break;
        }
        match catch_unwind(AssertUnwindSafe(|| snes.step())) {
            Ok(_) => executed += 1,
            Err(payload) => {
                panic_msg = Some(payload_to_string(payload));
                break;
            }
        }
        // Drain audio every 4 096 instructions so the queue never
        // saturates (= ~250 audio samples produced per batch).
        if audio_out.is_some() && executed % 4_096 == 0 {
            snes.apu_real.drain_audio(&mut audio_samples, 8_192);
        }
    }
    // Final drain.
    if audio_out.is_some() {
        snes.apu_real.drain_audio(&mut audio_samples, usize::MAX);
    }

    std::panic::set_hook(prev_hook);

    println!("--- final state ---");
    print_cpu_state(&snes);
    print_diag_state(&mut snes);
    println!("Instructions executed: {executed}");
    println!("Total master cycles:   {}", snes.total_mclk);
    if let Some(msg) = panic_msg {
        println!();
        println!("Stopped on CPU panic:");
        println!("  {msg}");
        // Returning success here: hitting an unimplemented opcode is the
        // expected state of P0.6, not a CLI failure.
    }

    // Screenshot dump: render whatever the PPU has accumulated.
    if let Some(out_path) = screenshot {
        match save_screenshot(&snes, out_path, force_display, bg) {
            Ok(()) => println!("\nScreenshot written to {}", out_path.display()),
            Err(e) => {
                eprintln!("\nerror: could not write screenshot: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(out_path) = audio_out {
        match write_wav(out_path, &audio_samples) {
            Ok(_) => println!(
                "Audio WAV written to {}  ({} samples @ 32 kHz stereo, {} s)",
                out_path.display(),
                audio_samples.len(),
                audio_samples.len() as f64 / 32_000.0,
            ),
            Err(e) => {
                eprintln!("\nerror: could not write audio WAV: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

/// Minimal RIFF/WAVE writer for 16-bit signed PCM stereo at 32 kHz.
/// We hand-roll instead of pulling a `hound` dependency just for
/// one diagnostic path.
fn write_wav(path: &std::path::Path, samples: &[(i16, i16)]) -> std::io::Result<()> {
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

fn print_header(cart: &Cartridge) {
    let h = &cart.header;
    println!("=== ROM ===");
    println!("Title:       {:?}", h.title);
    println!(
        "Mapper:      {:?}{}",
        h.mapper_kind,
        if h.fast_rom { " (FastROM)" } else { "" }
    );
    println!(
        "ROM size:    {} KB ({} bytes on disk)",
        h.rom_size_kb,
        cart.rom.len()
    );
    println!("SRAM size:   {} KB", h.sram_size_kb);
    println!("Region:      {:?}", h.region);
    println!("Version:     v{}", h.version);
    println!(
        "Checksum:    ${:04X} / complement ${:04X} (valid: {})",
        h.checksum,
        h.checksum_complement,
        h.checksum_valid()
    );
}

fn print_cpu_state(snes: &Snes) {
    let cpu = &snes.cpu;
    println!(
        "A=${:04X}  X=${:04X}  Y=${:04X}  SP=${:04X}  DP=${:04X}",
        cpu.a, cpu.x, cpu.y, cpu.sp, cpu.dp
    );
    println!(
        "PC=${:02X}:{:04X}  DB=${:02X}  P=${:02X}  E={}",
        cpu.pb,
        cpu.pc,
        cpu.db,
        cpu.p.bits(),
        u8::from(cpu.e)
    );
    println!("flags: {}", flag_string(cpu.p.bits(), cpu.e));
}

fn print_diag_state(snes: &mut Snes) {
    let p = &snes.ppu;
    println!(
        "PPU:  INIDISP=${:02X} (blanked={}, brightness={})  BGMODE=${:02X}  VRAM_addr=${:04X}",
        p.inidisp,
        p.inidisp & 0x80 != 0,
        p.inidisp & 0x0F,
        p.bgmode,
        p.vram.address
    );
    println!(
        "PPU:  INIDISP_writes={}  Backdrop=${:04X}",
        p.inidisp_write_count,
        p.cgram.color(0)
    );
    for (i, bg) in p.bg.iter().enumerate() {
        let base = (bg.tilemap_addr_words as usize) << 1;
        let mut nonzero = 0usize;
        for off in 0..(32 * 32 * 2) {
            let a = (base + off) & 0xFFFF;
            if p.vram.peek(a as u16) != 0 {
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
        snes.cpu_regs.nmitimen,
        snes.cpu_regs.hvbjoy,
        snes.frame_count,
        snes.nmis_serviced,
        snes.ppu_line,
    );
    let ports = &snes.apu_real.to_cpu_ports;
    println!(
        "APU:  SPC PC=${:04X}  stopped={}  past_ipl={}  $2140=${:02X} $2141=${:02X} $2142=${:02X} $2143=${:02X}",
        snes.apu_real.cpu.pc,
        snes.apu_real.cpu.stopped,
        snes.apu_real.past_iplrom,
        ports[0],
        ports[1],
        ports[2],
        ports[3]
    );
    // Audio pipeline diagnostic — show whether the music driver is
    // actually producing audio. If MVOL or active voices stay at 0,
    // we *can't* hear anything regardless of the audio backend.
    let mvol_l = snes.apu_real.dsp_regs[0x0C] as i8;
    let mvol_r = snes.apu_real.dsp_regs[0x1C] as i8;
    let kon = snes.apu_real.dsp_regs[0x4C];
    let endx = snes.apu_real.dsp_regs[0x7C];
    let active_count = snes.apu_real.voice_active.iter().filter(|a| **a).count();
    let any_envelope = snes.apu_real.voice_envelope.iter().any(|e| *e != 0);
    let queue_len = snes.apu_real.audio_queue.len();
    let (last_l, last_r) = snes.apu_real.audio_sample();
    println!(
        "Audio:  MVOL_L={mvol_l} MVOL_R={mvol_r}  KON=${kon:02X} ENDX=${endx:02X}  \
         active_voices={active_count}  any_env_nonzero={any_envelope}  \
         queue_len={queue_len}  last_sample=({last_l},{last_r})"
    );
    // Echo subsystem state — useful for verifying the music driver
    // actually configured echo (most SNES tracks use it heavily).
    let flg = snes.apu_real.dsp_regs[0x6C];
    let esa = snes.apu_real.dsp_regs[0x6D];
    let edl = snes.apu_real.dsp_regs[0x7D] & 0x0F;
    let efb = snes.apu_real.dsp_regs[0x0D] as i8;
    let evol_l = snes.apu_real.dsp_regs[0x2C] as i8;
    let evol_r = snes.apu_real.dsp_regs[0x3C] as i8;
    let eon = snes.apu_real.dsp_regs[0x4D];
    let pmon = snes.apu_real.dsp_regs[0x2D];
    let non = snes.apu_real.dsp_regs[0x3D];
    println!(
        "Echo:   FLG=${flg:02X} (reset={} mute={} ECEN={}) \
         ESA=${esa:02X} (=${esa:02X}00) EDL=${edl:X} ({} samples) \
         EFB={efb} EVOL=({evol_l},{evol_r}) EON=${eon:02X} PMON=${pmon:02X} NON=${non:02X}",
        flg >> 7 & 1,
        flg >> 6 & 1,
        flg >> 5 & 1,
        if edl == 0 { 1 } else { (edl as u16) * 512 }
    );
    if let Some((op, pc)) = snes.apu_real.cpu.unimplemented_opcode {
        // Dump 4 bytes around the offending PC so we can see the
        // operand pattern and recognise the addressing mode.
        let aram = &snes.apu_real.aram;
        println!(
            "APU:  STOPPED on unimplemented opcode ${op:02X} at SPC PC=${pc:04X}  (bytes: {:02X} {:02X} {:02X} {:02X})",
            aram[pc as usize],
            aram[pc.wrapping_add(1) as usize],
            aram[pc.wrapping_add(2) as usize],
            aram[pc.wrapping_add(3) as usize],
        );
    }
    // OAM occupancy + first few sprite entries (uses immutable `p`).
    let mut oam_non_zero = 0usize;
    for off in 0..0x220u16 {
        if p.oam.peek(off) != 0 {
            oam_non_zero += 1;
        }
    }
    println!(
        "OAM:   {oam_non_zero}/544 non-zero  |  OBSEL=${:02X}",
        p.obsel
    );
    // What's actually been written into OAM that *isn't* the hide
    // value? Helps distinguish "game uploaded N sprites" from
    // "game wrote the hide marker over everything".
    print!("OAM non-$F0/non-zero bytes: ");
    let mut shown = 0;
    for off in 0..0x220u16 {
        let b = p.oam.peek(off);
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
    print!("OAM shadow Y (non-$F0): ");
    let mut shown2 = 0;
    for (i, y) in p.oam.shadow_y.iter().enumerate() {
        if *y != 0xF0 {
            print!("[#{i}=${y:02X}] ");
            shown2 += 1;
            if shown2 >= 20 {
                print!("...");
                break;
            }
        }
    }
    println!();
    let all_sprites = luna_ppu::decode_all_sprites(p);
    let visible_count = all_sprites.iter().filter(|sp| sp.y < 224).count();
    println!("  visible sprites (y<224): {visible_count}");
    let mut shown = 0;
    for (i, sp) in all_sprites.iter().enumerate() {
        if sp.y >= 224 {
            continue;
        }
        if shown >= 12 {
            break;
        }
        shown += 1;
        println!(
            "  sprite #{i:>3}: x={:>4} y={:>3} tile=${:03X} pal={} pri={} {}x{} {}{}",
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
    let mut vram_non_zero = 0usize;
    for off in 0..0x10000u32 {
        if snes.ppu.vram.peek(off as u16) != 0 {
            vram_non_zero += 1;
        }
    }
    let mut cgram_non_zero = 0usize;
    for idx in 0..256u16 {
        if snes.ppu.cgram.color(idx as u8) != 0 {
            cgram_non_zero += 1;
        }
    }
    println!(
        "VRAM:  {vram_non_zero}/65536 non-zero bytes  |  CGRAM: {cgram_non_zero}/256 non-zero colours"
    );

    // @PC bytes need mutable bus access — run after all immutable
    // PPU diagnostics are done.
    let pc_bytes = snes.peek_pc_bytes(8);
    print!("@PC bytes:");
    for b in &pc_bytes {
        print!(" {b:02X}");
    }
    println!();
}

fn flag_string(p: u8, e: bool) -> String {
    let bit = |mask: u8, c: char, fallback: char| if p & mask != 0 { c } else { fallback };
    format!(
        "{}{}{}{}{}{}{}{} (e={})",
        bit(0b1000_0000, 'N', 'n'),
        bit(0b0100_0000, 'V', 'v'),
        bit(0b0010_0000, 'M', 'm'),
        bit(0b0001_0000, 'X', 'x'),
        bit(0b0000_1000, 'D', 'd'),
        bit(0b0000_0100, 'I', 'i'),
        bit(0b0000_0010, 'Z', 'z'),
        bit(0b0000_0001, 'C', 'c'),
        u8::from(e),
    )
}

fn save_screenshot(
    snes: &Snes,
    path: &std::path::Path,
    force_display: bool,
    bg: Option<u8>,
) -> Result<(), image::ImageError> {
    let opts = luna_ppu::RenderOptions {
        bypass_forced_blank: force_display,
    };
    let mut buf = Vec::with_capacity(luna_ppu::FRAME_W * luna_ppu::FRAME_H * 3);
    // Default path (no --bg, no --force-display): copy the persistent
    // framebuffer that the scheduler maintains per-scanline (gap G6
    // Phase 1). Debug paths (`--force-display` or single-BG render)
    // still go through the one-shot renderer.
    if bg.is_none() && !force_display {
        for px in snes.ppu.framebuffer() {
            buf.extend_from_slice(px);
        }
    } else {
        let frame = match bg {
            Some(n) => {
                let idx = (n.saturating_sub(1).min(3)) as usize;
                luna_ppu::render_frame_bg_with(&snes.ppu, idx, opts)
            }
            None => luna_ppu::render_frame_with(&snes.ppu, opts),
        };
        for px in frame {
            buf.extend_from_slice(&px);
        }
    }
    let img = image::RgbImage::from_raw(luna_ppu::FRAME_W as u32, luna_ppu::FRAME_H as u32, buf)
        .expect("frame buffer size matches dims");
    img.save(path)
}

fn payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(unknown panic payload)".to_string()
    }
}
