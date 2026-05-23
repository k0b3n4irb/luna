//! Luna's public stable API — the contract every transport (MCP, REST,
//! WebSocket, FFI, WASM) marshals to.
//!
//! Defines `EmulatorControl`, `EmulatorDebug`, `EmulatorSemantic` and
//! `EmulatorEvents` traits. This crate is the only one with strict SemVer
//! guarantees from V1 onwards.
//!
//! See `ARCHITECTURE.md` §7 and §9.5.
