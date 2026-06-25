<div align="center">

# 🌙 Luna

### A cycle-accurate SNES emulator, written in Rust — built so an AI agent can play, develop and debug Super Nintendo games on its own.

[![Release](https://img.shields.io/github/v/release/k0b3n4irb/luna?color=brightgreen)](https://github.com/k0b3n4irb/luna/releases/latest)
[![Docs](https://img.shields.io/badge/docs-k0b3n4irb.github.io%2Fluna-8a7cff)](https://k0b3n4irb.github.io/luna/)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange)](rust-toolchain.toml)

<!-- TODO: drop a GUI screenshot / gameplay GIF here — it makes the porch.
     Capture the debugger on a game and commit it to docs/assets/. -->

**[📖 Documentation](https://k0b3n4irb.github.io/luna/) · [⬇️ Download](https://github.com/k0b3n4irb/luna/releases/latest) · [📚 API reference](https://k0b3n4irb.github.io/luna/api/)**

</div>

---

Most emulators bolt AI on afterwards, by reading screenshots. **Luna makes the
agent ↔ machine dialogue first-class.** The whole machine state — CPU and PPU
registers, VRAM, OAM, palette, sprites, memory — is exposed as structured,
serializable snapshots, and a built-in **MCP server** lets an agent drive the
console over JSON-RPC to *play*, *build homebrew*, or *debug ROM hacks*.

And it does not trade away fidelity to get there. Luna is **cycle-accurate**:
both CPU cores pass their exhaustive per-instruction test suites 100%, the audio
and video paths are reconstructed down to per-access timing, and that accuracy
is held in place by a self-contained differential harness — so what the headless
CLI measures is exactly what you see on screen.

One binary runs three ways: **standalone** (a human plays), **spectator** (the
AI plays, a human watches), or **headless** (driven over MCP, for CI and agents).

## ✨ Features

- **Cycle-accurate cores** — 65C816 + SPC700 (per-instruction suites 100%), S-DSP audio.
- **The big coprocessors** — SA-1, Super FX (GSU), DSP-1, S-DD1.
- **AI-native** — a stable introspection API + an MCP server; an agent reads
  state and drives input through one contract the CLI, GUI and MCP all share.
- **A real debugger GUI** — memory, disassembly and live panels over winit + wgpu,
  two remappable controllers, plus SNES Mouse & Super Scope.
- **Never lose progress** — automatic `.srm` battery saves + 9 save-state slots.

## 🙏 Acknowledgements

Luna stands on the shoulders of the people and projects that made accurate SNES
emulation a shared, documented science. It would not exist without them:

- **[ares](https://ares-emu.net/)** — the gold standard for SNES hardware
  accuracy, and Luna's primary reference for getting each subsystem right.
- **[Mesen2](https://github.com/SourMesen/Mesen2)** — an independent second
  source and an invaluable debugging companion; its headless test runner makes
  Luna's differential validation possible.
- **[Tom Harte's processor tests](https://github.com/SingleStepTests)** — the
  exhaustive per-instruction test suites that pin down the 65C816 and SPC700 to
  the cycle.
- **The homebrew hardware-test ROM authors** — whose golden test ROMs exercise
  corners of the hardware no commercial game reaches.
- **The wider SNES emulation and reverse-engineering community** — decades of
  documentation, disassembly and patient measurement of real silicon.

Thank you. 🙇

## 📄 License

[Mozilla Public License 2.0](LICENSE).
