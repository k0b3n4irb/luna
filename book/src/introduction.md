# Luna

**A cycle-accurate SNES emulator, written in Rust — built so an AI agent can
play, develop and debug Super Nintendo games on its own.**

---

Most emulators bolt AI on afterwards, by reading screenshots. Luna makes the
**agent ↔ machine dialogue first-class**: the whole machine state — CPU and PPU
registers, VRAM, OAM, palette, sprites, memory — is exposed as structured,
serializable snapshots, and a built-in **MCP server** lets an agent drive the
console over JSON-RPC to *play*, *build homebrew*, or *debug ROM hacks*.

And it does not trade away fidelity to get there. Both CPU cores pass their
exhaustive per-instruction test suites 100%, and every subsystem is a **faithful
reconstruction of the real hardware** — verified, where it matters, by a headless
differential against a reference emulator.

## Choose your path

Four kinds of reader open this guide. Take the one that is you; each
path is three pages long and says what you have at the end of it.

### A. I want to play

1. [Install & first run](using/install.md): download the zip for your
   platform, start `luna-gui "game.sfc"`.
2. [Controls](using/controls.md): the keyboard layout, gamepads, the
   remap dialog.
3. [Saves & save states](using/saves.md): what is kept, and where.

Before you start, [Compatibility](using/compatibility.md) says which
cartridges run and which are refused at load. At the end: a game
running, your saves on disk in a format other emulators read.

### B. I write homebrew and want it tested

1. [Install & first run](using/install.md): the headless `luna` binary
   needs no display, so it runs on a CI machine as it does on yours.
2. [Developing homebrew with luna](using/homebrew-ci.md): one TOML
   manifest per test (ROM, inputs, how long to run, what must be true),
   and a GitHub Actions job to copy.
3. [`luna profile`](using/cli-api-mcp.md#luna-profile--real-master-cycles-per-symbol):
   what each function costs in master cycles, and a gate that fails the
   build when the NMI handler overruns.

At the end: `luna test tests/` returns 0, 1 or 2, and your pipeline
knows whether the game still boots, draws and sounds as it did.

### C. An agent drives the console

1. [`luna mcp`](using/cli-api-mcp.md#luna-mcp--mcp-server-over-stdio):
   start the server with the ROM already loaded.
2. [The MCP tool catalogue](using/cli-api-mcp.md#4-mcp-tool-catalogue-luna-mcp):
   every tool, and the `luna-api` method behind it.
3. [The state JSON](using/cli-api-mcp.md#2-the-state-json-emulatorstate):
   what one `state` call returns, block by block.

At the end: the agent loads a ROM, presses buttons, reads registers and
memory, sets breakpoints and takes screenshots, with no screen scraping.

### D. I want to understand luna, or change it

1. [Architecture overview](internals/architecture.md): the twelve
   crates, the scheduler, and one page per subsystem after it.
2. [Why "faithful port"](method/faithful-port.md) and
   [The differential harness](method/differential.md): how a divergence
   from the hardware is found and fixed here.
3. [Accuracy scorecard](method/accuracy.md): where luna stands, and
   what is still open.

Then the [API reference (rustdoc) ↗](api/index.html) for all twelve
crates, and
[CONTRIBUTING.md](https://github.com/k0b3n4irb/luna/blob/main/CONTRIBUTING.md)
for the checks a change must pass.

### You know what you want to do, not where it is

[I want to…](task-index.md) lists the tasks (a screenshot at frame 200,
who wrote this register, what this function costs) with the command, the
MCP tool and the section that shows each one running.

## At a glance

| | |
|---|---|
| **Language** | Rust (2024 edition) |
| **Cores** | 65C816 + SPC700 (per-instruction suites 100%), S-DSP audio |
| **Coprocessors** | SA-1, Super FX (GSU), DSP-1, S-DD1 |
| **Front-ends** | GUI debugger (winit + wgpu), headless CLI, MCP server |
| **Platform** | Linux (tested) · Windows x86_64 + macOS arm64 (release binaries provided, unsigned) |
| **License** | [MPL-2.0](https://github.com/k0b3n4irb/luna/blob/main/LICENSE) |

> Luna runs three ways: the **GUI debugger** (`luna-gui`, a human plays), the
> **headless CLI** (`luna run` / `state` / `test`, for scripts and CI), or the
> **MCP server** (`luna mcp`, an agent drives) — all through the *same*
> observation and control surface.
