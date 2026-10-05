# luna CLI / API reference

The complete, human-readable reference for driving luna headless: the
`luna` command-line binary, the `luna-api` Rust surface every front-end
shares, and the MCP tool catalogue.

> **Source of truth.** The CLI is self-documenting via `clap`: run
> `luna <command> --help` for the canonical, always-current flag list.
> The Rust API is documented inline — `cargo doc -p luna-api --open`
> browses every method. This file is the curated overview; if it ever
> disagrees with `--help` / rustdoc, those win.

luna is **API-first**: the CLI, the MCP server, and the GUI are all thin
consumers of the one `luna_api::Emulator` contract. What `luna state`
measures is exactly what the GUI shows — coherence by construction.


## The `luna` CLI

```
luna <COMMAND>

Commands:
  run         Load a ROM, step N instructions, optionally dump a screenshot.
  state       Run through luna-api and emit a JSON state snapshot (+ dumps/traces).
  frames      Capture EXACTLY-consecutive PPU frames as PNGs (temporal artefacts).
  diff        Compare two ROMs at equal PPU frame (or their sound): MATCH / DIFF.
  profile     Real master cycles per symbol, stack depth, coprocessor cost.
  wram-trace  Per-frame vblank-aligned WRAM page hashes (cross-emulator differential).
  bench       Run a whole ROM directory headless and write a compatibility report.
  spc-dump    Export the live APU state as a playable .spc sound file.
  assets-dump Dump the loaded graphics (VRAM tiles, tilemaps, palette, sprites) as PNGs.
  test        Run manifest-driven homebrew tests (see "Developing homebrew with luna").
  mcp         Serve the luna MCP server on stdio.

Global options:
  -h, --help     Print help
  -V, --version  Print version
```

Build it with `cargo build --release -p luna-cli`; the binary is
`./target/release/luna`.

## Exit codes (the CI contract)

| Code | Meaning |
|---|---|
| `0` | Run completed; every `--assert*` spec passed. |
| `1` | Runtime failure (ROM load, I/O, trace enable), at least one `--assert*` spec failed (each failing spec prints a `FAIL …` line on stdout), **or the emulator core panicked** during the run. In that last case `luna run`, `luna state` and `luna profile` still write the state, screenshot and traces of where it stopped (`step warning: emulator panicked: …` or `Stopped on CPU panic:` names the cause), then exit `1`. |
| `2` | Usage error — a malformed `--input` / `--mouse` / `--superscope` script (any subcommand). Fix the invocation, not the ROM. |

A test harness should treat `1` as "the ROM regressed" and `2` as "the
harness itself is broken".

## The pages

| Page | What it holds |
|---|---|
| [`luna run` and `luna state`](cli-run-state.md) | render a frame, dump the audio, snapshot the machine, peek and assert on memory, every trace |
| [Compare, measure, export](cli-analysis.md) | `luna frames`, `luna diff`, `luna profile`, `luna wram-trace`, `luna bench`, `luna spc-dump`, `luna assets-dump`, `luna test` |
| [The state JSON](state-json.md) | what `luna state --out` and the MCP `state` tool return, block by block |
| [Scripted joypad input](input-scripts.md) | the `--input` grammar, the Mouse, the Super Scope, the Super Multitap |
| [The MCP server](mcp.md) | `luna mcp` and the catalogue of its tools |
| [The `luna-api` Rust surface](rust-api.md) | the `Emulator` type every front-end drives |

Looking for a task and not a subcommand? [I want to…](../task-index.md)
lists them.
