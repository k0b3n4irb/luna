# CLAUDE.md

This file provides guidance to Claude Code when working in the luna
repository. Keep it lean — workflow detail belongs in `.claude/rules/`
(auto-loaded) and `.claude/commands/` (slash commands). Reference data
belongs in `docs/`.

## What luna is

A cycle-accurate-ish SNES emulator written in Rust. 11-crate workspace:

- `crates/luna-bus/` — foundation: `Bus` trait, `Addr24`, `MapperKind`
  enum, the per-mapper shims (LoROM / HiROM / ExHiROM / SA-1). Used by
  every CPU and the system glue.
- `crates/luna-cartridge/` — ROM header parser, mapper detection.
- `crates/luna-cpu-65c816/`, `crates/luna-cpu-spc700/` — CPU cores.
  Standalone, no SNES-specific glue — usable from any consumer.
- `crates/luna-apu/` — SPC700 bridge + S-DSP (cycle-accurate ares port
  in `src/dsp.rs`, see commit `25c3691`).
- `crates/luna-ppu/` — PPU + renderer + compositor.
- `crates/luna-core/` — system glue. Owns the top-level `Snes` struct,
  the CPU-driven master-clock scheduler, and the **DMA + SA-1 / future
  coprocessor** subsystems as `crate::dma` and `crate::coproc` modules
  (merged from the former `luna-dma` / `luna-coproc` crates in
  commit `5cf2220`).
- `crates/luna-api/` — introspection surface; produces serialisable
  `EmulatorState` snapshots for the CLI, GUI, and MCP server.
- `crates/luna-mcp-server/` — MCP transport (the GUI does not use it;
  invoked from `luna mcp` in the CLI binary).
- `crates/luna-cli/` — `luna run` / `luna state` / `luna mcp` binary.
- `crates/luna-gui/` — eframe-based debugger UI. Owns the dedicated
  emu thread (`src/emu_thread.rs`, audio-as-clock pacing) and the cpal
  output stream (`src/audio.rs`, with 6-point cubic Hermite resampler
  + 5 Hz DC blocker).

## Mandates (auto-loaded from `.claude/rules/`)

| Rule | Source | When it applies |
|---|---|---|
| **Faithful port + step-by-step dichotomy (THE method)** | `.claude/rules/faithful-port-and-dichotomy.md` | Any accuracy/timing bug; any subsystem that misbehaves vs hardware. Supersedes ad-hoc debugging. |
| Reference-first implementation | `.claude/rules/reference-first.md` | Any SNES subsystem feature change |
| Rebuild + lint discipline | `.claude/rules/rebuild-discipline.md` | Every code change before commit |
| Rust lint discipline (clippy `--all-features`) | `.claude/rules/rust-lint-discipline.md` | Every code change before commit (extends rebuild) |
| Coprocessor / DMA / PPU test sweep | `.claude/rules/coproc-testing.md` | Edits to luna-ppu, luna-core/src/dma/, luna-core/src/coproc/, luna-bus/sa1.rs |
| Test audible / visible fixes before commit | `.claude/rules/audible-fixes-test-first.md` | Any change to APU / PPU rendering / GUI audio or framebuffer |
| API-first (CLI / MCP / GUI all drive `luna-api`, never `luna-core` directly) | `.claude/rules/api-first.md` | Any `luna-gui` change touching emulation / input / audio / framebuffer, or any front-end need for core state |

Read the matching rule before touching the relevant code, not after.

## Slash commands (`.claude/commands/`)

| Command | Purpose |
|---|---|
| `/rebuild` | Canonical workspace rebuild (debug + release, all targets) |
| `/smoke-test [smrpg\|smw\|both]` | Visual regression screenshot via luna-cli |
| `/reference-fetch <subsys>` | Fetch ares + Mesen2 sources for a subsystem into `/tmp/` |

## Code style

- Rust edition + toolchain version live in `rust-toolchain.toml` and
  the workspace `Cargo.toml`. Don't pin elsewhere.
- `cargo fmt` is the formatter; `cargo clippy --workspace --all-targets
  -- -D warnings` is the lint gate.
- Public items get a one-line doc comment minimum. Don't write
  multi-paragraph docstrings for internal helpers — the code is the
  contract.
- Commit subjects follow `type(scope): description` —
  `feat(ppu): ...`, `fix(core): ...`, `chore(ci): ...`. Backslash-
  escape `$` in commit subjects (e.g. `\$4212`).

## Reference docs (in `docs/`)

- `CONTROLLER_BINDINGS.md` — keyboard → SNES button mapping (GUI).
- `ppu_compositor_reference.md` — synthesised ares + Mesen2 spec for
  the PPU compositor, color math, windows, DMA, NMI.
- `ares_ppu_notes.md`, `mesen2_ppu_notes.md` — raw per-source research
  notes the spec was built from.
- `accuracy_scorecard.md` + the per-subsystem `luna_*_gaps.md` — current
  correctness grades and open-work lists (the source of truth for gaps).

## What NOT to put here

- Long workflows — use `.claude/commands/` or `.claude/skills/`.
- Reference tables (key bindings, register maps, build matrices) —
  use `docs/`.
- Per-subsystem deep-dives — use `docs/` and link from the
  matching rule file.
- Anything Claude can derive from `git log`, `Cargo.toml`, or a
  one-shot read of the source.
