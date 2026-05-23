//! Luna SNES emulator — command-line entry point.
//!
//! Dispatches between execution modes (headless / standalone / spectator
//! / replay). See `ARCHITECTURE.md` §3.2.

fn main() {
    println!(
        "luna {} — SNES emulator with introspection API",
        env!("CARGO_PKG_VERSION")
    );
    println!("Phase 0 — no functionality yet. See ARCHITECTURE.md for the plan.");
}
