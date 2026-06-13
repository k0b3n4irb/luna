//! Luna SNES emulator — command-line entry point.
//!
//! Dispatches between execution modes (run / mcp / replay).
//! See `ARCHITECTURE.md` §3.2.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod bench;

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
// The `State` variant carries many optional diagnostic-output paths (one
// per trace/log kind); adding `--sa1-log` tipped it past the 200-byte
// variant-size delta. This enum is parsed exactly once at startup, so the
// size is irrelevant — boxing CLI-arg fields would only add noise.
#[allow(clippy::large_enum_variant)]
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
    /// Once started, Luna exposes a tool catalogue (`load_rom`, reset,
    /// step, state, screenshot, `drain_audio`, `peek_memory`, `peek_aram`)
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
        /// Force a cartridge mapper, bypassing header auto-detection.
        /// Needed for headerless homebrew test ROMs (e.g. the `PeterLemon`
        /// Super FX / GSU plot tests). One of: lorom, hirom, exhirom, sa1,
        /// superfx.
        #[arg(long = "force-mapper")]
        force_mapper: Option<String>,
        /// Install a DSP coprocessor firmware (`dsp1b.rom`) into luna's
        /// firmware folder, then load — needed for DSP-1 games (Super
        /// Mario Kart, Pilotwings). Persists for future runs.
        #[arg(long = "dsp1-rom")]
        dsp1_rom: Option<PathBuf>,
        /// Dump all 64 KB of PPU VRAM (raw bytes) to this file after the
        /// run. For diagnosing the framebuffer DMA → VRAM → display path.
        #[arg(long = "dump-vram")]
        dump_vram: Option<PathBuf>,
        /// Dump the coprocessor work RAM (Super FX Game Pak RAM) raw bytes
        /// to this file, bypassing GSU ownership gating. For comparing
        /// luna's CPU-prepared GSU inputs against a reference.
        #[arg(long = "dump-coproc-ram")]
        dump_coproc_ram: Option<PathBuf>,
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
        /// Optional CPU↔APU mailbox traffic log. When set, every
        /// CPU read/write of `$2140-$2143` during the run is captured
        /// and written to the given path as CSV with columns:
        /// `mclk_total,frame,pc,kind,port,value` (one row per event).
        /// Useful for diagnosing APU handshake stalls (e.g. SMW's
        /// music-driver "wait for ack" deadlock).
        #[arg(long = "apu-log")]
        apu_log: Option<PathBuf>,
        /// Optional SA-1 MMIO traffic log. When set, every CPU read/write
        /// of an SA-1 register `$2200-$23FF` during the run is captured and
        /// written to the given path as CSV with columns:
        /// `mclk_total,frame_ntsc,pc,kind,reg,value` (one row per event).
        /// Useful for diagnosing the CPU↔SA-1 handshake (e.g. the SMRPG
        /// intro deadlock).
        #[arg(long = "sa1-log")]
        sa1_log: Option<PathBuf>,
        /// Optional SA-1-*side* execution log. When set, the SA-1's own
        /// reads/writes of its registers `$2200-$23FF` AND its *writes*
        /// to I-RAM (`$3000-$37FF`, reported as `$30xx` even via the
        /// `$0000-$07FF` mirror) are captured with the SA-1 PC and
        /// written as CSV (`seq,sa1_pc,kind,reg,value`). The I-RAM
        /// writes expose the cross-CPU handshake flags (e.g. Kirby's
        /// `$300A`/`$300E`) that the MMIO-only view can't show. Reads of
        /// I-RAM are NOT logged (they flood when the SA-1 spins on a
        /// flag). Complements `--sa1-log` (S-CPU side) to see why the
        /// SA-1 (re)asserts a register, e.g. the SMRPG SCNT=$87 loop.
        #[arg(long = "sa1-side-log")]
        sa1_side_log: Option<PathBuf>,
        /// Optional FULL SA-1 instruction trace: a pre-opcode register
        /// snapshot per SA-1 instruction, written as CSV
        /// (`seq,pc,a,x,y,sp,p,db,dp,e`). Diff this PC stream against a
        /// bsnes/Mesen2 SA-1 trace to localise the SMRPG deadlock.
        #[arg(long = "sa1-trace")]
        sa1_trace: Option<PathBuf>,
        /// Cap the SA-1 instruction trace at this many events (default
        /// 200 000).
        #[arg(long = "sa1-trace-max", default_value_t = 200_000)]
        sa1_trace_max: usize,
        /// Optional FULL Super FX (GSU) instruction trace: a per-opcode
        /// snapshot written as CSV (`seq,pc,opcode,sfr,r0..r15`). Diff this
        /// PC/register stream against a bsnes/siena GSU trace to localise
        /// rendering divergences.
        #[arg(long = "superfx-trace")]
        superfx_trace: Option<PathBuf>,
        /// Cap the Super FX instruction trace at this many events (default
        /// 200 000).
        #[arg(long = "superfx-trace-max", default_value_t = 200_000)]
        superfx_trace_max: usize,
        /// Optional CPU instruction trace. When set, captures a
        /// per-instruction register snapshot (PC, A, X, Y, SP, P, DB,
        /// DP, e) into the given CSV file. Capture starts at instr
        /// count `--cpu-trace-from` (default 0) and stops after
        /// `--cpu-trace-max` events (default 100 000). Memory cost is
        /// ≈ 40 bytes × max-events.
        #[arg(long = "cpu-trace")]
        cpu_trace: Option<PathBuf>,
        /// Instruction count at which to begin populating
        /// `--cpu-trace`. Default 0 (= capture from the very first
        /// step). Set to a value near the scene you want to debug to
        /// keep the buffer small.
        #[arg(long = "cpu-trace-from", default_value_t = 0)]
        cpu_trace_from: u64,
        /// Max number of trace events to capture (hard cap on log
        /// size). Default 100 000 ≈ 4 MB CSV.
        #[arg(long = "cpu-trace-max", default_value_t = 100_000)]
        cpu_trace_max: usize,
        /// Optional memory access trace. When set, captures every
        /// CPU bus read/write into a CSV at PATH. Default: all
        /// banks. Combine with `--mem-trace-bank 7E` to focus on
        /// WRAM and skip ROM fetches. Gated by `--mem-trace-from`
        /// and `--mem-trace-max` analogous to `--cpu-trace-*`.
        #[arg(long = "mem-trace")]
        mem_trace: Option<PathBuf>,
        #[arg(long = "mem-trace-from", default_value_t = 0)]
        mem_trace_from: u64,
        #[arg(long = "mem-trace-max", default_value_t = 100_000)]
        mem_trace_max: usize,
        /// Hex bank to filter the memory trace on (e.g. `7E` for
        /// WRAM main page). Omit to capture every access.
        #[arg(long = "mem-trace-bank")]
        mem_trace_bank: Option<String>,
        /// Optional DMA→VRAM transfer-time trace. Captures every byte an
        /// MDMA writes to `$2118/$2119` as CSV
        /// (`seq,src,vram_word,reg,value`) — `src` is the 24-bit A-bus
        /// source, `vram_word` the VMADD word the byte lands at, `reg`
        /// $18/$19. The byte is captured AS READ during the transfer, so a
        /// coprocessor (Super FX) overwriting its source buffer afterwards
        /// can't confound the source→VRAM comparison. Gated by
        /// `--dma-trace-from`/`--dma-trace-max`.
        #[arg(long = "dma-trace")]
        dma_trace: Option<PathBuf>,
        /// Instruction count at which to begin the DMA→VRAM trace.
        #[arg(long = "dma-trace-from", default_value_t = 0)]
        dma_trace_from: u64,
        /// Max DMA→VRAM trace events (default 500 000).
        #[arg(long = "dma-trace-max", default_value_t = 500_000)]
        dma_trace_max: usize,
    },
    /// Capture a sequence of EXACTLY-consecutive PPU frames as PNGs in
    /// one run, via the same `luna-api` render path the GUI uses. Use
    /// this to diagnose *temporal* artefacts (flicker / double-buffer
    /// page-flip desync) that a single `state --screenshot` cannot show
    /// — it samples one frame, so a frame-to-frame "blink" is invisible
    /// to it. Each frame's PNG is tagged with its frame number and the
    /// INIDISP forced-blank flag, so you can see exactly what the GUI
    /// would (and would not) display.
    Frames {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// Warm-up CPU instructions to execute before capturing begins.
        #[arg(short = 'n', long, default_value_t = 1000)]
        steps: u64,
        /// Number of consecutive frames to capture.
        #[arg(short = 'c', long = "count", default_value_t = 8)]
        count: u64,
        /// Output directory for the PNG sequence (created if absent).
        #[arg(long = "out-dir", default_value = "/tmp/luna_frames")]
        out_dir: PathBuf,
        /// Force a cartridge mapper, bypassing header auto-detection
        /// (lorom, hirom, exhirom, sa1, superfx).
        #[arg(long = "force-mapper")]
        force_mapper: Option<String>,
        /// Scripted joypad-1 input, same `frame:hex` format as
        /// `state --input`, applied during the warm-up so the capture
        /// can land in gameplay rather than at a title screen.
        #[arg(long)]
        input: Option<String>,
    },
    /// Emit per-frame (vblank-aligned) WRAM page hashes for a
    /// confound-free cross-emulator differential. Each line is
    /// `<ppu_frame> <h0> <h1> ... <hN>` where each `h` is the FNV-1a
    /// hash of one WRAM page (`--page-size` bytes, default 4 KiB → 32
    /// pages). Because WRAM-at-vblank-N is the SAME game-frame in both
    /// luna and a reference emulator (no input ⟹ game logic advances
    /// once per NMI), the first frame whose page hash differs from the
    /// reference pins the first REAL state divergence — unlike scene-
    /// level windows the boot-frame offset confounds.
    WramTrace {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// Warm-up CPU instructions before frame-0 of the trace.
        #[arg(short = 'n', long, default_value_t = 0)]
        steps: u64,
        /// Number of consecutive frames to hash.
        #[arg(short = 'c', long = "count", default_value_t = 300)]
        count: u64,
        /// WRAM page size in bytes (power of two dividing 0x20000).
        #[arg(long = "page-size", default_value_t = 0x1000)]
        page_size: usize,
        /// Output path for the hash table (one line per frame).
        #[arg(long = "out", default_value = "/tmp/luna_wram_hashes.txt")]
        out: PathBuf,
        /// Optionally also dump the full 128 KiB WRAM as a raw .bin when the
        /// trace reaches this PPU frame (for byte-level diffing).
        #[arg(long = "dump-frame")]
        dump_frame: Option<u64>,
        /// Output path for the `--dump-frame` raw WRAM snapshot.
        #[arg(long = "dump-out", default_value = "/tmp/luna_wram_frame.bin")]
        dump_out: PathBuf,
        /// Force a cartridge mapper (lorom, hirom, exhirom, sa1, superfx).
        #[arg(long = "force-mapper")]
        force_mapper: Option<String>,
        /// Scripted joypad-1 input, same `frame:hex` format as `state --input`.
        #[arg(long)]
        input: Option<String>,
    },
    /// Run every ROM in a directory headless, detect anomalies (crashes,
    /// freezes, dead APU, missing firmware), and write a compatibility
    /// report + one markdown bug file per finding. Stresses the CLI/API
    /// across the whole corpus. Reports stay local (under `--out`).
    Bench {
        /// Directory of ROMs to scan (`.sfc` / `.smc`).
        #[arg(default_value = "tests/roms")]
        dir: PathBuf,
        /// Output directory for the report, screenshots, and bug files.
        #[arg(long, default_value = "tests/roms/bench")]
        out: PathBuf,
        /// Frames to run per ROM.
        #[arg(short = 'f', long, default_value_t = 600)]
        frames: u64,
        /// Override the default Start-pulse input (`frame:hex`, like
        /// `state --input`) applied to clear title screens.
        #[arg(long)]
        input: Option<String>,
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
            force_mapper,
            dsp1_rom,
            dump_vram,
            out,
            screenshot,
            audio_out,
            input,
            peek,
            apu_log,
            sa1_log,
            sa1_side_log,
            sa1_trace,
            sa1_trace_max,
            superfx_trace,
            superfx_trace_max,
            cpu_trace,
            cpu_trace_from,
            cpu_trace_max,
            mem_trace,
            mem_trace_from,
            mem_trace_max,
            mem_trace_bank,
            dma_trace,
            dma_trace_from,
            dma_trace_max,
            dump_coproc_ram,
        } => run_state(
            &rom,
            steps,
            force_mapper.as_deref(),
            dump_vram.as_deref(),
            dump_coproc_ram.as_deref(),
            &out,
            screenshot.as_deref(),
            audio_out.as_deref(),
            input.as_deref(),
            &peek,
            apu_log.as_deref(),
            sa1_log.as_deref(),
            sa1_side_log.as_deref(),
            sa1_trace.as_deref(),
            sa1_trace_max,
            superfx_trace.as_deref(),
            superfx_trace_max,
            cpu_trace.as_deref(),
            cpu_trace_from,
            cpu_trace_max,
            mem_trace.as_deref(),
            mem_trace_from,
            mem_trace_max,
            mem_trace_bank.as_deref(),
            dma_trace.as_deref(),
            dma_trace_from,
            dma_trace_max,
            dsp1_rom.as_deref(),
        ),
        Command::Frames {
            rom,
            steps,
            count,
            out_dir,
            force_mapper,
            input,
        } => run_frames(
            &rom,
            steps,
            count,
            &out_dir,
            force_mapper.as_deref(),
            input.as_deref(),
        ),
        Command::WramTrace {
            rom,
            steps,
            count,
            page_size,
            out,
            dump_frame,
            dump_out,
            force_mapper,
            input,
        } => run_wram_trace(
            &rom,
            steps,
            count,
            page_size,
            &out,
            dump_frame,
            &dump_out,
            force_mapper.as_deref(),
            input.as_deref(),
        ),
        Command::Bench {
            dir,
            out,
            frames,
            input,
        } => {
            let checkpoints = match input.as_deref().map(parse_input_script) {
                Some(Ok(c)) => Some(c),
                Some(Err(e)) => {
                    eprintln!("error: --input: {e}");
                    return ExitCode::from(2);
                }
                None => None,
            };
            bench::run_bench(&dir, &out, frames, checkpoints)
        }
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

/// Master clocks per NTSC frame: 262 scanlines × 1364 mclk = 357 368.
const NTSC_MCLK_PER_FRAME: u64 = 1364 * 262;

/// Format a 24-bit program counter / bus address as `$BB:OOOO`
/// (bank:offset) — the canonical PC column shared by the trace writers.
fn fmt_pc(pc_full: u32) -> String {
    format!("${:02X}:{:04X}", (pc_full >> 16) & 0xFF, pc_full & 0xFFFF)
}

/// Shared skeleton for the trace CSV writers: create `path`, write the
/// `header` line, then format each event via `row` (handed the writer,
/// the event index, and the event). Centralises the
/// File/BufWriter/header boilerplate every writer repeated.
fn write_csv<T>(
    path: &std::path::Path,
    header: &str,
    rows: &[T],
    mut row: impl FnMut(&mut dyn std::io::Write, usize, &T) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(f, "{header}")?;
    for (i, ev) in rows.iter().enumerate() {
        row(&mut f, i, ev)?;
    }
    f.flush()
}

/// Write APU mailbox events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc_bank_offset, kind, port, value_hex`.
/// `frame_ntsc` assumes NTSC (262 lines × 1364 mclk = 357 368 mclk/frame).
fn write_mailbox_log_csv(
    path: &std::path::Path,
    events: &[luna_api::MailboxEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,kind,port,value",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MailboxEventKind::Read => "R",
                luna_api::MailboxEventKind::Write => "W",
            };
            writeln!(
                f,
                "{},{},{},{},{},${:02X}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                kind,
                ev.port,
                ev.value
            )
        },
    )
}

/// Write SA-1 MMIO events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, kind, reg, value`. `reg` is the register
/// address (`$2200-$23FF`); `frame_ntsc` assumes NTSC.
fn write_sa1_log_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1LogEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,kind,reg,value",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MailboxEventKind::Read => "R",
                luna_api::MailboxEventKind::Write => "W",
            };
            writeln!(
                f,
                "{},{},{},{},${:04X},${:02X}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                kind,
                ev.reg,
                ev.value
            )
        },
    )
}

/// Write SA-1-side execution events as CSV. Columns:
/// `seq, sa1_pc, kind, reg, value`. `seq` is the event index (the SA-1
/// side has no master-clock handle); ordering is what reveals the loop.
fn write_sa1_side_log_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1SideEvent],
) -> std::io::Result<()> {
    write_csv(path, "seq,sa1_pc,kind,reg,value", events, |f, i, ev| {
        writeln!(
            f,
            "{},{},{},${:04X},${:02X}",
            i,
            fmt_pc(ev.sa1_pc),
            if ev.write { "W" } else { "R" },
            ev.reg,
            ev.value
        )
    })
}

/// Write the full SA-1 instruction trace as CSV. Columns:
/// `seq, pc, a, x, y, sp, p, db, dp, e`. Diff the `pc` column against a
/// reference (bsnes/Mesen2) SA-1 trace to find the first divergence.
fn write_sa1_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1TraceEvent],
) -> std::io::Result<()> {
    write_csv(path, "seq,pc,a,x,y,sp,p,db,dp,e", events, |f, i, ev| {
        writeln!(
            f,
            "{},{},${:04X},${:04X},${:04X},${:04X},${:02X},${:02X},${:04X},{}",
            i,
            fmt_pc(ev.pc_full),
            ev.a,
            ev.x,
            ev.y,
            ev.sp,
            ev.p,
            ev.db,
            ev.dp,
            u8::from(ev.e),
        )
    })
}

/// Write per-opcode Super FX (GSU) trace events as CSV. Columns:
/// `seq, pc, opcode, sfr, r0..r15`. Diff the `pc` / register columns
/// against a reference (bsnes / siena) GSU trace to find the first
/// divergence in the rendering.
fn write_superfx_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::SuperFxTraceEvent],
) -> std::io::Result<()> {
    let mut header = String::from("seq,mclk,go,stop,pc,opcode,sfr");
    for n in 0..16 {
        header.push_str(",r");
        header.push_str(&n.to_string());
    }
    write_csv(path, &header, events, |f, i, ev| {
        write!(
            f,
            "{},{},{},{},{},${:02X},${:04X}",
            i,
            ev.mclk,
            u8::from(ev.go_start),
            u8::from(ev.stop),
            fmt_pc(ev.pc_full),
            ev.opcode,
            ev.sfr,
        )?;
        for r in ev.r {
            write!(f, ",${r:04X}")?;
        }
        writeln!(f)
    })
}

/// Write DMA→VRAM transfer-time trace events as CSV. Columns:
/// `seq,src,vram_word,reg,value` — `src` is the 24-bit A-bus source
/// (`bank:offset`), `vram_word` the VMADD word the byte landed at, `reg`
/// the B-bus port ($2118 low / $2119 high), `value` the transferred byte.
fn write_dma_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::DmaTraceEvent],
) -> std::io::Result<()> {
    write_csv(path, "seq,src,vram_word,reg,value", events, |f, i, ev| {
        writeln!(
            f,
            "{},{},${:04X},${:02X},${:02X}",
            i,
            fmt_pc(ev.src_full),
            ev.vram_word,
            ev.b_offset,
            ev.value,
        )
    })
}

/// Write per-instruction CPU trace events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, a, x, y, sp, p_hex, db, dp, e`.
fn write_cpu_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::CpuTraceEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,a,x,y,sp,p,db,dp,e",
        events,
        |f, _, ev| {
            writeln!(
                f,
                "{},{},{},${:04X},${:04X},${:04X},${:04X},${:02X},${:02X},${:04X},{}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                ev.a,
                ev.x,
                ev.y,
                ev.sp,
                ev.p,
                ev.db,
                ev.dp,
                ev.e as u8
            )
        },
    )
}

/// Write per-access memory trace events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, addr, kind, value_hex`.
fn write_mem_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::MemTraceEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,addr,kind,value",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MemEventKind::Read => "R",
                luna_api::MemEventKind::Write => "W",
            };
            writeln!(
                f,
                "{},{},{},{},{},${:02X}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                fmt_pc(ev.addr_full),
                kind,
                ev.value
            )
        },
    )
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

/// Load `rom` into `em`, honouring an optional `--force-mapper` override.
/// Centralises the force-mapper parse + file read shared by the `state`
/// and `frames` subcommands. Returns a human-facing error string.
fn load_rom_into(
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

/// `luna frames` — capture `count` exactly-consecutive PPU frames as
/// PNGs via the same `luna-api` render path the GUI uses, tagging each
/// with its frame number and forced-blank flag. Lets us reproduce the
/// temporal artefacts (flicker / page-flip desync) that a single
/// `state --screenshot` is structurally blind to.
fn run_frames(
    rom: &std::path::Path,
    steps: u64,
    count: u64,
    out_dir: &std::path::Path,
    force_mapper: Option<&str>,
    input_script: Option<&str>,
) -> ExitCode {
    const FRAME_BUDGET: u64 = 200_000;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, None) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("error: creating {}: {e}", out_dir.display());
        return ExitCode::from(1);
    }
    // Scripted input during warm-up (same semantics as `state --input`),
    // so the capture can land in gameplay rather than at a title screen.
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
    // Capture loop: one PNG per consecutive frame, tagged frame# + blank.
    for i in 0..count {
        let executed = em.step_until_frame(FRAME_BUDGET).unwrap_or(0);
        let frame = em.frame_count().unwrap_or(0);
        let blanked = em.forced_blank().unwrap_or(false);
        let showed = em.frame_showed_content().unwrap_or(true);
        let png = match em.render_frame_png(false) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: render_frame_png: {e}");
                return ExitCode::from(1);
            }
        };
        // Tag on the per-frame "showed visible content" latch (what the GUI
        // publishes), not the instantaneous forced-blank bit — the latter
        // mislabels Super FX frames that re-blank at VBlank as "blank".
        let tag = if showed { "live" } else { "blank" };
        let path = out_dir.join(format!("frame_{i:03}_f{frame}_{tag}.png"));
        if let Err(e) = std::fs::write(&path, &png) {
            eprintln!("error: writing {}: {e}", path.display());
            return ExitCode::from(1);
        }
        println!(
            "frame {i:>3}: ppu_frame={frame} showed_content={showed} forced_blank={blanked} (+{executed} instr) -> {}",
            path.display()
        );
        if executed == 0 {
            eprintln!("note: step_until_frame returned 0 (emulator halted?) — stopping early");
            break;
        }
    }
    ExitCode::SUCCESS
}

/// `luna wram-trace` — emit per-frame (vblank-aligned) WRAM page hashes
/// for a confound-free cross-emulator differential (see the subcommand
/// doc). One line per frame: `<ppu_frame> <h0> <h1> ...`.
#[allow(clippy::too_many_arguments)]
fn run_wram_trace(
    rom: &std::path::Path,
    steps: u64,
    count: u64,
    page_size: usize,
    out: &std::path::Path,
    dump_frame: Option<u64>,
    dump_out: &std::path::Path,
    force_mapper: Option<&str>,
    input_script: Option<&str>,
) -> ExitCode {
    use std::fmt::Write as _;
    const FRAME_BUDGET: u64 = 200_000;
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, None) {
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
    if steps > 0 {
        if let Err(e) = em.step(steps) {
            eprintln!("step warning (warm-up): {e}");
        }
    }
    // Input checkpoints are applied DURING the capture loop, keyed by the
    // current PPU frame — so a scripted joypad pulse can span the frames
    // being hashed (front-loading them would consume the pulse before the
    // capture even starts).
    let mut ck_idx = 0usize;
    let mut buf = String::new();
    for _ in 0..count {
        let cur_frame = em.frame_count().unwrap_or(0);
        while ck_idx < checkpoints.len() && checkpoints[ck_idx].0 <= cur_frame {
            if let Err(e) = em.set_joypad(0, checkpoints[ck_idx].1) {
                eprintln!("error: set_joypad: {e}");
                return ExitCode::from(1);
            }
            ck_idx += 1;
        }
        let executed = em.step_until_frame(FRAME_BUDGET).unwrap_or(0);
        let frame = em.frame_count().unwrap_or(0);
        let hashes = match em.wram_page_hashes(page_size) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: wram_page_hashes: {e}");
                return ExitCode::from(1);
            }
        };
        let _ = write!(buf, "{frame}");
        for h in &hashes {
            let _ = write!(buf, " {h:016x}");
        }
        buf.push('\n');
        if dump_frame == Some(frame) {
            match em.wram_snapshot() {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(dump_out, &bytes) {
                        eprintln!("error: writing {}: {e}", dump_out.display());
                        return ExitCode::from(1);
                    }
                    println!(
                        "dumped {} WRAM bytes at frame {frame} -> {}",
                        bytes.len(),
                        dump_out.display()
                    );
                }
                Err(e) => {
                    eprintln!("error: wram_snapshot: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        if executed == 0 {
            eprintln!("note: step_until_frame returned 0 (emulator halted?) — stopping early");
            break;
        }
    }
    if let Err(e) = std::fs::write(out, &buf) {
        eprintln!("error: writing {}: {e}", out.display());
        return ExitCode::from(1);
    }
    println!(
        "wrote {count} frames x {} pages of {page_size}-byte WRAM hashes -> {}",
        0x2_0000 / page_size,
        out.display()
    );
    ExitCode::SUCCESS
}

/// `luna state` — exercise the public `luna-api` surface end-to-end.
#[allow(clippy::too_many_arguments)]
fn run_state(
    rom: &std::path::Path,
    steps: u64,
    force_mapper: Option<&str>,
    dump_vram_path: Option<&std::path::Path>,
    dump_coproc_ram_path: Option<&std::path::Path>,
    out: &std::path::Path,
    screenshot: Option<&std::path::Path>,
    audio_out: Option<&std::path::Path>,
    input_script: Option<&str>,
    peek_specs: &[String],
    apu_log_path: Option<&std::path::Path>,
    sa1_log_path: Option<&std::path::Path>,
    sa1_side_log_path: Option<&std::path::Path>,
    sa1_trace_path: Option<&std::path::Path>,
    sa1_trace_max: usize,
    superfx_trace_path: Option<&std::path::Path>,
    superfx_trace_max: usize,
    cpu_trace_path: Option<&std::path::Path>,
    cpu_trace_from: u64,
    cpu_trace_max: usize,
    mem_trace_path: Option<&std::path::Path>,
    mem_trace_from: u64,
    mem_trace_max: usize,
    mem_trace_bank: Option<&str>,
    dma_trace_path: Option<&std::path::Path>,
    dma_trace_from: u64,
    dma_trace_max: usize,
    dsp1_rom: Option<&std::path::Path>,
) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, dsp1_rom) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if apu_log_path.is_some() {
        if let Err(e) = em.enable_mailbox_log() {
            eprintln!("error: enable_mailbox_log: {e}");
            return ExitCode::from(1);
        }
    }
    if sa1_log_path.is_some() {
        if let Err(e) = em.enable_sa1_log() {
            eprintln!("error: enable_sa1_log: {e}");
            return ExitCode::from(1);
        }
    }
    if sa1_side_log_path.is_some() {
        if let Err(e) = em.enable_sa1_side_log() {
            eprintln!("error: enable_sa1_side_log: {e}");
            return ExitCode::from(1);
        }
    }
    if sa1_trace_path.is_some() {
        if let Err(e) = em.enable_sa1_trace(sa1_trace_max) {
            eprintln!("error: enable_sa1_trace: {e}");
            return ExitCode::from(1);
        }
    }
    if superfx_trace_path.is_some() {
        if let Err(e) = em.enable_superfx_trace(superfx_trace_max) {
            eprintln!("error: enable_superfx_trace: {e}");
            return ExitCode::from(1);
        }
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
    // Trace gating: bridge to the earliest of the two "trace-from"
    // targets, enable whichever crossed; bridge to the later one if
    // it's still ahead, enable that. Each trace caps itself at its
    // own --*-trace-max events regardless of remaining steps.
    let mut remaining = steps;
    let mut bridge_target = u64::MAX;
    if cpu_trace_path.is_some() {
        bridge_target = bridge_target.min(cpu_trace_from);
    }
    if mem_trace_path.is_some() {
        bridge_target = bridge_target.min(mem_trace_from);
    }
    let parsed_mem_bank: Option<u8> = match mem_trace_bank {
        None => None,
        Some(s) => match u8::from_str_radix(s.trim_start_matches("0x"), 16) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("error: --mem-trace-bank `{s}`: {e}");
                return ExitCode::from(1);
            }
        },
    };
    if bridge_target != u64::MAX {
        let current = em.instructions_executed();
        if bridge_target > current {
            let bridge = (bridge_target - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-trace bridge 1): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
        }
    }
    // Enable whichever traces are at or past their target now.
    if cpu_trace_path.is_some() && em.instructions_executed() >= cpu_trace_from {
        if let Err(e) = em.enable_cpu_trace(cpu_trace_max) {
            eprintln!("error: enable_cpu_trace: {e}");
            return ExitCode::from(1);
        }
    }
    if mem_trace_path.is_some() && em.instructions_executed() >= mem_trace_from {
        if let Err(e) = em.enable_mem_trace(mem_trace_max, parsed_mem_bank) {
            eprintln!("error: enable_mem_trace: {e}");
            return ExitCode::from(1);
        }
    }
    // If the two targets differ, bridge to the second one and enable.
    if cpu_trace_path.is_some() && mem_trace_path.is_some() && cpu_trace_from != mem_trace_from {
        let second_target = cpu_trace_from.max(mem_trace_from);
        let current = em.instructions_executed();
        if second_target > current {
            let bridge = (second_target - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-trace bridge 2): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
            // Re-check enables (whichever wasn't enabled yet).
            if cpu_trace_path.is_some() && em.instructions_executed() >= cpu_trace_from {
                let _ = em.enable_cpu_trace(cpu_trace_max);
            }
            if mem_trace_path.is_some() && em.instructions_executed() >= mem_trace_from {
                let _ = em.enable_mem_trace(mem_trace_max, parsed_mem_bank);
            }
        }
    }
    // DMA→VRAM trace: bridge to its start instruction independently of the
    // cpu/mem traces, then enable. Capturing from boot would drown the
    // window of interest, so `--dma-trace-from` skips the early uploads.
    if dma_trace_path.is_some() {
        let current = em.instructions_executed();
        if dma_trace_from > current {
            let bridge = (dma_trace_from - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-dma-trace bridge): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
        }
        if let Err(e) = em.enable_dma_trace(dma_trace_max) {
            eprintln!("error: enable_dma_trace: {e}");
            return ExitCode::from(1);
        }
    }
    // When --audio-out is set, step in chunks and drain the APU's
    // ~16k-sample bounded queue after each chunk; otherwise we'd lose
    // ~99% of audio on any run longer than ~0.5 s of emulated time.
    // The accumulated Vec is written to disk after the run.
    let mut audio_accum: Vec<(i16, i16)> = Vec::new();
    if audio_out.is_some() {
        // Chunk = 100k instructions ≈ ~10 ms of emulated SPC time
        // (well under the 512 ms queue capacity even at peak DSP
        // output rate). Drain the full queue after each chunk.
        const AUDIO_CHUNK: u64 = 100_000;
        let mut left = remaining;
        while left > 0 {
            let take = left.min(AUDIO_CHUNK);
            if let Err(e) = em.step(take) {
                eprintln!("step warning: {e}");
                break;
            }
            left -= take;
            match em.drain_audio(usize::MAX) {
                Ok(mut chunk) => audio_accum.append(&mut chunk),
                Err(e) => eprintln!("warning: drain_audio mid-run: {e}"),
            }
        }
    } else {
        match em.step(remaining) {
            Ok(_) => {}
            Err(e) => {
                // Step errors are informational — we still want a state
                // snapshot.
                eprintln!("step warning: {e}");
            }
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

    if let Some(path) = apu_log_path {
        match em.take_mailbox_log() {
            Ok(events) => match write_mailbox_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "APU mailbox log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing APU log: {e}"),
            },
            Err(e) => eprintln!("error: take_mailbox_log: {e}"),
        }
    }
    if let Some(path) = sa1_log_path {
        match em.take_sa1_log() {
            Ok(events) => match write_sa1_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1 MMIO log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1 log: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_log: {e}"),
        }
    }
    if let Some(path) = sa1_side_log_path {
        match em.take_sa1_side_log() {
            Ok(events) => match write_sa1_side_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1-side log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1-side log: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_side_log: {e}"),
        }
    }
    if let Some(path) = sa1_trace_path {
        match em.take_sa1_trace() {
            Ok(events) => match write_sa1_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1 instruction trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1 trace: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_trace: {e}"),
        }
    }
    if let Some(path) = superfx_trace_path {
        match em.take_superfx_trace() {
            Ok(events) => match write_superfx_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "Super FX instruction trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing Super FX trace: {e}"),
            },
            Err(e) => eprintln!("error: take_superfx_trace: {e}"),
        }
    }
    if let Some(path) = dump_vram_path {
        match em.vram_bytes() {
            Ok(bytes) => match std::fs::write(path, &bytes) {
                Ok(()) => eprintln!("VRAM ({} bytes) written to {}", bytes.len(), path.display()),
                Err(e) => eprintln!("error: writing VRAM dump: {e}"),
            },
            Err(e) => eprintln!("error: vram_bytes: {e}"),
        }
    }
    if let Some(path) = dump_coproc_ram_path {
        match em.coproc_ram() {
            Ok(Some(bytes)) => match std::fs::write(path, &bytes) {
                Ok(()) => eprintln!(
                    "coproc work RAM ({} bytes) written to {}",
                    bytes.len(),
                    path.display()
                ),
                Err(e) => eprintln!("error: writing coproc RAM dump: {e}"),
            },
            Ok(None) => eprintln!("note: cart has no coprocessor work RAM"),
            Err(e) => eprintln!("error: coproc_ram: {e}"),
        }
    }
    if let Some(path) = cpu_trace_path {
        match em.take_cpu_trace_log() {
            Ok(events) => match write_cpu_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "CPU trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing CPU trace: {e}"),
            },
            Err(e) => eprintln!("error: take_cpu_trace_log: {e}"),
        }
    }
    if let Some(path) = mem_trace_path {
        match em.take_mem_trace_log() {
            Ok(events) => match write_mem_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "Memory trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing memory trace: {e}"),
            },
            Err(e) => eprintln!("error: take_mem_trace_log: {e}"),
        }
    }
    if let Some(path) = dma_trace_path {
        match em.take_dma_trace() {
            Ok(events) => match write_dma_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "DMA→VRAM trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing DMA trace: {e}"),
            },
            Err(e) => eprintln!("error: take_dma_trace: {e}"),
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
        // Drain anything still queued at end-of-run, then write the
        // full accumulated stream.
        match em.drain_audio(usize::MAX) {
            Ok(mut tail) => audio_accum.append(&mut tail),
            Err(e) => eprintln!("warning: final drain_audio: {e}"),
        }
        if let Err(e) = write_wav(p, &audio_accum) {
            eprintln!("error: writing WAV: {e}");
            return ExitCode::from(1);
        }
        let secs = audio_accum.len() as f64 / 32_000.0;
        eprintln!(
            "Audio WAV written to {}  ({} samples = {:.2}s @ 32 kHz stereo)",
            p.display(),
            audio_accum.len(),
            secs
        );
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
    let mut em = luna_api::Emulator::new();
    let info = match em.load_rom(rom_path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    print_header(&info);

    // `load_rom` already runs the reset vector; report where it landed.
    {
        let cpu = em.state().cpu;
        println!("After reset: PC=${:02X}:{:04X}", cpu.pb, cpu.pc);
    }
    println!();

    let mut panic_msg: Option<String> = None;
    let mut stopped = false;
    // If we're recording audio, drain the APU queue every batch of
    // steps so it doesn't overflow (capped at 16 384 samples).
    let mut audio_samples: Vec<(i16, i16)> = if audio_out.is_some() {
        Vec::with_capacity(1 << 20)
    } else {
        Vec::new()
    };
    if audio_out.is_some() {
        // Step in 4 096-instruction batches, draining the APU's bounded
        // queue after each so it never saturates over a long run. A
        // short batch (`ran < want`) means the CPU hit STP.
        const BATCH: u64 = 4_096;
        loop {
            let done = em.instructions_executed();
            if done >= steps {
                break;
            }
            let want = (steps - done).min(BATCH);
            match em.step(want) {
                Ok(ran) => {
                    match em.drain_audio(8_192) {
                        Ok(mut chunk) => audio_samples.append(&mut chunk),
                        Err(e) => eprintln!("warning: drain_audio mid-run: {e}"),
                    }
                    if ran < want {
                        stopped = true;
                        break;
                    }
                }
                Err(luna_api::ApiError::Panic(msg)) => {
                    panic_msg = Some(msg);
                    break;
                }
                Err(e) => {
                    eprintln!("error: step: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        // Final drain.
        match em.drain_audio(usize::MAX) {
            Ok(mut chunk) => audio_samples.append(&mut chunk),
            Err(e) => eprintln!("warning: drain_audio final: {e}"),
        }
    } else {
        match em.step(steps) {
            Ok(ran) => stopped = ran < steps,
            Err(luna_api::ApiError::Panic(msg)) => panic_msg = Some(msg),
            Err(e) => {
                eprintln!("error: step: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if stopped {
        println!(
            "CPU halted by STP after {} instructions.",
            em.instructions_executed()
        );
    }

    println!("--- final state ---");
    print_cpu_state(&em.state().cpu);
    print_diag_state(&mut em);
    println!("Instructions executed: {}", em.instructions_executed());
    println!("Total master cycles:   {}", em.state().stats.total_mclk);
    if let Some(msg) = panic_msg {
        println!();
        println!("Stopped on CPU panic:");
        println!("  {msg}");
        // Returning success here: hitting an unimplemented opcode is the
        // expected state of P0.6, not a CLI failure.
    }

    // Screenshot dump: render whatever the PPU has accumulated.
    if let Some(out_path) = screenshot {
        match save_screenshot(&em, out_path, force_display, bg) {
            Ok(()) => println!("\nScreenshot written to {}", out_path.display()),
            Err(e) => {
                eprintln!("\nerror: could not write screenshot: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(out_path) = audio_out {
        match write_wav(out_path, &audio_samples) {
            Ok(()) => println!(
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

fn print_header(info: &luna_api::RomInfo) {
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

fn print_cpu_state(cpu: &luna_api::CpuState) {
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

fn print_diag_state(em: &mut luna_api::Emulator) {
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
