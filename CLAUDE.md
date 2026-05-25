# CLAUDE.md

This file provides guidance to Claude Code when working in the luna
repository. Keep it lean — workflow detail belongs in `.claude/rules/`
(auto-loaded) and `.claude/commands/` (slash commands). Reference data
belongs in `docs/`.

## What luna is

A cycle-accurate-ish SNES emulator written in Rust. Workspace layout:

- `crates/luna-core/` — system glue, CPU bus, scheduler.
- `crates/luna-cpu-65c816/`, `crates/luna-cpu-spc700/` — CPU cores.
- `crates/luna-ppu/` — PPU + renderer + compositor.
- `crates/luna-dma/`, `crates/luna-bus/`, `crates/luna-coproc/` —
  data movement + memory map + SA-1 / SuperFX / DSP-N copro shims.
- `crates/luna-cartridge/`, `crates/luna-apu/` — ROM parser + APU.
- `crates/luna-api/` — the introspection / driving surface used by the
  CLI, GUI, and MCP server.
- `crates/luna-cli/` — `luna run`, `luna state`, `luna mcp`.
- `crates/luna-gui/` — eframe-based debugger UI.
- `crates/luna-mcp-server/`, `crates/luna-mcp-client/` — MCP transport.
- `crates/luna-async/`, `crates/luna-overlay/` — runtime helpers.

## Mandates (auto-loaded from `.claude/rules/`)

| Rule | Source | When it applies |
|---|---|---|
| Reference-first implementation | `.claude/rules/reference-first.md` | Any SNES subsystem feature change |
| Rebuild + lint discipline | `.claude/rules/rebuild-discipline.md` | Every code change before commit |
| Coprocessor / DMA / PPU test sweep | `.claude/rules/coproc-testing.md` | Edits to luna-ppu, luna-dma, luna-bus/sa1.rs, luna-coproc/sa1.rs |
| Test audible / visible fixes before commit | `.claude/rules/audible-fixes-test-first.md` | Any change to APU / PPU rendering / GUI audio or framebuffer |

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
- `luna_ppu_gaps.md` — prioritised correctness gap list against the
  spec.
- `luna_ppu_inventory.md`, `ares_ppu_notes.md`, `mesen2_ppu_notes.md`
  — per-source research notes the spec was built from.

## What NOT to put here

- Long workflows — use `.claude/commands/` or `.claude/skills/`.
- Reference tables (key bindings, register maps, build matrices) —
  use `docs/`.
- Per-subsystem deep-dives — use `docs/` and link from the
  matching rule file.
- Anything Claude can derive from `git log`, `Cargo.toml`, or a
  one-shot read of the source.
