# CLAUDE.md

This file provides guidance to Claude Code when working in the luna
repository. Keep it lean — workflow detail belongs in `.claude/rules/`
(auto-loaded) and `.claude/commands/` (slash commands). Reference data
belongs in `docs/`.

## What luna is

A cycle-accurate-ish SNES emulator written in Rust. 12-crate workspace:

- `crates/luna-bus/` — foundation: `Bus` trait, `Addr24`, `MapperKind`
  enum, the per-mapper shims (LoROM / HiROM / ExHiROM / SA-1 /
  Super FX / S-DD1; the DSP-1 mapper lives in `luna-core`). Used by
  every CPU and the system glue.
- `crates/luna-cartridge/` — ROM header parser, mapper detection.
- `crates/luna-cpu-65c816/`, `crates/luna-cpu-spc700/` — CPU cores.
  Standalone, no SNES-specific glue — usable from any consumer.
- `crates/luna-cpu-upd96050/` — NEC uPD7725 / uPD96050 DSP core (drives
  DSP-1, etc.); standalone like the other CPU cores.
- `crates/luna-apu/` — SPC700 bridge + S-DSP (cycle-accurate ares port
  in `src/dsp.rs`, see commit `7a5c599`).
- `crates/luna-ppu/` — PPU + renderer + compositor.
- `crates/luna-core/` — system glue. Owns the top-level `Snes` struct,
  the CPU-driven master-clock scheduler, and the **DMA + coprocessor**
  (SA-1 chip side, DSP-1) subsystems as `crate::dma` and `crate::coproc` modules
  (merged from the former `luna-dma` / `luna-coproc` crates in
  commit `1398a2c`).
- `crates/luna-api/` — introspection surface; produces serialisable
  `EmulatorState` snapshots for the CLI, GUI, and MCP server.
- `crates/luna-mcp-server/` — MCP transport (the GUI does not use it;
  invoked from `luna mcp` in the CLI binary).
- `crates/luna-cli/` — the `luna` binary (`luna run` / `luna state` /
  `luna test` / `luna mcp` and more; `luna --help` lists the subcommands).
- `crates/luna-gui/` — hand-rolled debugger UI (winit + pixels +
  egui-wgpu; eframe was removed 2026-05-28). Owns the dedicated emu
  thread (`src/emu_thread.rs`, video-as-clock frame limiter) and the
  cpal output stream (`src/audio.rs`, with 6-point cubic Hermite
  resampler, dynamic rate control + 5 Hz DC blocker).

## Rules (`.claude/rules/`)

Path-scoped rules load when you touch their files; read the matching one
before the code, not after.

- **Always loaded**: `no-co-authored-by` (one author, GitHub never commits,
  releases are a fast-forward `develop:main`), `exchanges` (neighbours, the
  game tests `target/stable/luna`), `agents` (task → agent).
- **Method, on `crates/**`**: `faithful-port-and-dichotomy` (THE method:
  translate the reference's architecture, bisect), `reference-first` (ares
  + Mesen2 before any subsystem change), `rebuild-discipline` (pre-commit
  sequence, clippy `--all-features`, the blocking gate, `/tmp` cleanup).
- **By area**: `api-first` (front-ends drive `luna-api` only),
  `hdma-dma-faithful-audit`, `coproc-testing`, `audible-fixes-test-first`,
  `doc-drift`, `housekeeping`, `no-dependency-bots` (permanently banned).

## Slash commands (`.claude/commands/`)

| Command | Purpose |
|---|---|
| `/rebuild` | Canonical workspace rebuild (debug + release, all targets) |
| `/smoke-test [smrpg\|smw\|both]` | Visual regression screenshot via luna-cli |
| `/reference-fetch <subsys>` | Fetch ares + Mesen2 sources for a subsystem into `/tmp/luna-ref/` |
| `/housekeeping` | Delete releases beyond the five highest and the one OpenSNES pins, and Actions runs older than a month |
| `/clean-tmp` | Delete what luna's tests, smoke runs and diagnostics left in `/tmp` (after the tests of a session) |

## Code style

Toolchain in `rust-toolchain.toml`; `cargo fmt` and clippy as in
`rebuild-discipline`. Public items get a one-line doc comment. Commits:
`type(scope): description`, `\$` escaped in subjects, author per
`no-co-authored-by`.

## Reference docs (in `docs/`)

- `book/src/using/controls.md` (in the user guide, not `docs/`) —
  keyboard → SNES button mapping (GUI).
- `ppu_compositor_reference.md` — synthesised ares + Mesen2 spec for
  the PPU compositor, color math, windows, DMA, NMI.
- `ares_ppu_notes.md`, `mesen2_ppu_notes.md` — raw per-source research
  notes the spec was built from.
- `accuracy_scorecard.md` + the per-subsystem `luna_*_gaps.md` — current
  correctness grades and open-work lists (the source of truth for gaps).

## What NOT to put here

Long workflows (`.claude/commands/`), reference tables (`docs/`), anything
derivable from `git log`, `Cargo.toml` or the source.
