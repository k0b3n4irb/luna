//! Luna GUI built on egui + wgpu (via eframe).
//!
//! Serves two modes from the same binary:
//!
//! - **Standalone**: a human plays a SNES game with keyboard/gamepad input.
//! - **Spectator**: an AI plays via MCP, the human observes with overlays
//!   showing recent tool calls, highlighted sprites/memory the agent
//!   queried, and a live token-budget counter.
//!
//! Cross-target via `eframe` (native + web).
//!
//! See `ARCHITECTURE.md` §3.2 and §10.
