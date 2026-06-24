# Luna

**A cycle-accurate SNES emulator, written in Rust — built so an AI agent can
play, develop and debug Super Nintendo games on its own.**

---

Most emulators bolt AI on afterwards, by reading screenshots. Luna makes the
**agent ↔ machine dialogue first-class**: the whole machine state — CPU and PPU
registers, VRAM, OAM, palette, sprites, memory — is exposed as structured,
serializable snapshots, and a built-in **MCP server** lets an agent drive the
console over JSON-RPC to *play*, *build homebrew*, or *debug ROM hacks*.

And it does not trade away fidelity to get there. Both CPU cores pass the
SingleStepTests suites 100%, and every subsystem is a **faithful port of
[ares](https://ares-emu.net/) and [Mesen2](https://github.com/SourMesen/Mesen2)** —
verified, where it matters, by a headless differential against those references.

## How this guide is organised

- **[Using Luna](using/install.md)** — install, run a game, the controls and
  saves, and the headless CLI / MCP surface. Start here if you just want to
  *play*.

- **[How Luna Works](internals/architecture.md)** — the layered architecture and
  a tour of each subsystem (PPU, APU, the two CPUs, memory/DMA, the
  coprocessors). Start here if you want to *understand or hack on* Luna.

- **[The Faithful-Port Method](method/faithful-port.md)** — the heart of the
  project: why Luna is translated from ares/Mesen2 rather than invented, the
  self-contained differential harness that proves it, the accuracy scorecard,
  and the road to grade "A" everywhere.

- **[API reference (rustdoc) ↗](api/index.html)** — the generated Rust API docs
  for all twelve crates.

## At a glance

| | |
|---|---|
| **Language** | Rust (2024 edition) |
| **Cores** | 65C816 + SPC700 (SingleStepTests 100%), S-DSP audio |
| **Coprocessors** | SA-1, Super FX (GSU), DSP-1, S-DD1 |
| **Front-ends** | GUI debugger (winit + wgpu), headless CLI, MCP server |
| **Platform** | Linux (macOS / Windows may build, unsupported) |
| **License** | [MPL-2.0](https://github.com/kobenairb/luna/blob/main/LICENSE) |

> Luna is one binary that runs three ways: **standalone** (a human plays),
> **spectator** (the AI plays, a human watches), or **headless** (driven over
> MCP, for CI and agents) — all through the *same* observation and control
> surface.
