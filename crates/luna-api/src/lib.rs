//! Luna's public, transport-agnostic API.
//!
//! Every feature the emulator exposes — load a ROM, step the CPU,
//! peek memory, render a frame, drain audio — lives here as a method
//! on [`Emulator`]. The CLI, GUI, MCP server and any future
//! transport (HTTP, WebSocket, FFI) all call through this surface so
//! the emulator stays drivable from outside the binary that built it.
//!
//! ## Stability
//!
//! From V1 onward this crate carries strict `SemVer` guarantees: new
//! methods are additive, breaking changes bump the major version.
//! Today (P3.3) we're still pre-V1 and the surface is allowed to
//! churn freely.

use std::path::Path;

pub use luna_cartridge::Region;
use luna_cartridge::{CartError, Cartridge};
use luna_core::Snes;
/// Controller-port device kind (pad / mouse / super scope), re-exported so the
/// GUI's port-device selector goes through `luna-api`, not `luna-core`.
pub use luna_core::controller::PortDevice;
pub use luna_core::{
    BreakHit, BreakKind, BreakpointInfo, CpuTraceEvent, CpuTraceLog, DmaTraceEvent, DmaTraceLog,
    MailboxEvent, MailboxEventKind, MapperKind, MemEventKind, MemTraceEvent, MemTraceLog,
    Sa1LogEvent, Sa1SideEvent, Sa1TraceEvent, Spc700TraceEvent, SuperFxTraceEvent,
};
/// Decoded BG tilemap image (Tilemap Viewer), re-exported so the GUI uses
/// `luna_api::TilemapImage` rather than depending on `luna-ppu`.
pub use luna_ppu::TilemapImage;
/// Framebuffer dimensions (256×224), re-exported so front-ends size their
/// texture/window through `luna-api` rather than depending on `luna-ppu`.
pub use luna_ppu::{FRAME_H, FRAME_W};
use serde::Serialize;

/// Faithful Mesen2 SNES Event Viewer data layer (categories, colors, register
/// names, config). The GUI Event Viewer panel consumes this via the accessors
/// on [`Emulator`].
pub mod event_viewer;
pub mod symbols;
pub use event_viewer::{
    CATEGORY_COUNT, EventCategory, EventViewerConfig, EventViewerEvent, categorise, register_name,
};
pub use symbols::SymbolTable;
use thiserror::Error;

/// Errors surfaced from [`Emulator`] methods.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Caller asked for a ROM-dependent operation before
    /// [`Emulator::load_rom`] succeeded.
    #[error("no ROM loaded — call load_rom first")]
    NoRom,
    /// Cartridge parsing or layout detection failed.
    #[error("cartridge: {0}")]
    Cart(#[from] CartError),
    /// Cartridge needs a coprocessor luna does not yet emulate
    /// (S-DD1, SPC7110) — reachable via a forced mapper.
    #[error("{0}")]
    UnsupportedMapper(#[from] luna_core::UnsupportedMapper),
    /// `step` / etc. panicked inside the core.
    #[error("emulator panicked: {0}")]
    Panic(String),
    /// A debug call got an invalid argument (e.g. an unknown register name).
    #[error("{0}")]
    BadArg(String),
    /// Save-state encode/decode failed, or a loaded state did not match
    /// the running ROM / save-state format version.
    #[error("save state: {0}")]
    SaveState(String),
    /// PNG encoding failed during `render_frame`.
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    /// Generic I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Save-state format version. Bump on any breaking change to the
/// serialized layout so [`Emulator::load_state`] rejects stale blobs.
/// v2: dropped the always-`None` `Spc700::unimplemented_opcode` field
/// (the SPC700 dispatch is exhaustive), changing the bincode layout.
/// v4: `Ppu::open_bus` split into `ppu1_mdr` + `ppu2_mdr` (faithful
/// per-chip open-bus latches, ares `ppu1.mdr`/`ppu2.mdr`).
///
/// NOTE (2026-07): the container is encoded with bincode 1.x (EOL branch).
/// Evaluate migrating to bincode 2 at the NEXT version bump — a bump
/// already invalidates old blobs, so that is the free moment to switch.
pub const SAVE_STATE_VERSION: u32 = 4;

/// On-disk / on-wire save-state container produced by
/// [`Emulator::save_state`]. `core` is the bincode-encoded `Snes` (the
/// mapper trait object is skipped by serde); `mapper` is the live mapper's
/// mutable-state blob from [`luna_core::Mapper::save_state`].
#[derive(serde::Serialize, serde::Deserialize)]
struct SaveStateBundle {
    version: u32,
    rom_hash: u64,
    core: Vec<u8>,
    mapper: Vec<u8>,
}

/// Cartridge metadata returned by [`Emulator::load_rom`].
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RomInfo {
    /// Title field from the internal SNES header.
    pub title: String,
    /// Detected mapper kind, e.g. `"LoRom"`, `"HiRom"`, `"ExHiRom"`.
    pub mapper: String,
    /// Cartridge ROM size in bytes (on disk).
    pub rom_bytes: usize,
    /// ROM size in KB as declared in the internal header (distinct from
    /// the on-disk byte count `rom_bytes`).
    pub header_rom_size_kb: u32,
    /// Battery-backed SRAM size in kilobytes.
    pub sram_kb: u32,
    /// `Ntsc`, `Pal`, or `Unknown`.
    pub region: String,
    /// `FastROM` (`MEMSEL`) eligibility from the header.
    pub fast_rom: bool,
    /// Mask-ROM version byte from the header.
    pub version: u8,
    /// 16-bit header checksum.
    pub checksum: u16,
    /// 16-bit header checksum complement.
    pub checksum_complement: u16,
    /// Whether `checksum` and `checksum_complement` are bitwise complements.
    pub checksum_valid: bool,
    /// When the cart needs an external coprocessor firmware that wasn't
    /// found (e.g. a DSP-1 game with no `dsp1b.rom`), the required
    /// filename — so a front-end can prompt for / install it. `None` when
    /// no firmware is needed or it was resolved. The game still loads
    /// (the coprocessor stays inert) so it can be inspected meanwhile.
    pub missing_firmware: Option<String>,
}

/// Snapshot of the emulator's observable state. Every field maps to
/// something the GUI / debugger / tests want to inspect.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct EmulatorState {
    /// `null` until a ROM is loaded.
    pub rom: Option<RomInfo>,
    /// CPU 65C816 registers.
    pub cpu: CpuState,
    /// PPU registers + occupancy.
    pub ppu: PpuState,
    /// CPU-system registers ($4200-$421F).
    pub cpu_regs: CpuRegsState,
    /// Scanline scheduler.
    pub scheduler: SchedulerState,
    /// APU / SPC700 / DSP.
    pub apu: ApuState,
    /// DMA / HDMA controller registers ($420B-$420C, $43xx).
    pub dma: DmaState,
    /// Cumulative metrics.
    pub stats: Stats,
    /// SA-1 coprocessor CPU state, if the loaded cartridge hosts one.
    /// `None` for non-SA-1 carts. Diagnostic for main↔SA-1 mailbox
    /// debugging — lets you see at a glance whether the SA-1 PC is
    /// stuck in a polling loop, running random ROM bytes, or halted.
    pub sa1: Option<Sa1State>,
    /// DSP-1 (NEC uPD7725) state, if the loaded cartridge hosts one.
    /// `None` for non-DSP carts.
    pub dsp1: Option<Dsp1State>,
}

/// DSP-1 (NEC uPD7725) coprocessor snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Dsp1State {
    /// Program counter.
    pub pc: u16,
    /// Status register (`SR`).
    pub sr: u16,
    /// Accumulator A.
    pub a: i16,
    /// Accumulator B.
    pub b: i16,
    /// Data register (`DR`, the CPU port).
    pub dr: u16,
    /// `RQM` — set when the chip is awaiting the master.
    pub rqm: bool,
}

/// SA-1 coprocessor CPU snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Sa1State {
    /// Program counter within the program bank.
    pub pc: u16,
    /// Program bank.
    pub pb: u8,
    /// Processor status flags (N V M X D I Z C).
    pub p: u8,
    /// `true` while the SA-1 is released from reset (CCNT.5 clear).
    pub running: bool,
}

/// 65C816 register snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CpuState {
    /// Accumulator (16-bit, low byte active in 8-bit M mode).
    pub a: u16,
    /// X index register.
    pub x: u16,
    /// Y index register.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Program counter (within the program bank).
    pub pc: u16,
    /// Program bank.
    pub pb: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page register.
    pub dp: u16,
    /// Processor status flags (N V M X D I Z C).
    pub p: u8,
    /// Emulation mode flag.
    pub e: bool,
    /// `true` after STP.
    pub stopped: bool,
    /// `true` after WAI, until an interrupt arrives.
    pub waiting: bool,
}

/// SPC700 (audio CPU) register snapshot — the APU-core analogue of
/// [`CpuState`], for a debugger panel.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Spc700State {
    /// Accumulator.
    pub a: u8,
    /// X index register.
    pub x: u8,
    /// Y index register.
    pub y: u8,
    /// Stack pointer low byte (stack lives at `$0100 + sp`).
    pub sp: u8,
    /// Program counter.
    pub pc: u16,
    /// Program status word (N V P B H I Z C).
    pub psw: u8,
    /// `true` after `STOP` or an unimplemented opcode.
    pub stopped: bool,
    /// `true` after `SLEEP`, until an interrupt wakes the core.
    pub sleeping: bool,
}

/// Outcome of [`Emulator::run_until_break`]: either the step budget ran
/// out (`hit == None`) or a breakpoint/watchpoint fired.
#[derive(Debug, Clone, Serialize)]
pub struct RunOutcome {
    /// Instructions actually executed this call.
    pub steps: u64,
    /// The breakpoint hit that stopped the run, if any.
    pub hit: Option<BreakHit>,
    /// `true` if an external interrupt (a `pause`) ended the run before a
    /// breakpoint or the step budget (issue #92). Distinguishes a paused run
    /// (`hit == None && interrupted`) from a budget-exhausted one.
    pub interrupted: bool,
}

/// One recorded joypad transition captured during play (issue #83).
///
/// A capture logs only *changes* to the per-port button mask, keyed by the
/// `frame_count` at which the change was applied — the same `frame:mask`
/// checkpoint semantics the `--input` / `set_joypad` replay path consumes
/// (a mask holds until the next entry), so a capture round-trips through
/// [`input_capture_to_script`] straight back into a deterministic replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct InputCaptureEntry {
    /// `frame_count` at which this mask took effect.
    pub frame: u64,
    /// Controller port: `0` = player 1, `1` = player 2.
    pub port: u8,
    /// JOY1L/JOY1H button bitmask now in effect (see [`Emulator::set_joypad`]).
    pub mask: u16,
}

/// Render a capture (from [`Emulator::take_input_capture`]) as a `--input`
/// script string for one port: comma-separated `frame:0xMASK` checkpoints,
/// ready to feed back to `luna state --input` or the MCP replay path.
#[must_use]
pub fn input_capture_to_script(entries: &[InputCaptureEntry], port: u8) -> String {
    entries
        .iter()
        .filter(|e| e.port == port)
        .map(|e| format!("{}:0x{:04X}", e.frame, e.mask))
        .collect::<Vec<_>>()
        .join(",")
}

/// One disassembled instruction line, for a disassembly debug panel.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct DisasmLine {
    /// Address of the instruction (24-bit for the CPU bus; ≤`0xFFFF` for ARAM).
    pub addr: u32,
    /// The raw instruction bytes (1..=3).
    pub bytes: Vec<u8>,
    /// Canonical mnemonic + operands, e.g. `"MOV A, #$12"`.
    pub text: String,
    /// `true` if this line is the live program counter.
    pub is_pc: bool,
    /// Nearest symbol at or below `addr` (`name` or `name+0xNN`) when a
    /// WLA-DX `.sym` table is loaded; `None` otherwise.
    pub symbol: Option<String>,
}

/// PPU register snapshot + memory occupancy stats.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PpuState {
    /// `$2100` — bit 7 forced-blank, bits 0-3 brightness.
    pub inidisp: u8,
    /// `$2105` — bits 0-2 BG mode, bit 3 BG3 priority, bits 4-7 tile sizes.
    pub bgmode: u8,
    /// Current VRAM word address.
    pub vram_addr_words: u16,
    /// Count of writes to INIDISP since reset.
    pub inidisp_write_count: u64,
    /// CGRAM colour 0 (backdrop).
    pub backdrop: u16,
    /// `$2101` — sprite size + base.
    pub obsel: u8,
    /// How many of VRAM's 65 536 bytes are non-zero.
    pub vram_non_zero: usize,
    /// How many of CGRAM's 256 colours are non-zero.
    pub cgram_non_zero: usize,
    /// How many of OAM's 544 bytes are non-zero.
    pub oam_non_zero: usize,
    /// First 16 sprites' OAM low-table entries (64 bytes total, 4
    /// bytes per sprite: X.lo, Y, Tile.lo, Attrs). Lets debuggers
    /// see at a glance whether sprite 0 (e.g. Mario in SMW) has
    /// been written without dumping the full 544 bytes.
    pub oam_low_excerpt: Vec<u8>,
    /// High-table excerpt (first 4 bytes = sprites 0..15 size +
    /// X-bit-8). Together with `oam_low_excerpt` reconstructs the
    /// full state for the first 16 sprites.
    pub oam_high_excerpt: Vec<u8>,
    /// `$212C` TM — main-screen layer enable (bits 0..4 = BG1..BG4, OBJ).
    pub tm: u8,
    /// `$212D` TS — sub-screen layer enable.
    pub ts: u8,
    /// `$212E` TMW — main-screen window-clip mask.
    pub tmw: u8,
    /// `$212F` TSW — sub-screen window-clip mask.
    pub tsw: u8,
    /// `$2130` CGWSEL.
    pub cgwsel: u8,
    /// `$2131` CGADSUB.
    pub cgadsub: u8,
    /// `$2132` COLDATA red channel.
    pub coldata_r: u8,
    /// `$2132` COLDATA green channel.
    pub coldata_g: u8,
    /// `$2132` COLDATA blue channel.
    pub coldata_b: u8,
    /// `$2133` SETINI.
    pub setini: u8,
    /// `$2123` W12SEL.
    pub w12sel: u8,
    /// `$2124` W34SEL.
    pub w34sel: u8,
    /// `$2125` WOBJSEL.
    pub wobjsel: u8,
    /// `$212A` WBGLOG.
    pub wbglog: u8,
    /// `$212B` WOBJLOG.
    pub wobjlog: u8,
    /// `$2126..$2129` window left/right edges (WH0..WH3).
    pub windows: [u8; 4],
    /// Per-BG state: tilemap base, char base, scrolls, tilemap size.
    pub bgs: [BgInfo; 4],
    /// CGRAM dump — 256 BGR555 colours.
    pub cgram: Vec<u16>,
    /// Full OAM dump — 544 bytes (512 low table + 32 high table).
    pub oam_full: Vec<u8>,
    /// `$2106` MOSAIC.
    pub mosaic: u8,
    /// `$211A` M7SEL.
    pub m7sel: u8,
    /// `$211B` M7A — Mode-7 matrix A.
    pub m7a: i16,
    /// `$211C` M7B — Mode-7 matrix B.
    pub m7b: i16,
    /// `$211D` M7C — Mode-7 matrix C.
    pub m7c: i16,
    /// `$211E` M7D — Mode-7 matrix D.
    pub m7d: i16,
    /// `$211F` M7X — Mode-7 centre X.
    pub m7x: i16,
    /// `$2120` M7Y — Mode-7 centre Y.
    pub m7y: i16,
    /// `$210D` M7HOFS — Mode-7 horizontal scroll.
    pub m7_hofs: i16,
    /// `$210E` M7VOFS — Mode-7 vertical scroll.
    pub m7_vofs: i16,
    /// `$2134-$2136` MPYL/MPYM/MPYH 24-bit hardware product.
    pub mpy: i32,
    /// `$213E` STAT77 (PPU1 status).
    pub stat77: u8,
    /// `$213F` STAT78 (PPU2 status).
    pub stat78: u8,
    /// `$213C` OPHCT — latched horizontal counter.
    pub ophct: u16,
    /// `$213D` OPVCT — latched vertical counter.
    pub opvct: u16,
}

/// Per-BG serialisable view.
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct BgInfo {
    /// VRAM word address of the tilemap base ($2107..$210A).
    pub tilemap_addr_words: u16,
    /// VRAM word address of the character (tile) base.
    pub char_addr_words: u16,
    /// 10-bit horizontal scroll.
    pub h_scroll: u16,
    /// 10-bit vertical scroll.
    pub v_scroll: u16,
    /// BG*SC tilemap-size bits (0=32×32, 1=64×32, 2=32×64, 3=64×64).
    pub tilemap_size: u8,
}

/// CPU-system register snapshot (`$4200-$421F`).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CpuRegsState {
    /// `$4200`.
    pub nmitimen: u8,
    /// `$4212`.
    pub hvbjoy: u8,
    /// Latched NMI line (read of `$4210` clears it).
    pub nmi_flag: bool,
    /// Latched IRQ line.
    pub irq_flag: bool,
    /// `$4201` WRIO — programmable I/O port output.
    pub wrio: u8,
    /// `$4202` WRMPYA — multiply operand A.
    pub wrmpya: u8,
    /// `$4203` WRMPYB — multiply operand B.
    pub wrmpyb: u8,
    /// `$4204/$4205` WRDIV — 16-bit dividend.
    pub wrdiv: u16,
    /// `$4206` WRDVDD — 8-bit divisor.
    pub wrdvdd: u8,
    /// `$4207/$4208` HTIME — horizontal IRQ target.
    pub htime: u16,
    /// `$4209/$420A` VTIME — vertical IRQ target.
    pub vtime: u16,
    /// `$4216/$4217` RDMPY — multiply / remainder result.
    pub rdmpy: u16,
    /// `$4214/$4215` RDDIV — divide quotient result.
    pub rddiv: u16,
    /// `$4218/$4219` latched joypad 1 (auto-read).
    pub joy1: u16,
    /// `$421A/$421B` latched joypad 2 (auto-read).
    pub joy2: u16,
    /// `$420D` MEMSEL — `FastROM` enable (bit 0).
    pub memsel: u8,
}

/// One DMA/HDMA channel's registers (`$43x0-$43xA`).
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct DmaChannelState {
    /// `$43x0` `DMAPx` (raw byte).
    pub params: u8,
    /// `$43x1` `BBADx` — B-bus address (`$2100 + bbad`).
    pub bbad: u8,
    /// `$43x2/$43x3` `A1Tx` — A-bus address.
    pub a_addr: u16,
    /// `$43x4` `A1Bx` — A-bus bank.
    pub a_bank: u8,
    /// `$43x5/$43x6` `DASx` — byte count / HDMA indirect address.
    pub das: u16,
    /// `$43x7` `A2Bx` — HDMA indirect bank.
    pub dasb: u8,
    /// `$43x8/$43x9` `A2Ax` — HDMA table pointer.
    pub a2a: u16,
    /// `$43xA` `NTRLx` — HDMA line counter.
    pub ntlr: u8,
}

/// DMA / HDMA controller registers.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct DmaState {
    /// The 8 channels ($430x-$437x).
    pub channels: [DmaChannelState; 8],
    /// `$420B` MDMAEN — general-DMA enable mask.
    pub mdmaen: u8,
    /// `$420C` HDMAEN — HDMA enable mask.
    pub hdmaen: u8,
}

/// Scanline scheduler snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SchedulerState {
    /// 0..=261 on NTSC; 225..=261 is `VBlank`.
    pub ppu_line: u16,
    /// Master cycles consumed within the current scanline.
    pub mcycles_in_line: u32,
    /// Frames completed since reset.
    pub frame_count: u64,
    /// Number of NMIs delivered to the CPU.
    pub nmis_serviced: u64,
}

/// APU / SPC700 / DSP snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ApuState {
    /// SPC700 program counter.
    pub spc_pc: u16,
    /// `true` if the SPC700 has stopped (`STP`).
    pub spc_stopped: bool,
    /// `true` once the SPC has left the IPL ROM area at `$FFC0`.
    pub past_iplrom: bool,
    /// `$2140-$2143` bytes the SPC has placed on the CPU side.
    pub to_cpu_ports: [u8; 4],
    /// `$F4-$F7` bytes the CPU has placed on the SPC side.
    pub to_spc_ports: [u8; 4],
    /// Master volume left (signed 7-bit).
    pub mvol_l: i8,
    /// Master volume right.
    pub mvol_r: i8,
    /// KON register (`$4C`) most-recent write.
    pub kon: u8,
    /// ENDX register (`$7C`).
    pub endx: u8,
    /// Number of DSP voices currently playing.
    pub active_voices: u8,
    /// How many stereo samples are buffered in `audio_queue`.
    pub audio_queue_len: usize,
    /// The most recent mixed stereo sample.
    pub last_audio_sample: (i16, i16),
    /// Full 128-byte DSP register file (`$00..$7F`). Lets debuggers see
    /// per-voice volume/pitch/SRCN/ADSR/GAIN + global DIR/EVOL/EON/PMON
    /// without re-reading via the bus port.
    pub dsp_regs: Vec<u8>,
    /// First 64 bytes of ARAM at the BRR sample directory base
    /// (`DIR << 8`). Each 4-byte entry is `start_lo/start_hi/loop_lo`/
    /// `loop_hi`; 64 bytes covers 16 SRCN entries — enough to see what
    /// samples the driver has set up.
    pub dir_excerpt: Vec<u8>,
    /// Per-voice "is currently playing" (mirror of `voice_active`).
    pub voice_active: [bool; 8],
    /// Per-voice ADSR phase as a string for easy reading.
    pub voice_phase: [String; 8],
    /// Per-voice 11-bit envelope value (0..0x7FF). Independent of
    /// the `ENVX` register reading — this is the live value the DSP
    /// is using right now.
    pub voice_envelope: [u16; 8],
    /// Per-voice current BRR block address in ARAM. Set on KON from
    /// the sample directory and advanced by 9 bytes per consumed
    /// BRR block.
    pub voice_block_addr: [u16; 8],
    /// First 36 bytes of ARAM at each voice's current BRR block (4
    /// blocks of 9 bytes per voice) — lets debuggers spot whether the
    /// SPC has actually uploaded BRR data with sane headers (scale
    /// 0..12, varying filter, end/loop bits sparse).
    pub voice_brr_dump: Vec<Vec<u8>>,
    /// Per-voice 4-sample BRR history (newest first). Direct view of
    /// the post-IIR-decoded samples used as input to the Gaussian
    /// interpolator. If this is all zeros for an active voice with a
    /// non-zero envelope, decoding is failing somewhere upstream.
    pub voice_brr_history: Vec<Vec<i16>>,
    /// Per-voice 14-bit pitch accumulator. Threshold 0x4000 = 1 BRR
    /// sample per output tick.
    pub voice_pitch_acc: [u16; 8],
}

/// One decoded OAM sprite (size, flips, and high-table bits resolved),
/// mirroring `luna_ppu::SpriteEntry` so front-ends can list sprites
/// without depending on `luna-ppu` directly.
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct SpriteInfo {
    /// OAM index (0..=127).
    pub index: usize,
    /// Signed X position (-256..=255 after high-table sign extension).
    pub x: i16,
    /// 8-bit Y position.
    pub y: u8,
    /// 9-bit tile number.
    pub tile: u16,
    /// 3-bit palette index.
    pub palette: u8,
    /// 2-bit priority (0-3).
    pub priority: u8,
    /// Horizontal flip flag.
    pub h_flip: bool,
    /// Vertical flip flag.
    pub v_flip: bool,
    /// Sprite width in pixels.
    pub w: u16,
    /// Sprite height in pixels.
    pub h: u16,
}

/// Cumulative metrics since reset.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Stats {
    /// Total CPU instructions executed.
    pub instructions_executed: u64,
    /// Total master cycles consumed.
    pub total_mclk: u64,
}

/// Result of [`Emulator::loop_probe`] — a CPU-liveness measurement.
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct LoopProbe {
    /// Distinct `PB:PC` addresses executed during the probe. A tiny count
    /// over many instructions means the CPU is spinning in a tight loop.
    pub distinct_pcs: usize,
    /// Instructions actually executed (≤ `max_steps`; less if the CPU halted).
    pub executed: u64,
}

/// In-progress joypad capture buffer (issue #83). Records only mask
/// *changes* per port; `last_mask` de-dups so a held button costs one
/// entry, not one-per-frame.
struct InputCapture {
    entries: Vec<InputCaptureEntry>,
    last_mask: [u16; 2],
}

impl InputCapture {
    fn note(&mut self, frame: u64, port: u8, mask: u16) {
        let Some(slot) = self.last_mask.get_mut(usize::from(port)) else {
            return; // only ports 0/1 exist
        };
        if *slot != mask {
            *slot = mask;
            self.entries.push(InputCaptureEntry { frame, port, mask });
        }
    }
}

/// The public emulator handle. Owns at most one cartridge + Snes
/// machine at a time.
pub struct Emulator {
    snes: Option<Snes>,
    rom_info: Option<RomInfo>,
    instructions_executed: u64,
    /// Stable hash of the loaded ROM bytes, captured at `load_rom` time.
    /// A save state records this so [`Emulator::load_state`] can refuse a
    /// state produced against a different ROM.
    rom_hash: u64,
    /// Loaded WLA-DX symbol table (issue #67); `None` until
    /// [`Emulator::load_symbols`] runs.
    symbols: Option<SymbolTable>,
    /// Event Viewer category/DMA visibility config.
    event_config: event_viewer::EventViewerConfig,
    /// The just-completed frame's captured events (Mesen2 `_debugEvents`),
    /// CPU IO + DMA merged and sorted by `(scanline, cycle)`.
    last_frame_events: Vec<CapturedEvent>,
    /// The frame before that (Mesen2 `_prevDebugEvents`) — drawn trailing when
    /// `show_previous_frame` is on.
    prev_frame_events: Vec<CapturedEvent>,
    /// Active joypad-input capture, if recording (issue #83). `None` when
    /// not recording; fed by [`Emulator::set_joypad`], drained by
    /// [`Emulator::take_input_capture`].
    input_capture: Option<InputCapture>,
    /// Video-standard override applied to every subsequent ROM load — see
    /// [`Emulator::set_forced_region`]. `None` = honour the cartridge header.
    forced_region: Option<Region>,
}

/// One raw captured Event Viewer event before category/filter decode — either
/// a CPU bus access (mem-trace) or a DMA B-bus write (dma-trace). Kept as the
/// raw source so [`Emulator::event_snapshot`] can re-decode under the live
/// config (visibility / DMA-channel toggles) without re-running the frame.
#[derive(Clone, Copy)]
enum CapturedEvent {
    /// A CPU bus access or NMI/IRQ marker.
    Cpu(MemTraceEvent),
    /// A DMA B-bus write, carrying its source channel.
    Dma(DmaTraceEvent),
    /// A breakpoint hit (issue #68) — Mesen2's `MarkedBreakpoint` overlay
    /// category. Injected by `run_until_break` at the halt position.
    Break { pc: u32, line: u16, hclock: u16 },
}

impl CapturedEvent {
    /// Sort key `(scanline, cycle)` — Mesen2 orders the event list/overlay by
    /// PPU position. Cycle is the exact H-clock (Mesen2 `GetHClock`).
    const fn sort_key(&self) -> (u16, u16) {
        match self {
            Self::Cpu(e) => (e.line, e.hclock),
            Self::Dma(e) => (e.line, e.hclock),
            Self::Break { line, hclock, .. } => (*line, *hclock),
        }
    }

    /// The position key Mesen2's `FilterEvents` compares against the cursor:
    /// `(scanline << 16) + cycle`.
    fn position_key(&self) -> u32 {
        let (line, cycle) = self.sort_key();
        (u32::from(line) << 16) | u32::from(cycle)
    }

    /// Decode under `cfg`, flagging prev-frame membership. `None` when the
    /// access has no category or is hidden by a category / DMA-channel filter.
    fn decode(
        &self,
        cfg: &event_viewer::EventViewerConfig,
        is_prev_frame: bool,
    ) -> Option<EventViewerEvent> {
        match self {
            Self::Cpu(e) => event_viewer::decode_event(e, cfg, is_prev_frame),
            Self::Dma(e) => event_viewer::decode_dma_event(e, cfg, is_prev_frame),
            Self::Break { pc, line, hclock } => {
                let category = event_viewer::EventCategory::MarkedBreakpoint;
                if !cfg.visible[category.index()] {
                    return None;
                }
                Some(EventViewerEvent {
                    scanline: *line,
                    cycle: *hclock,
                    addr: (*pc & 0xFFFF) as u16,
                    value: 0,
                    pc: *pc,
                    category,
                    is_prev_frame,
                    dma_channel: None,
                })
            }
        }
    }
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Emulator {
    /// Build a fresh emulator with no ROM loaded yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            snes: None,
            rom_info: None,
            instructions_executed: 0,
            rom_hash: 0,
            symbols: None,
            event_config: event_viewer::EventViewerConfig {
                visible: [true; event_viewer::CATEGORY_COUNT],
                show_previous_frame: true,
                show_dma_channels: [true; 8],
            },
            last_frame_events: Vec::new(),
            prev_frame_events: Vec::new(),
            input_capture: None,
            forced_region: None,
        }
    }

    /// Whether a ROM is currently loaded.
    #[must_use]
    pub const fn has_rom(&self) -> bool {
        self.snes.is_some()
    }

    /// The loaded cartridge's parsed metadata, or `None` before any load.
    /// Lets a caller that loaded through a helper (e.g. the CLI's shared
    /// `load_rom_into`) recover the [`RomInfo`] for a header print-out.
    #[must_use]
    pub const fn rom_info(&self) -> Option<&RomInfo> {
        self.rom_info.as_ref()
    }

    /// Load a ROM file. On success, the emulator is ready for
    /// stepping. Returns the parsed cartridge metadata for callers
    /// that want to surface it (window title, MCP `load_rom` tool).
    pub fn load_rom(&mut self, path: &Path) -> Result<RomInfo, ApiError> {
        let mut cart = Cartridge::load(path)?;
        // Beside-ROM + embedded firmware are handled by `Cartridge::load`;
        // additionally search the luna firmware folder so a once-installed
        // `dsp1b.rom` is found for any ROM, anywhere.
        if cart.needs_coprocessor_firmware() {
            if let (Some(name), Some(dir)) =
                (cart.required_firmware_filename(), Self::firmware_dir())
            {
                if let Ok(bytes) = std::fs::read(dir.join(name)) {
                    cart.set_coprocessor_firmware(bytes);
                }
            }
        }
        self.load_cartridge(cart)
    }

    /// luna's coprocessor-firmware folder (`<config>/luna/firmware`), where
    /// files like `dsp1b.rom` live. `None` if no config dir is available.
    #[must_use]
    pub fn firmware_dir() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("luna").join("firmware"))
    }

    /// Install a coprocessor-firmware file into the firmware folder under
    /// the canonical name for its kind (the firmware dir is created if
    /// needed). Front-ends call this from a CLI flag or a "locate firmware"
    /// prompt; afterwards `load_rom` finds it automatically. `target` picks
    /// the destination filename (e.g. `"dsp1b.rom"`).
    pub fn install_firmware(src: &Path, target: &str) -> Result<std::path::PathBuf, ApiError> {
        let dir = Self::firmware_dir().ok_or_else(|| {
            ApiError::Io(std::io::Error::other("no config directory for firmware"))
        })?;
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(target);
        std::fs::copy(src, &dest)?;
        Ok(dest)
    }

    /// Lower-level entry point used by tests: load a ROM blob
    /// already parsed into bytes (skips filesystem I/O).
    pub fn load_rom_bytes(&mut self, bytes: Vec<u8>) -> Result<RomInfo, ApiError> {
        let cart = Cartridge::from_bytes(bytes)?;
        self.load_cartridge(cart)
    }

    /// Load a ROM blob with a **forced** mapper, bypassing header
    /// auto-detection. Needed for headerless homebrew test ROMs (e.g. the
    /// `PeterLemon` Super FX / GSU plot tests) that carry no chipset byte.
    pub fn load_rom_bytes_forced(
        &mut self,
        bytes: Vec<u8>,
        mapper: luna_core::MapperKind,
    ) -> Result<RomInfo, ApiError> {
        let cart = Cartridge::from_bytes_forced(bytes, mapper)?;
        self.load_cartridge(cart)
    }

    /// Load a ROM **file** with a forced mapper, bypassing header
    /// auto-detection. The path-based sibling of [`Self::load_rom_bytes_forced`]
    /// — reads the file, strips a copier header, searches the firmware folder
    /// like [`Self::load_rom`], then applies `mapper`. Lets a front-end open a
    /// checksum-invalid homebrew/test ROM (`PeterLemon` corpus) that
    /// auto-detection would reject (issue #88).
    pub fn load_rom_forced(
        &mut self,
        path: &Path,
        mapper: MapperKind,
    ) -> Result<RomInfo, ApiError> {
        let bytes = std::fs::read(path)?;
        let mut cart = Cartridge::from_bytes_forced(bytes, mapper)?;
        if cart.needs_coprocessor_firmware() {
            if let (Some(name), Some(dir)) =
                (cart.required_firmware_filename(), Self::firmware_dir())
            {
                if let Ok(fw) = std::fs::read(dir.join(name)) {
                    cart.set_coprocessor_firmware(fw);
                }
            }
        }
        self.load_cartridge(cart)
    }

    /// Force the video standard (NTSC / PAL) for every subsequent ROM load,
    /// overriding the cartridge header's country byte. `None` restores header
    /// auto-detection.
    ///
    /// The region decides the scanline count (262 NTSC / 312 PAL) and the
    /// frame rate, so it changes every timing-derived observable — which is
    /// the point: the golden harness runs the `PeterLemon` corpus as PAL to
    /// match krom's reference captures, and a differential investigation must
    /// be able to reproduce that exact configuration from the CLI (issue
    /// #109). Sticky across loads (like a front-end setting), NOT cleared by
    /// [`Emulator::reset`]; takes effect on the next load, not retroactively.
    pub const fn set_forced_region(&mut self, region: Option<Region>) {
        self.forced_region = region;
    }

    /// The active video-standard override, if any — see
    /// [`Emulator::set_forced_region`].
    #[must_use]
    pub const fn forced_region(&self) -> Option<Region> {
        self.forced_region
    }

    fn load_cartridge(&mut self, mut cart: Cartridge) -> Result<RomInfo, ApiError> {
        if let Some(region) = self.forced_region {
            cart.header.region = region;
        }
        // Capture a stable hash of the ROM bytes before the cartridge is
        // consumed by `try_from_cartridge`; the save-state layer uses it to
        // refuse states produced against a different ROM.
        let rom_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cart.rom.hash(&mut h);
            h.finish()
        };
        let info = RomInfo {
            title: cart.header.title.clone(),
            mapper: format!("{:?}", cart.header.mapper_kind),
            rom_bytes: cart.rom.len(),
            header_rom_size_kb: cart.header.rom_size_kb,
            sram_kb: cart.header.sram_size_kb,
            region: format!("{:?}", cart.header.region),
            fast_rom: cart.header.fast_rom,
            version: cart.header.version,
            checksum: cart.header.checksum,
            checksum_complement: cart.header.checksum_complement,
            checksum_valid: cart.header.checksum_valid(),
            missing_firmware: if cart.needs_coprocessor_firmware() {
                cart.required_firmware_filename().map(str::to_string)
            } else {
                None
            },
        };
        // Unsupported coprocessor carts surface as a typed
        // `UnsupportedMapper` error (no longer a panic). The
        // `catch_unwind` stays as a backstop for any *other* panic
        // during construction / `reset`, so a malformed ROM can't tear
        // down the whole transport.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut snes = Snes::try_from_cartridge(cart)?;
            snes.reset();
            Ok::<_, luna_core::UnsupportedMapper>(snes)
        }));
        match result {
            Ok(Ok(snes)) => {
                self.snes = Some(snes);
                self.rom_info = Some(info.clone());
                self.instructions_executed = 0;
                self.rom_hash = rom_hash;
                Ok(info)
            }
            Ok(Err(unsupported)) => Err(ApiError::UnsupportedMapper(unsupported)),
            Err(payload) => Err(ApiError::Panic(panic_message(&payload))),
        }
    }

    /// Reset the loaded emulator to its power-on state. Equivalent
    /// to running the SNES reset vector. Errors if no ROM is loaded.
    pub fn reset(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.reset();
        self.instructions_executed = 0;
        // A reset rewinds frame_count to 0; a capture spanning it would have
        // incoherent frames, so drop it (issue #83).
        self.input_capture = None;
        Ok(())
    }

    /// Set the joypad button bitmask for controller `port` (`0` or
    /// `1`). The mask is the SNES JOY1L/JOY1H layout — bit 15 = B,
    /// 14 = Y, 13 = Select, 12 = Start, 11..8 = Up/Down/Left/Right,
    /// 7..4 = A/X/L/R, 3..0 = 0 (signature). The mask is latched on
    /// the next auto-read pulse (`VBlank` entry when `NMITIMEN.0` is
    /// set), so callers should hold the mask for at least one frame
    /// before reading state.
    pub fn set_joypad(&mut self, port: u8, mask: u16) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.cpu_regs.set_joypad(usize::from(port), mask);
        let frame = snes.frame_count;
        // Feed an in-progress capture (issue #83): records only the change,
        // keyed by the frame it lands on — the checkpoint the replay reads.
        if let Some(cap) = self.input_capture.as_mut() {
            cap.note(frame, port, mask);
        }
        Ok(())
    }

    /// Start recording joypad input (issue #83): every subsequent
    /// [`Self::set_joypad`] change is logged as a `frame:mask` checkpoint.
    /// The masks currently in effect become the capture's baseline, so a
    /// button already held when recording starts is captured too. Replaces
    /// any capture already in progress.
    pub fn start_input_capture(&mut self) {
        let (frame, m0, m1) = self.snes.as_ref().map_or((0, 0, 0), |s| {
            (s.frame_count, s.cpu_regs.joypad1, s.cpu_regs.joypad2)
        });
        // Baseline last_mask = the default idle state, so an idle start adds
        // no entries but a held-at-start button (mask != 0) does.
        let mut cap = InputCapture {
            entries: Vec::new(),
            last_mask: [0, 0],
        };
        cap.note(frame, 0, m0);
        cap.note(frame, 1, m1);
        self.input_capture = Some(cap);
    }

    /// Whether a joypad-input capture is currently recording (issue #83).
    #[must_use]
    pub const fn is_capturing_input(&self) -> bool {
        self.input_capture.is_some()
    }

    /// Stop recording and return the captured joypad checkpoints, sorted by
    /// frame (issue #83). Render them as an `--input` script with
    /// [`input_capture_to_script`]. Returns an empty vec if not recording.
    pub fn take_input_capture(&mut self) -> Vec<InputCaptureEntry> {
        let mut entries = self
            .input_capture
            .take()
            .map_or_else(Vec::new, |c| c.entries);
        entries.sort_by_key(|e| e.frame);
        entries
    }

    /// Select the device on a controller port (`port` 0 = port 1, 1 = port 2):
    /// `true` = SNES Mouse, `false` = standard pad. A Mouse feeds its 16-bit
    /// signature to the auto-joypad-read (how `mouseInit` detects it) and its
    /// full 32-bit stream to the matching manual `$4016`/`$4017` serial read.
    pub fn set_port_mouse(&mut self, port: u8, on: bool) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let dev = if on {
            luna_core::controller::PortDevice::Mouse
        } else {
            luna_core::controller::PortDevice::Pad
        };
        if port == 0 {
            snes.cpu_regs.port1 = dev;
        } else {
            snes.cpu_regs.port2 = dev;
        }
        Ok(())
    }

    /// Set the port-2 Mouse's this-frame signed motion (`+dx` right, `+dy`
    /// down) and buttons (bit 0 = left, bit 1 = right). Effective once
    /// [`Self::set_port2_mouse`] is enabled.
    pub fn set_mouse(&mut self, dx: i32, dy: i32, buttons: u8) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.cpu_regs.mouse.dx = dx;
        snes.cpu_regs.mouse.dy = dy;
        snes.cpu_regs.mouse.buttons = buttons;
        Ok(())
    }

    /// Set a controller port's device (0 = port 1, 1 = port 2) — pad, mouse,
    /// or super scope. The GUI's per-port device selector drives this.
    pub fn set_port_device(&mut self, port: u8, device: PortDevice) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        if port == 0 {
            snes.cpu_regs.port1 = device;
        } else {
            snes.cpu_regs.port2 = device;
        }
        Ok(())
    }

    /// The device currently on a controller port, so the GUI can reflect the
    /// active selection (defaults to pad with no ROM).
    pub fn port_device(&self, port: u8) -> PortDevice {
        self.snes.as_ref().map_or(PortDevice::Pad, |s| {
            if port == 0 {
                s.cpu_regs.port1
            } else {
                s.cpu_regs.port2
            }
        })
    }

    /// Set the Super Scope's aimed beam position (`x`/`y` in screen pixels) and
    /// buttons (bit0 = trigger, bit1 = cursor, bit2 = turbo, bit3 = pause).
    /// Effective when a port is set to [`PortDevice::SuperScope`].
    pub fn set_superscope(&mut self, x: i32, y: i32, buttons: u8) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let s = &mut snes.cpu_regs.super_scope;
        s.cx = x;
        s.cy = y;
        s.trigger = buttons & 0x01 != 0;
        s.cursor = buttons & 0x02 != 0;
        s.turbo = buttons & 0x04 != 0;
        s.pause = buttons & 0x08 != 0;
        Ok(())
    }

    /// Step the CPU `count` instructions (or stop early if the CPU
    /// halts or panics). Returns the number actually executed.
    pub fn step(&mut self, count: u64) -> Result<u64, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while executed < count {
                if snes.cpu.stopped {
                    break;
                }
                snes.step();
                executed += 1;
            }
            executed
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(n) => {
                self.instructions_executed += n;
                Ok(n)
            }
            Err(payload) => {
                self.instructions_executed += executed;
                Err(ApiError::Panic(panic_message(&payload)))
            }
        }
    }

    /// Run instructions until the PPU completes one frame (i.e.
    /// `frame_count` advances). Returns the number of instructions
    /// executed. Bounded by `max_steps` as a safety belt.
    pub fn step_until_frame(&mut self, max_steps: u64) -> Result<u64, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let start_frame = snes.frame_count;
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while executed < max_steps && !snes.cpu.stopped && snes.frame_count == start_frame {
                snes.step();
                executed += 1;
            }
            executed
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(n) => {
                self.instructions_executed += n;
                Ok(n)
            }
            Err(payload) => {
                self.instructions_executed += executed;
                Err(ApiError::Panic(panic_message(&payload)))
            }
        }
    }

    /// Probe CPU liveness from the current state: step up to `max_steps`
    /// instructions (stopping early if the CPU halts), recording how many
    /// **distinct** program addresses (`PB:PC`) were executed. A live game
    /// touches hundreds–thousands of addresses; a hung game spins over a
    /// handful — so a tiny `distinct_pcs` is the signal a frozen game is in
    /// an infinite loop (vs. STP, which `cpu_state().stopped` already
    /// reports). Mutates state (advances the CPU); a diagnostic, not part of
    /// normal stepping. Panic-safe (a crashing ROM returns `Err`).
    pub fn loop_probe(&mut self, max_steps: u64) -> Result<LoopProbe, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut seen = std::collections::HashSet::new();
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while executed < max_steps && !snes.cpu.stopped {
                seen.insert((u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc));
                snes.step();
                executed += 1;
            }
        }));
        std::panic::set_hook(prev_hook);
        self.instructions_executed += executed;
        result.map_err(|p| ApiError::Panic(panic_message(&p)))?;
        Ok(LoopProbe {
            distinct_pcs: seen.len(),
            executed,
        })
    }

    /// Take a JSON-serialisable snapshot of the entire observable
    /// emulator state.
    pub fn state(&mut self) -> EmulatorState {
        let cpu = self
            .snes
            .as_ref()
            .map_or_else(default_cpu_state, |s| CpuState {
                a: s.cpu.a,
                x: s.cpu.x,
                y: s.cpu.y,
                sp: s.cpu.sp,
                pc: s.cpu.pc,
                pb: s.cpu.pb,
                db: s.cpu.db,
                dp: s.cpu.dp,
                p: s.cpu.p.bits(),
                e: s.cpu.e,
                stopped: s.cpu.stopped,
                waiting: s.cpu.waiting,
            });
        let ppu = self.snes.as_ref().map_or_else(default_ppu_state, |s| {
            let mut vram_nz = 0;
            for off in 0..0x10000u32 {
                if s.ppu.vram.peek(off as u16) != 0 {
                    vram_nz += 1;
                }
            }
            let mut cgram_nz = 0;
            for idx in 0..256u16 {
                if s.ppu.cgram.color(idx as u8) != 0 {
                    cgram_nz += 1;
                }
            }
            let mut oam_nz = 0;
            for off in 0..0x220u16 {
                if s.ppu.oam.peek(off) != 0 {
                    oam_nz += 1;
                }
            }
            // First 16 sprites = 64 bytes of low table + 4 bytes
            // of high table.
            let oam_low_excerpt: Vec<u8> = (0..64u16).map(|i| s.ppu.oam.peek(i)).collect();
            let oam_high_excerpt: Vec<u8> = (0..4u16).map(|i| s.ppu.oam.peek(0x200 + i)).collect();
            let cgram: Vec<u16> = (0..256u16).map(|i| s.ppu.cgram.color(i as u8)).collect();
            let oam_full: Vec<u8> = (0..0x220u16).map(|i| s.ppu.oam.peek(i)).collect();
            let bgs = std::array::from_fn(|i| BgInfo {
                tilemap_addr_words: s.ppu.bg[i].tilemap_addr_words,
                char_addr_words: s.ppu.bg[i].char_addr_words,
                h_scroll: s.ppu.bg[i].h_scroll,
                v_scroll: s.ppu.bg[i].v_scroll,
                tilemap_size: s.ppu.bg[i].tilemap_size,
            });
            PpuState {
                inidisp: s.ppu.inidisp,
                bgmode: s.ppu.bgmode,
                vram_addr_words: s.ppu.vram.address,
                inidisp_write_count: s.ppu.inidisp_write_count,
                backdrop: s.ppu.cgram.color(0),
                obsel: s.ppu.obsel,
                vram_non_zero: vram_nz,
                cgram_non_zero: cgram_nz,
                oam_non_zero: oam_nz,
                oam_low_excerpt,
                oam_high_excerpt,
                tm: s.ppu.tm,
                ts: s.ppu.ts,
                tmw: s.ppu.tmw,
                tsw: s.ppu.tsw,
                cgwsel: s.ppu.cgwsel,
                cgadsub: s.ppu.cgadsub,
                coldata_r: s.ppu.coldata_r,
                coldata_g: s.ppu.coldata_g,
                coldata_b: s.ppu.coldata_b,
                setini: s.ppu.setini,
                w12sel: s.ppu.w12sel,
                w34sel: s.ppu.w34sel,
                wobjsel: s.ppu.wobjsel,
                wbglog: s.ppu.wbglog,
                wobjlog: s.ppu.wobjlog,
                windows: [s.ppu.wh0, s.ppu.wh1, s.ppu.wh2, s.ppu.wh3],
                bgs,
                cgram,
                oam_full,
                mosaic: s.ppu.mosaic,
                m7sel: s.ppu.m7sel,
                m7a: s.ppu.m7a,
                m7b: s.ppu.m7b,
                m7c: s.ppu.m7c,
                m7d: s.ppu.m7d,
                m7x: s.ppu.m7x,
                m7y: s.ppu.m7y,
                m7_hofs: s.ppu.m7_hofs,
                m7_vofs: s.ppu.m7_vofs,
                mpy: s.ppu.mpy_result,
                stat77: s.ppu.stat77,
                stat78: s.ppu.stat78,
                ophct: s.ppu.ophct,
                opvct: s.ppu.opvct,
            }
        });
        let cpu_regs = self
            .snes
            .as_ref()
            .map_or_else(default_cpu_regs_state, |s| CpuRegsState {
                nmitimen: s.cpu_regs.nmitimen,
                hvbjoy: s.cpu_regs.hvbjoy,
                nmi_flag: s.cpu_regs.nmi_flag,
                irq_flag: s.cpu_regs.irq_flag,
                wrio: s.cpu_regs.wrio,
                wrmpya: s.cpu_regs.wrmpya,
                wrmpyb: s.cpu_regs.wrmpyb,
                wrdiv: s.cpu_regs.wrdiv,
                wrdvdd: s.cpu_regs.wrdvdd,
                htime: s.cpu_regs.htime,
                vtime: s.cpu_regs.vtime,
                rdmpy: s.cpu_regs.rdmpy,
                rddiv: s.cpu_regs.rddiv,
                joy1: s.cpu_regs.joypad1_latched,
                joy2: s.cpu_regs.joypad2_latched,
                memsel: u8::from(s.fast_rom),
            });
        let dma = self
            .snes
            .as_ref()
            .map_or_else(default_dma_state, |s| DmaState {
                channels: std::array::from_fn(|i| {
                    let c = &s.dma.channels[i];
                    DmaChannelState {
                        params: c.params.to_byte(),
                        bbad: c.bbad,
                        a_addr: c.a_addr,
                        a_bank: c.a_bank,
                        das: c.das,
                        dasb: c.dasb,
                        a2a: c.a2a,
                        ntlr: c.ntlr,
                    }
                }),
                mdmaen: s.dma.mdmaen,
                hdmaen: s.dma.hdmaen,
            });
        let scheduler = self
            .snes
            .as_ref()
            .map_or_else(default_scheduler_state, |s| SchedulerState {
                ppu_line: s.ppu_line,
                mcycles_in_line: s.mcycles_in_line,
                frame_count: s.frame_count,
                nmis_serviced: s.nmis_serviced,
            });
        let apu = self
            .snes
            .as_ref()
            .map_or_else(default_apu_state, |s| ApuState {
                spc_pc: s.apu_real.cpu.pc,
                spc_stopped: s.apu_real.cpu.stopped,
                past_iplrom: s.apu_real.past_iplrom,
                to_cpu_ports: s.apu_real.to_cpu_ports,
                to_spc_ports: s.apu_real.to_spc_ports,
                mvol_l: s.apu_real.dsp.registers[0x0C] as i8,
                mvol_r: s.apu_real.dsp.registers[0x1C] as i8,
                kon: s.apu_real.dsp.registers[0x4C],
                endx: s.apu_real.dsp.registers[0x7C],
                active_voices: s
                    .apu_real
                    .dsp
                    .voices
                    .iter()
                    .filter(|v| {
                        v.envelope_mode != luna_apu::dsp::EnvelopeMode::Release || v.envelope != 0
                    })
                    .count() as u8,
                audio_queue_len: s.apu_real.audio_queue.len(),
                last_audio_sample: s.apu_real.audio_sample(),
                dsp_regs: s.apu_real.dsp.registers.to_vec(),
                dir_excerpt: {
                    let base = (s.apu_real.dsp.registers[0x5D] as usize) << 8;
                    let mut v = Vec::with_capacity(64);
                    for i in 0..64 {
                        v.push(s.apu_real.aram[(base + i) & 0xFFFF]);
                    }
                    v
                },
                voice_active: std::array::from_fn(|i| {
                    let v = &s.apu_real.dsp.voices[i];
                    v.envelope_mode != luna_apu::dsp::EnvelopeMode::Release || v.envelope != 0
                }),
                voice_phase: std::array::from_fn(|i| {
                    format!("{:?}", s.apu_real.dsp.voices[i].envelope_mode)
                }),
                voice_envelope: std::array::from_fn(|i| s.apu_real.dsp.voices[i].envelope),
                voice_block_addr: std::array::from_fn(|i| s.apu_real.dsp.voices[i].brr_address),
                voice_brr_dump: (0..8)
                    .map(|v| {
                        let base = s.apu_real.dsp.voices[v].brr_address as usize;
                        (0..36)
                            .map(|i| s.apu_real.aram[(base + i) & 0xFFFF])
                            .collect()
                    })
                    .collect(),
                voice_brr_history: (0..8)
                    .map(|v| s.apu_real.dsp.voices[v].buffer.to_vec())
                    .collect(),
                voice_pitch_acc: std::array::from_fn(|i| s.apu_real.dsp.voices[i].gaussian_offset),
            });
        let stats = Stats {
            instructions_executed: self.instructions_executed,
            total_mclk: self.snes.as_ref().map_or(0, |s| s.total_mclk),
        };
        let sa1 = self
            .snes
            .as_ref()
            .and_then(|s| s.mapper.sa1_snapshot())
            .map(|snap| Sa1State {
                pc: snap.pc,
                pb: snap.pb,
                p: snap.p,
                running: snap.running,
            });
        let dsp1 = self
            .snes
            .as_ref()
            .and_then(|s| s.mapper.dsp1_snapshot())
            .map(|snap| Dsp1State {
                pc: snap.pc,
                sr: snap.sr,
                a: snap.a,
                b: snap.b,
                dr: snap.dr,
                rqm: snap.rqm,
            });
        EmulatorState {
            rom: self.rom_info.clone(),
            cpu,
            ppu,
            cpu_regs,
            scheduler,
            apu,
            stats,
            sa1,
            dma,
            dsp1,
        }
    }

    /// Render the current PPU framebuffer (256×224, composited
    /// BG3-over-BG1-over-BG2 + sprites) as a PNG-encoded byte vector.
    ///
    /// Default path (`force_display=false`) is zero-cost — it copies
    /// the persistent framebuffer that the scheduler has been
    /// populating one scanline at a time (gap G6 Phase 1). The
    /// force-display debug path still re-renders the whole frame
    /// via `render_frame_with` with `bypass_forced_blank: true`.
    pub fn render_frame_png(&self, force_display: bool) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
        if force_display {
            // Debug-only path: rebuild the frame with forced-blank
            // bypass so the user can see VRAM contents even when the
            // game is keeping the screen blanked.
            let opts = luna_ppu::RenderOptions {
                bypass_forced_blank: true,
            };
            let frame = luna_ppu::render_frame_with(&snes.ppu, opts);
            for px in frame {
                buf.extend_from_slice(&px);
            }
        } else {
            for px in snes.ppu.framebuffer() {
                buf.extend_from_slice(px);
            }
        }
        let img =
            image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf).expect("size matches");
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H);
        let dyn_image: image::DynamicImage = img.into();
        dyn_image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok(out)
    }

    /// Render a single BG layer (`bg_idx` 0..=3) in isolation as a
    /// PNG-encoded byte vector — the `luna run --bg N` debug path.
    /// `force_display` bypasses INIDISP forced-blank.
    pub fn render_frame_bg_png(
        &self,
        bg_idx: usize,
        force_display: bool,
    ) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let opts = luna_ppu::RenderOptions {
            bypass_forced_blank: force_display,
        };
        let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
        for px in luna_ppu::render_frame_bg_with(&snes.ppu, bg_idx, opts) {
            buf.extend_from_slice(&px);
        }
        let img =
            image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf).expect("size matches");
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H);
        let dyn_image: image::DynamicImage = img.into();
        dyn_image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok(out)
    }

    /// Peek `count` bytes starting at the current CPU program counter
    /// (`PB:PC`), through the CPU bus view. Diagnostic (needs `&mut` for
    /// the bus). Wraps the core's `peek_pc_bytes`.
    pub fn peek_pc_bytes(&mut self, count: usize) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.peek_pc_bytes(count))
    }

    /// Decode all 128 OAM sprites (size, flips, and high-table bits
    /// resolved). Diagnostic surface so front-ends needn't import
    /// `luna-ppu`'s renderer to list sprites.
    pub fn decode_sprites(&self) -> Result<Vec<SpriteInfo>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(luna_ppu::decode_all_sprites(&snes.ppu)
            .iter()
            .enumerate()
            .map(|(index, s)| SpriteInfo {
                index,
                x: s.x,
                y: s.y,
                tile: s.tile,
                palette: s.palette,
                priority: s.priority,
                h_flip: s.h_flip,
                v_flip: s.v_flip,
                w: s.w,
                h: s.h,
            })
            .collect())
    }

    /// Render the current PPU framebuffer as raw **RGBA** bytes
    /// (`256 × 224 × 4`, row-major, alpha forced to `0xFF`) — the
    /// uncompressed form a GUI uploads straight to a texture, sharing the
    /// exact render path as [`Emulator::render_frame_png`] so the CLI and
    /// GUI cannot disagree on pixels. `force_display` bypasses INIDISP
    /// forced-blank (debug: see VRAM even while the game blanks).
    pub fn render_frame_rgba(&self, force_display: bool) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H * 4);
        let mut push_rgb = |px: &[u8; 3]| {
            out.extend_from_slice(px);
            out.push(0xFF);
        };
        if force_display {
            let opts = luna_ppu::RenderOptions {
                bypass_forced_blank: true,
            };
            for px in luna_ppu::render_frame_with(&snes.ppu, opts) {
                push_rgb(&px);
            }
        } else {
            for px in snes.ppu.framebuffer() {
                push_rgb(px);
            }
        }
        Ok(out)
    }

    /// Cheap 65C816 register snapshot for a debugger panel — reads the
    /// main CPU directly, without building (and cloning) a full
    /// [`Emulator::state`] every frame. Same data as
    /// [`EmulatorState::cpu`].
    pub fn cpu_state(&self) -> Result<CpuState, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let c = &snes.cpu;
        Ok(CpuState {
            a: c.a,
            x: c.x,
            y: c.y,
            sp: c.sp,
            pc: c.pc,
            pb: c.pb,
            db: c.db,
            dp: c.dp,
            p: c.p.bits(),
            e: c.e,
            stopped: c.stopped,
            waiting: c.waiting,
        })
    }

    /// Cheap SPC700 register snapshot for a debugger panel — reads the
    /// audio CPU directly, without building a full [`Emulator::state`]
    /// (which clones the whole DSP register file + BRR excerpts). The
    /// APU-core analogue of reading [`EmulatorState::cpu`].
    pub fn spc700_state(&self) -> Result<Spc700State, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let c = &snes.apu_real.cpu;
        Ok(Spc700State {
            a: c.a,
            x: c.x,
            y: c.y,
            sp: c.sp,
            pc: c.pc,
            psw: c.psw.0,
            stopped: c.stopped,
            sleeping: c.sleeping,
        })
    }

    /// Disassemble `count` SPC700 instructions starting at `start`.
    /// Instruction bytes are read from raw ARAM (side-effect-free — never
    /// touches the SPC I/O ports / timers), and the line at the live SPC
    /// program counter is flagged `is_pc`. For a disassembly panel.
    pub fn disassemble_spc(&self, start: u16, count: u16) -> Result<Vec<DisasmLine>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let aram = &snes.apu_real.aram;
        let read = |a: u16| aram[usize::from(a)];
        let pc = snes.apu_real.cpu.pc;
        let mut addr = start;
        let mut out = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let insn = luna_cpu_spc700::disassemble(read, addr);
            let bytes = (0..u16::from(insn.length))
                .map(|i| read(addr.wrapping_add(i)))
                .collect();
            out.push(DisasmLine {
                addr: u32::from(addr),
                bytes,
                text: insn.text,
                is_pc: addr == pc,
                // ARAM addresses live outside the CPU bus's bank space the
                // .sym labels describe.
                symbol: None,
            });
            addr = addr.wrapping_add(u16::from(insn.length));
        }
        Ok(out)
    }

    /// Disassemble `count` 65C816 instructions starting at the 24-bit address
    /// `start`, with effective accumulator/index widths `m8`/`x8`. Bytes are
    /// read side-effect-free through `peek_memory`; the line at the live
    /// `PB:PC` is flagged `is_pc`. Tracks `REP`/`SEP` forward so immediate
    /// widths stay correct across a width change inside the window. For a
    /// CPU disassembly panel.
    pub fn disassemble_cpu(
        &mut self,
        start: u32,
        count: u16,
        m8: bool,
        x8: bool,
    ) -> Result<Vec<DisasmLine>, ApiError> {
        let pc_full = {
            let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
            (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc)
        };
        let bank = (start >> 16) as u8;
        let start_off = start as u16;
        // One side-effect-free peek of the whole window (instructions are
        // ≤4 bytes, so over-read a little to cover the last one).
        let span = u16::try_from(usize::from(count) * 4 + 3).unwrap_or(u16::MAX);
        let buf = self.peek_memory(bank, start_off, span)?;
        let read = |o: u16| {
            buf.get(usize::from(o.wrapping_sub(start_off)))
                .copied()
                .unwrap_or(0)
        };

        let (mut m8, mut x8) = (m8, x8);
        let mut off = start_off;
        let mut out = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let insn = luna_cpu_65c816::disassemble(read, off, m8, x8);
            let bytes = (0..u16::from(insn.length))
                .map(|i| read(off.wrapping_add(i)))
                .collect();
            let addr_full = (u32::from(bank) << 16) | u32::from(off);
            out.push(DisasmLine {
                addr: addr_full,
                bytes,
                text: insn.text,
                is_pc: addr_full == pc_full,
                symbol: self.symbols.as_ref().and_then(|t| t.nearest(addr_full)),
            });
            // Track REP/SEP so later lines use the right immediate widths.
            match read(off) {
                0xC2 => {
                    let v = read(off.wrapping_add(1));
                    if v & 0x20 != 0 {
                        m8 = false;
                    }
                    if v & 0x10 != 0 {
                        x8 = false;
                    }
                }
                0xE2 => {
                    let v = read(off.wrapping_add(1));
                    if v & 0x20 != 0 {
                        m8 = true;
                    }
                    if v & 0x10 != 0 {
                        x8 = true;
                    }
                }
                _ => {}
            }
            off = off.wrapping_add(u16::from(insn.length));
        }
        Ok(out)
    }

    /// The emulated PPU frame counter — cheap, for a GUI's frame-boundary
    /// detection without a full [`Emulator::state`] snapshot.
    pub fn frame_count(&self) -> Result<u64, ApiError> {
        Ok(self.snes.as_ref().ok_or(ApiError::NoRom)?.frame_count)
    }

    /// Whether the screen is in INIDISP forced-blank (bit 7) right now —
    /// cheap accessor so a GUI can hold the last non-blank frame without
    /// snapshotting full state. The forced-blank *render* policy itself
    /// lives in [`Emulator::render_frame_rgba`]; this only reports it.
    pub fn forced_blank(&self) -> Result<bool, ApiError> {
        Ok(self.snes.as_ref().ok_or(ApiError::NoRom)?.ppu.inidisp & 0x80 != 0)
    }

    /// Whether the just-completed frame scanned out any visible (non-
    /// forced-blank) content. Unlike [`Emulator::forced_blank`] — the
    /// *instantaneous* INIDISP bit, which a Super FX title re-asserts
    /// every `VBlank` to prep its next double-buffer — this is a per-frame
    /// latch describing the frame whose framebuffer is now complete. A
    /// front-end should gate "publish this frame vs hold the last good
    /// one" on this, so frames that displayed content during active
    /// scanout but re-blanked at the boundary aren't dropped (the Star Fox
    /// "blink": its framebuffer is correct every frame, yet the
    /// instantaneous flag reads blank ~14 frames in 15).
    pub fn frame_showed_content(&self) -> Result<bool, ApiError> {
        Ok(self
            .snes
            .as_ref()
            .ok_or(ApiError::NoRom)?
            .ppu
            .frame_visible_content)
    }

    /// Cheap, exact hash of the current **displayed** framebuffer (the same
    /// `256 × 224` RGB pixels [`Emulator::render_frame_rgba`] emits with
    /// `force_display = false`). Comparing this across frames detects a
    /// wholly static screen *exactly and every frame* — both cheaper than
    /// re-rendering to RGBA and hashing (no conversion, no allocation) and
    /// more reliable than sampling every Nth frame, which can stride over a
    /// brief change. A hash that never moves across a run is a frozen
    /// display (vs. [`Emulator::frame_showed_content`], which reports
    /// forced-blank); the two together separate "rendering nothing" from
    /// "rendering the same thing forever".
    pub fn framebuffer_hash(&self) -> Result<u64, ApiError> {
        use std::hash::{Hash, Hasher};
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        snes.ppu.framebuffer().hash(&mut h);
        Ok(h.finish())
    }

    /// Hash of the **displayed** RGBA frame ([`Self::render_frame_rgba`] at the
    /// given `force_display`) — a stable visual-regression key. Deterministic
    /// and **cross-architecture** stable (fixed-seed `SipHash` over the
    /// integer-rendered bytes — no float), so a baseline captured on `x86_64`
    /// matches one on `aarch64`. Hashes exactly the pixels a `--screenshot` at
    /// the same `force_display` would write, **before** PNG encoding — avoiding
    /// the build-dependent encoder variability of hashing the encoded file.
    pub fn frame_hash(&self, force_display: bool) -> Result<u64, ApiError> {
        use std::hash::Hasher;
        let rgba = self.render_frame_rgba(force_display)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write(&rgba);
        Ok(h.finish())
    }

    /// Enable / disable the **native 512×448 capture** (issue #115).
    ///
    /// The PPU genuinely computes 512 horizontal samples in the hi-res modes
    /// (5/6 and pseudo-512) and two interlace fields per line — then collapses
    /// both by averaging into the 256×224 framebuffer every front-end shows.
    /// With capture on, the un-collapsed pixels are also kept, per scanline
    /// (so mid-frame register changes and HDMA effects are honoured exactly
    /// like the displayed frame): the sub/main subpixel pair side by side
    /// (lores dots duplicated), the two fields on rows `2y` / `2y+1`
    /// (duplicated when not interlaced). Off by default — it costs render
    /// time. Enable it, run at least one full frame, then read
    /// [`Self::render_frame_png_native`] / [`Self::frame_hash_native`].
    pub fn set_native_capture(&mut self, on: bool) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.ppu.set_native_capture(on);
        Ok(())
    }

    /// The native 512×448 frame as PNG bytes — see
    /// [`Self::set_native_capture`]. Errors if capture is not enabled.
    pub fn render_frame_png_native(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        if snes.ppu.native_framebuffer.is_empty() {
            return Err(ApiError::Io(std::io::Error::other(
                "native capture is not enabled (set_native_capture / --native-res)",
            )));
        }
        let (w, h) = (FRAME_W * 2, FRAME_H * 2);
        let mut buf = Vec::with_capacity(w * h * 3);
        for px in &snes.ppu.native_framebuffer {
            buf.extend_from_slice(px);
        }
        let img = image::RgbImage::from_raw(w as u32, h as u32, buf)
            .ok_or_else(|| ApiError::Io(std::io::Error::other("native frame size mismatch")))?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| ApiError::Io(std::io::Error::other(e.to_string())))?;
        Ok(png)
    }

    /// Hash of the native 512×448 frame — the exact-resolution regression key
    /// issue #115 asks for. Same construction as [`Self::frame_hash`]
    /// (fixed-seed hasher over raw pixel bytes, cross-architecture stable).
    /// Errors if capture is not enabled.
    pub fn frame_hash_native(&self) -> Result<u64, ApiError> {
        use std::hash::{Hash, Hasher};
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        if snes.ppu.native_framebuffer.is_empty() {
            return Err(ApiError::Io(std::io::Error::other(
                "native capture is not enabled (set_native_capture / --native-res)",
            )));
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        snes.ppu.native_framebuffer.hash(&mut h);
        Ok(h.finish())
    }

    /// Serialize the full running-machine state into a portable blob.
    ///
    /// The blob captures the mutable `Snes` state (CPU / PPU / APU / DMA /
    /// scheduler / WRAM) plus the cartridge mapper's mutable state (SRAM,
    /// coprocessor RAM / registers / CPU). It deliberately does **not**
    /// contain the ROM: a state is only ever loadable into an emulator
    /// already running the same ROM (enforced by [`Emulator::load_state`]
    /// via the recorded ROM hash). Errors with [`ApiError::NoRom`] if no
    /// ROM is loaded, or [`ApiError::SaveState`] on an encode failure.
    pub fn save_state(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let core = bincode::serialize(snes)
            .map_err(|e| ApiError::SaveState(format!("core encode: {e}")))?;
        let mapper = snes.mapper.save_state();
        let bundle = SaveStateBundle {
            version: SAVE_STATE_VERSION,
            rom_hash: self.rom_hash,
            core,
            mapper,
        };
        bincode::serialize(&bundle).map_err(|e| ApiError::SaveState(format!("bundle encode: {e}")))
    }

    /// Restore a blob produced by [`Emulator::save_state`] into the live
    /// emulator. The ROM must already be loaded and identical to the one
    /// the state was saved from (matched by hash) — this is a rewind of the
    /// running machine, not a ROM swap.
    ///
    /// Errors with [`ApiError::NoRom`] when no ROM is loaded, or
    /// [`ApiError::SaveState`] on a decode failure, a format-version
    /// mismatch, or a ROM-hash mismatch.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), ApiError> {
        if self.snes.is_none() {
            return Err(ApiError::NoRom);
        }
        let bundle: SaveStateBundle = bincode::deserialize(data)
            .map_err(|e| ApiError::SaveState(format!("bundle decode: {e}")))?;
        if bundle.version != SAVE_STATE_VERSION {
            return Err(ApiError::SaveState(format!(
                "format version mismatch: state is v{}, this build expects v{SAVE_STATE_VERSION}",
                bundle.version
            )));
        }
        if bundle.rom_hash != self.rom_hash {
            return Err(ApiError::SaveState(
                "ROM mismatch: this save state was produced against a different ROM".to_string(),
            ));
        }
        let mut restored: Snes = bincode::deserialize(&bundle.core)
            .map_err(|e| ApiError::SaveState(format!("core decode: {e}")))?;
        // The deserialized `Snes` has a placeholder mapper (the trait object
        // is `serde(skip)`). Move the LIVE mapper — which still owns the ROM
        // — into it, then replay the mapper's saved mutable state onto it.
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        restored.mapper = std::mem::replace(&mut snes.mapper, luna_core::null_mapper());
        restored.mapper.load_state(&bundle.mapper);
        *snes = restored;
        Ok(())
    }

    /// Number of stereo samples currently waiting in the APU output
    /// queue — cheap, lets an audio-paced GUI drain exactly the host
    /// ring's free space and tell whether the ring (not the queue) was
    /// the limiter, without re-queuing rejected samples.
    pub fn audio_queue_len(&self) -> Result<usize, ApiError> {
        Ok(self
            .snes
            .as_ref()
            .ok_or(ApiError::NoRom)?
            .apu_real
            .audio_queue
            .len())
    }

    /// Drain up to `max` stereo (i16, i16) audio samples from the
    /// APU's output queue. Returns the actual samples consumed.
    pub fn drain_audio(&mut self, max: usize) -> Result<Vec<(i16, i16)>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(max.min(snes.apu_real.audio_queue.len()));
        snes.apu_real.drain_audio(&mut out, max);
        Ok(out)
    }

    /// Read `count` bytes starting at the 24-bit CPU address `bank:offset`,
    /// **side-effect-free**: charges no cycles (does not advance the master
    /// clock / APU) and never reads MMIO (no latch toggles, flag clears, or
    /// address-port advances). Safe to poll every frame from a debugger
    /// memory view. WRAM and ROM/SRAM/coproc-work-RAM return real bytes; the
    /// `$2000-$5FFF` register band reads back as `0`.
    pub fn peek_memory(&mut self, bank: u8, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.dbg_peek_bytes(bank, offset, usize::from(count)))
    }

    /// Debug poke (L8): write bytes into WRAM (`$7E-$7F` or the `$00-3F`/
    /// `$80-BF` low-RAM mirror) directly — inject a test state without a full
    /// save-state. Returns bytes written (non-WRAM addresses are skipped).
    pub fn poke_memory(&mut self, bank: u8, offset: u16, data: &[u8]) -> Result<usize, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.dbg_poke_bytes(bank, offset, data))
    }

    /// Memory search (L9): every 24-bit `$7E-$7F` WRAM address whose bytes
    /// match `pattern`. Empty `pattern` → no hits.
    pub fn search_memory(&self, pattern: &[u8]) -> Result<Vec<u32>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        let wram = &snes.wram[..];
        let mut hits = Vec::new();
        for i in 0..=wram.len().saturating_sub(pattern.len()) {
            if &wram[i..i + pattern.len()] == pattern {
                hits.push(0x7E_0000 + i as u32);
            }
        }
        Ok(hits)
    }

    /// Controlled execution (L10): step until the CPU PC reaches `pc`
    /// (`pb << 16 | pc`) or `max_steps` instructions elapse. Returns `true`
    /// if `pc` was reached (checked BEFORE each step, so a current match
    /// returns immediately).
    pub fn run_until_pc(&mut self, pc: u32, max_steps: u64) -> Result<bool, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        for _ in 0..max_steps {
            let cur = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
            if cur == pc {
                return Ok(true);
            }
            snes.step();
        }
        let cur = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
        Ok(cur == pc)
    }

    // -----------------------------------------------------------------
    // WLA-DX symbol tables (issue #67, epic #63)
    // -----------------------------------------------------------------

    /// Load a WLA-DX `.sym` file, replacing any previous table. Returns
    /// the number of labels parsed. Once loaded, `disassemble_cpu`
    /// annotates lines with the nearest label and
    /// [`Self::resolve_symbol`] maps names to addresses for the
    /// address-taking APIs.
    pub fn load_symbols(&mut self, path: &std::path::Path) -> Result<usize, ApiError> {
        let table = SymbolTable::load(path)?;
        let n = table.len();
        self.symbols = Some(table);
        Ok(n)
    }

    /// Parse a `.sym` table from a string (the file-less form used by
    /// tests and by transports that already hold the text).
    pub fn load_symbols_str(&mut self, text: &str) -> usize {
        let table = SymbolTable::parse(text);
        let n = table.len();
        self.symbols = Some(table);
        n
    }

    /// Drop the loaded symbol table.
    pub fn clear_symbols(&mut self) {
        self.symbols = None;
    }

    /// Resolve a label name to its 24-bit `bank:offset` address.
    #[must_use]
    pub fn resolve_symbol(&self, name: &str) -> Option<u32> {
        self.symbols.as_ref().and_then(|t| t.resolve(name))
    }

    /// Nearest label at or below `addr` in the same bank (`name` or
    /// `name+0xNN`), when a table is loaded.
    #[must_use]
    pub fn symbol_for_addr(&self, addr: u32) -> Option<String> {
        self.symbols.as_ref().and_then(|t| t.nearest(addr))
    }

    /// Clone of the loaded symbol table (tables are small — a debugger
    /// consumer can annotate outside the emulator lock).
    #[must_use]
    pub fn symbols_cloned(&self) -> Option<SymbolTable> {
        self.symbols.clone()
    }

    // -----------------------------------------------------------------
    // Breakpoint / watchpoint registry (issue #66, epic #63)
    // -----------------------------------------------------------------

    /// Get (installing on first use) the breakpoint registry.
    fn bp_registry(&mut self) -> Result<&mut luna_core::BreakpointSet, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes
            .breakpoints
            .get_or_insert_with(|| Box::new(luna_core::BreakpointSet::new())))
    }

    /// Register an exec breakpoint at a 24-bit `PB:PC`. Returns its id.
    pub fn bp_add_exec(&mut self, pc: u32) -> Result<u32, ApiError> {
        Ok(self.bp_registry()?.add_exec(pc))
    }

    /// Register a memory watchpoint over the inclusive 24-bit bus range
    /// `lo..=hi`, firing on reads and/or writes. Returns its id.
    pub fn bp_add_mem(
        &mut self,
        lo: u32,
        hi: u32,
        on_read: bool,
        on_write: bool,
    ) -> Result<u32, ApiError> {
        if lo > hi {
            return Err(ApiError::BadArg(format!(
                "watch range lo ({lo:#08X}) > hi ({hi:#08X})"
            )));
        }
        if !on_read && !on_write {
            return Err(ApiError::BadArg(
                "watchpoint must fire on reads, writes, or both".into(),
            ));
        }
        // Mirror-folded by default (issue #91): a watch on a WRAM/MMIO address
        // also fires on accesses through its bank mirrors, so the caller need
        // not know the exact executing bank (LoROM $00:2100 vs FastROM $80:2100).
        Ok(self.bp_registry()?.add_mem(lo, hi, on_read, on_write, true))
    }

    /// Remove a breakpoint by id. Returns `true` if it existed.
    pub fn bp_remove(&mut self, id: u32) -> Result<bool, ApiError> {
        Ok(self.bp_registry()?.remove(id))
    }

    /// Remove every breakpoint.
    pub fn bp_clear(&mut self) -> Result<(), ApiError> {
        self.bp_registry()?.clear();
        Ok(())
    }

    /// Snapshot the registered breakpoints.
    pub fn bp_list(&self) -> Result<Vec<BreakpointInfo>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(snes
            .breakpoints
            .as_ref()
            .map(|b| b.list())
            .unwrap_or_default())
    }

    /// Run at full emulation speed until a registered breakpoint fires or
    /// `max_steps` instructions elapse.
    ///
    /// Semantics:
    /// - **exec breakpoints** halt BEFORE the instruction at the target PC
    ///   executes. Resume-friendly: the check is skipped for the very first
    ///   instruction of the run, so calling again after a hit moves past it.
    /// - **memory watchpoints** halt AFTER the accessing instruction
    ///   completes (instruction-atomic, like luna's interrupt model); the
    ///   hit reports the exact accessing PC, address and value.
    ///
    /// A stale unconsumed watchpoint hit from a previous plain `step()` run
    /// is discarded at entry.
    ///
    /// Service level matches [`Self::step`]: executed instructions count
    /// into the cumulative stats, a `STOP`ped CPU ends the run early, and a
    /// core panic is caught and surfaced as [`ApiError::Panic`] (so a GUI
    /// driving loop keeps its emu-dead latch semantics).
    pub fn run_until_break(&mut self, max_steps: u64) -> Result<RunOutcome, ApiError> {
        self.run_until_break_interruptible(max_steps, &std::sync::atomic::AtomicBool::new(false))
    }

    /// Like [`Self::run_until_break`], but an external `interrupt` flag ends the
    /// run early (issue #92): a front-end holds the flag *outside* the emulator
    /// lock and raises it (a `pause`) to stop an in-progress run without racing
    /// for the lock. Pass a large `max_steps` for an effectively unbounded run
    /// that only a breakpoint, a `STOP`, or `interrupt` stops. The returned
    /// [`RunOutcome`] has `interrupted == true` when the flag ended it.
    pub fn run_until_break_interruptible(
        &mut self,
        max_steps: u64,
        interrupt: &std::sync::atomic::AtomicBool,
    ) -> Result<RunOutcome, ApiError> {
        use std::sync::atomic::Ordering;
        // Clear any stale raise so this run isn't stopped by a previous pause.
        interrupt.store(false, Ordering::Relaxed);
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        if let Some(bp) = snes.breakpoints.as_mut() {
            bp.take_pending(); // discard stale
        }
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for i in 0..max_steps {
                if snes.cpu.stopped {
                    break;
                }
                // Check the pause flag periodically (every 4096 instructions) —
                // cheap enough to be invisible on the run, responsive enough
                // that a pause returns within microseconds.
                if i & 0xFFF == 0 && interrupt.load(Ordering::Relaxed) {
                    return RunOutcome {
                        steps: i,
                        hit: None,
                        interrupted: true,
                    };
                }
                let cur = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
                if i > 0 {
                    if let Some(hit) = snes.breakpoints.as_ref().and_then(|b| b.check_exec(cur)) {
                        return RunOutcome {
                            steps: i,
                            hit: Some(hit),
                            interrupted: false,
                        };
                    }
                }
                snes.step();
                executed = i + 1;
                if let Some(hit) = snes.breakpoints.as_mut().and_then(|b| b.take_pending()) {
                    return RunOutcome {
                        steps: i + 1,
                        hit: Some(hit),
                        interrupted: false,
                    };
                }
            }
            RunOutcome {
                steps: executed,
                hit: None,
                interrupted: false,
            }
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(out) => {
                self.instructions_executed += out.steps;
                if let Some(hit) = out.hit {
                    // Surface the hit on the Event Viewer overlay (Mesen2's
                    // MarkedBreakpoint category, issue #68). Injected straight
                    // into the completed-frame buffer so it is visible while
                    // paused at the halt (the in-progress frame won't roll
                    // until resume). Sorted insert keeps the list ordered.
                    let (line, hclock) = self.snes.as_ref().map_or((0, 0), |s| {
                        (
                            s.ppu_line,
                            (s.mcycles_in_line).min(u32::from(u16::MAX)) as u16,
                        )
                    });
                    let ev = CapturedEvent::Break {
                        pc: hit.pc,
                        line,
                        hclock,
                    };
                    let key = ev.sort_key();
                    let idx = self
                        .last_frame_events
                        .partition_point(|e| e.sort_key() <= key);
                    self.last_frame_events.insert(idx, ev);
                }
                Ok(out)
            }
            Err(payload) => {
                self.instructions_executed += executed;
                Err(ApiError::Panic(panic_message(&payload)))
            }
        }
    }

    /// Controlled execution (L10): set a CPU register by name — one of
    /// `a/x/y/sp/dp/pc/pb/db/p` (case-insensitive). Unknown name → `BadArg`.
    pub fn set_cpu_register(&mut self, reg: &str, val: u32) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let c = &mut snes.cpu;
        match reg.to_ascii_lowercase().as_str() {
            "a" => c.a = val as u16,
            "x" => c.x = val as u16,
            "y" => c.y = val as u16,
            "sp" | "s" => c.sp = val as u16,
            "dp" | "d" => c.dp = val as u16,
            "pc" => c.pc = val as u16,
            "pb" | "k" => c.pb = val as u8,
            "db" | "dbr" => c.db = val as u8,
            "p" => c.p.0 = val as u8,
            other => return Err(ApiError::BadArg(format!("unknown register `{other}`"))),
        }
        Ok(())
    }

    /// Breakpoint (L7): step until an instruction WRITES `addr_full`
    /// (`bank << 16 | offset`), or `max_steps` elapse. Returns the
    /// `(pc, value)` of the writing instruction, or `None` if not hit.
    pub fn run_until_mem_write(
        &mut self,
        addr_full: u32,
        max_steps: u64,
    ) -> Result<Option<(u32, u8)>, ApiError> {
        self.run_until_mem_access(addr_full, MemEventKind::Write, max_steps)
    }

    /// Breakpoint (L7): like [`Self::run_until_mem_write`] but for READS.
    pub fn run_until_mem_read(
        &mut self,
        addr_full: u32,
        max_steps: u64,
    ) -> Result<Option<(u32, u8)>, ApiError> {
        self.run_until_mem_access(addr_full, MemEventKind::Read, max_steps)
    }

    fn run_until_mem_access(
        &mut self,
        addr_full: u32,
        want: MemEventKind,
        max_steps: u64,
    ) -> Result<Option<(u32, u8)>, ApiError> {
        let bank = (addr_full >> 16) as u8;
        {
            let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
            // Capture this bank's accesses; drained every step so it never
            // overflows. (Re-enabling resets the buffer.)
            snes.enable_mem_trace(1 << 16, Some(bank), None);
        }
        for _ in 0..max_steps {
            {
                let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
                snes.step();
            }
            let events = {
                let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
                snes.take_mem_trace_log()
            };
            if let Some(ev) = events
                .into_iter()
                .find(|ev| ev.addr_full == addr_full && ev.kind == want)
            {
                return Ok(Some((ev.pc_full, ev.value)));
            }
        }
        Ok(None)
    }

    /// Enable APU mailbox (`$2140-$2143`) event logging. Every CPU
    /// read or write of those ports from this point onward is captured
    /// in an in-memory ring buffer that the caller can drain with
    /// [`Emulator::take_mailbox_log`]. Cheap when disabled.
    pub fn enable_mailbox_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_mailbox_log();
        Ok(())
    }

    /// Take ownership of the accumulated mailbox events, resetting the
    /// buffer to empty. Returns an empty `Vec` if logging is disabled
    /// or no events were captured.
    pub fn take_mailbox_log(&mut self) -> Result<Vec<MailboxEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_mailbox_log())
    }

    /// Enable SA-1 MMIO (`$2200-$23FF`) event logging. Every CPU read or
    /// write of an SA-1 register from this point is captured for draining
    /// with [`Emulator::take_sa1_log`]. Cheap when disabled.
    pub fn enable_sa1_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_log();
        Ok(())
    }

    /// Take ownership of the accumulated SA-1 MMIO events, resetting the
    /// buffer. Returns an empty `Vec` if logging is disabled.
    pub fn take_sa1_log(&mut self) -> Result<Vec<Sa1LogEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_log())
    }

    /// Enable SA-1-*side* execution logging (the SA-1's own `$2200-$23FF`
    /// accesses with its PC). Complements [`Emulator::enable_sa1_log`].
    /// No-op for non-SA-1 carts.
    pub fn enable_sa1_side_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_side_log();
        Ok(())
    }

    /// Drain the SA-1-side execution log (empty if disabled / not SA-1).
    pub fn take_sa1_side_log(&mut self) -> Result<Vec<Sa1SideEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_side_log())
    }

    /// Enable a full SA-1 instruction trace (pre-opcode register snapshot
    /// per SA-1 instruction, capped at `max_events`). No-op for non-SA-1.
    pub fn enable_sa1_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_trace(max_events);
        Ok(())
    }

    /// Drain the SA-1 instruction trace (empty if disabled / not SA-1).
    pub fn take_sa1_trace(&mut self) -> Result<Vec<Sa1TraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_trace())
    }

    /// Enable a per-opcode Super FX (GSU) instruction trace (PC + register
    /// file per opcode), for diffing the GSU stream against a reference.
    pub fn enable_superfx_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_superfx_trace(max_events);
        Ok(())
    }

    /// Drain the Super FX instruction trace (empty if disabled / not GSU).
    pub fn take_superfx_trace(&mut self) -> Result<Vec<SuperFxTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_superfx_trace())
    }

    /// Enable a per-opcode SPC700 instruction trace (PC + registers per
    /// opcode), for diffing the SPC700 stream against a Mesen2 reference
    /// (e.g. the SMRPG/CT Akao CPU↔SPC handshake divergence).
    pub fn enable_spc_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_spc_trace(max_events);
        Ok(())
    }

    /// Drain the SPC700 instruction trace (empty if disabled).
    pub fn take_spc_trace(&mut self) -> Result<Vec<Spc700TraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_spc_trace())
    }

    /// Diagnostic: the coprocessor's work RAM (Super FX Game Pak RAM) read
    /// directly, bypassing the SNES-side ownership gating that returns
    /// open-bus while the GSU owns it. `None` if the cart has no coproc RAM.
    /// Lets a front-end compare luna's CPU-prepared GSU inputs vs a reference.
    pub fn coproc_ram(&self) -> Result<Option<Vec<u8>>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(snes.mapper.coproc_ram().map(<[u8]>::to_vec))
    }

    /// Battery-backed cartridge SRAM — the raw contents of a `.srm` file.
    /// Empty if the cart has none. Unlike a save-state (in-run snapshot),
    /// this is the cross-run-persistent battery data, for power-cycle tests.
    pub fn sram(&self) -> Vec<u8> {
        self.snes
            .as_ref()
            .map(|s| s.mapper.sram().to_vec())
            .unwrap_or_default()
    }

    /// Load battery SRAM (e.g. read from a `.srm` file). Copies up to the
    /// cartridge's SRAM size.
    pub fn load_sram(&mut self, data: &[u8]) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.mapper.load_sram(data);
        Ok(())
    }

    /// Nominal display frame rate of the loaded cart's region — NTSC
    /// 60.0988 Hz / PAL 50.007 Hz. Front-ends pace presentation to this so
    /// the emulator runs at real time (defaults to NTSC with no ROM).
    pub fn frame_rate_hz(&self) -> f64 {
        match self.snes.as_ref().map(|s| s.region) {
            Some(luna_cartridge::Region::Pal) => 50.0070,
            _ => 60.0988,
        }
    }

    /// Diagnostic: a full copy of the 128 KiB WRAM (`$7E0000`-`$7FFFFF`).
    /// For byte-level cross-emulator diffing once `wram_page_hashes` has
    /// localised the first diverging frame + page.
    pub fn wram_snapshot(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(snes.wram.to_vec())
    }

    /// Diagnostic: per-page FNV-1a hashes of the 128 KiB WRAM (`$7E0000`-
    /// `$7FFFFF`), one `u64` per `page_size` bytes (default 4 KiB → 32
    /// pages). Frame-aligned (NMI-count) hashing of these is the
    /// confound-free way to bisect a CPU-state divergence vs a reference
    /// emulator: WRAM-at-vblank-N is the same game-frame in both, so the
    /// first diverging page pins the first real divergence — unlike scene-
    /// level windows which the boot-frame offset confounds. `page_size` must
    /// be a power of two that divides `0x20000`.
    pub fn wram_page_hashes(&self, page_size: usize) -> Result<Vec<u64>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let ps = if page_size == 0 { 0x1000 } else { page_size };
        assert!(
            ps.is_power_of_two() && 0x2_0000 % ps == 0,
            "page_size must be a power of two dividing 0x20000"
        );
        Ok(snes
            .wram
            .chunks_exact(ps)
            .map(|page| {
                // FNV-1a 64-bit
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for &b in page {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
                h
            })
            .collect())
    }

    /// Enable the DMA→VRAM transfer-time trace: every byte an MDMA writes
    /// to `$2118/$2119` is captured as (source A-bus address → VMADD word
    /// → byte) AT transfer time. Lets a coprocessor framebuffer
    /// (Super FX) be checked against the VRAM it produced without the
    /// double-buffer confound of a post-hoc source dump.
    pub fn enable_dma_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.dma.enable_dma_trace(max_events);
        Ok(())
    }

    /// Drain the DMA→VRAM trace (empty if disabled).
    pub fn take_dma_trace(&mut self) -> Result<Vec<DmaTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.dma.take_dma_trace())
    }

    /// Dump all 64 KB of PPU VRAM (byte-addressed). For diagnosing the
    /// framebuffer DMA → VRAM → display path of coprocessor renderers.
    pub fn vram_bytes(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok((0u32..0x1_0000)
            .map(|a| snes.ppu.vram.peek(a as u16))
            .collect())
    }

    /// All 64 KB of APU audio RAM (ARAM), for diagnosing the SPC700 sound
    /// driver — e.g. comparing the CPU↔SPC700 `$2140-$2143` handshake or
    /// disassembling a stuck driver against a reference.
    pub fn aram_bytes(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(snes.apu_real.aram.to_vec())
    }

    /// Export the current APU state as a 66 048-byte `.spc` sound file
    /// (`SNES-SPC700 Sound File Data v0.30`): the live SPC700 registers,
    /// all 64 KB of ARAM, the 128 DSP registers, and the IPL ROM — the
    /// exact snapshot any SPC player needs to resume playback. Capture it
    /// once the game's music driver is running (step far enough in, and
    /// inject Start to pass title screens). A minimal ID666 text tag is
    /// filled from the cartridge title.
    pub fn export_spc(&self) -> Result<Vec<u8>, ApiError> {
        // Null-pad a UTF-8 string into a fixed-width ID666 text field.
        fn put_text(dst: &mut [u8], s: &str) {
            let n = s.len().min(dst.len());
            dst[..n].copy_from_slice(&s.as_bytes()[..n]);
        }

        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let apu = &snes.apu_real;
        let mut buf = vec![0u8; 0x1_0200];
        // 0x00: 33-byte signature, then 0x1A 0x1A.
        buf[0x00..0x21].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
        buf[0x21] = 0x1A;
        buf[0x22] = 0x1A;
        buf[0x23] = 0x1A; // ID666 tag present (text format)
        buf[0x24] = 30; // minor version

        // 0x25: SPC700 register file at the dump moment.
        let c = &apu.cpu;
        buf[0x25..0x27].copy_from_slice(&c.pc.to_le_bytes());
        buf[0x27] = c.a;
        buf[0x28] = c.x;
        buf[0x29] = c.y;
        buf[0x2A] = c.psw.0;
        buf[0x2B] = c.sp;
        // 0x2C..0x2E reserved (zero).

        // 0x2E: ID666 text tag.
        let title = self.rom_info.as_ref().map_or("", |r| r.title.trim_end());
        put_text(&mut buf[0x2E..0x4E], title); // song title
        put_text(&mut buf[0x4E..0x6E], title); // game title
        put_text(&mut buf[0x6E..0x7E], "luna"); // dumper
        put_text(&mut buf[0xA9..0xAC], "180"); // seconds before fade
        put_text(&mut buf[0xAC..0xB1], "10000"); // fade length (ms)
        // 0xD2 emulator used: 0 = unknown.

        // 0x100: 64 KB ARAM. 0x10100: 128 DSP registers.
        buf[0x100..0x1_0100].copy_from_slice(&apu.aram[..]);
        buf[0x1_0100..0x1_0180].copy_from_slice(&apu.dsp.registers);
        // 0x10180..0x101C0 unused. 0x101C0: 64-byte IPL ROM.
        buf[0x1_01C0..0x1_0200].copy_from_slice(&luna_cpu_spc700::IPL_ROM);
        Ok(buf)
    }

    /// Enable per-instruction CPU tracing. Every subsequent
    /// [`Emulator::step`] / [`Emulator::step_until_frame`] tick
    /// captures a register-file snapshot until `max_events` events
    /// have been recorded. Drain with [`Emulator::take_cpu_trace_log`].
    pub fn enable_cpu_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_cpu_trace(max_events);
        Ok(())
    }

    /// Take ownership of the accumulated CPU trace events.
    pub fn take_cpu_trace_log(&mut self) -> Result<Vec<CpuTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_cpu_trace_log())
    }

    /// Current cumulative instruction count since the last reset /
    /// rom-load. Used by the CLI to gate "start tracing at instr N".
    #[must_use]
    pub const fn instructions_executed(&self) -> u64 {
        self.instructions_executed
    }

    /// Enable per-access memory tracing. Every CPU bus read/write
    /// from this point matching `bank_filter` (or every access when
    /// `None`) is captured into the log until `max_events` is
    /// reached. Drain with [`Emulator::take_mem_trace_log`].
    pub fn enable_mem_trace(
        &mut self,
        max_events: usize,
        bank_filter: Option<u8>,
        offset_filter: Option<(u16, u16)>,
    ) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_mem_trace(max_events, bank_filter, offset_filter);
        Ok(())
    }

    /// Take ownership of the accumulated memory access events.
    pub fn take_mem_trace_log(&mut self) -> Result<Vec<MemTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_mem_trace_log())
    }

    // --- Event Viewer (faithful Mesen2 port; see `event_viewer` module) ---

    /// Turn Event Viewer capture on or off. When on, the emulator records the
    /// register IO range (`$2100-$437F`) plus NMI/IRQ markers; the GUI panel
    /// drains them per frame via [`Emulator::swap_frame_events`].
    pub fn enable_event_capture(&mut self, on: bool) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        if on {
            snes.enable_mem_trace(16_384, None, Some((0x2100, 0x437F)));
            // Also capture DMA B-bus writes (OAM/VRAM/CGRAM DMA) — the CPU
            // mem-trace never sees them (the CPU is halted during a burst),
            // so without this the overlay is sparse vs Mesen2.
            snes.dma.enable_dma_trace(16_384);
        } else {
            snes.disable_mem_trace();
            snes.dma.dma_trace = None;
            self.last_frame_events.clear();
            self.prev_frame_events.clear();
        }
        Ok(())
    }

    /// Roll the events captured since the last call into the completed-frame
    /// buffers (Mesen2's `ClearFrameEvents`: `_prevDebugEvents = _debugEvents`).
    /// The emu loop calls this once per emulated frame. A frame with no captured
    /// events keeps the previous set (avoids flicker on quiet frames).
    pub fn swap_frame_events(&mut self) {
        let Some(snes) = self.snes.as_mut() else {
            return;
        };
        // Drain BOTH traces: CPU IO accesses (mem-trace) and DMA B-bus
        // writes (dma-trace). The CPU is halted during a DMA burst, so DMA
        // register writes never appear in the mem-trace — merging the two is
        // what makes the overlay dense (OAM/VRAM DMA show up).
        let cpu = snes.take_mem_trace_log();
        let dma = snes.dma.take_dma_trace();
        if cpu.is_empty() && dma.is_empty() {
            // Quiet frame: keep the previous set (avoids overlay flicker).
            return;
        }
        let mut merged: Vec<CapturedEvent> = Vec::with_capacity(cpu.len() + dma.len());
        merged.extend(cpu.into_iter().map(CapturedEvent::Cpu));
        merged.extend(dma.into_iter().map(CapturedEvent::Dma));
        // Order by PPU position so the list/overlay are sequential.
        merged.sort_by_key(CapturedEvent::sort_key);
        std::mem::swap(&mut self.prev_frame_events, &mut self.last_frame_events);
        self.last_frame_events = merged;
    }

    /// The current Event Viewer snapshot: the just-finished frame's visible
    /// events, plus (when `show_previous_frame` is on) the previous frame's,
    /// each decoded + filtered by the category visibility mask.
    #[must_use]
    pub fn event_snapshot(&self) -> Vec<EventViewerEvent> {
        let mut out = Vec::with_capacity(self.last_frame_events.len());
        for ev in &self.last_frame_events {
            if let Some(e) = ev.decode(&self.event_config, false) {
                out.push(e);
            }
        }
        if self.event_config.show_previous_frame {
            // Mesen2 `BaseEventManager::FilterEvents` (`:9-22`): include a
            // previous-frame event only when its `(scanline<<16)+cycle` is
            // GREATER than the cursor, so the prev frame fills in the part of
            // the frame the current one has not reached yet (no duplication).
            // luna polls at the frame boundary (no pause), so the live cursor
            // is the last current-frame event's position — i.e. prev-frame
            // events later than that fill the tail of the frame.
            let cursor = self
                .last_frame_events
                .iter()
                .map(CapturedEvent::position_key)
                .max()
                .unwrap_or(0);
            for ev in &self.prev_frame_events {
                if ev.position_key() <= cursor {
                    continue;
                }
                if let Some(e) = ev.decode(&self.event_config, true) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Read the Event Viewer config (category/DMA visibility, previous-frame).
    #[must_use]
    pub const fn event_config(&self) -> &EventViewerConfig {
        &self.event_config
    }

    /// Mutate the Event Viewer config (the GUI toggles category/DMA visibility).
    pub const fn event_config_mut(&mut self) -> &mut EventViewerConfig {
        &mut self.event_config
    }

    /// Enable capture of `$21FC` Nocash-TTY writes (the SDK debug channel
    /// behind `SNES_NOCASH` / `SNES_ASSERT`). A headless harness can then read
    /// the program's own log / assertion output without a GUI debugger.
    pub fn enable_nocash_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_nocash_log();
        Ok(())
    }

    /// Drain the captured `$21FC` Nocash byte stream.
    pub fn take_nocash_log(&mut self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_nocash_log())
    }

    /// Enable capture of `WDM` (`$42`) executions — the SDK breakpoint /
    /// assert channel (`SNES_ASSERT` → `WDM $00`). Complements the `$21FC`
    /// Nocash log: `$21FC` carries text, `WDM` carries the binary
    /// "assertion fired here" signal `(pc_full, operand)`.
    pub fn enable_wdm_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.cpu.enable_wdm_log();
        Ok(())
    }

    /// Drain the captured `WDM` events as `(pc_full, operand)` per hit.
    pub fn take_wdm_log(&mut self) -> Result<Vec<(u32, u8)>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.cpu.take_wdm_log())
    }

    /// Direct read of the SPC700's ARAM. Read-only, no bus side
    /// effects.
    pub fn peek_aram(&self, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            out.push(snes.apu_real.aram[offset.wrapping_add(i) as usize]);
        }
        Ok(out)
    }

    /// Direct read of PPU VRAM. `offset` is a *byte* address (0..0xFFFF
    /// — VRAM is 64 KB), `count` how many consecutive bytes to read.
    /// Read-only, no bus side effects.
    pub fn peek_vram(&self, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            out.push(snes.ppu.vram.peek(offset.wrapping_add(i)));
        }
        Ok(out)
    }

    /// All 256 CGRAM entries as raw BGR555 words (index 0 = backdrop).
    /// Cheap, read-only — the Palette Viewer's per-frame source, avoiding
    /// the full `state()` VRAM occupancy scan.
    pub fn peek_cgram(&self) -> Result<Vec<u16>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok((0..256u16).map(|i| snes.ppu.cgram.color(i as u8)).collect())
    }

    /// All 544 OAM bytes (512-byte low table + 32-byte high table) as raw
    /// bytes (issue #89). Cheap, read-only — the same bytes `state().ppu.
    /// oam_full` exposes, without the rest of the `state()` snapshot. The
    /// per-frame source for a sprite/OAM viewer.
    pub fn peek_oam(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok((0..0x220u16).map(|i| snes.ppu.oam.peek(i)).collect())
    }

    /// Render BG `bg_idx` (0..3)'s full tilemap to an RGBA image for the
    /// GUI Tilemap Viewer. Debug render — ignores scroll/priority/blank,
    /// shows raw palette colours, but honours per-tile flip + bases. In
    /// Mode 7 `bg_idx` is ignored and the 128×128 field is rendered.
    pub fn render_tilemap_rgba(&self, bg_idx: usize) -> Result<TilemapImage, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(luna_ppu::render_bg_tilemap(&snes.ppu, bg_idx))
    }

    /// PNG-encode BG `bg_idx`'s tilemap (the [`render_tilemap_rgba`]
    /// image). For `luna assets-dump` / a tilemap viewer's "export".
    ///
    /// [`render_tilemap_rgba`]: Self::render_tilemap_rgba
    pub fn render_tilemap_png(&self, bg_idx: usize) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        tilemap_to_png(&luna_ppu::render_bg_tilemap(&snes.ppu, bg_idx))
    }

    /// PNG of the whole 64 KB of VRAM decoded as a 16-wide tile sheet at
    /// `bpp` (2/4/8) using CGRAM sub-palette `palette_row`. An asset rip
    /// of every tile currently loaded.
    pub fn render_vram_tiles_png(&self, bpp: u8, palette_row: u8) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        tilemap_to_png(&luna_ppu::render_vram_tiles(&snes.ppu, bpp, palette_row))
    }

    /// PNG of the 256-colour CGRAM as a 16×16 swatch grid (`cell` px per
    /// swatch).
    pub fn render_palette_png(&self, cell: u32) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        tilemap_to_png(&luna_ppu::render_cgram_palette(&snes.ppu, cell))
    }

    /// PNG sprite sheet: all 128 OAM sprites decoded at native size with
    /// their OBJ palettes, transparent where index 0. RGBA (with alpha).
    pub fn render_sprite_sheet_png(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        tilemap_to_png(&luna_ppu::render_obj_sheet(&snes.ppu))
    }

    /// The BG `bg_idx` bits-per-pixel for the current BGMODE (2/4/8; 0 if
    /// the layer is disabled in this mode). Lets a consumer pick a
    /// sensible default `bpp` for [`render_vram_tiles_png`].
    ///
    /// [`render_vram_tiles_png`]: Self::render_vram_tiles_png
    pub fn bg_bpp(&self, bg_idx: usize) -> Result<u8, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(luna_ppu::bg_bpp(snes.ppu.bgmode, bg_idx))
    }
}

/// PNG-encode a [`TilemapImage`]'s RGBA buffer (with alpha — sprite
/// sheets carry transparency).
fn tilemap_to_png(img: &TilemapImage) -> Result<Vec<u8>, ApiError> {
    let buf = image::RgbaImage::from_raw(img.width, img.height, img.rgba.clone())
        .expect("rgba length matches width*height*4");
    let mut out = Vec::new();
    image::DynamicImage::from(buf)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

const fn default_cpu_state() -> CpuState {
    CpuState {
        a: 0,
        x: 0,
        y: 0,
        sp: 0,
        pc: 0,
        pb: 0,
        db: 0,
        dp: 0,
        p: 0,
        e: false,
        stopped: false,
        waiting: false,
    }
}

const fn default_ppu_state() -> PpuState {
    PpuState {
        inidisp: 0,
        bgmode: 0,
        vram_addr_words: 0,
        inidisp_write_count: 0,
        backdrop: 0,
        obsel: 0,
        vram_non_zero: 0,
        cgram_non_zero: 0,
        oam_non_zero: 0,
        oam_low_excerpt: Vec::new(),
        oam_high_excerpt: Vec::new(),
        tm: 0,
        ts: 0,
        tmw: 0,
        tsw: 0,
        cgwsel: 0,
        cgadsub: 0,
        coldata_r: 0,
        coldata_g: 0,
        coldata_b: 0,
        setini: 0,
        w12sel: 0,
        w34sel: 0,
        wobjsel: 0,
        wbglog: 0,
        wobjlog: 0,
        windows: [0; 4],
        bgs: [BgInfo {
            tilemap_addr_words: 0,
            char_addr_words: 0,
            h_scroll: 0,
            v_scroll: 0,
            tilemap_size: 0,
        }; 4],
        cgram: Vec::new(),
        oam_full: Vec::new(),
        mosaic: 0,
        m7sel: 0,
        m7a: 0,
        m7b: 0,
        m7c: 0,
        m7d: 0,
        m7x: 0,
        m7y: 0,
        m7_hofs: 0,
        m7_vofs: 0,
        mpy: 0,
        stat77: 0,
        stat78: 0,
        ophct: 0,
        opvct: 0,
    }
}

const fn default_cpu_regs_state() -> CpuRegsState {
    CpuRegsState {
        nmitimen: 0,
        hvbjoy: 0,
        nmi_flag: false,
        irq_flag: false,
        wrio: 0,
        wrmpya: 0,
        wrmpyb: 0,
        wrdiv: 0,
        wrdvdd: 0,
        htime: 0,
        vtime: 0,
        rdmpy: 0,
        rddiv: 0,
        joy1: 0,
        joy2: 0,
        memsel: 0,
    }
}

const fn default_dma_state() -> DmaState {
    DmaState {
        channels: [DmaChannelState {
            params: 0,
            bbad: 0,
            a_addr: 0,
            a_bank: 0,
            das: 0,
            dasb: 0,
            a2a: 0,
            ntlr: 0,
        }; 8],
        mdmaen: 0,
        hdmaen: 0,
    }
}

const fn default_scheduler_state() -> SchedulerState {
    SchedulerState {
        ppu_line: 0,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
    }
}

fn default_apu_state() -> ApuState {
    ApuState {
        spc_pc: 0,
        spc_stopped: false,
        past_iplrom: false,
        to_cpu_ports: [0; 4],
        to_spc_ports: [0; 4],
        mvol_l: 0,
        mvol_r: 0,
        kon: 0,
        endx: 0,
        active_voices: 0,
        audio_queue_len: 0,
        last_audio_sample: (0, 0),
        dsp_regs: vec![0; 128],
        dir_excerpt: vec![0; 64],
        voice_active: [false; 8],
        voice_phase: std::array::from_fn(|_| "Off".to_string()),
        voice_envelope: [0; 8],
        voice_block_addr: [0; 8],
        voice_brr_dump: vec![vec![0; 36]; 8],
        voice_brr_history: vec![vec![0; 4]; 8],
        voice_pitch_acc: [0; 8],
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(unknown panic payload)".to_string()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 32 KB `LoROM` cart for tests. Has a valid reset
    /// vector + cartridge-checksum so the parser accepts it.
    fn demo_lorom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // Reset vector at LoROM $00:FFFC = ROM offset $7FFC → $8000.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Title at $7FC0..$7FD4 (21 bytes ASCII, space-padded).
        let title = b"LUNA API TEST DEMO   ";
        rom[0x7FC0..0x7FC0 + title.len()].copy_from_slice(title);
        // Map mode byte at $7FD5 = $20 (LoROM, slow).
        rom[0x7FD5] = 0x20;
        // ROM size byte at $7FD7 = $07 (1 << 7 = 128 KB).
        rom[0x7FD7] = 0x07;
        // SRAM byte at $7FD8 = 0 (no SRAM).
        rom[0x7FD8] = 0x00;
        // Compute checksum + complement.
        let mut sum = 0u32;
        for (i, b) in rom.iter().enumerate() {
            if !(0x7FDC..=0x7FDF).contains(&i) {
                sum += u32::from(*b);
            }
        }
        let checksum = (sum & 0xFFFF) as u16;
        let complement = !checksum;
        rom[0x7FDC] = complement as u8;
        rom[0x7FDD] = (complement >> 8) as u8;
        rom[0x7FDE] = checksum as u8;
        rom[0x7FDF] = (checksum >> 8) as u8;
        rom
    }

    #[test]
    fn debug_poke_search_run_until_set_register() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();

        // L8 poke + L9 search round-trip through WRAM.
        assert_eq!(
            e.poke_memory(0x7E, 0x0100, &[0xDE, 0xAD, 0xBE, 0xEF])
                .unwrap(),
            4
        );
        assert_eq!(
            e.peek_memory(0x7E, 0x0100, 4).unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        // low-RAM mirror writes the same bytes ($00:0100 → $7E:0100).
        assert_eq!(e.peek_memory(0x00, 0x0100, 2).unwrap(), vec![0xDE, 0xAD]);
        assert!(
            e.search_memory(&[0xDE, 0xAD, 0xBE, 0xEF])
                .unwrap()
                .contains(&0x7E_0100)
        );
        assert!(e.search_memory(&[]).unwrap().is_empty());

        // L10 set_cpu_register + run_until_pc.
        e.set_cpu_register("pb", 0x00).unwrap();
        e.set_cpu_register("pc", 0x9000).unwrap();
        e.set_cpu_register("a", 0x1234).unwrap();
        assert!(e.set_cpu_register("bogus", 0).is_err());
        // Already at $00:9000 → run_until returns immediately.
        assert!(e.run_until_pc(0x00_9000, 10).unwrap());
        // A PC we won't reach within 1 step from here is not hit.
        assert!(!e.run_until_pc(0x12_3456, 1).unwrap());

        // L7: a memory-write breakpoint on an address the boot code never
        // touches returns None within the step budget (no panic, no hang).
        assert_eq!(e.run_until_mem_write(0x7E_FFFE, 50).unwrap(), None);
        assert_eq!(e.run_until_mem_read(0x7E_FFFE, 50).unwrap(), None);
    }

    /// WLA-DX symbols (issue #67): parse, resolve, annotate the
    /// disassembly, and drive the address-taking APIs by name.
    #[test]
    fn symbol_table_resolves_and_annotates() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();

        // Same injected loop as the breakpoint test, now with names.
        e.poke_memory(
            0x7E,
            0x0100,
            &[0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
        )
        .unwrap();
        let n =
            e.load_symbols_str("[labels]\n00:0100 main\n00:0105 main_jump\n00:0200 monster_x\n");
        assert_eq!(n, 3);
        assert_eq!(e.resolve_symbol("main"), Some(0x00_0100));
        assert_eq!(e.resolve_symbol("nope"), None);
        assert_eq!(e.symbol_for_addr(0x00_0102).as_deref(), Some("main+0x02"));

        // Disassembly lines carry the nearest label.
        e.set_cpu_register("pb", 0x00).unwrap();
        e.set_cpu_register("pc", 0x0100).unwrap();
        let lines = e.disassemble_cpu(0x00_0100, 3, true, true).unwrap();
        assert_eq!(lines[0].symbol.as_deref(), Some("main"));
        assert_eq!(lines[1].symbol.as_deref(), Some("main+0x02"));
        assert_eq!(lines[2].symbol.as_deref(), Some("main_jump"));

        // Symbol-driven control flow: resolve + the existing typed APIs.
        let target = e.resolve_symbol("main_jump").unwrap();
        assert!(e.run_until_pc(target, 10).unwrap());

        // A watchpoint at a named WRAM address reports the write.
        let wp_addr = e.resolve_symbol("monster_x").unwrap();
        e.bp_add_mem(wp_addr, wp_addr, false, true).unwrap();
        let out = e.run_until_break(20).unwrap();
        assert_eq!(out.hit.unwrap().addr, Some(0x00_0200));

        e.clear_symbols();
        assert_eq!(e.resolve_symbol("main"), None);
        let lines = e.disassemble_cpu(0x00_0100, 1, true, true).unwrap();
        assert!(lines[0].symbol.is_none());
    }

    /// Breakpoint registry (issue #66): exec breakpoints halt-at-speed
    /// with resume semantics, watchpoints report the exact accessing
    /// instruction, and the registry lifecycle works end-to-end.
    #[test]
    fn breakpoint_registry_halts_at_speed() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();

        // Inject a tiny loop at $00:0100 (WRAM mirror):
        //   0100: A9 42     LDA #$42
        //   0102: 8D 00 02  STA $0200
        //   0105: 4C 00 01  JMP $0100
        e.poke_memory(
            0x7E,
            0x0100,
            &[0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
        )
        .unwrap();
        e.set_cpu_register("pb", 0x00).unwrap();
        e.set_cpu_register("pc", 0x0100).unwrap();
        e.set_cpu_register("db", 0x00).unwrap();

        // No registry installed: the run completes its budget, hit = None,
        // and the instructions count into the cumulative stats (same
        // service level as `step` — the GUI loop depends on this).
        let before = e.instructions_executed();
        let out = e.run_until_break(6).unwrap();
        assert_eq!((out.steps, out.hit.is_none()), (6, true));
        assert_eq!(e.instructions_executed(), before + 6);

        // --- exec breakpoint: halts BEFORE the instruction at the target.
        e.set_cpu_register("pc", 0x0100).unwrap();
        let bp_jmp = e.bp_add_exec(0x00_0105).unwrap();
        let out = e.run_until_break(100).unwrap();
        let hit = out.hit.expect("exec bp hit");
        assert_eq!(hit.kind, BreakKind::Exec);
        assert_eq!((hit.id, hit.pc), (bp_jmp, 0x00_0105));
        assert_eq!(out.steps, 2, "LDA + STA executed, JMP not yet");
        assert_eq!(e.cpu_state().unwrap().pc, 0x0105);

        // Resume semantics: calling again moves PAST the breakpoint and
        // comes back around the loop to hit it again.
        let out = e.run_until_break(100).unwrap();
        assert_eq!(out.hit.unwrap().pc, 0x00_0105);
        assert_eq!(out.steps, 3, "JMP + LDA + STA this time");

        // --- memory watchpoint: exact accessing instruction reported.
        assert!(e.bp_remove(bp_jmp).unwrap());
        let bp_w = e.bp_add_mem(0x00_0200, 0x00_0200, false, true).unwrap();
        e.set_cpu_register("pc", 0x0100).unwrap();
        let out = e.run_until_break(100).unwrap();
        let hit = out.hit.expect("watchpoint hit");
        assert_eq!(hit.kind, BreakKind::Write);
        assert_eq!(hit.id, bp_w);
        assert_eq!(hit.addr, Some(0x00_0200));
        assert_eq!(hit.value, Some(0x42));
        assert_eq!(hit.pc, 0x00_0102, "the STA instruction did the write");
        assert_eq!(out.steps, 2, "halts after the accessing instruction");

        // --- registry lifecycle + argument validation.
        assert_eq!(e.bp_list().unwrap().len(), 1);
        assert!(e.bp_add_mem(0x10, 0x00, true, true).is_err(), "lo > hi");
        assert!(
            e.bp_add_mem(0x00, 0x10, false, false).is_err(),
            "neither read nor write"
        );
        e.bp_clear().unwrap();
        assert!(e.bp_list().unwrap().is_empty());
        // Cleared registry: the loop runs to its budget again.
        let out = e.run_until_break(10).unwrap();
        assert!(out.hit.is_none());

        // A hit surfaces on the Event Viewer as a MarkedBreakpoint event
        // (issue #68) — visible immediately while paused at the halt.
        e.set_cpu_register("pc", 0x0100).unwrap();
        e.bp_add_exec(0x00_0105).unwrap();
        let out = e.run_until_break(100).unwrap();
        assert!(out.hit.is_some());
        assert!(
            e.event_snapshot().iter().any(|ev| ev.category
                == event_viewer::EventCategory::MarkedBreakpoint
                && ev.pc == 0x00_0105),
            "the hit is injected into the Event Viewer buffer"
        );
    }

    #[test]
    fn mem_trace_offset_filter_and_frame_blank_columns() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        // L14: capture only the stack page ($0100-$01FF). The boot BRK loop
        // pushes to the stack, so this catches real accesses while skipping
        // the ROM code fetches.
        e.enable_mem_trace(10_000, None, Some((0x0100, 0x01FF)))
            .unwrap();
        e.step(4_000).unwrap();
        let evs = e.take_mem_trace_log().unwrap();
        assert!(
            !evs.is_empty(),
            "offset filter should still capture stack accesses"
        );
        for ev in &evs {
            // L14: every captured offset is inside the filter window.
            let off = (ev.addr_full & 0xFFFF) as u16;
            assert!(
                (0x0100..=0x01FF).contains(&off),
                "offset {off:#06X} escaped the filter"
            );
            // L13: the per-access line is a valid scanline and `blank` agrees
            // with it (NTSC vblank starts at line 225).
            assert!(ev.line < 262, "scanline {} out of range", ev.line);
            assert_eq!(ev.blank, ev.line >= 225, "blank flag must track vblank");
        }
    }

    #[test]
    fn fresh_emulator_has_no_rom() {
        let e = Emulator::new();
        assert!(!e.has_rom());
    }

    #[test]
    fn save_state_requires_a_rom() {
        let e = Emulator::new();
        assert!(matches!(e.save_state(), Err(ApiError::NoRom)));
        let mut e2 = Emulator::new();
        assert!(matches!(e2.load_state(&[]), Err(ApiError::NoRom)));
    }

    #[test]
    fn load_state_rejects_a_wrong_rom() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(200_000).unwrap();
        let saved = e.save_state().expect("save");

        // Fresh emulator with the SAME ROM accepts it.
        let mut ok = Emulator::new();
        ok.load_rom_bytes(demo_lorom()).unwrap();
        assert!(ok.load_state(&saved).is_ok());

        // A garbage blob is refused, not panicked on.
        assert!(matches!(
            ok.load_state(&[1, 2, 3, 4]),
            Err(ApiError::SaveState(_))
        ));
    }

    #[test]
    fn save_state_round_trip_rewinds_and_stays_deterministic() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();

        // Run a few frames, then snapshot.
        for _ in 0..3 {
            e.step_until_frame(1_000_000).unwrap();
        }
        let saved = e.save_state().expect("save");
        let hash_at_save = e.framebuffer_hash().expect("hash");
        let cpu_at_save = e.cpu_state().expect("cpu");

        // Run further so the machine has visibly advanced past the save.
        for _ in 0..3 {
            e.step_until_frame(1_000_000).unwrap();
        }
        let hash_after_more = e.framebuffer_hash().expect("hash");
        let cpu_after_more = e.cpu_state().expect("cpu");

        // Loading the state rewinds to exactly the save point.
        e.load_state(&saved).expect("load");
        assert_eq!(
            e.framebuffer_hash().expect("hash"),
            hash_at_save,
            "loading a state must restore the framebuffer hash captured at save time"
        );
        assert_eq!(
            e.cpu_state().expect("cpu").pc,
            cpu_at_save.pc,
            "loading a state must restore the CPU PC captured at save time"
        );

        // Determinism: re-running the same number of frames from the restored
        // point reproduces the post-save run bit-for-bit.
        for _ in 0..3 {
            e.step_until_frame(1_000_000).unwrap();
        }
        assert_eq!(
            e.framebuffer_hash().expect("hash"),
            hash_after_more,
            "replaying from a restored state must reproduce the same frame"
        );
        assert_eq!(e.cpu_state().expect("cpu").pc, cpu_after_more.pc);
    }

    #[test]
    fn load_rom_bytes_populates_rom_info() {
        let mut e = Emulator::new();
        let info = e.load_rom_bytes(demo_lorom()).expect("load");
        assert_eq!(info.title.trim_end(), "LUNA API TEST DEMO");
        assert_eq!(info.mapper, "LoRom");
        assert!(e.has_rom());
    }

    #[test]
    fn step_advances_instruction_count() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let n = e.step(50).expect("step");
        assert!(n > 0, "should execute at least one instruction");
        assert_eq!(e.state().stats.instructions_executed, n);
    }

    #[test]
    fn export_spc_has_valid_header_and_embeds_aram_dsp_iplrom() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step(2000).expect("step");
        let spc = e.export_spc().expect("export_spc");

        // Exact v0.30 file size and signature.
        assert_eq!(spc.len(), 0x1_0200);
        assert_eq!(&spc[0x00..0x21], b"SNES-SPC700 Sound File Data v0.30");
        assert_eq!(&spc[0x21..0x23], &[0x1A, 0x1A]);
        assert_eq!(spc[0x23], 0x1A, "ID666 tag present");

        // SPC700 register block matches the live state.
        let s = e.spc700_state().expect("spc700");
        assert_eq!(u16::from_le_bytes([spc[0x25], spc[0x26]]), s.pc);
        assert_eq!(spc[0x27], s.a);
        assert_eq!(spc[0x2B], s.sp);

        // Game-title ID666 field carries the cartridge title.
        assert!(spc[0x4E..0x6E].starts_with(b"LUNA API TEST DEMO"));

        // Payload regions match the dedicated accessors / the IPL ROM.
        assert_eq!(&spc[0x100..0x1_0100], e.aram_bytes().unwrap().as_slice());
        assert_eq!(&spc[0x1_01C0..0x1_0200], &luna_cpu_spc700::IPL_ROM);
    }

    #[test]
    fn step_until_frame_returns_when_frame_count_changes_or_caps() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let n = e.step_until_frame(1_000_000).expect("step_until_frame");
        let s = e.state();
        assert!(n > 0);
        assert!(s.scheduler.frame_count >= 1 || n == 1_000_000);
    }

    #[test]
    fn state_serialises_to_json() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let s = e.state();
        let json = serde_json::to_string(&s).expect("serialise");
        assert!(json.contains("\"rom\""));
        assert!(json.contains("\"cpu\""));
        assert!(json.contains("\"apu\""));
    }

    #[test]
    fn no_rom_returns_no_rom_error() {
        let mut e = Emulator::new();
        let err = e.step(1).unwrap_err();
        assert!(matches!(err, ApiError::NoRom));
    }

    #[test]
    fn peek_memory_reads_through_the_bus() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        // Reset vector at $00:FFFC..$FFFD should map to ROM bytes
        // $00, $80.
        let bytes = e.peek_memory(0x00, 0xFFFC, 2).unwrap();
        assert_eq!(bytes, vec![0x00, 0x80]);
    }

    #[test]
    fn render_frame_png_round_trips_via_image_crate() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let png = e.render_frame_png(false).expect("png");
        assert!(png.starts_with(b"\x89PNG"), "header should be PNG magic");
    }

    #[test]
    fn framebuffer_hash_is_deterministic_and_matches_rgba_pixels() {
        let mut e = Emulator::new();
        assert!(matches!(e.framebuffer_hash(), Err(ApiError::NoRom)));
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(1_000_000).unwrap();
        let h1 = e.framebuffer_hash().expect("hash");
        // Pure function of state: identical when nothing changed.
        assert_eq!(h1, e.framebuffer_hash().unwrap(), "hash must be stable");
        // It hashes the same displayed pixels render_frame_rgba emits: an
        // independent hash of the RGB channels of that buffer agrees.
        let rgba = e.render_frame_rgba(false).unwrap();
        let mut ref_h = std::collections::hash_map::DefaultHasher::new();
        let rgb: Vec<[u8; 3]> = rgba.chunks_exact(4).map(|c| [c[0], c[1], c[2]]).collect();
        std::hash::Hash::hash(&rgb[..], &mut ref_h);
        assert_eq!(
            h1,
            std::hash::Hasher::finish(&ref_h),
            "hashes the displayed RGB"
        );
    }

    #[test]
    fn frame_hash_is_deterministic_and_matches_render_frame_rgba() {
        let mut e = Emulator::new();
        assert!(matches!(e.frame_hash(false), Err(ApiError::NoRom)));
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(1_000_000).unwrap();
        let h = e.frame_hash(false).expect("hash");
        // Pure function of state — stable across calls (the property a
        // cross-arch baseline relies on).
        assert_eq!(h, e.frame_hash(false).unwrap(), "frame_hash must be stable");
        // It is exactly a fixed-seed hash of the displayed RGBA bytes.
        let rgba = e.render_frame_rgba(false).unwrap();
        let mut ref_h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hasher::write(&mut ref_h, &rgba);
        assert_eq!(h, std::hash::Hasher::finish(&ref_h));
    }

    #[test]
    fn input_capture_records_only_changes_keyed_by_frame() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(1_000_000).unwrap();
        let f0 = e.frame_count().unwrap();

        e.start_input_capture();
        assert!(e.is_capturing_input());

        // Idle baseline (mask 0) at f0 adds nothing; press Start (bit 12)…
        e.set_joypad(0, 0x1000).unwrap();
        // …the same mask again is de-duped (a held button = one entry).
        e.set_joypad(0, 0x1000).unwrap();

        e.step_until_frame(1_000_000).unwrap();
        let f1 = e.frame_count().unwrap();
        e.set_joypad(0, 0).unwrap(); // release P1
        e.set_joypad(1, 0x8000).unwrap(); // P2 presses B

        let entries = e.take_input_capture();
        assert!(!e.is_capturing_input(), "take stops the capture");
        assert_eq!(
            entries,
            vec![
                InputCaptureEntry {
                    frame: f0,
                    port: 0,
                    mask: 0x1000
                },
                InputCaptureEntry {
                    frame: f1,
                    port: 0,
                    mask: 0
                },
                InputCaptureEntry {
                    frame: f1,
                    port: 1,
                    mask: 0x8000
                },
            ]
        );
        // Per-port scripts round-trip into the `--input` replay format.
        assert_eq!(
            input_capture_to_script(&entries, 0),
            format!("{f0}:0x1000,{f1}:0x0000")
        );
        assert_eq!(input_capture_to_script(&entries, 1), format!("{f1}:0x8000"));
    }

    #[test]
    fn input_capture_baseline_captures_held_button_at_start() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(1_000_000).unwrap();
        let f0 = e.frame_count().unwrap();
        // Hold A *before* recording — the baseline must still capture it.
        e.set_joypad(0, 0x0080).unwrap();
        e.start_input_capture();
        assert_eq!(
            e.take_input_capture(),
            vec![InputCaptureEntry {
                frame: f0,
                port: 0,
                mask: 0x0080
            }]
        );
    }

    #[test]
    fn reset_clears_in_progress_input_capture() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.start_input_capture();
        e.set_joypad(0, 0x1000).unwrap();
        assert!(e.is_capturing_input());
        e.reset().unwrap();
        assert!(!e.is_capturing_input(), "reset drops the capture");
        assert!(e.take_input_capture().is_empty());
    }

    #[test]
    fn load_rom_forced_from_path_bypasses_autodetect() {
        // Write a ROM to disk and load it with a forced mapper (issue #88) —
        // the path the GUI's Force-mapper menu uses. Proves read + forced
        // parse + core build end-to-end.
        let dir = std::env::temp_dir();
        let path = dir.join("luna_test_forced_load.sfc");
        std::fs::write(&path, demo_lorom()).unwrap();
        let mut e = Emulator::new();
        let info = e
            .load_rom_forced(&path, MapperKind::LoRom)
            .expect("forced LoROM load");
        let _ = std::fs::remove_file(&path);
        assert!(e.has_rom());
        assert_eq!(info.mapper, "LoRom");
        // The forced-loaded core actually runs.
        e.step_until_frame(1_000_000).unwrap();
        assert!(e.frame_count().unwrap() >= 1);
    }

    #[test]
    fn run_until_break_interruptible_stops_on_pause_flag() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let flag = AtomicBool::new(false);
        // The run holds `e` on this thread; a scoped thread raises the pause
        // flag mid-run (the real front-end model: pause off the emulator lock).
        let out = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(5));
                flag.store(true, Ordering::Relaxed);
            });
            e.run_until_break_interruptible(1_000_000_000, &flag)
                .unwrap()
        });
        assert!(out.interrupted, "the pause flag ended the run");
        assert!(out.hit.is_none(), "paused, not a breakpoint hit");
        assert!(
            out.steps > 0 && out.steps < 1_000_000_000,
            "stopped mid-run"
        );
    }

    #[test]
    fn peek_oam_matches_state_oam_full() {
        let mut e = Emulator::new();
        assert!(matches!(e.peek_oam(), Err(ApiError::NoRom)));
        e.load_rom_bytes(demo_lorom()).unwrap();
        e.step_until_frame(1_000_000).unwrap();
        let oam = e.peek_oam().unwrap();
        assert_eq!(oam.len(), 0x220, "544 OAM bytes");
        assert_eq!(oam, e.state().ppu.oam_full, "peek_oam == state oam_full");
    }
}
