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
//! Transport is stdio by default ([`serve_stdio`]); a future commit
//! will add HTTP-SSE for browser clients.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use luna_api::{ApiError, Emulator, EmulatorState, RomInfo};
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
    /// 8-bit CPU bank (`$00..$FF`).
    pub bank: u8,
    /// 16-bit offset within that bank.
    pub offset: u16,
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
    pub bank: u8,
    /// 16-bit offset within that bank.
    pub offset: u16,
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
    /// 24-bit target PC (`pb << 16 | pc`).
    pub pc: u32,
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
            em.peek_memory(params.bank, params.offset, params.count)
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
            em.poke_memory(params.bank, params.offset, &params.data)
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
            em.run_until_pc(params.pc, params.max_steps)
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
