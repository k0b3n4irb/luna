//! Cross-target async runtime façade for Luna.
//!
//! Exposes a minimal API (`spawn`, `sleep`, `mpsc`, `oneshot`) backed by
//! `tokio` on native targets and `wasm-bindgen-futures` + `gloo-timers`
//! on `wasm32-unknown-unknown`.
//!
//! Bans direct use of `tokio::*` and `crossbeam-channel` in the Luna core,
//! both of which fail (or panic at runtime) under WASM.
//!
//! See `ARCHITECTURE.md` §4.1.
