//! MCP server for Luna — native-only.
//!
//! Wraps `rmcp` (Anthropic Rust MCP SDK) and exposes the Luna catalogue of
//! tools (control, debug, semantic) over stdio / HTTP-SSE / streamable
//! HTTP. Does **not** build for `wasm32-unknown-unknown`: rmcp depends on
//! tokio mainline.
//!
//! Luna Studio Web (V2 WASM) talks to a remote native instance via
//! WebSocket relayed by `luna-mcp-client`.
//!
//! See `ARCHITECTURE.md` §8 and §9.2.
