//! Luna SNES emulator — command-line entry point.
//!
//! Dispatches between execution modes (run / mcp / replay).
//! See `ARCHITECTURE.md` §3.2.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod bench;
mod csv;
mod dumps;
mod fmt;
mod frames;
mod output;
mod parsers;
mod rom;
mod run;
mod state;
mod wram_trace;

use dumps::{run_assets_dump, run_spc_dump};
use frames::run_frames;
use run::run;
use state::run_state;
use wram_trace::run_wram_trace;

use parsers::parse_input_script;

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
    /// Unimplemented opcodes panic and are caught — partial state is
    /// still dumped.
    Run {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// Maximum number of CPU instructions to execute before dumping.
        #[arg(short = 'n', long, default_value_t = 64)]
        steps: u64,
        /// If set, render a 256×224 PNG of the composited framebuffer
        /// and write it to the given path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Bypass INIDISP forced-blank when rendering. Lets you see
        /// whatever the game has uploaded to VRAM/CGRAM even if its
        /// init left the screen blanked (e.g. a title still waiting on
        /// a Start press).
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
        /// If set, capture the program's `$21FC` Nocash-TTY output (the SDK's
        /// `SNES_NOCASH` / `SNES_ASSERT` debug channel) and write the raw byte
        /// stream to this file. Lets a headless harness read the ROM's own
        /// log / assertion output with no GUI debugger.
        #[arg(long)]
        nocash_out: Option<PathBuf>,
        /// If set, write captured `WDM $xx` executions (the SDK breakpoint /
        /// `SNES_ASSERT` channel — `WDM $00`) to this file, one
        /// `PC=$xxxxxx operand=$xx` line per hit. A non-empty file means an
        /// assertion or breakpoint fired during the run.
        #[arg(long)]
        wdm_out: Option<PathBuf>,
        /// If set, print a hash of the displayed frame (honouring
        /// `--force-display`) to stdout as `fbhash=<16-hex>`. A stable,
        /// cross-architecture visual-regression key — it hashes the same
        /// pixels `--screenshot` writes, before PNG encoding (so it is immune
        /// to the build-dependent PNG encoder). Ideal as an external test
        /// harness's baseline key. See `docs/trace_determinism.md` for which
        /// outputs (fbhash / trace counts / WRAM bytes) are cross-arch-stable
        /// and how strongly each may be asserted.
        #[arg(long)]
        print_fbhash: bool,
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
    /// snapshot — the same data the MCP `state` tool returns.
    ///
    /// This is the dogfood path: the API surface that the CLI, GUI and
    /// MCP server all share. Use it to test the API directly without
    /// going through any transport.
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
        /// Load a WLA-DX `.sym` symbol file (overrides the automatic
        /// `<rom>.sym` detection). Symbol names then annotate the CPU
        /// disassembly and resolve in the debug tooling.
        #[arg(long = "sym")]
        sym: Option<PathBuf>,
        /// Load a save state (a `.luna` blob from the GUI's save-state
        /// slots, `~/.local/luna/states/<rom-slug>.slot<N>.luna`) right
        /// after the ROM loads, before the `-n` warm-up. Lets a headless
        /// run resume from a GUI-captured scene (e.g. to inspect a bug the
        /// CLI can't reach without input). The state must match this ROM.
        #[arg(long = "load-state")]
        load_state: Option<PathBuf>,
        /// Dump all 64 KB of PPU VRAM (raw bytes) to this file after the
        /// run. For diagnosing the framebuffer DMA → VRAM → display path.
        #[arg(long = "dump-vram")]
        dump_vram: Option<PathBuf>,
        /// Dump the coprocessor work RAM (Super FX Game Pak RAM) raw bytes
        /// to this file, bypassing GSU ownership gating. For comparing
        /// luna's CPU-prepared GSU inputs against a reference.
        #[arg(long = "dump-coproc-ram")]
        dump_coproc_ram: Option<PathBuf>,
        /// Dump all 64 KB of APU audio RAM (ARAM) raw bytes to this file.
        /// For diagnosing the SPC700 sound driver / CPU↔SPC handshake.
        #[arg(long = "dump-aram")]
        dump_aram: Option<PathBuf>,
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
        /// Controller port-1 device: `pad` (default), `mouse`, or `superscope`.
        #[arg(long, default_value = "pad")]
        port1: String,
        /// Controller port-2 device: `pad` (default), `mouse`, or `superscope`.
        #[arg(long, default_value = "pad")]
        port2: String,
        /// Scripted SNES Mouse motion, applied to whichever port is set to
        /// `mouse`. `frame:dx,dy,buttons` entries separated by `;` (signed
        /// dx/dy; buttons bit0=left, bit1=right). Example: `--mouse "60:5,-3,1"`.
        #[arg(long)]
        mouse: Option<String>,
        /// Scripted Super Scope aim, applied to whichever port is set to
        /// `superscope`. `frame:x,y,buttons` entries separated by `;` (absolute
        /// screen pixels; buttons bit0=trigger, bit1=cursor, bit2=turbo,
        /// bit3=pause). Example: `--superscope "120:128,112,1"`.
        #[arg(long)]
        superscope: Option<String>,
        /// Optional memory peek(s) after snapshot.  Format:
        /// `BANK:OFFSET:COUNT` (all hex, no `0x` prefix).  Can be
        /// specified multiple times.  Output goes to stderr as a
        /// labelled hex dump.  Example: `--peek 7E:0200:220` reads
        /// 544 bytes of SMW shadow-OAM.
        #[arg(long = "peek")]
        peek: Vec<String>,
        /// Assert memory equals expected bytes after warm-up. Format:
        /// `BANK:OFFSET=HEX` (all hex). Prints `PASS`/`FAIL` per spec and
        /// exits non-zero if any fails. Repeatable. Example:
        /// `--assert 7E:0010=AB12`.
        #[arg(long = "assert")]
        assert: Vec<String>,
        /// Assert APU-RAM equals expected bytes (`OFFSET=HEX`). Like
        /// `--assert` but over ARAM. Repeatable.
        #[arg(long = "assert-aram")]
        assert_aram: Vec<String>,
        /// Assert VRAM equals expected bytes (`OFFSET=HEX`). Like
        /// `--assert` but over VRAM. Repeatable.
        #[arg(long = "assert-vram")]
        assert_vram: Vec<String>,
        /// Assert CGRAM (palette) equals expected bytes (`OFFSET=HEX`, byte
        /// offset into the 512-byte CGRAM, low byte of each colour first).
        /// Like `--assert` but over CGRAM. Repeatable.
        #[arg(long = "assert-cgram")]
        assert_cgram: Vec<String>,
        /// Run until PPU frame N (then snapshot), instead of stopping at the
        /// `-n` instruction count. Makes input→assert probes land on an exact
        /// frame rather than a generous `-n`.
        #[arg(long = "until-frame")]
        until_frame: Option<u64>,
        /// Load battery SRAM from a `.srm` file before running (the other
        /// half of a power-cycle test — write it with `--srm-out` in run A,
        /// read it back in run B).
        #[arg(long = "srm-in")]
        srm_in: Option<PathBuf>,
        /// Write battery SRAM to a `.srm` file after the run.
        #[arg(long = "srm-out")]
        srm_out: Option<PathBuf>,
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
        /// Optional FULL SPC700 instruction trace: a per-opcode register
        /// snapshot written as CSV (`seq,pc,a,x,y,sp,psw`). Diff this PC
        /// stream against a Mesen2 SPC700 trace to localise audio-driver
        /// (Akao CPU↔SPC handshake) divergences (e.g. SMRPG/CT).
        #[arg(long = "spc-trace")]
        spc_trace: Option<PathBuf>,
        /// Cap the SPC700 instruction trace at this many events (default
        /// 200 000).
        #[arg(long = "spc-trace-max", default_value_t = 200_000)]
        spc_trace_max: usize,
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
        /// CPU bus read/write into a CSV at PATH, columns
        /// `mclk_total,frame_ntsc,pc,addr,kind,value,line,blank,force_blank`
        /// (`blank` = V-blank, `force_blank` = INIDISP `$2100` bit 7 at the
        /// access; a VRAM write is safe iff `blank||force_blank`). Default: all
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
        /// Hex offset range `LO:HI` (inclusive) to filter the memory
        /// trace on, e.g. `2100:21FF` to capture only PPU/CPU MMIO
        /// across all banks without the bank filter's code-fetch flood.
        /// Composes with `--mem-trace-bank` (both must match).
        #[arg(long = "mem-trace-addr")]
        mem_trace_addr: Option<String>,
        /// Optional DMA→VRAM transfer-time trace. Captures every byte an
        /// MDMA writes to `$2118/$2119` as CSV
        /// (`seq,frame,line,blank,force_blank,src,vram_word,reg,value`) —
        /// `blank` = V-blank period, `force_blank` = INIDISP (`$2100`) bit 7
        /// at the write (a write is safe iff `blank||force_blank`), `src` the
        /// 24-bit A-bus source, `vram_word` the VMADD word the byte lands at,
        /// `reg` $18/$19. The byte is captured AS READ during the transfer, so
        /// a coprocessor (Super FX) overwriting its source buffer afterwards
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
    /// Run a ROM until its music driver is playing, then export the APU
    /// state as a `.spc` sound file (SNES-SPC700 Sound File Data v0.30):
    /// SPC700 registers + 64 KB ARAM + DSP registers + IPL ROM, playable
    /// in any SPC player. Step far enough in (and pulse Start via
    /// `--input`) that the music has started before the snapshot.
    SpcDump {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// CPU instructions to execute before the snapshot.
        #[arg(short = 'n', long, default_value_t = 5_000_000)]
        steps: u64,
        /// Output path for the `.spc`. Defaults to the ROM's name with a
        /// `.spc` extension, in the current directory.
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Force a cartridge mapper (lorom, hirom, exhirom, sa1, superfx).
        #[arg(long = "force-mapper")]
        force_mapper: Option<String>,
        /// Install a DSP coprocessor firmware (`dsp1b.rom`) then load —
        /// needed for DSP-1 games (Super Mario Kart, Pilotwings).
        #[arg(long = "dsp1-rom")]
        dsp1_rom: Option<PathBuf>,
        /// Scripted joypad-1 input, same `frame:hex` format as
        /// `state --input`, applied before the snapshot so playback can
        /// start past a title screen.
        #[arg(long)]
        input: Option<String>,
    },
    /// Run a ROM to a scene, then dump every graphics asset currently
    /// loaded as PNGs: the VRAM tile sheet, the four BG tilemaps, the
    /// CGRAM palette, the OAM sprite sheet, and the composited screen —
    /// plus raw `vram.bin` / `cgram.bin` and `oam.json`. This captures
    /// the assets *loaded at this instant* (already decompressed by the
    /// game); snapshot several scenes (via `--input` / different `-n`) to
    /// cover a whole game. A static whole-ROM rip is NOT possible — SNES
    /// graphics are game-specific-compressed with no standard layout.
    AssetsDump {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// CPU instructions to execute before the snapshot.
        #[arg(short = 'n', long, default_value_t = 5_000_000)]
        steps: u64,
        /// Output directory (created if absent).
        #[arg(long = "out", default_value = "/tmp/luna_assets")]
        out: PathBuf,
        /// VRAM tile-sheet bits-per-pixel (2/4/8). Default: auto from the
        /// current BG1 mode.
        #[arg(long)]
        bpp: Option<u8>,
        /// CGRAM sub-palette row for the VRAM tile sheet (2bpp/4bpp).
        #[arg(long, default_value_t = 0)]
        palette: u8,
        /// Force a cartridge mapper (lorom, hirom, exhirom, sa1, superfx).
        #[arg(long = "force-mapper")]
        force_mapper: Option<String>,
        /// Install a DSP coprocessor firmware (`dsp1b.rom`) then load.
        #[arg(long = "dsp1-rom")]
        dsp1_rom: Option<PathBuf>,
        /// Scripted joypad-1 input, same `frame:hex` format as
        /// `state --input`, applied before the snapshot.
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
            nocash_out,
            wdm_out,
            print_fbhash,
        } => run(
            &rom,
            steps,
            screenshot.as_deref(),
            force_display,
            bg,
            audio_out.as_deref(),
            nocash_out.as_deref(),
            wdm_out.as_deref(),
            print_fbhash,
        ),
        Command::Mcp => serve_mcp(),
        Command::State {
            rom,
            steps,
            force_mapper,
            dsp1_rom,
            sym,
            load_state,
            dump_vram,
            out,
            screenshot,
            audio_out,
            input,
            port1,
            port2,
            mouse,
            superscope,
            peek,
            assert,
            assert_aram,
            assert_vram,
            assert_cgram,
            until_frame,
            srm_in,
            srm_out,
            apu_log,
            sa1_log,
            sa1_side_log,
            sa1_trace,
            sa1_trace_max,
            superfx_trace,
            superfx_trace_max,
            spc_trace,
            spc_trace_max,
            cpu_trace,
            cpu_trace_from,
            cpu_trace_max,
            mem_trace,
            mem_trace_from,
            mem_trace_max,
            mem_trace_bank,
            mem_trace_addr,
            dma_trace,
            dma_trace_from,
            dma_trace_max,
            dump_coproc_ram,
            dump_aram,
        } => run_state(
            &rom,
            steps,
            force_mapper.as_deref(),
            dump_vram.as_deref(),
            dump_coproc_ram.as_deref(),
            dump_aram.as_deref(),
            &out,
            screenshot.as_deref(),
            audio_out.as_deref(),
            input.as_deref(),
            &port1,
            &port2,
            mouse.as_deref(),
            superscope.as_deref(),
            &peek,
            &assert,
            &assert_aram,
            &assert_vram,
            &assert_cgram,
            until_frame,
            srm_in.as_deref(),
            srm_out.as_deref(),
            apu_log.as_deref(),
            sa1_log.as_deref(),
            sa1_side_log.as_deref(),
            sa1_trace.as_deref(),
            sa1_trace_max,
            superfx_trace.as_deref(),
            superfx_trace_max,
            spc_trace.as_deref(),
            spc_trace_max,
            cpu_trace.as_deref(),
            cpu_trace_from,
            cpu_trace_max,
            mem_trace.as_deref(),
            mem_trace_from,
            mem_trace_max,
            mem_trace_bank.as_deref(),
            mem_trace_addr.as_deref(),
            dma_trace.as_deref(),
            dma_trace_from,
            dma_trace_max,
            dsp1_rom.as_deref(),
            sym.as_deref(),
            load_state.as_deref(),
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
        Command::SpcDump {
            rom,
            steps,
            out,
            force_mapper,
            dsp1_rom,
            input,
        } => run_spc_dump(
            &rom,
            steps,
            out.as_deref(),
            force_mapper.as_deref(),
            dsp1_rom.as_deref(),
            input.as_deref(),
        ),
        Command::AssetsDump {
            rom,
            steps,
            out,
            bpp,
            palette,
            force_mapper,
            dsp1_rom,
            input,
        } => run_assets_dump(
            &rom,
            steps,
            &out,
            bpp,
            palette,
            force_mapper.as_deref(),
            dsp1_rom.as_deref(),
            input.as_deref(),
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
