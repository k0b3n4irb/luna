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
//!
//! Transport is stdio by default ([`serve_stdio`]); a future commit
//! will add HTTP-SSE for browser clients.

use std::path::PathBuf;
use std::sync::Arc;

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
    tool_router: ToolRouter<Self>,
}

// ---------------- Tool parameter types ----------------

/// `load_rom` parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LoadRomParams {
    /// Absolute path to a `.sfc` / `.smc` ROM file on the local
    /// filesystem.
    pub path: String,
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
            tool_router: Self::tool_router(),
        }
    }

    #[rmcp::tool(
        description = "Load a SNES ROM (.sfc / .smc) from a path on the host filesystem. \
                                Returns parsed cartridge metadata."
    )]
    async fn load_rom(
        &self,
        Parameters(params): Parameters<LoadRomParams>,
    ) -> Result<rmcp::Json<LoadRomResult>, ErrorData> {
        let info = {
            let mut em = self.emulator.lock().await;
            em.load_rom(&PathBuf::from(params.path))
                .map_err(|e| api_err_to_mcp(&e))?
        };
        Ok(rmcp::Json(LoadRomResult { rom: info }))
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
        let png_base64 = base64::engine::general_purpose::STANDARD.encode(&png);
        Ok(rmcp::Json(ScreenshotResult {
            png_base64,
            width: 256,
            height: 224,
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
            state_base64: base64::engine::general_purpose::STANDARD.encode(&blob),
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
            png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
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
            png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
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
            png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
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
            png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
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
        Ok(rmcp::Json(match out.hit {
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
            },
            None => RunUntilBreakResult {
                steps: out.steps,
                hit: false,
                bp_id: None,
                kind: None,
                pc: None,
                addr: None,
                value: None,
            },
        }))
    }

    #[rmcp::tool(
        description = "Feed SNES Mouse input for the next joypad auto-read: accumulated \
                                `dx`/`dy` displacement plus the button bitmask (bit 0 = left, \
                                bit 1 = right). Select the mouse first via the GUI or the CLI \
                                `--port1 mouse`."
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
                                turbo). Select the scope first (GUI or CLI `--port2 superscope`)."
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

    /// Loading a non-existent ROM bubbles the I/O error up through
    /// the MCP layer.
    #[tokio::test]
    async fn server_load_rom_missing_file_returns_error() {
        let s = LunaServer::new();
        let result = s
            .load_rom(Parameters(LoadRomParams {
                path: "/tmp/luna-this-file-does-not-exist.smc".into(),
            }))
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
            .load_rom(Parameters(LoadRomParams {
                path: path.to_string_lossy().into(),
            }))
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
        s.load_rom(Parameters(LoadRomParams {
            path: path.to_string_lossy().into(),
        }))
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
        s.load_rom(Parameters(LoadRomParams {
            path: path.to_string_lossy().into(),
        }))
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
        s.load_rom(Parameters(LoadRomParams {
            path: rom_path.to_string_lossy().into(),
        }))
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
