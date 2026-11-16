# CLAUDE.md

This file provides guidance to Claude Code when working in the luna
repository. Keep it lean — workflow detail belongs in `.claude/rules/`
(auto-loaded) and `.claude/commands/` (slash commands). Reference data
belongs in `docs/`.

## What luna is

A cycle-accurate-ish SNES emulator in Rust, a 12-crate workspace (`crates/`,
see `Cargo.toml`): `luna-bus`, `luna-cartridge`, the CPU cores (`luna-cpu-65c816`,
`-spc700`, `-upd96050`), `luna-apu`, `luna-ppu`, `luna-core` (system glue, scheduler,
DMA + coprocessors), `luna-api` (introspection surface, the only thing front-ends
drive), `luna-mcp-server`, `luna-cli` (the `luna` binary, `luna --help`) and
`luna-gui` (winit + pixels + egui-wgpu; emu thread, cpal audio).

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
