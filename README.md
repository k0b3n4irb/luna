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

Most SNES emulators bolt AI on afterwards via OCR over screenshots. Luna makes
the **agent ↔ machine** dialogue first-class: the full machine state (CPU/PPU
registers, VRAM, OAM, palette, sprites, memory) is exposed as **structured,
serializable** snapshots, and a built-in **MCP** server lets an agent drive the
machine over JSON-RPC — to **play**, **dev** (homebrew), or **debug** (ROM
hacks). Fidelity isn't sacrificed: CPU cores pass the SingleStepTests suites and
every subsystem is ported from ares / Mesen2. Full vision in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

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
| System glue, scheduler, DMA / HDMA | `luna-core` | ✅ |
| Coprocessors: SA-1, Super FX (GSU), DSP-1 | `luna-core` / `luna-bus` | ✅ |
| NEC uPD7725 / uPD96050 DSP core (DSP-1) | `luna-cpu-upd96050` | ✅ |
| Introspection API (`EmulatorState` snapshots) | `luna-api` | ✅ |
| MCP server (stdio) | `luna-mcp-server` | ✅ |
| CLI binary (`run` / `state` / `frames` / `wram-trace` / `mcp`) | `luna-cli` | ✅ |
| GUI debugger (winit + pixels + egui-wgpu, audio-as-clock pacing) | `luna-gui` | ✅ |

Commercial titles play across the major chips — SMW, Super Mario RPG (SA-1),
Star Fox / Doom (Super FX), Super Mario Kart / Pilotwings (DSP-1 Mode 7) — and
the GUI ships Mesen2-style debugger panels. Remaining coprocessors (DSP-2/3/4,
Cx4, S-DD1, SPC7110), REST/WebSocket transports and a WASM target are on the
[roadmap](ARCHITECTURE.md#14-roadmap--phasing).

## Platform support

Luna is currently **developed and tested on Linux only**. The stack
(winit + pixels + egui-wgpu for the GUI, cpal for audio) is cross-platform in
principle, but
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

## Controls & firmware

Single **Player 1** keyboard controller, remappable in the GUI — arrows =
D-pad; `A`/`Z`/`S`/`X` = B/Y/A/X; `Q`/`W` = L/R; `D`/`E` = Start/Select; `F12` =
screenshot. **No Player 2, Mouse, or Super Scope yet.** Full table:
[`docs/CONTROLLER_BINDINGS.md`](docs/CONTROLLER_BINDINGS.md).

**DSP-1 games** (Super Mario Kart, Pilotwings) need a user-supplied `dsp1b.rom`
firmware — luna prompts for it (GUI) or takes `--dsp1-rom` (CLI). Setup:
[`docs/firmware.md`](docs/firmware.md).

## Architecture at a glance

Luna is a 12-crate Cargo workspace, organized in layers that communicate only
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

The same binary runs **headless** (driven via MCP — CI / production),
**standalone** (a human plays), or **spectator** (the AI plays, a human
watches). Full design — vision, layers, threading, determinism, roadmap — in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Documentation

| Document | Contents |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Full system design, layers, roadmap |
| [`CLAUDE.md`](CLAUDE.md) | Repository conventions for contributors (and agents) |
| [`docs/CONTROLLER_BINDINGS.md`](docs/CONTROLLER_BINDINGS.md) | Keyboard → SNES button map |
| [`docs/firmware.md`](docs/firmware.md) | Coprocessor firmware (DSP-1 `dsp1b.rom`) setup |
| [`docs/`](docs/) | Reference specs, accuracy scorecard, per-subsystem gap lists |
| [`docs/emulator_landscape.md`](docs/emulator_landscape.md) | Survey of existing SNES emulators |

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
