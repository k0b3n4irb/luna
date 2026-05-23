//! MCP types shared by the server and client crates — cross-target.
//!
//! Holds `Tool`, `Resource`, `ToolCall`, `ToolResult` and the JSON schemas
//! derived from `luna-api` types via `schemars`. No runtime, no I/O —
//! compiles cleanly on `wasm32-unknown-unknown`.
//!
//! See `ARCHITECTURE.md` §8 and §9.2.
