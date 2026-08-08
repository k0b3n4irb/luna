//! MCP server for Luna — wraps `luna-api::Emulator` and exposes its
//! methods as MCP tools via [`rmcp`].
//!
//! Designed so Claude (or any MCP-aware client) can drive the
//! emulator end-to-end:
//!
//! - `load_rom { path }` → loads a cartridge, returns its metadata.
//! - `reset` → power-on reset.
//! - `step { count }` → advance the CPU N instructions.
//! - `step_until_frame { max_steps }` → advance to the next PPU
//!   frame boundary or hit the cap.
//! - `state` → JSON snapshot of every observable bit of emulator
//!   state (CPU regs, PPU regs + occupancy, APU/SPC, scheduler).
//! - `screenshot { force_display? }` → PNG of the current
//!   framebuffer, returned base64-encoded so MCP clients can render
//!   it inline.
//! - `drain_audio { max }` → consume up to N stereo (i16,i16)
//!   samples from the APU queue, returned as a flat
//!   `[l0, r0, l1, r1, …]` array.
//! - `peek_memory { bank, offset, count }` → read through the bus.
//! - `peek_aram { offset, count }` → direct SPC700 ARAM read.
//! - `peek_vram { offset, count }` → direct VRAM read.
//! - `poke_memory { bank, offset, data }` → inject WRAM bytes.
//! - `search_memory { pattern }` → find a byte pattern in WRAM.
//! - `run_until_pc { pc, max_steps }` → step to a target PC.
//! - `set_cpu_register { reg, val }` → set a CPU register.
//!
//! Interactive-debugger surface (issue #65 / epic #63 — feeds the
//! `OpenSNES` snesdbg-retirement workflows):
//!
//! - `disasm_cpu { addr?, lines?, m8?, x8? }` → 65C816 disassembly
//!   (defaults: live PC + live M/X widths).
//! - `disasm_spc { addr?, lines? }` → SPC700 disassembly (default:
//!   live SPC PC).
//! - `save_state` / `load_state { state_base64 }` → full-machine
//!   save-state round-trip (versioned, ROM-hash-guarded).
//! - `peek_cgram` → all 256 CGRAM BGR555 words.
//! - `render_tilemap { bg }` / `render_vram_tiles { bpp?, palette_row? }`
//!   / `render_palette { cell? }` / `render_sprite_sheet` → debug PNGs,
//!   base64-encoded.
//! - `enable_cpu_trace { max_events }` / `take_cpu_trace` → per-
//!   instruction CPU trace ring.
//! - `enable_mem_trace { max_events, bank?, lo?, hi? }` /
//!   `take_mem_trace` → per-bus-access memory trace with filters.
//! - `set_mouse { dx, dy, buttons }` / `set_superscope { x, y, buttons }`
//!   → pointer-device input.
//! - `enable_nocash_log` / `take_nocash_log` and `enable_wdm_log` /
//!   `take_wdm_log` → the SDK assert/log channels ($21FC Nocash TTY
//!   text + WDM assert hits).
//!
//! Transport is stdio by default ([`serve_stdio`]); a future commit
//! will add HTTP-SSE for browser clients.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use luna_api::{
    ApiError, Emulator, EmulatorState, InputCaptureEntry, RomInfo, input_capture_to_script,
};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars,
    transport::io::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// MCP service wrapper around a single-process Luna `Emulator`.
///
/// All tools take a shared `Arc<Mutex<Emulator>>` lock for the
/// duration of the call. That's fine for the stdio transport (one
/// client at a time); a multi-client HTTP transport would need a
/// per-session emulator.
#[derive(Clone)]
pub struct LunaServer {
    emulator: Arc<Mutex<Emulator>>,
    /// Pause flag for an in-progress `run` (issue #92). Held *outside* the
    /// emulator mutex so the `pause` tool can raise it without waiting for the
    /// running tool to release the lock (rmcp dispatches each request on its
    /// own task, so `pause` runs concurrently with `run`).
    interrupt: Arc<AtomicBool>,
    /// The `max_events` passed to the last `enable_dsp1_trace` — needed by
    /// `take_dsp1_trace {decode_commands}` to tell a capped (truncated)
    /// drain from a complete one before decoding transactions.
    dsp1_trace_max: Arc<std::sync::atomic::AtomicUsize>,
    tool_router: ToolRouter<Self>,
}

// ---------------- Tool parameter types ----------------

/// `load_rom` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadRomParams {
    /// Absolute path to a `.sfc` / `.smc` ROM file on the local
    /// filesystem.
    pub path: String,
    /// Force the mapper instead of header auto-detection — needed for
    /// headerless / checksum-invalid homebrew. One of `lorom`, `hirom`,
    /// `exhirom`, `sa1`, `superfx`, `dsp1`, `sdd1`, `spc7110`.
    #[serde(default)]
    pub force_mapper: Option<String>,
    /// Force the video standard (`ntsc` or `pal`), overriding the header's
    /// country byte. Omitting it restores auto-detection.
    #[serde(default)]
    pub force_region: Option<String>,
}

/// `load_rom_bytes` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadRomBytesParams {
    /// The raw `.sfc` / `.smc` image, base64-encoded (standard alphabet).
    pub rom_base64: String,
    /// Force the mapper instead of header auto-detection (same values as
    /// `load_rom`).
    #[serde(default)]
    pub force_mapper: Option<String>,
    /// Force the video standard (`ntsc` or `pal`). Omitting it restores
    /// auto-detection.
    #[serde(default)]
    pub force_region: Option<String>,
}

/// `set_port_device` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetPortDeviceParams {
    /// Controller port: `0` = P1, `1` = P2.
    pub port: u8,
    /// Device to plug in: `joypad`, `mouse`, or `superscope`.
    pub device: String,
}

/// `frame_hash` parameters.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FrameHashParams {
    /// Render as if the display were on even during forced blank (the
    /// screenshot `force_display` policy). Ignored when `native` is set.
    #[serde(default)]
    pub force_display: bool,
    /// Hash the native 512×448 capture instead of the composited 256×224
    /// frame. Requires `set_native_capture {enabled: true}` before the
    /// frame renders. Native and non-native hashes use different
    /// constructions — never compare one against the other.
    #[serde(default)]
    pub native: bool,
}

/// `set_native_capture` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetNativeCaptureParams {
    /// `true` to start capturing at native 512×448 resolution (hi-res /
    /// interlace modes render meaningfully; everything else is doubled).
    pub enabled: bool,
}

/// `wram_page_hashes` parameters.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct WramPageHashesParams {
    /// Page size in bytes — a power of two dividing 128 KiB (0x20000).
    /// Omit (or 0) for the default 4 KiB pages (32 hashes).
    #[serde(default)]
    pub page_size: usize,
}

/// `wram_snapshot` parameters.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct WramSnapshotParams {
    /// Also return the full 128 KiB WRAM image base64-encoded. Off by
    /// default — the hash alone answers "did anything change?".
    #[serde(default)]
    pub include_data: bool,
}

/// `loop_probe` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoopProbeParams {
    /// Instructions to execute while collecting distinct PCs. Mutates
    /// state (the CPU advances).
    pub max_steps: u64,
}

/// Shared parameters for the capped `enable_*_trace` tools
/// (dma / dsp / `sa1_trace` / superfx / spc).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EnableRingTraceParams {
    /// Hard cap on recorded events; recording stops when the ring is full.
    pub max_events: usize,
}

/// `enable_dsp1_trace` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EnableDsp1TraceParams {
    /// Hard cap on recorded events.
    pub max_events: usize,
    /// Record only the CPU-side DR/SR port traffic, skipping microcode
    /// `exec` events — the handshake view without the firehose.
    #[serde(default)]
    pub ports_only: bool,
}

/// `take_dsp1_trace` parameters.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TakeDsp1TraceParams {
    /// Also decode the drained port traffic into DSP-1 command
    /// transactions (command byte, word counts, status vs. the known
    /// command table).
    #[serde(default)]
    pub decode_commands: bool,
}

/// `step` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StepParams {
    /// Number of CPU instructions to execute.
    pub count: u64,
}

/// `set_joypad` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetJoypadParams {
    /// Controller index: `0` = Player 1 (`$4218/$4219`),
    /// `1` = Player 2 (`$421A/$421B`).
    pub port: u8,
    /// 16-bit JOY1 bitmask. Bit layout (high → low):
    /// B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R,
    /// 0, 0, 0, 0. So `0x1000` = Start, `0x8000` = B,
    /// `0xF000` = Start + Select + Y + B, etc.
    pub mask: u16,
}

/// `step_until_frame` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StepUntilFrameParams {
    /// Maximum instructions to execute before bailing out (safety
    /// belt against runaway loops). 1 000 000 is a reasonable
    /// default for "one game frame".
    pub max_steps: u64,
}

/// `screenshot` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct ScreenshotParams {
    /// When `true`, render with `INIDISP` forced-blank ignored and
    /// master brightness clamped to `$0F` — useful to peek at VRAM
    /// even when a game keeps the screen blanked. Defaults to false.
    #[serde(default)]
    pub force_display: bool,
}

/// `drain_audio` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DrainAudioParams {
    /// Maximum stereo samples to drain.
    pub max: usize,
}

/// `peek_memory` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeekMemoryParams {
    /// 8-bit CPU bank (`$00..$FF`). Ignored when `symbol` is given.
    #[serde(default)]
    pub bank: u8,
    /// 16-bit offset within that bank. Ignored when `symbol` is given.
    #[serde(default)]
    pub offset: u16,
    /// Read at a loaded WLA-DX symbol instead of `bank:offset`
    /// (requires `load_symbols`).
    #[serde(default)]
    pub symbol: Option<String>,
    /// Number of bytes to read.
    pub count: u16,
}

/// `peek_aram` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeekAramParams {
    /// 16-bit offset within the SPC700's 64 KB ARAM.
    pub offset: u16,
    /// Number of bytes to read.
    pub count: u16,
}

/// `peek_vram` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeekVramParams {
    /// 16-bit word/byte offset within the 64 KB VRAM.
    pub offset: u16,
    /// Number of bytes to read.
    pub count: u16,
}

/// `poke_memory` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PokeMemoryParams {
    /// 8-bit CPU bank (`$7E-$7F` or a `$00-3F`/`$80-BF` low-RAM mirror).
    /// Ignored when `symbol` is given.
    #[serde(default)]
    pub bank: u8,
    /// 16-bit offset within that bank. Ignored when `symbol` is given.
    #[serde(default)]
    pub offset: u16,
    /// Write at a loaded WLA-DX symbol instead of `bank:offset`.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Bytes to write (JSON array, e.g. `[222, 173]`).
    pub data: Vec<u8>,
}

/// `search_memory` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    /// Byte pattern to find in `$7E-$7F` WRAM.
    pub pattern: Vec<u8>,
}

/// `run_until_pc` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RunUntilPcParams {
    /// 24-bit target PC (`pb << 16 | pc`). Ignored when `symbol` is given.
    #[serde(default)]
    pub pc: u32,
    /// Run to a loaded WLA-DX symbol instead of `pc`.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Maximum instructions to step before giving up.
    pub max_steps: u64,
}

/// `set_cpu_register` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetRegisterParams {
    /// Register name: `a/x/y/sp/dp/pc/pb/db/p` (case-insensitive).
    pub reg: String,
    /// Value (low byte/word used per the register's width).
    pub val: u32,
}

/// `run_until_mem_write` / `run_until_mem_read` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MemBreakpointParams {
    /// 24-bit bus address to watch (`bank << 16 | offset`). Ignored when
    /// `symbol` is given.
    #[serde(default)]
    pub addr: u32,
    /// Watch a loaded WLA-DX symbol instead of `addr`.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Maximum instructions to step before giving up.
    pub max_steps: u64,
}

/// `disasm_cpu` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct DisasmCpuParams {
    /// 24-bit start address (`pb << 16 | pc`). Defaults to the live PC.
    #[serde(default)]
    pub addr: Option<u32>,
    /// Number of instructions to decode. Defaults to 16.
    #[serde(default)]
    pub lines: Option<u16>,
    /// Force 8-bit accumulator immediates. Defaults to the live M flag
    /// (true in emulation mode).
    #[serde(default)]
    pub m8: Option<bool>,
    /// Force 8-bit index immediates. Defaults to the live X flag
    /// (true in emulation mode).
    #[serde(default)]
    pub x8: Option<bool>,
}

/// `disasm_spc` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct DisasmSpcParams {
    /// 16-bit ARAM start address. Defaults to the live SPC700 PC.
    #[serde(default)]
    pub addr: Option<u16>,
    /// Number of instructions to decode. Defaults to 16.
    #[serde(default)]
    pub lines: Option<u16>,
}

/// `load_state` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadStateParams {
    /// A base64-encoded save-state blob previously returned by
    /// `save_state`. Rejected if the version or ROM hash mismatch.
    pub state_base64: String,
}

/// `render_tilemap` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RenderTilemapParams {
    /// Background layer, 1..=4 (matches the GUI/CLI convention).
    pub bg: u8,
}

/// `render_vram_tiles` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct RenderVramTilesParams {
    /// Tile bit depth: 2, 4 or 8. Defaults to 4.
    #[serde(default)]
    pub bpp: Option<u8>,
    /// CGRAM palette row used to colour the tiles. Defaults to 0.
    #[serde(default)]
    pub palette_row: Option<u8>,
}

/// `render_palette` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct RenderPaletteParams {
    /// Pixel size of each of the 16×16 swatches. Defaults to 16.
    #[serde(default)]
    pub cell: Option<u32>,
}

/// `enable_cpu_trace` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EnableCpuTraceParams {
    /// Hard cap on recorded events (memory guard for long runs).
    pub max_events: usize,
}

/// `enable_mem_trace` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EnableMemTraceParams {
    /// Hard cap on recorded events.
    pub max_events: usize,
    /// Only record accesses whose bank matches (e.g. `0x7E`).
    #[serde(default)]
    pub bank: Option<u8>,
    /// With `hi`, only record offsets in `lo..=hi` (e.g. `0x2100`).
    #[serde(default)]
    pub lo: Option<u16>,
    /// Upper bound of the offset filter (inclusive).
    #[serde(default)]
    pub hi: Option<u16>,
}

/// `bp_add` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BpAddParams {
    /// `"exec"` for a PC breakpoint, `"mem"` for a memory watchpoint.
    pub kind: String,
    /// Exec: the 24-bit `PB:PC`. Mem: the 24-bit range start. Ignored
    /// when `symbol` is given.
    #[serde(default)]
    pub addr: u32,
    /// Break at a loaded WLA-DX symbol instead of `addr`.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Mem only: inclusive range end (defaults to `addr` — a single
    /// address watch).
    #[serde(default)]
    pub hi: Option<u32>,
    /// Mem only: fire on reads (default false).
    #[serde(default)]
    pub on_read: bool,
    /// Mem only: fire on writes (default true).
    #[serde(default = "default_true")]
    pub on_write: bool,
}

const fn default_true() -> bool {
    true
}

/// `bp_remove` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BpRemoveParams {
    /// The id returned by `bp_add`.
    pub id: u32,
}

/// `run_until_break` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RunUntilBreakParams {
    /// Maximum instructions to execute before giving up.
    pub max_steps: u64,
}

/// `run` parameters (issue #92).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Optional safety cap on instructions; omit for an effectively unbounded
    /// run that only a breakpoint, a `STOP`, or a `pause` stops.
    pub max_steps: Option<u64>,
}

/// `load_symbols` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadSymbolsParams {
    /// Path to a WLA-DX `.sym` file on the host filesystem.
    pub path: String,
}

/// `resolve_symbol` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ResolveSymbolParams {
    /// Label name exactly as it appears in the `.sym` file.
    pub name: String,
}

/// `set_mouse` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetMouseParams {
    /// Accumulated X displacement since the last auto-read.
    pub dx: i32,
    /// Accumulated Y displacement since the last auto-read.
    pub dy: i32,
    /// Button bitmask: bit 0 = left, bit 1 = right.
    pub buttons: u8,
}

/// `set_superscope` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetSuperscopeParams {
    /// Aim X in screen space (0..=255; off-screen values allowed).
    pub x: i32,
    /// Aim Y in screen space (0..=224).
    pub y: i32,
    /// Button bitmask: bit 0 fire, bit 1 cursor, bit 2 pause,
    /// bit 3 turbo.
    pub buttons: u8,
}

// ---------------- Tool result types ----------------

/// `load_rom` / `state` result wrappers.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LoadRomResult {
    /// Cartridge metadata extracted from the internal SNES header.
    pub rom: RomInfo,
}

/// `step` result wrapper.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StepResult {
    /// Number of instructions actually executed (may be less than
    /// requested if the CPU halted or panicked).
    pub executed: u64,
}

/// `state` result wrapper.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StateResult {
    /// Full emulator state snapshot — every observable register and
    /// counter, suitable for debugger UIs and regression tests.
    pub state: EmulatorState,
}

/// `screenshot` result wrapper.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScreenshotResult {
    /// PNG-encoded framebuffer bytes, base64-encoded for safe JSON
    /// transport.
    pub png_base64: String,
    /// Convenience width — saves callers from decoding the PNG header.
    pub width: u32,
    /// Convenience height — saves callers from decoding the PNG header.
    pub height: u32,
}

/// `drain_audio` result wrapper.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DrainAudioResult {
    /// Interleaved stereo PCM samples: `[l0, r0, l1, r1, …]` as
    /// signed 16-bit values produced at 32 kHz.
    pub samples: Vec<i16>,
    /// Stereo sample count (= `samples.len() / 2`).
    pub frames: usize,
}

/// `peek_memory` / `peek_aram` / `peek_vram` result wrapper.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemoryResult {
    /// Bytes read.
    pub bytes: Vec<u8>,
}

/// `poke_memory` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PokeResult {
    /// Bytes actually written (non-WRAM addresses are skipped).
    pub written: usize,
}

/// `search_memory` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchResult {
    /// 24-bit `$7E-$7F` addresses of every match.
    pub addresses: Vec<u32>,
}

/// `run_until_pc` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RunUntilResult {
    /// `true` if the target PC was reached within `max_steps`.
    pub hit: bool,
}

/// `run_until_mem_write` / `run_until_mem_read` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemBreakResult {
    /// `true` if the watched access happened within `max_steps`.
    pub hit: bool,
    /// 24-bit PC of the instruction that did the access (0 if not hit).
    pub pc: u32,
    /// Byte transferred (0 if not hit).
    pub value: u8,
}

/// `disasm_cpu` / `disasm_spc` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DisasmResult {
    /// Decoded instruction lines, in address order. `is_pc` marks the
    /// live program counter's line.
    pub lines: Vec<luna_api::DisasmLine>,
}

/// `save_state` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SaveStateResult {
    /// The serialized machine state, base64-encoded. Feed back to
    /// `load_state` to restore.
    pub state_base64: String,
    /// Decoded blob size in bytes.
    pub bytes: usize,
}

/// `peek_cgram` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CgramResult {
    /// All 256 CGRAM entries as raw BGR555 words (index 0 = backdrop).
    pub colors: Vec<u16>,
}

/// `peek_oam` result (issue #89).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OamResult {
    /// All 544 OAM bytes: the 512-byte low table + the 32-byte high table.
    pub bytes: Vec<u8>,
}

/// `capabilities` result (issue #90).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CapabilitiesResult {
    /// luna release version, e.g. `"1.8.0"`.
    pub version: String,
    /// Every registered tool name — the live catalogue, a stable contract for
    /// client feature-detection (no need to guess from a stale `--help`).
    pub tools: Vec<String>,
}

/// Result for the debug-render tools (`render_tilemap`,
/// `render_vram_tiles`, `render_palette`, `render_sprite_sheet`).
/// Dimensions vary per render — decode the PNG header if needed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PngResult {
    /// PNG bytes, base64-encoded.
    pub png_base64: String,
}

/// One CPU-trace event (`take_cpu_trace`) — a flattened, transport-
/// friendly copy of `luna_api::CpuTraceEvent`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CpuTraceLine {
    /// Master cycles since reset at instruction start.
    pub mclk: u64,
    /// 24-bit PC of the about-to-execute instruction.
    pub pc: u32,
    /// Accumulator (16-bit).
    pub a: u16,
    /// X index register.
    pub x: u16,
    /// Y index register.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Processor status flags.
    pub p: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page register.
    pub dp: u16,
    /// Emulation-mode flag.
    pub e: bool,
    /// Nearest symbol for `pc` when a `.sym` table is loaded.
    pub symbol: Option<String>,
}

/// `take_cpu_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CpuTraceResult {
    /// Recorded instructions, oldest first. Draining resets the ring.
    pub events: Vec<CpuTraceLine>,
}

/// One memory-trace event (`take_mem_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemTraceLine {
    /// Master cycles since reset at the access.
    pub mclk: u64,
    /// 24-bit PC of the instruction performing the access.
    pub pc: u32,
    /// 24-bit bus address accessed.
    pub addr: u32,
    /// `"read"` or `"write"`.
    pub kind: String,
    /// Byte transferred.
    pub value: u8,
    /// PPU scanline at the access.
    pub line: u16,
    /// Exact horizontal master-clock (0..1363) at the access.
    pub hclock: u16,
    /// `true` if the access happened inside vertical blank.
    pub blank: bool,
    /// `true` if INIDISP forced-blank was set at the access.
    pub force_blank: bool,
    /// Nearest symbol for `addr` when a `.sym` table is loaded.
    pub symbol: Option<String>,
}

/// `take_mem_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemTraceResult {
    /// Recorded accesses, oldest first. Draining resets the ring.
    pub events: Vec<MemTraceLine>,
}

/// `take_nocash_log` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NocashLogResult {
    /// The drained byte stream decoded as lossy UTF-8 (the usual case —
    /// `SNES_NOCASH` emits text).
    pub text: String,
    /// The exact drained bytes, base64-encoded (lossless).
    pub base64: String,
}

/// One WDM assert/breakpoint event (`take_wdm_log`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WdmEvent {
    /// 24-bit PC of the `WDM` opcode.
    pub pc: u32,
    /// The WDM operand byte (`SNES_ASSERT` fires `WDM $00`).
    pub operand: u8,
    /// Nearest symbol for `pc` when a `.sym` table is loaded.
    pub symbol: Option<String>,
}

/// `take_wdm_log` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WdmLogResult {
    /// Recorded WDM hits, oldest first. Draining resets the buffer.
    pub events: Vec<WdmEvent>,
}

/// `frame_hash` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FrameHashResult {
    /// The 64-bit frame hash as 16 lowercase hex chars — the exact value
    /// the CLI prints as `fbhash=` (hex string because a JSON number
    /// cannot carry a full u64).
    pub hash: String,
}

/// `wram_page_hashes` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WramPageHashesResult {
    /// The effective page size in bytes.
    pub page_size: usize,
    /// One stable FNV-1a-64 hash per page (16 hex chars each), page 0
    /// first ($7E:0000). Diff two calls to localise a WRAM change.
    pub hashes: Vec<String>,
}

/// `wram_snapshot` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WramSnapshotResult {
    /// Stable FNV-1a-64 over all 128 KiB (16 hex chars) — equal hashes ⇒
    /// identical WRAM.
    pub hash: String,
    /// Snapshot size in bytes (always 0x20000).
    pub bytes: usize,
    /// The raw WRAM image, base64-encoded — only when `include_data` was
    /// requested.
    pub wram_base64: Option<String>,
}

/// `loop_probe` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LoopProbeResult {
    /// Distinct 24-bit PCs seen. A healthy game touches hundreds+; a
    /// handful means the CPU is spinning in a tight (likely hung) loop.
    pub distinct_pcs: usize,
    /// Instructions actually executed (may stop early on STP).
    pub executed: u64,
}

/// One DMA→VRAM transfer byte (`take_dma_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DmaTraceLine {
    /// 24-bit A-bus source address of this byte.
    pub src: u32,
    /// PPU VRAM word address (`$2116/7`) the byte targets.
    pub vram_word: u16,
    /// B-bus register offset (byte targets `$2100 + b_offset`).
    pub b_offset: u8,
    /// The transferred byte.
    pub value: u8,
    /// DMA channel (0-7).
    pub channel: u8,
    /// Completed-frame counter at the start of the owning burst.
    pub frame: u64,
    /// PPU scanline at the start of the owning burst.
    pub line: u16,
    /// Horizontal master-clock (0..1363) at the transfer.
    pub hclock: u16,
    /// Burst started inside vertical blank.
    pub blank: bool,
    /// INIDISP forced-blank was set at the write. A VRAM write is safe
    /// iff `blank || force_blank`.
    pub force_blank: bool,
}

/// `take_dma_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DmaTraceResult {
    /// Recorded transfers, oldest first. Draining resets the ring.
    pub events: Vec<DmaTraceLine>,
}

/// One S-DSP register write (`take_dsp_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DspTraceLine {
    /// SPC700 cycles since reset at the write.
    pub spc_cycles: u64,
    /// DSP register index (`$00-$7F`).
    pub reg: u8,
    /// Byte written.
    pub value: u8,
}

/// `take_dsp_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DspTraceResult {
    /// Recorded writes, oldest first. Draining resets the ring.
    pub events: Vec<DspTraceLine>,
}

/// One CPU↔APU mailbox access (`take_mailbox_log`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MailboxLine {
    /// Master cycles since reset at the access.
    pub mclk: u64,
    /// 24-bit CPU PC of the accessing instruction.
    pub pc: u32,
    /// `"read"` (CPU ← APU) or `"write"` (CPU → APU).
    pub kind: String,
    /// Mailbox port `0..=3` (`$2140 + port`).
    pub port: u8,
    /// Byte transferred.
    pub value: u8,
    /// Nearest symbol for `pc` when a `.sym` table is loaded.
    pub symbol: Option<String>,
}

/// `take_mailbox_log` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MailboxLogResult {
    /// Recorded accesses, oldest first. Draining resets the log.
    pub events: Vec<MailboxLine>,
}

/// One main-CPU access to an SA-1 MMIO register (`take_sa1_log`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1LogLine {
    /// Master cycles since reset at the access.
    pub mclk: u64,
    /// 24-bit CPU PC of the accessing instruction.
    pub pc: u32,
    /// `"read"` or `"write"`.
    pub kind: String,
    /// Register address in `$2200..=$23FF`.
    pub reg: u16,
    /// Byte transferred.
    pub value: u8,
    /// Nearest symbol for `pc` when a `.sym` table is loaded.
    pub symbol: Option<String>,
}

/// `take_sa1_log` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1LogResult {
    /// Recorded accesses, oldest first. Draining resets the log.
    pub events: Vec<Sa1LogLine>,
}

/// One SA-1-side MMIO access (`take_sa1_side_log`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1SideLine {
    /// 24-bit SA-1 PC at the start of the accessing instruction.
    pub sa1_pc: u32,
    /// `true` = write, `false` = read.
    pub write: bool,
    /// Register address in `$2200..=$23FF`.
    pub reg: u16,
    /// Byte transferred.
    pub value: u8,
}

/// `take_sa1_side_log` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1SideLogResult {
    /// Recorded accesses, oldest first. Draining resets the log.
    pub events: Vec<Sa1SideLine>,
}

/// One SA-1 pre-instruction register snapshot (`take_sa1_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1TraceLine {
    /// 24-bit SA-1 PC before the opcode runs.
    pub pc: u32,
    /// Accumulator.
    pub a: u16,
    /// X index.
    pub x: u16,
    /// Y index.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Processor status.
    pub p: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page.
    pub dp: u16,
    /// Emulation-mode flag.
    pub e: bool,
}

/// `take_sa1_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Sa1TraceResult {
    /// Recorded instructions, oldest first. Draining resets the ring.
    pub events: Vec<Sa1TraceLine>,
}

/// One Super FX (GSU) opcode event (`take_superfx_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SuperFxTraceLine {
    /// GSU PC (`pbr << 16 | r15`) at the fetch.
    pub pc: u32,
    /// Opcode byte executed.
    pub opcode: u8,
    /// Raw 16-bit status flag register.
    pub sfr: u16,
    /// General-purpose registers R0–R15 (R15 = PC).
    pub r: Vec<u16>,
    /// GSU clock position on the shared master-clock axis.
    pub mclk: u64,
    /// First instruction of a GO task.
    pub go_start: bool,
    /// This instruction cleared SFR.G (STOP).
    pub stop: bool,
}

/// `take_superfx_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SuperFxTraceResult {
    /// Recorded opcodes, oldest first. Draining resets the ring.
    pub events: Vec<SuperFxTraceLine>,
}

/// One DSP-1 trace event (`take_dsp1_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Dsp1TraceLine {
    /// `"exec"` (microcode instruction), `"dr_write"` / `"dr_read"` (CPU
    /// port traffic), or `"sr_read"` (status poll).
    pub kind: String,
    /// Microcode PC (on port events: where the microcode was sitting).
    pub pc: u16,
    /// 24-bit microcode word (`exec` only).
    pub opcode: u32,
    /// Byte crossing the CPU port (port events only).
    pub value: u8,
    /// Accumulator A after the event.
    pub a: i16,
    /// Accumulator B after the event.
    pub b: i16,
    /// Data register after the event.
    pub dr: u16,
    /// Status register after the event.
    pub sr: u16,
    /// RQM handshake bit after the event.
    pub rqm: bool,
}

/// `take_dsp1_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Dsp1TraceResult {
    /// Recorded events, oldest first. Draining resets the ring.
    pub events: Vec<Dsp1TraceLine>,
    /// Decoded command transactions — only when `decode_commands` was set.
    pub commands: Option<Vec<luna_api::dsp1_commands::Transaction>>,
    /// The drain hit the ring cap, so the final transaction may be cut
    /// off mid-stream (decoding accounts for this).
    pub truncated: bool,
}

/// One SPC700 pre-instruction register snapshot (`take_spc_trace`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SpcTraceLine {
    /// 16-bit SPC700 PC before the opcode runs.
    pub pc: u16,
    /// Accumulator.
    pub a: u8,
    /// X index.
    pub x: u8,
    /// Y index.
    pub y: u8,
    /// Stack pointer.
    pub sp: u8,
    /// Processor status word (PSW).
    pub psw: u8,
    /// Running SPC-cycle counter at this opcode (wraps at 2^32).
    pub spc_cycle: u32,
    /// Timer 2 internal counter.
    pub t2_int: u16,
    /// Timer 2 output (the value `$FF` reads clear).
    pub t2_out: u8,
}

/// `take_spc_trace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SpcTraceResult {
    /// Recorded instructions, oldest first. Draining resets the ring.
    pub events: Vec<SpcTraceLine>,
}

/// `bp_add` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BpAddResult {
    /// Registry id of the new breakpoint (stable until removed).
    pub id: u32,
}

/// `bp_remove` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BpRemoveResult {
    /// `true` if the id existed and was removed.
    pub removed: bool,
}

/// One registered breakpoint (`bp_list`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BpEntry {
    /// Registry id.
    pub id: u32,
    /// `"exec"` or `"mem"`.
    pub kind: String,
    /// Exec: the PC. Mem: range start.
    pub lo: u32,
    /// Exec: same as `lo`. Mem: inclusive range end.
    pub hi: u32,
    /// Mem: fires on reads.
    pub on_read: bool,
    /// Mem: fires on writes.
    pub on_write: bool,
}

/// `bp_list` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BpListResult {
    /// Every registered breakpoint, ordered by id.
    pub breakpoints: Vec<BpEntry>,
}

/// `take_input_capture` result (issue #83).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InputCaptureResult {
    /// Every recorded joypad change (`frame`, `port`, `mask`), sorted by frame.
    pub entries: Vec<InputCaptureEntry>,
    /// Player-1 changes as a `--input` script (`frame:0xMASK,…`), ready to
    /// replay via `set_joypad` + `step_until_frame` or `luna state --input`.
    pub script_p1: String,
    /// Player-2 changes rendered the same way (empty if P2 was untouched).
    pub script_p2: String,
}

/// `run_until_break` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RunUntilBreakResult {
    /// Instructions actually executed this call.
    pub steps: u64,
    /// `true` if a breakpoint fired (fields below are then meaningful).
    pub hit: bool,
    /// Id of the breakpoint that fired.
    pub bp_id: Option<u32>,
    /// `"exec"`, `"read"` or `"write"`.
    pub kind: Option<String>,
    /// Exec: the about-to-execute PC. Mem: the accessing instruction's PC.
    pub pc: Option<u32>,
    /// Mem hits: the accessed 24-bit bus address.
    pub addr: Option<u32>,
    /// Mem hits: the byte transferred.
    pub value: Option<u8>,
    /// `true` if a `pause` ended the run before a breakpoint or the step
    /// budget (issue #92): `hit == false && interrupted == true`.
    pub interrupted: bool,
}

/// `load_symbols` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LoadSymbolsResult {
    /// Number of labels parsed from the file.
    pub count: usize,
}

/// `resolve_symbol` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ResolveSymbolResult {
    /// 24-bit `bank << 16 | offset` address, or null if unknown.
    pub addr: Option<u32>,
}

/// Resolve an optional symbol name against the emulator's loaded table,
/// falling back to the numeric address. Unknown symbol → invalid-params.
fn resolve_addr(em: &Emulator, symbol: Option<&str>, numeric: u32) -> Result<u32, ErrorData> {
    match symbol {
        None => Ok(numeric),
        Some(name) => em.resolve_symbol(name).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("unknown symbol `{name}` (is the right .sym loaded?)"),
                None,
            )
        }),
    }
}

// ---------------- Server impl ----------------

#[rmcp::tool_router]
impl LunaServer {
    /// Build a new server backed by a freshly-constructed `Emulator`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            emulator: Arc::new(Mutex::new(Emulator::new())),
            interrupt: Arc::new(AtomicBool::new(false)),
            dsp1_trace_max: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tool_router: Self::tool_router(),
        }
    }

    #[rmcp::tool(
        description = "Load a SNES ROM (.sfc / .smc) from a path on the host filesystem. \
                                Returns parsed cartridge metadata. Optional `force_mapper` \
                                (lorom/hirom/exhirom/sa1/superfx/dsp1/sdd1/spc7110) bypasses \
                                header auto-detection for headerless or checksum-invalid \
                                homebrew; optional `force_region` (ntsc/pal) overrides the \
                                header's country byte — omit either to auto-detect."
    )]
    async fn load_rom(
        &self,
        Parameters(params): Parameters<LoadRomParams>,
    ) -> Result<rmcp::Json<LoadRomResult>, ErrorData> {
        let mapper = parse_force_mapper(params.force_mapper.as_deref())?;
        let region = parse_force_region(params.force_region.as_deref())?;
        let info = {
            let mut em = self.emulator.lock().await;
            em.set_forced_region(region);
            let path = PathBuf::from(params.path);
            match mapper {
                Some(kind) => em.load_rom_forced(&path, kind),
                None => em.load_rom(&path),
            }
            .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(LoadRomResult { rom: info }))
    }

    #[rmcp::tool(
        description = "Load a SNES ROM from base64-encoded bytes (no host file needed — \
                                e.g. a freshly assembled homebrew image). Same optional \
                                `force_mapper` / `force_region` as `load_rom`. Caveat: unlike \
                                the path-based `load_rom`, this does NOT search the firmware \
                                folder, so a cart needing coprocessor firmware (e.g. DSP-1) \
                                loads with the coprocessor inert — check `missing_firmware` \
                                in the result and prefer `load_rom` for those."
    )]
    async fn load_rom_bytes(
        &self,
        Parameters(params): Parameters<LoadRomBytesParams>,
    ) -> Result<rmcp::Json<LoadRomResult>, ErrorData> {
        let mapper = parse_force_mapper(params.force_mapper.as_deref())?;
        let region = parse_force_region(params.force_region.as_deref())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(params.rom_base64.as_bytes())
            .map_err(|e| ErrorData::invalid_params(format!("bad base64: {e}"), None))?;
        let info = {
            let mut em = self.emulator.lock().await;
            em.set_forced_region(region);
            match mapper {
                Some(kind) => em.load_rom_bytes_forced(bytes, kind),
                None => em.load_rom_bytes(bytes),
            }
            .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(LoadRomResult { rom: info }))
    }

    #[rmcp::tool(
        description = "Plug a device into a controller port (0 = P1, 1 = P2): `joypad`, \
                                `mouse`, or `superscope`. Feed it afterwards with `set_joypad`, \
                                `set_mouse`, or `set_superscope`."
    )]
    async fn set_port_device(
        &self,
        Parameters(params): Parameters<SetPortDeviceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        let device = match params.device.to_ascii_lowercase().as_str() {
            "joypad" | "pad" => luna_api::PortDevice::Pad,
            "mouse" => luna_api::PortDevice::Mouse,
            "superscope" => luna_api::PortDevice::SuperScope,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown device `{other}` (joypad, mouse, superscope)"),
                    None,
                ));
            }
        };
        {
            let mut em = self.emulator.lock().await;
            em.set_port_device(params.port, device)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(description = "Reset the loaded emulator to its power-on state. \
                                Errors if no ROM is currently loaded.")]
    async fn reset(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.reset().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Set the joypad button bitmask for controller `port` (0 = P1, \
                                1 = P2). Bit layout matches the SNES JOY1L/JOY1H pair: \
                                B(15) Y(14) Select(13) Start(12) Up(11) Down(10) Left(9) \
                                Right(8) A(7) X(6) L(5) R(4) + 4-bit signature. The press \
                                is latched on the next VBlank auto-read (one frame later) \
                                — hold the mask for at least 2 frames before reading back \
                                game state, then write 0 to release."
    )]
    async fn set_joypad(
        &self,
        Parameters(params): Parameters<SetJoypadParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.set_joypad(params.port, params.mask)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Step the CPU `count` instructions (or stop early if the CPU halts \
                                or panics). Returns how many were actually executed."
    )]
    async fn step(
        &self,
        Parameters(params): Parameters<StepParams>,
    ) -> Result<rmcp::Json<StepResult>, ErrorData> {
        let executed = {
            let mut em = self.emulator.lock().await;
            em.step(params.count).map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(StepResult { executed }))
    }

    #[rmcp::tool(
        description = "Run instructions until the PPU completes one frame, bounded by \
                                `max_steps`. Useful for advancing the emulator one game frame at \
                                a time."
    )]
    async fn step_until_frame(
        &self,
        Parameters(params): Parameters<StepUntilFrameParams>,
    ) -> Result<rmcp::Json<StepResult>, ErrorData> {
        let executed = {
            let mut em = self.emulator.lock().await;
            em.step_until_frame(params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(StepResult { executed }))
    }

    #[rmcp::tool(
        description = "Return a JSON snapshot of the emulator's full observable state — \
                                CPU registers, PPU registers + memory occupancy, APU / SPC700 / \
                                DSP, scheduler cursor, cumulative stats."
    )]
    async fn state(&self) -> rmcp::Json<StateResult> {
        let mut em = self.emulator.lock().await;
        rmcp::Json(StateResult { state: em.state() })
    }

    #[rmcp::tool(
        description = "Render the current PPU framebuffer (256×224, composited \
                                BG3-over-BG1-over-BG2 + sprites) as a PNG and return it \
                                base64-encoded."
    )]
    async fn screenshot(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> Result<rmcp::Json<ScreenshotResult>, ErrorData> {
        let png = {
            let em = self.emulator.lock().await;
            em.render_frame_png(params.force_display)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        // Report whatever the API actually rendered (hardcoding 256×224
        // would lie the day a native-res / per-BG render lands here).
        let (width, height) = png_dimensions(&png);
        let png_base64 = b64(&png);
        Ok(rmcp::Json(ScreenshotResult {
            png_base64,
            width,
            height,
        }))
    }

    #[rmcp::tool(
        description = "Drain up to `max` stereo audio samples from the APU output \
                                queue. Returns interleaved [l, r, l, r, …] signed-16-bit samples \
                                at 32 kHz."
    )]
    async fn drain_audio(
        &self,
        Parameters(params): Parameters<DrainAudioParams>,
    ) -> Result<rmcp::Json<DrainAudioResult>, ErrorData> {
        let samples = {
            let mut em = self.emulator.lock().await;
            em.drain_audio(params.max).map_err(|e| api_err_to_mcp(&e))?
        };
        let frames = samples.len();
        let mut flat = Vec::with_capacity(frames * 2);
        for (l, r) in samples {
            flat.push(l);
            flat.push(r);
        }
        Ok(rmcp::Json(DrainAudioResult {
            samples: flat,
            frames,
        }))
    }

    #[rmcp::tool(
        description = "Read `count` bytes from the CPU bus starting at `bank:offset`. \
                                Reads go through MMIO when the address lands in a register range, \
                                so use non-MMIO regions for plain memory dumps."
    )]
    async fn peek_memory(
        &self,
        Parameters(params): Parameters<PeekMemoryParams>,
    ) -> Result<rmcp::Json<MemoryResult>, ErrorData> {
        let bytes = {
            let mut em = self.emulator.lock().await;
            let addr = resolve_addr(
                &em,
                params.symbol.as_deref(),
                (u32::from(params.bank) << 16) | u32::from(params.offset),
            )?;
            em.peek_memory((addr >> 16) as u8, addr as u16, params.count)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(MemoryResult { bytes }))
    }

    #[rmcp::tool(
        description = "Read `count` bytes from the SPC700's 64 KB ARAM at the given \
                                offset. Read-only; no bus side effects."
    )]
    async fn peek_aram(
        &self,
        Parameters(params): Parameters<PeekAramParams>,
    ) -> Result<rmcp::Json<MemoryResult>, ErrorData> {
        let bytes = {
            let em = self.emulator.lock().await;
            em.peek_aram(params.offset, params.count)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(MemoryResult { bytes }))
    }

    #[rmcp::tool(
        description = "Read `count` bytes from the 64 KB VRAM at the given offset. \
                                Read-only."
    )]
    async fn peek_vram(
        &self,
        Parameters(params): Parameters<PeekVramParams>,
    ) -> Result<rmcp::Json<MemoryResult>, ErrorData> {
        let bytes = {
            let em = self.emulator.lock().await;
            em.peek_vram(params.offset, params.count)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(MemoryResult { bytes }))
    }

    #[rmcp::tool(
        description = "Write bytes directly into WRAM ($7E-$7F or the $00-3F/$80-BF \
                                low-RAM mirror) — inject a test state without a save-state. \
                                Returns bytes written (non-WRAM addresses are skipped)."
    )]
    async fn poke_memory(
        &self,
        Parameters(params): Parameters<PokeMemoryParams>,
    ) -> Result<rmcp::Json<PokeResult>, ErrorData> {
        let written = {
            let mut em = self.emulator.lock().await;
            let addr = resolve_addr(
                &em,
                params.symbol.as_deref(),
                (u32::from(params.bank) << 16) | u32::from(params.offset),
            )?;
            em.poke_memory((addr >> 16) as u8, addr as u16, &params.data)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(PokeResult { written }))
    }

    #[rmcp::tool(
        description = "Find every $7E-$7F WRAM address whose bytes match `pattern`. \
                                Returns the 24-bit addresses."
    )]
    async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<rmcp::Json<SearchResult>, ErrorData> {
        let addresses = {
            let em = self.emulator.lock().await;
            em.search_memory(&params.pattern)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(SearchResult { addresses }))
    }

    #[rmcp::tool(
        description = "Step the CPU until PB:PC reaches `pc` (24-bit) or `max_steps` \
                                instructions elapse. Returns whether the target was hit."
    )]
    async fn run_until_pc(
        &self,
        Parameters(params): Parameters<RunUntilPcParams>,
    ) -> Result<rmcp::Json<RunUntilResult>, ErrorData> {
        let hit = {
            let mut em = self.emulator.lock().await;
            let pc = resolve_addr(&em, params.symbol.as_deref(), params.pc)?;
            em.run_until_pc(pc, params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(RunUntilResult { hit }))
    }

    #[rmcp::tool(
        description = "Set a CPU register by name (a/x/y/sp/dp/pc/pb/db/p). For setting \
                                up a test state before stepping."
    )]
    async fn set_cpu_register(
        &self,
        Parameters(params): Parameters<SetRegisterParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.set_cpu_register(&params.reg, params.val)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Step until an instruction WRITES the 24-bit bus address `addr`, \
                                or `max_steps` elapse. Returns the writing PC + value (a memory \
                                write-breakpoint — e.g. catch what zeroes a pointer)."
    )]
    async fn run_until_mem_write(
        &self,
        Parameters(params): Parameters<MemBreakpointParams>,
    ) -> Result<rmcp::Json<MemBreakResult>, ErrorData> {
        let hit = {
            let mut em = self.emulator.lock().await;
            let addr = resolve_addr(&em, params.symbol.as_deref(), params.addr)?;
            em.run_until_mem_write(addr, params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(match hit {
            Some((pc, value)) => MemBreakResult {
                hit: true,
                pc,
                value,
            },
            None => MemBreakResult {
                hit: false,
                pc: 0,
                value: 0,
            },
        }))
    }

    #[rmcp::tool(
        description = "Step until an instruction READS the 24-bit bus address `addr`, \
                                or `max_steps` elapse. Returns the reading PC + value."
    )]
    async fn run_until_mem_read(
        &self,
        Parameters(params): Parameters<MemBreakpointParams>,
    ) -> Result<rmcp::Json<MemBreakResult>, ErrorData> {
        let hit = {
            let mut em = self.emulator.lock().await;
            let addr = resolve_addr(&em, params.symbol.as_deref(), params.addr)?;
            em.run_until_mem_read(addr, params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(match hit {
            Some((pc, value)) => MemBreakResult {
                hit: true,
                pc,
                value,
            },
            None => MemBreakResult {
                hit: false,
                pc: 0,
                value: 0,
            },
        }))
    }

    // ------------- Interactive-debugger surface (issue #65) -------------

    #[rmcp::tool(
        description = "Disassemble 65C816 instructions. Defaults: start at the live \
                                PB:PC with immediate-operand widths from the live M/X flags; \
                                override `addr` (24-bit), `lines` (default 16), `m8`/`x8` to \
                                inspect elsewhere. `is_pc` marks the live-PC line."
    )]
    async fn disasm_cpu(
        &self,
        Parameters(params): Parameters<DisasmCpuParams>,
    ) -> Result<rmcp::Json<DisasmResult>, ErrorData> {
        let lines = {
            let mut em = self.emulator.lock().await;
            let cpu = em.cpu_state().map_err(|e| api_err_to_mcp(&e))?;
            let addr = params
                .addr
                .unwrap_or_else(|| (u32::from(cpu.pb) << 16) | u32::from(cpu.pc));
            let m8 = params.m8.unwrap_or(cpu.e || cpu.p & 0x20 != 0);
            let x8 = params.x8.unwrap_or(cpu.e || cpu.p & 0x10 != 0);
            em.disassemble_cpu(addr, params.lines.unwrap_or(16), m8, x8)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(DisasmResult { lines }))
    }

    #[rmcp::tool(
        description = "Disassemble SPC700 instructions from ARAM. Defaults: start at \
                                the live SPC PC; override `addr` (16-bit) and `lines` \
                                (default 16). `is_pc` marks the live-PC line."
    )]
    async fn disasm_spc(
        &self,
        Parameters(params): Parameters<DisasmSpcParams>,
    ) -> Result<rmcp::Json<DisasmResult>, ErrorData> {
        let lines = {
            let em = self.emulator.lock().await;
            let addr = match params.addr {
                Some(a) => a,
                None => em.spc700_state().map_err(|e| api_err_to_mcp(&e))?.pc,
            };
            em.disassemble_spc(addr, params.lines.unwrap_or(16))
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(DisasmResult { lines }))
    }

    #[rmcp::tool(
        description = "Serialize the full machine state (CPU/PPU/APU/DMA/coproc/WRAM + \
                                mapper) to a versioned, ROM-hash-guarded blob, returned \
                                base64-encoded. Restore later with `load_state`."
    )]
    async fn save_state(&self) -> Result<rmcp::Json<SaveStateResult>, ErrorData> {
        let blob = {
            let em = self.emulator.lock().await;
            em.save_state().map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(SaveStateResult {
            bytes: blob.len(),
            state_base64: b64(&blob),
        }))
    }

    #[rmcp::tool(
        description = "Restore a save-state blob produced by `save_state` (base64). \
                                Rejected if the format version or the loaded ROM's hash \
                                mismatch — a state only loads against its own ROM."
    )]
    async fn load_state(
        &self,
        Parameters(params): Parameters<LoadStateParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(&params.state_base64)
            .map_err(|e| ErrorData::invalid_params(format!("bad base64: {e}"), None))?;
        {
            let mut em = self.emulator.lock().await;
            em.load_state(&blob).map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Read all 256 CGRAM palette entries as raw BGR555 words \
                                (index 0 = backdrop). Read-only."
    )]
    async fn peek_cgram(&self) -> Result<rmcp::Json<CgramResult>, ErrorData> {
        let colors = {
            let em = self.emulator.lock().await;
            em.peek_cgram().map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(CgramResult { colors }))
    }

    #[rmcp::tool(
        description = "Read all 544 OAM bytes (512-byte low table + 32-byte high table) \
                                as raw bytes — sprite attributes for an OAM/sprite viewer. \
                                Read-only."
    )]
    async fn peek_oam(&self) -> Result<rmcp::Json<OamResult>, ErrorData> {
        let bytes = {
            let em = self.emulator.lock().await;
            em.peek_oam().map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(OamResult { bytes }))
    }

    #[rmcp::tool(
        description = "Report this luna MCP server's version and the live tool catalogue, \
                                so a client can feature-detect instead of guessing from a stale \
                                --help. `version` is the luna release; `tools` is every \
                                registered tool name."
    )]
    async fn capabilities(&self) -> rmcp::Json<CapabilitiesResult> {
        let tools = self
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        rmcp::Json(CapabilitiesResult {
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools,
        })
    }

    #[rmcp::tool(
        description = "Render background layer `bg` (1..=4)'s full tilemap as a PNG \
                                (base64) — the whole scrollable field, not just the viewport. \
                                Mode 7 renders the 128×128 field on BG1."
    )]
    async fn render_tilemap(
        &self,
        Parameters(params): Parameters<RenderTilemapParams>,
    ) -> Result<rmcp::Json<PngResult>, ErrorData> {
        let png = {
            let em = self.emulator.lock().await;
            let idx = usize::from(params.bg.saturating_sub(1).min(3));
            em.render_tilemap_png(idx).map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(PngResult {
            png_base64: b64(&png),
        }))
    }

    #[rmcp::tool(
        description = "Render the VRAM tile set as a PNG (base64) decoded at `bpp` \
                                (2/4/8, default 4) using CGRAM `palette_row` (default 0)."
    )]
    async fn render_vram_tiles(
        &self,
        Parameters(params): Parameters<RenderVramTilesParams>,
    ) -> Result<rmcp::Json<PngResult>, ErrorData> {
        let png = {
            let em = self.emulator.lock().await;
            em.render_vram_tiles_png(params.bpp.unwrap_or(4), params.palette_row.unwrap_or(0))
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(PngResult {
            png_base64: b64(&png),
        }))
    }

    #[rmcp::tool(
        description = "Render the 256-colour CGRAM palette as a 16×16 swatch-grid PNG \
                                (base64); `cell` = pixels per swatch (default 16)."
    )]
    async fn render_palette(
        &self,
        Parameters(params): Parameters<RenderPaletteParams>,
    ) -> Result<rmcp::Json<PngResult>, ErrorData> {
        let png = {
            let em = self.emulator.lock().await;
            em.render_palette_png(params.cell.unwrap_or(16))
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(PngResult {
            png_base64: b64(&png),
        }))
    }

    #[rmcp::tool(
        description = "Render all 128 OAM sprites at native size with their OBJ \
                                palettes as a transparent-background PNG sprite sheet (base64)."
    )]
    async fn render_sprite_sheet(&self) -> Result<rmcp::Json<PngResult>, ErrorData> {
        let png = {
            let em = self.emulator.lock().await;
            em.render_sprite_sheet_png()
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(PngResult {
            png_base64: b64(&png),
        }))
    }

    #[rmcp::tool(
        description = "Start recording a per-instruction CPU trace (PC + registers), \
                                capped at `max_events`. Drain with `take_cpu_trace`."
    )]
    async fn enable_cpu_trace(
        &self,
        Parameters(params): Parameters<EnableCpuTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_cpu_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the recorded CPU trace (oldest first) and reset the ring. \
                                Enable first with `enable_cpu_trace`, then `step`."
    )]
    async fn take_cpu_trace(&self) -> Result<rmcp::Json<CpuTraceResult>, ErrorData> {
        let (events, syms) = {
            let mut em = self.emulator.lock().await;
            let events = em.take_cpu_trace_log().map_err(|e| api_err_to_mcp(&e))?;
            (events, em.symbols_cloned())
        };
        let lines = events
            .into_iter()
            .map(|ev| CpuTraceLine {
                mclk: ev.mclk_total,
                pc: ev.pc_full,
                a: ev.a,
                x: ev.x,
                y: ev.y,
                sp: ev.sp,
                p: ev.p,
                db: ev.db,
                dp: ev.dp,
                e: ev.e,
                symbol: syms.as_ref().and_then(|t| t.nearest(ev.pc_full)),
            })
            .collect();
        Ok(rmcp::Json(CpuTraceResult { events: lines }))
    }

    #[rmcp::tool(
        description = "Start recording every CPU bus access (PC, address, r/w, value, \
                                scanline/H-clock), capped at `max_events`. Optional filters: \
                                `bank` (e.g. 0x7E) and/or an inclusive offset range `lo`..=`hi` \
                                (e.g. 0x2100..0x21FF). Drain with `take_mem_trace`."
    )]
    async fn enable_mem_trace(
        &self,
        Parameters(params): Parameters<EnableMemTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        let offset_filter = match (params.lo, params.hi) {
            (Some(lo), Some(hi)) if lo <= hi => Some((lo, hi)),
            (None, None) => None,
            _ => {
                return Err(ErrorData::invalid_params(
                    "offset filter needs both `lo` and `hi` with lo <= hi",
                    None,
                ));
            }
        };
        {
            let mut em = self.emulator.lock().await;
            em.enable_mem_trace(params.max_events, params.bank, offset_filter)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the recorded memory-access trace (oldest first) and reset \
                                the ring. Enable first with `enable_mem_trace`, then `step`."
    )]
    async fn take_mem_trace(&self) -> Result<rmcp::Json<MemTraceResult>, ErrorData> {
        let (events, syms) = {
            let mut em = self.emulator.lock().await;
            let events = em.take_mem_trace_log().map_err(|e| api_err_to_mcp(&e))?;
            (events, em.symbols_cloned())
        };
        let lines = events
            .into_iter()
            .map(|ev| MemTraceLine {
                mclk: ev.mclk_total,
                pc: ev.pc_full,
                addr: ev.addr_full,
                kind: match ev.kind {
                    luna_api::MemEventKind::Read => "read".into(),
                    luna_api::MemEventKind::Write => "write".into(),
                    luna_api::MemEventKind::NmiSignal => "nmi".into(),
                    luna_api::MemEventKind::IrqSignal => "irq".into(),
                },
                value: ev.value,
                line: ev.line,
                hclock: ev.hclock,
                blank: ev.blank,
                force_blank: ev.force_blank,
                symbol: syms.as_ref().and_then(|t| t.nearest(ev.addr_full)),
            })
            .collect();
        Ok(rmcp::Json(MemTraceResult { events: lines }))
    }

    // ------------- WLA-DX symbols (issue #67) -------------

    #[rmcp::tool(
        description = "Load a WLA-DX `.sym` symbol file (the wlalink output every \
                                WLA-DX-built ROM ships). Once loaded: `disasm_cpu` lines carry \
                                the nearest label, cpu/mem traces are annotated, and the \
                                address-taking tools (`peek_memory`, `poke_memory`, \
                                `run_until_pc`, `run_until_mem_*`, `bp_add`) accept a `symbol` \
                                name instead of a numeric address."
    )]
    async fn load_symbols(
        &self,
        Parameters(params): Parameters<LoadSymbolsParams>,
    ) -> Result<rmcp::Json<LoadSymbolsResult>, ErrorData> {
        let count = {
            let mut em = self.emulator.lock().await;
            em.load_symbols(std::path::Path::new(&params.path))
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(LoadSymbolsResult { count }))
    }

    #[rmcp::tool(
        description = "Resolve a loaded WLA-DX label to its 24-bit `bank:offset` \
                                address (null if unknown)."
    )]
    async fn resolve_symbol(
        &self,
        Parameters(params): Parameters<ResolveSymbolParams>,
    ) -> Result<rmcp::Json<ResolveSymbolResult>, ErrorData> {
        let addr = {
            let em = self.emulator.lock().await;
            em.resolve_symbol(&params.name)
        };
        Ok(rmcp::Json(ResolveSymbolResult { addr }))
    }

    // ------------- Breakpoint registry (issue #66) -------------

    #[rmcp::tool(
        description = "Register a breakpoint. kind='exec': halt BEFORE the instruction \
                                at 24-bit PB:PC `addr` executes. kind='mem': watchpoint over the \
                                inclusive bus range `addr`..=`hi` (default single address), firing \
                                on reads (`on_read`) and/or writes (`on_write`, default true); \
                                halts AFTER the accessing instruction with its PC/addr/value. \
                                Returns the registry id. Run with `run_until_break`."
    )]
    async fn bp_add(
        &self,
        Parameters(params): Parameters<BpAddParams>,
    ) -> Result<rmcp::Json<BpAddResult>, ErrorData> {
        let id = {
            let mut em = self.emulator.lock().await;
            let addr = resolve_addr(&em, params.symbol.as_deref(), params.addr)?;
            match params.kind.as_str() {
                "exec" => em.bp_add_exec(addr).map_err(|e| api_err_to_mcp(&e))?,
                "mem" => em
                    .bp_add_mem(
                        addr,
                        params.hi.unwrap_or(addr),
                        params.on_read,
                        params.on_write,
                    )
                    .map_err(|e| api_err_to_mcp(&e))?,
                other => {
                    return Err(ErrorData::invalid_params(
                        format!("kind must be 'exec' or 'mem', got `{other}`"),
                        None,
                    ));
                }
            }
        };
        Ok(rmcp::Json(BpAddResult { id }))
    }

    #[rmcp::tool(description = "Remove a breakpoint by the id `bp_add` returned.")]
    async fn bp_remove(
        &self,
        Parameters(params): Parameters<BpRemoveParams>,
    ) -> Result<rmcp::Json<BpRemoveResult>, ErrorData> {
        let removed = {
            let mut em = self.emulator.lock().await;
            em.bp_remove(params.id).map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(BpRemoveResult { removed }))
    }

    #[rmcp::tool(description = "Remove every registered breakpoint and watchpoint.")]
    async fn bp_clear_all(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.bp_clear().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Start recording player joypad input (issue #83). Every subsequent \
                                set_joypad change is logged as a frame:mask checkpoint; masks \
                                already held become the baseline. Stop and retrieve with \
                                take_input_capture. Replaces any capture already running."
    )]
    async fn start_input_capture(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.start_input_capture();
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Stop the joypad input capture and return the recorded frame:mask \
                                checkpoints — per-port entries plus ready-to-replay P1/P2 --input \
                                scripts. Empty if no capture was running."
    )]
    async fn take_input_capture(&self) -> Result<rmcp::Json<InputCaptureResult>, ErrorData> {
        let entries = {
            let mut em = self.emulator.lock().await;
            em.take_input_capture()
        };
        let script_p1 = input_capture_to_script(&entries, 0);
        let script_p2 = input_capture_to_script(&entries, 1);
        Ok(rmcp::Json(InputCaptureResult {
            entries,
            script_p1,
            script_p2,
        }))
    }

    #[rmcp::tool(description = "List every registered breakpoint/watchpoint, ordered by id.")]
    async fn bp_list(&self) -> Result<rmcp::Json<BpListResult>, ErrorData> {
        let list = {
            let em = self.emulator.lock().await;
            em.bp_list().map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(BpListResult {
            breakpoints: list
                .into_iter()
                .map(|b| BpEntry {
                    id: b.id,
                    kind: if b.exec { "exec".into() } else { "mem".into() },
                    lo: b.lo,
                    hi: b.hi,
                    on_read: b.on_read,
                    on_write: b.on_write,
                })
                .collect(),
        }))
    }

    #[rmcp::tool(
        description = "Run at full emulation speed until a registered breakpoint fires \
                                or `max_steps` instructions elapse. Exec breakpoints halt before \
                                their instruction (resume-friendly: the first instruction of the \
                                run is exempt); watchpoints halt after the accessing instruction \
                                and report its PC, the address and the byte."
    )]
    async fn run_until_break(
        &self,
        Parameters(params): Parameters<RunUntilBreakParams>,
    ) -> Result<rmcp::Json<RunUntilBreakResult>, ErrorData> {
        let out = {
            let mut em = self.emulator.lock().await;
            em.run_until_break(params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(outcome_to_result(&out)))
    }

    #[rmcp::tool(
        description = "Run at full emulation speed until a breakpoint fires, the CPU \
                                STOPs, or a `pause` is issued (issue #92) — an interruptible run \
                                with no mandatory step budget. `max_steps` is an optional safety \
                                cap (default effectively unbounded). Returns the same fields as \
                                run_until_break plus `interrupted` (true when a pause ended it). \
                                Because rmcp handles each request on its own task, a `pause` \
                                call lands while this run is in flight."
    )]
    async fn run(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<rmcp::Json<RunUntilBreakResult>, ErrorData> {
        let max_steps = params.max_steps.unwrap_or(u64::MAX);
        let out = {
            let mut em = self.emulator.lock().await;
            em.run_until_break_interruptible(max_steps, &self.interrupt)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(outcome_to_result(&out)))
    }

    #[rmcp::tool(
        description = "Ask an in-progress `run` to stop as soon as possible (issue #92). \
                                Raises a shared pause flag without taking the emulator lock, so \
                                it returns immediately even while `run` holds the emulator; the \
                                run then returns with `interrupted: true`. A no-op if nothing is \
                                running."
    )]
    async fn pause(&self) -> rmcp::Json<EmptyOk> {
        self.interrupt.store(true, Ordering::Relaxed);
        rmcp::Json(EmptyOk { ok: true })
    }

    #[rmcp::tool(
        description = "Feed SNES Mouse input for the next joypad auto-read: accumulated \
                                `dx`/`dy` displacement plus the button bitmask (bit 0 = left, \
                                bit 1 = right). Plug the mouse in first with \
                                `set_port_device {port: 0, device: \"mouse\"}` (CLI \
                                equivalent: `--port1 mouse`)."
    )]
    async fn set_mouse(
        &self,
        Parameters(params): Parameters<SetMouseParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.set_mouse(params.dx, params.dy, params.buttons)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Feed Super Scope input: screen-space aim (`x`, `y`) and the \
                                button bitmask (bit 0 fire, bit 1 cursor, bit 2 pause, bit 3 \
                                turbo). Plug the scope in first with \
                                `set_port_device {port: 1, device: \"superscope\"}` (CLI \
                                equivalent: `--port2 superscope`)."
    )]
    async fn set_superscope(
        &self,
        Parameters(params): Parameters<SetSuperscopeParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.set_superscope(params.x, params.y, params.buttons)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    // ------------- SDK assert/log channels (issue #168) -------------

    #[rmcp::tool(
        description = "Start capturing the $21FC Nocash TTY — the SDK debug text \
                                channel behind SNES_NOCASH / SNES_ASSERT. Drain with \
                                `take_nocash_log`."
    )]
    async fn enable_nocash_log(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_nocash_log().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the captured $21FC Nocash byte stream: the text as lossy \
                                UTF-8 plus the exact bytes base64-encoded. Enable first with \
                                `enable_nocash_log`, then run/step."
    )]
    async fn take_nocash_log(&self) -> Result<rmcp::Json<NocashLogResult>, ErrorData> {
        let bytes = {
            let mut em = self.emulator.lock().await;
            em.take_nocash_log().map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(NocashLogResult {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            base64: b64(&bytes),
        }))
    }

    #[rmcp::tool(
        description = "Start capturing WDM ($42) executions — the SDK assert/breakpoint \
                                channel (SNES_ASSERT fires WDM $00). Complements the $21FC \
                                Nocash text log with a binary \"assertion fired here\" signal. \
                                Drain with `take_wdm_log`."
    )]
    async fn enable_wdm_log(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_wdm_log().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the captured WDM events as `{pc, operand, symbol}` per hit, \
                                oldest first. Enable first with `enable_wdm_log`, then run/step."
    )]
    async fn take_wdm_log(&self) -> Result<rmcp::Json<WdmLogResult>, ErrorData> {
        let (events, syms) = {
            let mut em = self.emulator.lock().await;
            let events = em.take_wdm_log().map_err(|e| api_err_to_mcp(&e))?;
            (events, em.symbols_cloned())
        };
        let events = events
            .into_iter()
            .map(|(pc, operand)| WdmEvent {
                pc,
                operand,
                symbol: syms.as_ref().and_then(|t| t.nearest(pc)),
            })
            .collect();
        Ok(rmcp::Json(WdmLogResult { events }))
    }

    // ------------- Determinism oracles (issue #170) -------------

    #[rmcp::tool(
        description = "64-bit hash of the current frame's pixels (pre-PNG, so it is \
                                stable across builds) — the CLI's `fbhash=` value, as 16 hex \
                                chars. `force_display` renders through forced blank; `native` \
                                hashes the 512×448 capture instead (enable it first with \
                                `set_native_capture`; native and non-native values are not \
                                comparable)."
    )]
    async fn frame_hash(
        &self,
        Parameters(params): Parameters<FrameHashParams>,
    ) -> Result<rmcp::Json<FrameHashResult>, ErrorData> {
        let hash = {
            let em = self.emulator.lock().await;
            if params.native {
                em.frame_hash_native()
            } else {
                em.frame_hash(params.force_display)
            }
            .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(FrameHashResult {
            hash: format!("{hash:016x}"),
        }))
    }

    #[rmcp::tool(
        description = "Enable / disable native 512×448 frame capture (hi-res and \
                                interlace detail). Enable it, run at least one frame, then \
                                `screenshot` / `frame_hash` with `native: true`."
    )]
    async fn set_native_capture(
        &self,
        Parameters(params): Parameters<SetNativeCaptureParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.set_native_capture(params.enabled)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Stable FNV-1a-64 hash per WRAM page (default 4 KiB pages → 32 \
                                hashes, 16 hex chars each). Diff two calls to localise which \
                                page a WRAM change landed in, then `peek_memory` to bisect — \
                                the CLI `luna wram-trace` workflow over MCP."
    )]
    async fn wram_page_hashes(
        &self,
        Parameters(params): Parameters<WramPageHashesParams>,
    ) -> Result<rmcp::Json<WramPageHashesResult>, ErrorData> {
        let hashes = {
            let em = self.emulator.lock().await;
            em.wram_page_hashes(params.page_size)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        let effective = if params.page_size == 0 {
            0x1000
        } else {
            params.page_size
        };
        Ok(rmcp::Json(WramPageHashesResult {
            page_size: effective,
            hashes: hashes.iter().map(|h| format!("{h:016x}")).collect(),
        }))
    }

    #[rmcp::tool(
        description = "Snapshot all 128 KiB of WRAM: a stable FNV-1a-64 hash always, \
                                plus the raw image base64-encoded when `include_data` is set. \
                                Equal hashes ⇒ byte-identical WRAM (the determinism oracle for \
                                CI and A/B bisection)."
    )]
    async fn wram_snapshot(
        &self,
        Parameters(params): Parameters<WramSnapshotParams>,
    ) -> Result<rmcp::Json<WramSnapshotResult>, ErrorData> {
        let (hash, data) = {
            let em = self.emulator.lock().await;
            // One full-width "page" = one stable hash over the whole 128 KiB.
            let hash = em
                .wram_page_hashes(0x20000)
                .map_err(|e| api_err_to_mcp(&e))?[0];
            let data = if params.include_data {
                Some(em.wram_snapshot().map_err(|e| api_err_to_mcp(&e))?)
            } else {
                None
            };
            (hash, data)
        };
        Ok(rmcp::Json(WramSnapshotResult {
            hash: format!("{hash:016x}"),
            bytes: 0x20000,
            wram_base64: data.as_deref().map(b64),
        }))
    }

    #[rmcp::tool(
        description = "Hang diagnostic: execute up to `max_steps` instructions and count \
                                the distinct PCs visited. A healthy game loop touches hundreds+ \
                                of addresses; a handful means the CPU is spinning in a tight \
                                wait/hang loop (STP is reported separately by `state`). Mutates \
                                state — the CPU really advances."
    )]
    async fn loop_probe(
        &self,
        Parameters(params): Parameters<LoopProbeParams>,
    ) -> Result<rmcp::Json<LoopProbeResult>, ErrorData> {
        let probe = {
            let mut em = self.emulator.lock().await;
            em.loop_probe(params.max_steps)
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(LoopProbeResult {
            distinct_pcs: probe.distinct_pcs,
            executed: probe.executed,
        }))
    }

    // ------------- Coprocessor / driver trace parity (issue #172) -------------

    #[rmcp::tool(
        description = "Start recording DMA→VRAM transfer bytes (source, VRAM word, \
                                channel, scanline/H-clock, blank flags), capped at \
                                `max_events`. Drain with `take_dma_trace`."
    )]
    async fn enable_dma_trace(
        &self,
        Parameters(params): Parameters<EnableRingTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_dma_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the DMA→VRAM trace (oldest first) and reset the ring. A \
                                write is display-safe iff `blank || force_blank`."
    )]
    async fn take_dma_trace(&self) -> Result<rmcp::Json<DmaTraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_dma_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| DmaTraceLine {
                src: ev.src_full,
                vram_word: ev.vram_word,
                b_offset: ev.b_offset,
                value: ev.value,
                channel: ev.channel,
                frame: ev.frame,
                line: ev.line,
                hclock: ev.hclock,
                blank: ev.blank,
                force_blank: ev.force_blank,
            })
            .collect();
        Ok(rmcp::Json(DmaTraceResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording S-DSP register writes ($F2/$F3 from the SPC700 \
                                side), capped at `max_events`. Drain with `take_dsp_trace` — \
                                proves whether/what the sound driver programs the DSP."
    )]
    async fn enable_dsp_trace(
        &self,
        Parameters(params): Parameters<EnableRingTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_dsp_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the S-DSP register-write trace (oldest first) and reset \
                                the ring."
    )]
    async fn take_dsp_trace(&self) -> Result<rmcp::Json<DspTraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_dsp_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| DspTraceLine {
                spc_cycles: ev.spc_cycles,
                reg: ev.reg,
                value: ev.value,
            })
            .collect();
        Ok(rmcp::Json(DspTraceResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording CPU↔APU mailbox traffic ($2140-$2143, both \
                                directions, with the accessing PC). Unbounded until drained — \
                                enable, run the window of interest, then `take_mailbox_log`."
    )]
    async fn enable_mailbox_log(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_mailbox_log().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the CPU↔APU mailbox log (oldest first) and reset it — the \
                                CPU↔SPC handshake stream (e.g. sound-driver upload stalls)."
    )]
    async fn take_mailbox_log(&self) -> Result<rmcp::Json<MailboxLogResult>, ErrorData> {
        let (events, syms) = {
            let mut em = self.emulator.lock().await;
            let events = em.take_mailbox_log().map_err(|e| api_err_to_mcp(&e))?;
            (events, em.symbols_cloned())
        };
        let events = events
            .into_iter()
            .map(|ev| MailboxLine {
                mclk: ev.mclk_total,
                pc: ev.pc_full,
                kind: match ev.kind {
                    luna_api::MailboxEventKind::Read => "read".into(),
                    luna_api::MailboxEventKind::Write => "write".into(),
                },
                port: ev.port,
                value: ev.value,
                symbol: syms.as_ref().and_then(|t| t.nearest(ev.pc_full)),
            })
            .collect();
        Ok(rmcp::Json(MailboxLogResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording main-CPU accesses to the SA-1 MMIO range \
                                ($2200-$23FF) with the accessing PC. Unbounded until drained."
    )]
    async fn enable_sa1_log(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_sa1_log().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the main-CPU→SA-1 MMIO log (oldest first) and reset it — \
                                one half of a CPU↔SA-1 handshake diagnosis (pair with \
                                `take_sa1_side_log`)."
    )]
    async fn take_sa1_log(&self) -> Result<rmcp::Json<Sa1LogResult>, ErrorData> {
        let (events, syms) = {
            let mut em = self.emulator.lock().await;
            let events = em.take_sa1_log().map_err(|e| api_err_to_mcp(&e))?;
            (events, em.symbols_cloned())
        };
        let events = events
            .into_iter()
            .map(|ev| Sa1LogLine {
                mclk: ev.mclk_total,
                pc: ev.pc_full,
                kind: match ev.kind {
                    luna_api::MailboxEventKind::Read => "read".into(),
                    luna_api::MailboxEventKind::Write => "write".into(),
                },
                reg: ev.reg,
                value: ev.value,
                symbol: syms.as_ref().and_then(|t| t.nearest(ev.pc_full)),
            })
            .collect();
        Ok(rmcp::Json(Sa1LogResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording SA-1-side accesses to its own MMIO registers \
                                (which SA-1 code touches $2200-$23FF). Unbounded until drained."
    )]
    async fn enable_sa1_side_log(&self) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_sa1_side_log().map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the SA-1-side MMIO log (oldest first) and reset it — the \
                                other half of a CPU↔SA-1 handshake diagnosis."
    )]
    async fn take_sa1_side_log(&self) -> Result<rmcp::Json<Sa1SideLogResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_sa1_side_log().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| Sa1SideLine {
                sa1_pc: ev.sa1_pc,
                write: ev.write,
                reg: ev.reg,
                value: ev.value,
            })
            .collect();
        Ok(rmcp::Json(Sa1SideLogResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording a per-instruction SA-1 CPU trace (PC + register \
                                file), capped at `max_events`. Drain with `take_sa1_trace`."
    )]
    async fn enable_sa1_trace(
        &self,
        Parameters(params): Parameters<EnableRingTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_sa1_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the SA-1 instruction trace (oldest first) and reset the \
                                ring. Empty when the cart has no SA-1."
    )]
    async fn take_sa1_trace(&self) -> Result<rmcp::Json<Sa1TraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_sa1_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| Sa1TraceLine {
                pc: ev.pc_full,
                a: ev.a,
                x: ev.x,
                y: ev.y,
                sp: ev.sp,
                p: ev.p,
                db: ev.db,
                dp: ev.dp,
                e: ev.e,
            })
            .collect();
        Ok(rmcp::Json(Sa1TraceResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording a per-opcode Super FX (GSU) trace (PC, opcode, \
                                SFR, R0-R15, mclk, GO/STOP edges), capped at `max_events`. \
                                Drain with `take_superfx_trace`."
    )]
    async fn enable_superfx_trace(
        &self,
        Parameters(params): Parameters<EnableRingTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_superfx_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the Super FX opcode trace (oldest first) and reset the \
                                ring. Empty when the cart has no GSU."
    )]
    async fn take_superfx_trace(&self) -> Result<rmcp::Json<SuperFxTraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_superfx_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| SuperFxTraceLine {
                pc: ev.pc_full,
                opcode: ev.opcode,
                sfr: ev.sfr,
                r: ev.r.to_vec(),
                mclk: ev.mclk,
                go_start: ev.go_start,
                stop: ev.stop,
            })
            .collect();
        Ok(rmcp::Json(SuperFxTraceResult { events }))
    }

    #[rmcp::tool(
        description = "Start recording the DSP-1 (µPD77C25) trace: microcode execution \
                                and CPU-side DR/SR port traffic in one stream, capped at \
                                `max_events`. `ports_only` keeps just the handshake traffic. \
                                Drain with `take_dsp1_trace`."
    )]
    async fn enable_dsp1_trace(
        &self,
        Parameters(params): Parameters<EnableDsp1TraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_dsp1_trace(params.max_events, params.ports_only)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        self.dsp1_trace_max
            .store(params.max_events, Ordering::Relaxed);
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the DSP-1 trace (oldest first) and reset the ring; empty \
                                when the cart has no DSP-1. With `decode_commands`, also \
                                returns the port traffic decoded into command transactions \
                                (command byte, word counts, match status vs. the known table)."
    )]
    async fn take_dsp1_trace(
        &self,
        Parameters(params): Parameters<TakeDsp1TraceParams>,
    ) -> Result<rmcp::Json<Dsp1TraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_dsp1_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        // Hitting the cap leaves the final transaction cut off mid-stream;
        // decoding must know that rather than report a short count as a
        // table mismatch (same rule as the CLI --dsp1-trace-commands path).
        let max = self.dsp1_trace_max.load(Ordering::Relaxed);
        let truncated = max > 0 && events.len() >= max;
        let commands = params
            .decode_commands
            .then(|| luna_api::dsp1_commands::decode(&events, truncated));
        let events = events
            .into_iter()
            .map(|ev| Dsp1TraceLine {
                kind: match ev.kind {
                    luna_api::Dsp1TraceKind::Exec => "exec".into(),
                    luna_api::Dsp1TraceKind::DrWrite => "dr_write".into(),
                    luna_api::Dsp1TraceKind::DrRead => "dr_read".into(),
                    luna_api::Dsp1TraceKind::SrRead => "sr_read".into(),
                },
                pc: ev.pc,
                opcode: ev.opcode,
                value: ev.value,
                a: ev.a,
                b: ev.b,
                dr: ev.dr,
                sr: ev.sr,
                rqm: ev.rqm,
            })
            .collect();
        Ok(rmcp::Json(Dsp1TraceResult {
            events,
            commands,
            truncated,
        }))
    }

    #[rmcp::tool(
        description = "Start recording a per-instruction SPC700 trace (PC, registers, \
                                SPC cycle, timer-2 state), capped at `max_events`. Drain with \
                                `take_spc_trace`."
    )]
    async fn enable_spc_trace(
        &self,
        Parameters(params): Parameters<EnableRingTraceParams>,
    ) -> Result<rmcp::Json<EmptyOk>, ErrorData> {
        {
            let mut em = self.emulator.lock().await;
            em.enable_spc_trace(params.max_events)
                .map_err(|e| api_err_to_mcp(&e))?;
        }
        Ok(rmcp::Json(EmptyOk { ok: true }))
    }

    #[rmcp::tool(
        description = "Drain the SPC700 instruction trace (oldest first) and reset the \
                                ring."
    )]
    async fn take_spc_trace(&self) -> Result<rmcp::Json<SpcTraceResult>, ErrorData> {
        let events = {
            let mut em = self.emulator.lock().await;
            em.take_spc_trace().map_err(|e| api_err_to_mcp(&e))?
        };
        let events = events
            .into_iter()
            .map(|ev| SpcTraceLine {
                pc: ev.pc,
                a: ev.a,
                x: ev.x,
                y: ev.y,
                sp: ev.sp,
                psw: ev.psw,
                spc_cycle: ev.spc_cycle,
                t2_int: ev.t2_int,
                t2_out: ev.t2_out,
            })
            .collect();
        Ok(rmcp::Json(SpcTraceResult { events }))
    }
}

impl Default for LunaServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Trivial `{ ok: true }` payload for tools whose only failure mode
/// is an explicit error.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EmptyOk {
    /// Always `true`. Errors come back as an MCP error response.
    pub ok: bool,
}

#[rmcp::tool_handler]
impl ServerHandler for LunaServer {}

/// Map [`luna_api::ApiError`] onto an MCP `internal_error` payload.
fn api_err_to_mcp(e: &ApiError) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

/// Base64-encode bytes with the standard alphabet — the one wire encoding
/// shared by every binary payload this server returns.
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Parse an optional `force_mapper` tool param into a [`luna_api::MapperKind`],
/// sharing the CLI's `--force-mapper` vocabulary.
fn parse_force_mapper(s: Option<&str>) -> Result<Option<luna_api::MapperKind>, ErrorData> {
    s.map(|k| {
        luna_api::MapperKind::from_cli_str(k).ok_or_else(|| {
            ErrorData::invalid_params(
                format!(
                    "unknown force_mapper `{k}` (lorom, hirom, exhirom, sa1, superfx, dsp1, \
                     sdd1, spc7110)"
                ),
                None,
            )
        })
    })
    .transpose()
}

/// Parse an optional `force_region` tool param, sharing the CLI's
/// `--force-region` vocabulary.
fn parse_force_region(s: Option<&str>) -> Result<Option<luna_api::Region>, ErrorData> {
    s.map(|r| match r.to_ascii_lowercase().as_str() {
        "ntsc" => Ok(luna_api::Region::Ntsc),
        "pal" => Ok(luna_api::Region::Pal),
        _ => Err(ErrorData::invalid_params(
            format!("unknown force_region `{r}` (ntsc, pal)"),
            None,
        )),
    })
    .transpose()
}

/// Width/height straight from the PNG IHDR (fixed offsets 16..24,
/// big-endian — IHDR is required to be the first chunk), so the reported
/// dimensions are those of whatever the API actually rendered. `(0, 0)`
/// on a malformed buffer.
fn png_dimensions(png: &[u8]) -> (u32, u32) {
    let field = |o: usize| {
        png.get(o..o + 4)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    (field(16), field(20))
}

/// Flatten a [`luna_api::RunOutcome`] into the wire result shared by
/// `run_until_break` and `run` (issue #92).
fn outcome_to_result(out: &luna_api::RunOutcome) -> RunUntilBreakResult {
    match &out.hit {
        Some(hit) => RunUntilBreakResult {
            steps: out.steps,
            hit: true,
            bp_id: Some(hit.id),
            kind: Some(
                match hit.kind {
                    luna_api::BreakKind::Exec => "exec",
                    luna_api::BreakKind::Read => "read",
                    luna_api::BreakKind::Write => "write",
                }
                .into(),
            ),
            pc: Some(hit.pc),
            addr: hit.addr,
            value: hit.value,
            interrupted: out.interrupted,
        },
        None => RunUntilBreakResult {
            steps: out.steps,
            hit: false,
            bp_id: None,
            kind: None,
            pc: None,
            addr: None,
            value: None,
            interrupted: out.interrupted,
        },
    }
}

/// Run the Luna MCP server on stdio until the client disconnects.
///
/// Intended entry point for the `luna mcp serve` CLI subcommand and
/// for `claude_desktop_config.json`-style spawns. Blocks until the
/// MCP client closes the stream or sends a shutdown.
pub async fn serve_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (stdin, stdout) = stdio();
    let server = LunaServer::new().serve((stdin, stdout)).await?;
    server.waiting().await?;
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Smoke test: build a server with no ROM loaded, fetch state.
    /// We can't exercise the full MCP protocol from a unit test
    /// without setting up an in-memory transport pair (which is its
    /// own dance); this verifies the wiring at the type level —
    /// `state()` is `async`, the underlying call works, and the
    /// emulator is wrapped consistently.
    #[tokio::test]
    async fn server_state_works_without_rom() {
        let s = LunaServer::new();
        let result = s.state().await;
        // No ROM loaded → the embedded RomInfo is None.
        assert!(result.0.state.rom.is_none());
    }

    /// `step` without a ROM returns a `NoRom` `ApiError` mapped to an
    /// MCP error.
    #[tokio::test]
    async fn server_step_without_rom_returns_error() {
        let s = LunaServer::new();
        let result = s.step(Parameters(StepParams { count: 1 })).await;
        let Err(err) = result else {
            panic!("expected error for stepping without a ROM");
        };
        assert!(err.message.contains("no ROM"));
    }

    /// `png_dimensions` reads the IHDR fields; a malformed buffer yields
    /// `(0, 0)` instead of panicking. (The screenshot round-trip test
    /// below covers the real-PNG path: it asserts 256×224 on an actual
    /// render.)
    #[test]
    fn png_dimensions_reads_the_ihdr() {
        let mut buf = vec![0u8; 24];
        buf[16..20].copy_from_slice(&512u32.to_be_bytes());
        buf[20..24].copy_from_slice(&448u32.to_be_bytes());
        assert_eq!(png_dimensions(&buf), (512, 448));
        assert_eq!(png_dimensions(&[]), (0, 0));
    }

    /// Loading a non-existent ROM bubbles the I/O error up through
    /// the MCP layer.
    #[tokio::test]
    async fn server_load_rom_missing_file_returns_error() {
        let s = LunaServer::new();
        let result = s
            .load_rom(Parameters(rom_params(
                "/tmp/luna-this-file-does-not-exist.smc",
            )))
            .await;
        let Err(err) = result else {
            panic!("expected error for missing ROM");
        };
        let msg = err.message.to_lowercase();
        assert!(msg.contains("i/o") || msg.contains("io"));
    }

    /// Smoke-test the full happy path: load a tiny ROM, step it,
    /// dump state, render a PNG. Uses the same demo cart the
    /// `luna-api` tests use, just to ensure the MCP wrappers
    /// faithfully forward.
    #[tokio::test]
    async fn server_load_step_state_screenshot_round_trip() {
        let s = LunaServer::new();
        // Write demo cart to a tempfile so `load_rom` (which takes
        // a path) can read it.
        let path = PathBuf::from("/tmp/luna_mcp_demo.smc");
        std::fs::write(&path, demo_lorom()).unwrap();
        let info = s
            .load_rom(Parameters(rom_params(&path.to_string_lossy())))
            .await
            .unwrap();
        assert_eq!(info.0.rom.mapper, "LoRom");
        let stepped = s.step(Parameters(StepParams { count: 100 })).await.unwrap();
        assert!(stepped.0.executed > 0);
        let st = s.state().await;
        assert!(st.0.state.rom.is_some());
        let png = s
            .screenshot(Parameters(ScreenshotParams::default()))
            .await
            .unwrap();
        assert_eq!(png.0.width, 256);
        assert_eq!(png.0.height, 224);
        // PNG header check via base64-decode.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&png.0.png_base64)
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        let _ = std::fs::remove_file(&path);
    }

    /// P1 surface round-trip (issue #65): disasm at the live PC, a
    /// save→mutate→load state cycle, CGRAM + every debug render, both
    /// trace rings, and the pointer-device setters.
    #[tokio::test]
    async fn server_p1_debugger_surface_round_trip() {
        let s = LunaServer::new();
        let path = PathBuf::from("/tmp/luna_mcp_p1_demo.smc");
        std::fs::write(&path, demo_lorom()).unwrap();
        s.load_rom(Parameters(rom_params(&path.to_string_lossy())))
            .await
            .unwrap();
        s.step(Parameters(StepParams { count: 50 })).await.unwrap();

        // disasm_cpu with all defaults → decodes at the live PC.
        let d = s
            .disasm_cpu(Parameters(DisasmCpuParams::default()))
            .await
            .unwrap();
        assert_eq!(d.0.lines.len(), 16);
        assert!(d.0.lines[0].is_pc, "first default line is the live PC");
        // disasm_spc with defaults.
        let d = s
            .disasm_spc(Parameters(DisasmSpcParams::default()))
            .await
            .unwrap();
        assert_eq!(d.0.lines.len(), 16);

        // save → run further → load → the saved position is restored.
        let saved = s.save_state().await.unwrap();
        assert!(saved.0.bytes > 0);
        let pc_at_save = {
            let mut em = s.emulator.lock().await;
            em.state().cpu.pc
        };
        s.step(Parameters(StepParams { count: 200 })).await.unwrap();
        s.load_state(Parameters(LoadStateParams {
            state_base64: saved.0.state_base64,
        }))
        .await
        .unwrap();
        let pc_after_load = {
            let mut em = s.emulator.lock().await;
            em.state().cpu.pc
        };
        assert_eq!(pc_at_save, pc_after_load, "load_state restores the PC");
        // Corrupt base64 → invalid-params error, not a panic.
        assert!(
            s.load_state(Parameters(LoadStateParams {
                state_base64: "not-base64!".into(),
            }))
            .await
            .is_err()
        );

        // CGRAM + the four debug renders all return valid payloads.
        let cg = s.peek_cgram().await.unwrap();
        assert_eq!(cg.0.colors.len(), 256);
        for png_b64 in [
            s.render_tilemap(Parameters(RenderTilemapParams { bg: 1 }))
                .await
                .unwrap()
                .0
                .png_base64,
            s.render_vram_tiles(Parameters(RenderVramTilesParams::default()))
                .await
                .unwrap()
                .0
                .png_base64,
            s.render_palette(Parameters(RenderPaletteParams::default()))
                .await
                .unwrap()
                .0
                .png_base64,
            s.render_sprite_sheet().await.unwrap().0.png_base64,
        ] {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&png_b64)
                .unwrap();
            assert!(bytes.starts_with(b"\x89PNG"));
        }

        // Trace rings: enable → step → drain (non-empty), drain again (empty).
        s.enable_cpu_trace(Parameters(EnableCpuTraceParams { max_events: 1000 }))
            .await
            .unwrap();
        s.enable_mem_trace(Parameters(EnableMemTraceParams {
            max_events: 1000,
            bank: None,
            lo: None,
            hi: None,
        }))
        .await
        .unwrap();
        s.step(Parameters(StepParams { count: 20 })).await.unwrap();
        let ct = s.take_cpu_trace().await.unwrap();
        assert!(!ct.0.events.is_empty(), "cpu trace recorded");
        let mt = s.take_mem_trace().await.unwrap();
        assert!(!mt.0.events.is_empty(), "mem trace recorded");
        assert!(
            mt.0.events
                .iter()
                .all(|e| matches!(e.kind.as_str(), "read" | "write" | "nmi" | "irq"))
        );
        assert!(s.take_cpu_trace().await.unwrap().0.events.is_empty());
        // Bad offset filter (lo without hi) → invalid-params.
        assert!(
            s.enable_mem_trace(Parameters(EnableMemTraceParams {
                max_events: 10,
                bank: None,
                lo: Some(0x2100),
                hi: None,
            }))
            .await
            .is_err()
        );

        // Pointer devices accept input without error.
        s.set_mouse(Parameters(SetMouseParams {
            dx: -3,
            dy: 4,
            buttons: 1,
        }))
        .await
        .unwrap();
        s.set_superscope(Parameters(SetSuperscopeParams {
            x: 128,
            y: 112,
            buttons: 1,
        }))
        .await
        .unwrap();

        let _ = std::fs::remove_file(&path);
    }

    /// P2 breakpoint surface (issue #66): add/list/run/remove/clear over
    /// MCP, on an injected WRAM loop.
    #[tokio::test]
    async fn server_p2_breakpoints_round_trip() {
        let s = LunaServer::new();
        let path = PathBuf::from("/tmp/luna_mcp_p2_demo.smc");
        std::fs::write(&path, demo_lorom()).unwrap();
        s.load_rom(Parameters(rom_params(&path.to_string_lossy())))
            .await
            .unwrap();
        // Inject `LDA #$42; STA $0200; JMP $0100` at $00:0100 and aim PC.
        s.poke_memory(Parameters(PokeMemoryParams {
            bank: 0x7E,
            offset: 0x0100,
            symbol: None,
            data: vec![0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
        }))
        .await
        .unwrap();
        for (reg, val) in [("pb", 0x00u32), ("pc", 0x0100), ("db", 0x00)] {
            s.set_cpu_register(Parameters(SetRegisterParams {
                reg: reg.into(),
                val,
            }))
            .await
            .unwrap();
        }

        // Watchpoint on the STA target (defaults: single address, write).
        let wp = s
            .bp_add(Parameters(BpAddParams {
                kind: "mem".into(),
                addr: 0x00_0200,
                symbol: None,
                hi: None,
                on_read: false,
                on_write: true,
            }))
            .await
            .unwrap()
            .0
            .id;
        // Exec bp on the JMP.
        let xp = s
            .bp_add(Parameters(BpAddParams {
                kind: "exec".into(),
                addr: 0x00_0105,
                symbol: None,
                hi: None,
                on_read: false,
                on_write: true,
            }))
            .await
            .unwrap()
            .0
            .id;
        assert!(
            s.bp_add(Parameters(BpAddParams {
                kind: "bogus".into(),
                addr: 0,
                symbol: None,
                hi: None,
                on_read: false,
                on_write: true,
            }))
            .await
            .is_err()
        );
        assert_eq!(s.bp_list().await.unwrap().0.breakpoints.len(), 2);

        // The watchpoint (STA at $0102) fires first.
        let out = s
            .run_until_break(Parameters(RunUntilBreakParams { max_steps: 100 }))
            .await
            .unwrap()
            .0;
        assert!(out.hit);
        assert_eq!(out.bp_id, Some(wp));
        assert_eq!(out.kind.as_deref(), Some("write"));
        assert_eq!((out.addr, out.value), (Some(0x00_0200), Some(0x42)));
        assert_eq!(out.pc, Some(0x00_0102));

        // Remove it; the exec bp fires next (before the JMP executes).
        assert!(
            s.bp_remove(Parameters(BpRemoveParams { id: wp }))
                .await
                .unwrap()
                .0
                .removed
        );
        let out = s
            .run_until_break(Parameters(RunUntilBreakParams { max_steps: 100 }))
            .await
            .unwrap()
            .0;
        assert_eq!(out.bp_id, Some(xp));
        assert_eq!(out.kind.as_deref(), Some("exec"));
        assert_eq!(out.pc, Some(0x00_0105));

        // Clear all: the run completes its budget.
        s.bp_clear_all().await.unwrap();
        let out = s
            .run_until_break(Parameters(RunUntilBreakParams { max_steps: 10 }))
            .await
            .unwrap()
            .0;
        assert!(!out.hit);
        assert_eq!(out.steps, 10);

        let _ = std::fs::remove_file(&path);
    }

    /// P3 symbol surface (issue #67): load a .sym over MCP, resolve,
    /// then drive `peek`/`poke`/`bp_add` by name and see annotated disasm.
    #[tokio::test]
    async fn server_p3_symbols_round_trip() {
        let s = LunaServer::new();
        let rom_path = PathBuf::from("/tmp/luna_mcp_p3_demo.smc");
        std::fs::write(&rom_path, demo_lorom()).unwrap();
        s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
            .await
            .unwrap();

        let sym_path = PathBuf::from("/tmp/luna_mcp_p3_demo.sym");
        std::fs::write(&sym_path, "[labels]\n00:0100 main\n7e:0200 monster_x\n").unwrap();
        let n = s
            .load_symbols(Parameters(LoadSymbolsParams {
                path: sym_path.to_string_lossy().into(),
            }))
            .await
            .unwrap();
        assert_eq!(n.0.count, 2);

        // resolve_symbol: known and unknown.
        let r = s
            .resolve_symbol(Parameters(ResolveSymbolParams {
                name: "monster_x".into(),
            }))
            .await
            .unwrap();
        assert_eq!(r.0.addr, Some(0x7E_0200));
        let r = s
            .resolve_symbol(Parameters(ResolveSymbolParams {
                name: "nope".into(),
            }))
            .await
            .unwrap();
        assert_eq!(r.0.addr, None);

        // poke by symbol, peek back by symbol.
        s.poke_memory(Parameters(PokeMemoryParams {
            bank: 0,
            offset: 0,
            symbol: Some("monster_x".into()),
            data: vec![0xAB, 0xCD],
        }))
        .await
        .unwrap();
        let bytes = s
            .peek_memory(Parameters(PeekMemoryParams {
                bank: 0,
                offset: 0,
                symbol: Some("monster_x".into()),
                count: 2,
            }))
            .await
            .unwrap();
        assert_eq!(bytes.0.bytes, vec![0xAB, 0xCD]);
        // Unknown symbol → invalid-params, not a silent bank-0 read.
        assert!(
            s.peek_memory(Parameters(PeekMemoryParams {
                bank: 0,
                offset: 0,
                symbol: Some("nope".into()),
                count: 1,
            }))
            .await
            .is_err()
        );

        // bp_add by symbol registers at the resolved address.
        let id = s
            .bp_add(Parameters(BpAddParams {
                kind: "mem".into(),
                addr: 0,
                symbol: Some("monster_x".into()),
                hi: None,
                on_read: false,
                on_write: true,
            }))
            .await
            .unwrap()
            .0
            .id;
        let list = s.bp_list().await.unwrap().0.breakpoints;
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].lo, 0x7E_0200);

        // Annotated disassembly at a labeled address.
        let d = s
            .disasm_cpu(Parameters(DisasmCpuParams {
                addr: Some(0x00_0100),
                lines: Some(1),
                m8: Some(true),
                x8: Some(true),
            }))
            .await
            .unwrap();
        assert_eq!(d.0.lines[0].symbol.as_deref(), Some("main"));

        let _ = std::fs::remove_file(&rom_path);
        let _ = std::fs::remove_file(&sym_path);
    }

    #[tokio::test]
    async fn server_nocash_and_wdm_logs_round_trip() {
        let s = LunaServer::new();

        // Without a ROM both enables surface an error.
        assert!(s.enable_nocash_log().await.is_err());
        assert!(s.enable_wdm_log().await.is_err());

        // Patch a program into the demo ROM at $00:8000: emit "HI" on the
        // $21FC Nocash TTY, fire the SNES_ASSERT-style `WDM #$00`, then spin.
        let mut rom = demo_lorom();
        let prog: &[u8] = &[
            0xA9, 0x48, // LDA #'H'
            0x8D, 0xFC, 0x21, // STA $21FC
            0xA9, 0x49, // LDA #'I'
            0x8D, 0xFC, 0x21, // STA $21FC
            0x42, 0x00, // WDM #$00
            0x80, 0xFE, // BRA *
        ];
        rom[..prog.len()].copy_from_slice(prog);
        // Re-fix the header checksum the patch just invalidated.
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

        let rom_path = PathBuf::from("/tmp/luna_mcp_nocash_wdm_demo.smc");
        std::fs::write(&rom_path, rom).unwrap();
        s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
            .await
            .unwrap();

        s.enable_nocash_log().await.unwrap();
        s.enable_wdm_log().await.unwrap();

        // A label so take_wdm_log symbolises the recorded PC.
        let sym_path = PathBuf::from("/tmp/luna_mcp_nocash_wdm_demo.sym");
        std::fs::write(&sym_path, "[labels]\n00:8000 main\n").unwrap();
        s.load_symbols(Parameters(LoadSymbolsParams {
            path: sym_path.to_string_lossy().into(),
        }))
        .await
        .unwrap();

        s.step(Parameters(StepParams { count: 32 })).await.unwrap();

        let nocash = s.take_nocash_log().await.unwrap();
        assert_eq!(nocash.0.text, "HI");
        assert_eq!(nocash.0.base64, "SEk=");

        let wdm = s.take_wdm_log().await.unwrap();
        assert_eq!(wdm.0.events.len(), 1);
        let ev = &wdm.0.events[0];
        assert_eq!(ev.operand, 0x00);
        // The core records the operand byte's address (opcode at $00:800A).
        assert_eq!(ev.pc, 0x00_800B);
        assert_eq!(ev.symbol.as_deref(), Some("main+0x0B"));

        // Draining resets both channels.
        assert!(s.take_nocash_log().await.unwrap().0.text.is_empty());
        assert!(s.take_wdm_log().await.unwrap().0.events.is_empty());
    }

    #[tokio::test]
    async fn server_forced_loading_and_port_device_round_trip() {
        let s = LunaServer::new();
        let rom_path = PathBuf::from("/tmp/luna_mcp_forced_demo.smc");
        std::fs::write(&rom_path, demo_lorom()).unwrap();

        // Auto-detection sees the LoROM header...
        let info = s
            .load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
            .await
            .unwrap();
        assert_eq!(info.0.rom.mapper, "LoRom");

        // A checksum-corrupted image is rejected by auto-detection...
        let mut broken = demo_lorom();
        broken[0x7FDC] ^= 0xFF;
        let broken_path = PathBuf::from("/tmp/luna_mcp_forced_demo_broken.smc");
        std::fs::write(&broken_path, &broken).unwrap();
        assert!(
            s.load_rom(Parameters(rom_params(&broken_path.to_string_lossy())))
                .await
                .is_err()
        );

        // ...but force_mapper loads it anyway (the point of the flag).
        let info = s
            .load_rom(Parameters(LoadRomParams {
                path: broken_path.to_string_lossy().into(),
                force_mapper: Some("lorom".into()),
                force_region: Some("pal".into()),
            }))
            .await
            .unwrap();
        assert_eq!(info.0.rom.mapper, "LoRom");
        assert!(!info.0.rom.checksum_valid);

        // Bad vocabulary is an invalid_params error, not a load attempt.
        assert!(
            s.load_rom(Parameters(LoadRomParams {
                path: rom_path.to_string_lossy().into(),
                force_mapper: Some("wat".into()),
                force_region: None,
            }))
            .await
            .is_err()
        );
        assert!(
            s.load_rom(Parameters(LoadRomParams {
                path: rom_path.to_string_lossy().into(),
                force_mapper: None,
                force_region: Some("secam".into()),
            }))
            .await
            .is_err()
        );

        // load_rom_bytes: same image over base64, no host file involved.
        let info = s
            .load_rom_bytes(Parameters(LoadRomBytesParams {
                rom_base64: b64(&demo_lorom()),
                force_mapper: None,
                force_region: None,
            }))
            .await
            .unwrap();
        assert_eq!(info.0.rom.title.trim(), "LUNA MCP DEMO");
        assert!(
            s.load_rom_bytes(Parameters(LoadRomBytesParams {
                rom_base64: "not-base64!!".into(),
                force_mapper: None,
                force_region: None,
            }))
            .await
            .is_err()
        );

        // set_port_device: plug a mouse into P1, feed it, unplug back to pad.
        for device in ["mouse", "joypad"] {
            s.set_port_device(Parameters(SetPortDeviceParams {
                port: 0,
                device: device.into(),
            }))
            .await
            .unwrap();
        }
        assert!(
            s.set_port_device(Parameters(SetPortDeviceParams {
                port: 0,
                device: "lightgun".into(),
            }))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn server_determinism_oracles_round_trip() {
        let s = LunaServer::new();

        // Everything errors cleanly without a ROM.
        assert!(
            s.frame_hash(Parameters(FrameHashParams::default()))
                .await
                .is_err()
        );
        assert!(
            s.wram_page_hashes(Parameters(WramPageHashesParams::default()))
                .await
                .is_err()
        );
        assert!(
            s.loop_probe(Parameters(LoopProbeParams { max_steps: 10 }))
                .await
                .is_err()
        );

        let rom_path = PathBuf::from("/tmp/luna_mcp_oracles_demo.smc");
        std::fs::write(&rom_path, demo_lorom()).unwrap();
        s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
            .await
            .unwrap();

        // frame_hash: 16 hex chars, deterministic while nothing steps.
        let h1 = s
            .frame_hash(Parameters(FrameHashParams::default()))
            .await
            .unwrap();
        assert_eq!(h1.0.hash.len(), 16);
        assert!(h1.0.hash.chars().all(|c| c.is_ascii_hexdigit()));
        let h2 = s
            .frame_hash(Parameters(FrameHashParams::default()))
            .await
            .unwrap();
        assert_eq!(h1.0.hash, h2.0.hash);

        // native gate: BadArg until set_native_capture flips it on.
        assert!(
            s.frame_hash(Parameters(FrameHashParams {
                force_display: false,
                native: true,
            }))
            .await
            .is_err()
        );
        s.set_native_capture(Parameters(SetNativeCaptureParams { enabled: true }))
            .await
            .unwrap();
        s.step_until_frame(Parameters(StepUntilFrameParams {
            max_steps: 1_000_000,
        }))
        .await
        .unwrap();
        let hn = s
            .frame_hash(Parameters(FrameHashParams {
                force_display: false,
                native: true,
            }))
            .await
            .unwrap();
        assert_eq!(hn.0.hash.len(), 16);

        // wram_page_hashes: default page size → 32 pages; bad size errors.
        let pages = s
            .wram_page_hashes(Parameters(WramPageHashesParams::default()))
            .await
            .unwrap();
        assert_eq!(pages.0.page_size, 0x1000);
        assert_eq!(pages.0.hashes.len(), 32);
        assert!(
            s.wram_page_hashes(Parameters(WramPageHashesParams { page_size: 3 }))
                .await
                .is_err()
        );

        // wram_snapshot: hash equals the one full-width page hash, data
        // round-trips at 128 KiB only when asked for.
        let snap = s
            .wram_snapshot(Parameters(WramSnapshotParams::default()))
            .await
            .unwrap();
        assert!(snap.0.wram_base64.is_none());
        let full = s
            .wram_page_hashes(Parameters(WramPageHashesParams { page_size: 0x20000 }))
            .await
            .unwrap();
        assert_eq!(snap.0.hash, full.0.hashes[0]);
        let snap = s
            .wram_snapshot(Parameters(WramSnapshotParams { include_data: true }))
            .await
            .unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .decode(snap.0.wram_base64.unwrap())
            .unwrap();
        assert_eq!(data.len(), 0x20000);

        // loop_probe advances the CPU and reports a plausible shape.
        let probe = s
            .loop_probe(Parameters(LoopProbeParams { max_steps: 500 }))
            .await
            .unwrap();
        assert!(probe.0.executed <= 500);
        assert!(probe.0.distinct_pcs >= 1);
    }

    #[tokio::test]
    async fn server_trace_parity_round_trip() {
        let s = LunaServer::new();

        // Every enable errors cleanly without a ROM.
        assert!(s.enable_mailbox_log().await.is_err());
        assert!(
            s.enable_dma_trace(Parameters(EnableRingTraceParams { max_events: 16 }))
                .await
                .is_err()
        );

        // A program that pokes the APU mailbox so the log has real traffic.
        let mut rom = demo_lorom();
        let prog: &[u8] = &[
            0xA9, 0xCC, // LDA #$CC
            0x8D, 0x40, 0x21, // STA $2140
            0xAD, 0x40, 0x21, // LDA $2140
            0x80, 0xFE, // BRA *
        ];
        rom[..prog.len()].copy_from_slice(prog);
        let rom_path = PathBuf::from("/tmp/luna_mcp_traces_demo.smc");
        std::fs::write(&rom_path, rom).unwrap();
        s.load_rom(Parameters(LoadRomParams {
            path: rom_path.to_string_lossy().into(),
            force_mapper: Some("lorom".into()),
            force_region: None,
        }))
        .await
        .unwrap();

        // Enable all nine, run a while, drain all nine.
        s.enable_mailbox_log().await.unwrap();
        s.enable_sa1_log().await.unwrap();
        s.enable_sa1_side_log().await.unwrap();
        for max in [64usize] {
            s.enable_dma_trace(Parameters(EnableRingTraceParams { max_events: max }))
                .await
                .unwrap();
            s.enable_dsp_trace(Parameters(EnableRingTraceParams { max_events: max }))
                .await
                .unwrap();
            s.enable_sa1_trace(Parameters(EnableRingTraceParams { max_events: max }))
                .await
                .unwrap();
            s.enable_superfx_trace(Parameters(EnableRingTraceParams { max_events: max }))
                .await
                .unwrap();
            s.enable_spc_trace(Parameters(EnableRingTraceParams { max_events: max }))
                .await
                .unwrap();
        }
        s.enable_dsp1_trace(Parameters(EnableDsp1TraceParams {
            max_events: 64,
            ports_only: false,
        }))
        .await
        .unwrap();

        s.step(Parameters(StepParams { count: 2000 }))
            .await
            .unwrap();

        // The mailbox saw our $2140 write + read, tagged with the writing PC.
        let mail = s.take_mailbox_log().await.unwrap();
        assert!(!mail.0.events.is_empty());
        let w = mail.0.events.iter().find(|e| e.kind == "write").unwrap();
        assert_eq!(w.port, 0);
        assert_eq!(w.value, 0xCC);
        assert_eq!(w.pc >> 16, 0x00);

        // The SPC700 IPL boot ROM executed instructions.
        let spc = s.take_spc_trace().await.unwrap();
        assert!(!spc.0.events.is_empty());

        // Coprocessor traces drain empty on a plain LoROM cart, not error.
        assert!(s.take_sa1_log().await.unwrap().0.events.is_empty());
        assert!(s.take_sa1_side_log().await.unwrap().0.events.is_empty());
        assert!(s.take_sa1_trace().await.unwrap().0.events.is_empty());
        assert!(s.take_superfx_trace().await.unwrap().0.events.is_empty());
        let dsp1 = s
            .take_dsp1_trace(Parameters(TakeDsp1TraceParams {
                decode_commands: true,
            }))
            .await
            .unwrap();
        assert!(dsp1.0.events.is_empty());
        assert!(dsp1.0.commands.is_some_and(|c| c.is_empty()));
        assert!(!dsp1.0.truncated);

        // DMA + DSP traces drain Ok (the demo program does no DMA; the IPL
        // may or may not touch DSP registers — shape only).
        s.take_dma_trace().await.unwrap();
        s.take_dsp_trace().await.unwrap();

        // Draining reset the mailbox log.
        assert!(s.take_mailbox_log().await.unwrap().0.events.is_empty());
    }

    /// `load_rom` params with no mapper/region override — the common case.
    fn rom_params(path: &str) -> LoadRomParams {
        LoadRomParams {
            path: path.into(),
            force_mapper: None,
            force_region: None,
        }
    }

    fn demo_lorom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        let title = b"LUNA MCP DEMO        ";
        rom[0x7FC0..0x7FC0 + title.len()].copy_from_slice(title);
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x07;
        rom[0x7FD8] = 0x00;
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
}
