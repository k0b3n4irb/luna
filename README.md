# Luna

> A **cycle-accurate** SNES emulator written in Rust, designed so that an AI
> agent can **play**, **develop** and **debug** Super Nintendo games
> autonomously — through a rich introspection API and a built-in MCP server.

[![Rust edition](https://img.shields.io/badge/Rust-2024-orange)](rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](LICENSE)
[![Status](https://img.shields.io/badge/status-pre--1.0-yellow)](#status)
[![Platform](https://img.shields.io/badge/platform-Linux-informational)](#platform-support)

---

## Why Luna?

Traditional SNES emulators treat AI as a second-class use case — something you
bolt on afterwards via OCR over screenshots. Luna flips that priority: the
**agent ↔ machine** dialogue is a central design goal.

In practice, the full machine state (CPU registers, VRAM, OAM, palette,
scroll, tilemap, sprites, memory) is exposed in a **structured, serializable**
form, and a built-in **MCP** (Model Context Protocol) server lets an agent like
Claude drive the machine through a catalogue of standardized JSON-RPC tools —
without ever looking at a single pixel if it doesn't want to.

Three use cases are first-class from the start:

- 🎮 **Play** — the agent plays an existing game.
- 🛠️ **Dev** — the agent develops a homebrew.
- 🐛 **Debug** — the agent inspects a ROM hack (breakpoints, trace, memory).

Hardware fidelity is not sacrificed for it: the CPU cores are validated
against reference test suites, and every subsystem is implemented by reading
the reference emulators (ares, Mesen2) before writing a single line — see
[`docs/emulator_landscape.md`](docs/emulator_landscape.md) for the survey that
motivated those reference choices.

## Status

Project under **active development, pre-1.0** (`v0.0.1`). What runs today:

| Subsystem | Crate | State |
|---|---|---|
| Bus & memory map (LoROM / HiROM / ExHiROM / SA-1) | `luna-bus` | ✅ |
| ROM parsing & mapper detection | `luna-cartridge` | ✅ |
| 65C816 CPU (cycle-accurate, SingleStepTests suite 100%) | `luna-cpu-65c816` | ✅ |
| SPC700 CPU (cycle-accurate, SingleStepTests suite 100%) | `luna-cpu-spc700` | ✅ |
| APU — SPC700 + S-DSP (cycle-accurate ares port) | `luna-apu` | ✅ |
| PPU + renderer + compositor | `luna-ppu` | ✅ |
| System glue, scheduler, DMA / HDMA, SA-1 coprocessor | `luna-core` | ✅ |
| Introspection API (`EmulatorState` snapshots) | `luna-api` | ✅ |
| MCP server (stdio) | `luna-mcp-server` | ✅ |
| CLI binary (`run` / `state` / `mcp`) | `luna-cli` | ✅ |
| GUI debugger (eframe, audio-as-clock pacing) | `luna-gui` | ✅ |

Coprocessors beyond SA-1 (Super FX, DSP-1…), REST/WebSocket transports and a
WASM target are on the [roadmap](ARCHITECTURE.md#14-roadmap--phasing), not yet
shipped.

## Platform support

Luna is currently **developed and tested on Linux only**. The stack
(eframe/wgpu for the GUI, cpal for audio) is cross-platform in principle, but
macOS and Windows are **not tested or supported yet** — they may build and run,
but no guarantees. Contributions to validate other platforms are welcome.

## Quick start

Prerequisites: the Rust toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
(2024 edition, Rust ≥ 1.85), on Linux.

```bash
# Full build (debug + release)
cargo build --release --workspace

# Launch the graphical debugger on a ROM
cargo run --release -p luna-gui -- "path/to/game.sfc"
```

### The `luna` binary (CLI)

```bash
# Run N instructions and dump a screenshot (headless, no GUI)
./target/release/luna run "game.sfc" -n 2000000 --screenshot /tmp/frame.png

# Emit a JSON snapshot of the machine state (the same data the MCP get_state tool returns)
./target/release/luna state "game.sfc" -n 30000 --out -

# Serve the MCP server over stdio (for Claude Desktop / Claude Code / custom clients)
./target/release/luna mcp
```

## Architecture at a glance

Luna is an 11-crate Cargo workspace, organized in layers that communicate only
through Rust contracts (traits + serializable types) — no lower layer ever
depends on a higher one.

```
┌──────────────────────────────────────────────────────────┐
│  MCP server (luna-mcp-server)  — JSON-RPC over stdio       │
├──────────────────────────────────────────────────────────┤
│  Introspection API (luna-api)  — stable public contract    │
├──────────────────────────────────────────────────────────┤
│  Emulation core (luna-core)                                │
│   65C816 · PPU · SPC700/DSP · DMA · SA-1 · scheduler       │
├──────────────────────────────────────────────────────────┤
│  Bus & mappers (luna-bus)                                  │
└──────────────────────────────────────────────────────────┘
        ▲                                  ▲
   luna-cli (headless)              luna-gui (egui/wgpu)
```

This decoupling enables three **execution modes**, combinable on the same
binary:

- **Headless** — no window, driven 100% via MCP (AI in production, CI).
- **Standalone** — native window, keyboard/gamepad (a human plays).
- **Spectator** — the AI plays, the human watches the framebuffer and the
  agent's activity in real time.

The full design (vision, non-goals, layers, threading, determinism, roadmap)
is documented in **[`ARCHITECTURE.md`](ARCHITECTURE.md)**.

## Documentation

| Document | Contents |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Full system design, layers, roadmap |
| [`RESEARCH.md`](RESEARCH.md) | Pre-Phase-0 research (fork vs from-scratch, WASM, scheduler) |
| [`CLAUDE.md`](CLAUDE.md) | Repository conventions for contributors (and agents) |
| [`docs/`](docs/) | PPU/APU/SA-1 reference specs, accuracy scorecard, gap lists |
| [`docs/emulator_landscape.md`](docs/emulator_landscape.md) | Comparative survey of existing SNES emulators |

## Development

The canonical sequence before any commit (rebuild + tests + lint):

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Detailed conventions (reference-first, coprocessor test discipline,
audio/video validation workflow) live in [`CLAUDE.md`](CLAUDE.md) and
`.claude/rules/`.

## License

Distributed under the **Mozilla Public License 2.0** — see [`LICENSE`](LICENSE).
