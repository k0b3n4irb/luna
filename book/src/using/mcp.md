# The MCP server

`luna mcp` serves the emulator to an agent over stdio. Each tool is a
thin wrapper over a `luna_api::Emulator` method, the same one the CLI
and the GUI call.

## `luna mcp` — MCP server over stdio

```
luna mcp [--rom <PATH> [--sym <PATH>] [--force-mapper <KIND>] [--force-region <ntsc|pal>]]
```

Serves the tool catalogue in [the tool catalogue](#mcp-tool-catalogue) to any connected MCP client (Claude
Desktop, Claude Code, custom). Stays alive until the client closes the
stream.

| Option | Default | Purpose |
|---|---|---|
| `--rom <PATH>` | none | Preload a ROM so the session starts ready — the client's first `state`/`step` works with no `load_rom` call (and no host-path hunting). A `<rom>.sym` beside it auto-loads, wlalink-style. |
| `--sym <PATH>` | beside-ROM auto-detect | Explicit WLA-DX `.sym` (overrides the auto-detection). |
| `--force-mapper <KIND>` | header auto-detect | Same vocabulary as `luna state` — for headerless/checksum-invalid homebrew. |
| `--force-region <ntsc\|pal>` | header country byte | Force the video standard for the preloaded ROM. |

```bash
# A Claude Code MCP entry that opens the work-in-progress ROM directly:
luna mcp --rom game.sfc --force-mapper lorom
# → the client's first `state` already reports rom.title, no load_rom step
```

The handshake now identifies the server as `luna` with luna's real
version (previously it reported the rmcp library's), and carries server
instructions describing the load → run → observe → trace workflow — an
MCP client sees how to drive the emulator before listing a single tool.


### Which luna a project's MCP runs

An MCP client starts the server itself: the `command` of the project's
`.mcp.json` (or `claude mcp add`) is the binary that runs, nothing else —
no luna registers itself anywhere. A bare `luna` is looked up in `PATH`.
A session keeps the process it started: a newer binary is only picked up
when the client reconnects the server.

```json
{
  "mcpServers": {
    "luna": {
      "command": "luna",
      "args": ["mcp", "--rom", "build/game.sfc"]
    }
  }
}
```

Name the binary for what the project needs: a release's for a fixed,
reproducible version (OpenSNES pins one for its CI); a build of `develop`
to follow luna as it moves. Every build says what it was made from, in
`luna --version` and in the version the server announces to its client
(`serverInfo`): the workspace version, the commit, and `-dirty` when its
code differed from that commit:

```text
$ luna --version
luna 1.39.0+4360f7c-dirty
```

A build without git (from a source archive) reports the plain version.

## MCP tool catalogue

Each tool is a thin wrapper over the matching `luna_api::Emulator`
method, so the MCP transport adds reach, not capability.

| Tool | Maps to | Purpose |
|---|---|---|
| `load_rom` | `load_rom` / `load_rom_forced` | Load a `.sfc`/`.smc` from a host path. Optional `force_mapper` (`lorom`, `hirom`, `exhirom`, `sa1`, `superfx`, `dsp1`, `sdd1`; `spc7110` is recognised but not emulated, so forcing it fails with an unsupported-mapper error) and `force_region` (`ntsc`, `pal`) bypass header auto-detection — same vocabulary as the CLI `--force-mapper` / `--force-region`. `power_on` (`zero` default, `ones`, `random`, `random=<seed>`) is the CLI `--power-on` (cartridge RAM without a battery included); a random load returns the seed as `power_on_seed`. A WLA-DX `<rom>.sym` next to the ROM is loaded automatically (count in `rom.symbols_loaded`). |
| `load_rom_bytes` | `load_rom_bytes` / `load_rom_bytes_forced` | Load a ROM from base64 bytes (e.g. a freshly assembled image, no host file). Same force and `power_on` params. Unlike `load_rom` it does **not** search the firmware folder (nor for a `.sym`) — check `missing_firmware` in the result. |
| `set_port_device` | `set_port_device` | Plug a device into port 0 or 1, then feed it with the matching `set_*` tool. Same names as the CLI `--port1`: `pad` (or `joypad`), `mouse`, `superscope`, `multitap`, and `none` to unplug the port. |
| `reset` | `reset` | Reset to power-on state. |
| `set_joypad` | `set_joypad` | Set the button bitmask for `port` (0 = P1, 1 = P2; 2-4 = a multitap's pads B-D, players 3-5 with the tap on port 2). |
| `set_mouse` | `set_mouse` | Feed SNES Mouse `dx`/`dy`/buttons for the next auto-read. |
| `set_superscope` | `set_superscope` | Feed Super Scope aim (`x`, `y`) + buttons. |
| `step` | `step` | Step `count` instructions (stops early if the CPU halts). |
| `step_until_frame` | `step_until_frame` | Run until one PPU frame completes (bounded). |
| `run_until_pc` | `run_until_pc` | Step until PB:PC hits a 24-bit target (bounded). |
| `run_until_mem_write` | `run_until_mem_write` | Step until an address is written; returns PC + value. |
| `run_until_mem_read` | `run_until_mem_read` | Step until an address is read; returns PC + value. |
| `run_until_gsu_go` / `run_until_gsu_stop` | `run_until_gsu` | Step until the Super FX starts / finishes a job, or `max_steps` instructions elapse; returns `{hit}`. The **transition** is watched, not the level: called while a job runs, `run_until_gsu_go` waits for the next one. An error on a cartridge with no Super FX. See the example below. |
| `state` | `state` | Full observable-state JSON snapshot ([the state JSON](state-json.md)). |
| `screenshot` | `render_frame_png` / `render_frame_png_native` / `render_frame_bg_png` | Render the composited 256×224 frame to PNG (256×239 under overscan — `height` in the result says which); `native: true` captures 512×448 (or 512×478; enable `set_native_capture` first), `bg: 1..=4` renders one layer in isolation, `force_display: true` renders through forced blank at full brightness. |
| `sram_get` / `sram_set` | `sram` / `load_sram` | Battery-RAM image as base64 — the MCP form of `--srm-out` / `--srm-in`. |
| `export_spc` | `export_spc` | Standard `.spc` (v0.30) music snapshot, base64 — playable in any SPC player. |
| `decode_sprites` | `decode_sprites` | All 128 OAM entries as a structured list — the queryable `render_sprite_sheet`. |
| `drain_audio` | `drain_audio` | Drain up to `max` stereo samples from the APU. |
| `peek_memory` | `peek_memory` | Read `count` bytes from the CPU bus at `bank:offset`. |
| `peek_coproc_ram` | `coproc_ram` | `count` bytes from `offset` of the coprocessor work RAM (Super FX Game Pak RAM, SA-1 BW-RAM), ungated by the CPU mapping — the CLI `--dump-coproc-ram`. Empty on a cart without one. |
| `dsp_registers` | `dsp_registers` | The 128 S-DSP registers (`$00-$7F`) — what `[asserts.dsp]` in a `luna test` manifest reads. |
| `peek_aram` | `peek_aram` | Read `count` bytes from the SPC700's 64 KB ARAM (`count` up to `0x10000` — a full dump needs no paging). |
| `peek_vram` | `peek_vram` | Read `count` bytes from the 64 KB VRAM (same one-call full-dump range). |
| `peek_cgram` | `peek_cgram` | All 256 CGRAM palette entries as BGR555 words. |
| `poke_memory` | `poke_memory` | Write bytes into WRAM (state injection). |
| `memory_kinds` | `memory_kinds` | The memories this cartridge has, by Mesen2's name, with their size: `SnesMemory` (the CPU bus), `SpcMemory`, `SnesPrgRom`, `SnesWorkRam`, `SnesSaveRam`, `SnesVideoRam`, `SnesSpriteRam`, `SnesCgRam`, `SnesRegister` (the I/O registers by their 16-bit address, read without side effects), `SpcRam`, `SpcRom`, `SpcDspRegisters`, `DspProgramRom` / `DspDataRom` / `DspDataRam`, `Sa1InternalRam`, `GsuWorkRam`, `Sa1Memory` / `GsuMemory` (the SA-1's, the Super FX GSU's bus, read only). |
| `read_memory` / `write_memory` | `read_memory` / `write_memory` | `count` bytes of any of them from `offset` (or `symbol`), and back, without side effects: a `SnesMemory` address goes to the memory behind it, never through the bus, so a read starts no S-DD1 decompression and drains no DSP-1 port (a register reads 0, open bus `$FF`). Writes reach ROM too. See the example below. |
| `eval` | `evaluate` | A debugger expression in Mesen2's language, evaluated as a watch: `{"value": 1, "kind": "boolean"}`. See [Expressions](#expressions) below. |
| `absolute_address` | `absolute_address` | The memory and offset behind a CPU-bus address or label — `{"kind": "SnesPrgRom", "offset": 16}` for `$80:8010` on a 32 KB `LoROM` — as the mapper's banking selects it now; `{"kind": "SnesRegister", "offset": 8448}` for `$00:2100` and its mirrors; `null` for open bus or a coprocessor's register window. |
| `poke_vram` / `poke_cgram` / `poke_oam` / `poke_aram` | same names | Direct writes into the other memory spaces (bus-bypassing state injection; each wraps at its size). |
| `freeze_add` / `freeze_remove` / `freeze_list` | same names | Cheat-style per-frame pinning: the byte is re-applied at every frame boundary in **every** run path (CLI, MCP and GUI behave identically), and once immediately on add. WRAM only. |
| `enable_call_stack` / `call_stack` | `enable_call_stack` / `call_stack` | Opt-in JSR/JSL/RTS/RTL + interrupt tracking → `[{pc, from, kind, symbol}]`, oldest first. The CLI form is `luna state --call-stack` (the `--out` JSON gains a `call_stack` array). |
| `search_memory` | `search_memory` | Find a byte pattern in `$7E-$7F` WRAM (hits report canonical `$7E`/`$7F` addresses). |
| `search_begin` / `search_refine` / `search_results` | `search_begin` / `search_refine` / `search_results` | The classic narrowing "find my variable" loop: begin (`u8`/`u16`), then alternate gameplay with refines (`eq`/`ne`/`lt`/`gt` a value, or `changed`/`unchanged` vs the last snapshot) until few candidates remain. `search_results` returns up to `limit` rows (default 64). |
| `set_cpu_register` | `set_cpu_register` | Set a CPU register by name. |
| `disasm_cpu` | `disassemble_cpu` | 65C816 disassembly (defaults: live PC + live M/X widths). |
| `disasm_spc` | `disassemble_spc` | SPC700 disassembly (default: live SPC PC). |
| `save_state` | `save_state` | Full-machine save-state blob, base64 (versioned, ROM-hash-guarded). |
| `load_state` | `load_state` | Restore a `save_state` blob. |
| `render_tilemap` | `render_tilemap_png` | Full tilemap of BG 1..=4 as PNG. |
| `render_vram_tiles` | `render_vram_tiles_png` | VRAM tile set decoded at 2/4/8 bpp as PNG. |
| `render_palette` | `render_palette_png` | CGRAM as a 16×16 swatch-grid PNG. |
| `render_sprite_sheet` | `render_sprite_sheet_png` | All 128 OAM sprites as a transparent PNG sheet. |
| `enable_cpu_trace` / `take_cpu_trace` | `enable_cpu_trace` / `take_cpu_trace_log` | Per-instruction CPU trace ring (PC + registers). |
| `enable_profile` / `take_profile` | `enable_profile` / `take_profile` | Master cycles per symbol (folded, heaviest first). (Per-PC samples are the Rust API's `take_profile_raw`; there is no MCP tool for them.) |
| `enable_mem_trace` / `take_mem_trace` | `enable_mem_trace_filtered` / `take_mem_trace_log` | Per-bus-access trace with bank / offset-range / offset-list / writes-only filters; every event carries `origin` (`cpu`, `dma<n>`, `hdma<n>`). |
| `bp_add` | `bp_add_exec` / `bp_add_mem`, `bp_set_condition`, `bp_set_mark` | Register an exec breakpoint or a read/write watchpoint range. `mirror: false` makes a mem watch bank-exact (default follows WRAM/MMIO mirrors); `name` (defaulting to the `symbol` used) labels it in `bp_list`. `condition` is an [expression](#expressions) that must be non-zero for it to fire; `mark: true` puts a mark on the Event Viewer instead of halting. `cpu: "spc"` / `"sa1"` / `"gsu"` / `"dsp"` on an `exec` breakpoint stops that CPU at the start of an instruction (`addr` in ARAM, or an ARAM `symbol`, for the SPC700; `kind: "spc_exec"` is the short form). `memory` on a `mem` watch watches another memory: `"SnesVideoRam"` / `"SnesCgRam"` / `"SnesSpriteRam"` (through the PPU's ports), `"SpcMemory"` / `"SpcRam"` / `"SpcRom"` (the SPC700's data accesses), `"Sa1Memory"` / `"GsuMemory"` (that chip's bus), `"DspDataRam"` (the DSP-1's). A hit's `cpu` names the CPU that stopped. `kind: "forbid"` is a range where a CPU does not halt (see [Forbid ranges](#forbid-ranges)). See [Conditional breakpoints](#conditional-breakpoints). |
| `bp_set_enabled` | `bp_set_enabled` | Disable/re-enable without removing — id, name and hit count survive. |
| `bp_remove` / `bp_clear_all` / `bp_list` | `bp_remove` / `bp_clear` / `bp_list` | Manage the registry. `bp_list` rows now carry `enabled`, `hit_count` (mem: at most one per instruction), `mirror` and `name`. |
| `run_until_break` | `run_until_break` | Run at full speed until a breakpoint or a break source fires (or a step budget). |
| `step_kind` | `step_kind` | One debugger step, as Mesen2's: `into`, `over` (a `JSR` / `JSL` whole), `out` (until the routine returns), `scanline`, `frame`, or `count` `instructions` / `scanlines` / `frames`. A breakpoint on the way stops it first; otherwise `kind` is `step`. No CPU-cycle step: luna runs instructions whole. |
| `set_break_sources` | `set_break_sources` | What else stops `run_until_break` / `run`, as Mesen2's "break on" options: `brk`, `cop`, `wdm`, `stp` (before that opcode runs), `nmi` / `irq` (on the handler's first instruction — Mesen2's "run to NMI / IRQ"), `scanline: N`. The result's `kind` names the source and `bp_id` is absent. |
| `run` / `pause` | `run_until_break_interruptible` | Unbounded interruptible run: `run` goes until a breakpoint / `STOP` / `pause`; `pause` stops it (returns `interrupted: true`). No mandatory step budget. `pause` also ends every other run tool early — `step`, `step_until_frame`, `run_until_pc`, `run_until_break`, `run_until_mem_read` / `_write` — so a huge `max_steps` can never wedge the session. |
| `peek_oam` | `peek_oam` | All 544 OAM bytes (512 low table + 32 high table). |
| `capabilities` | — | luna `version` + the live tool catalogue, for client feature-detection (the handshake `serverInfo` also reports luna's name and version). |
| `start_input_capture` / `take_input_capture` | `start_input_capture` / `take_input_capture` | Record joypad changes and export a `frame:mask` script (replay with `--input @file`). |
| `load_symbols` | `load_symbols` | Load a WLA-DX `.sym`; disasm + traces become annotated. |
| `load_symbols_str` | `load_symbols_str` | Load `.sym` text directly (no host file — e.g. an in-memory build's output). Replaces the table. |
| `clear_symbols` | `clear_symbols` | Drop the loaded table. |
| `label_set` / `label_remove` | `set_label` / `remove_label` | Add, replace or remove a debugger label: a name and/or comment on bytes of a memory. See [Labels and comments](#labels-and-comments). |
| `label_list` | `labels` | The labels, by memory then address; `user_only` leaves out the register defaults. |
| `labels_import` / `labels_export` | `import_labels_mlb` / `export_labels_mlb` | Read or write a Mesen2 `.mlb` label file. |
| `enable_code_log` | `enable_code_log` | Log which bytes the 65C816 and the SPC700 run as code (what `listing` knows code by). |
| `listing` | `listing` / `listing_back` / `listing_pos_of` | The debugger's code listing, as Mesen2's Disassembly pane draws it. See [The code listing](#the-code-listing). |
| `watch` | `watch_value` | Watch entries in Mesen2's formats (`, H2`, `, S`, `[addr, n]`). See [The debugger's panes](#the-debuggers-panes). |
| `goto` / `goto_all` | `goto_address` / `goto_all` | Mesen2's Go To (a label, bare hex, an expression) and Go To All (labels and `.sym` names by terms). |
| `find` / `find_all` | `listing_find` / `listing_find_all` | Find in the listing from a row; Find All Occurrences (up to 500 rows). |
| `functions` | `functions` | Every subroutine entry the code log saw in the program ROM. |
| `debugger_status` | `debugger_status` | The Debugger window's Status pane: registers, pending interrupts, PPU position, the stack. |
| `workspace_save` / `workspace_load` | `workspace` / `apply_workspace` | The ROM's debugger workspace (labels set, breakpoints), the file the GUI's Debugger window uses. See [The debugger's panes](#the-debuggers-panes). |
| `resolve_symbol` | `resolve_symbol` | Label name → 24-bit address (null if unknown). A bare name stands for the only label spelled `name.<suffix>` (a C `static`: `player_x` for `player_x.main`); several candidates are an error that names them. The same rule applies to every tool's `symbol` argument. |
| `symbol_for_addr` | `symbol_for_addr` | 24-bit address → nearest preceding label in its bank (the inverse). |
| `enable_dma_trace` / `take_dma_trace` | `enable_dma_trace` / `take_dma_trace` | DMA→VRAM transfer bytes with scanline/H-clock + blank flags (the CLI `--dma-trace`). |
| `enable_dsp_trace` / `take_dsp_trace` | `enable_dsp_trace` / `take_dsp_trace` | S-DSP register writes from the SPC700 side (the CLI `--dsp-trace`). |
| `enable_mailbox_log` / `take_mailbox_log` | `enable_mailbox_log` / `take_mailbox_log` | CPU↔APU `$2140-43` traffic with the accessing PC, symbolised (the CLI `--apu-log`). |
| `enable_sa1_log` / `take_sa1_log` | `enable_sa1_log` / `take_sa1_log` | Main-CPU accesses to SA-1 MMIO, symbolised (the CLI `--sa1-log`). |
| `enable_sa1_side_log` / `take_sa1_side_log` | `enable_sa1_side_log` / `take_sa1_side_log` | SA-1-side MMIO accesses (the CLI `--sa1-side-log`). |
| `enable_sa1_trace` / `take_sa1_trace` | `enable_sa1_trace` / `take_sa1_trace` | Per-instruction SA-1 register trace (the CLI `--sa1-trace`). |
| `enable_superfx_trace` / `take_superfx_trace` | `enable_superfx_trace` / `take_superfx_trace` | Per-opcode GSU trace incl. GO/STOP edges (the CLI `--superfx-trace`). |
| `enable_dsp1_trace` / `take_dsp1_trace` | `enable_dsp1_trace` / `take_dsp1_trace` | DSP-1 microcode + DR/SR port stream; `ports_only: true` on enable keeps only the DR/SR traffic (the CLI `--dsp1-trace-ports`); `take` optionally decodes command transactions (the CLI `--dsp1-trace` / `--dsp1-trace-commands`). |
| `enable_spc_trace` / `take_spc_trace` | `enable_spc_trace` / `take_spc_trace` | Per-instruction SPC700 trace with timer-2 state (the CLI `--spc-trace`). |
| `frame_hash` | `frame_hash` / `frame_hash_native` | 64-bit pixel hash of the current frame as 16 hex chars — the CLI's `fbhash=` value. `force_display: true` hashes the frame as rendered through forced blank; `native: true` hashes the 512×448 capture (enable it first; native and non-native values are not comparable). |
| `set_native_capture` | `set_native_capture` | Toggle native 512×448 capture for `screenshot`/`frame_hash` `native` modes. |
| `wram_page_hashes` | `wram_page_hashes` | Stable FNV-1a-64 per WRAM page (default 4 KiB → 32 hashes). Diff two calls to localise a WRAM change. |
| `wram_snapshot` | `wram_snapshot` | Full-WRAM FNV-1a-64 hash (+ the raw 128 KiB base64 with `include_data`). |
| `loop_probe` | `loop_probe` | Hang diagnostic: run `max_steps` and count distinct PCs (a handful ⇒ tight spin loop). |
| `enable_nocash_log` / `take_nocash_log` | `enable_nocash_log` / `take_nocash_log` | The `$21FC` Nocash TTY (`SNES_NOCASH` text): drain returns `{text, base64}`. |
| `enable_wdm_log` / `take_wdm_log` | `enable_wdm_log` / `take_wdm_log` | The `WDM` assert channel (`SNES_ASSERT` → `WDM $00`): drain returns `[{pc, operand, symbol}]`. |

The `enable_*_trace` tools that record a stream (CPU, memory, DMA, DSP,
DSP-1, SA-1, Super FX, SPC700) take `max_events`, the cap on what one
capture keeps — the MCP form of the CLI's `--…-trace-max` flags.

With a symbol table loaded, the address-taking tools (`peek_memory`,
`poke_memory`, `run_until_pc`, `run_until_mem_*`, `bp_add`, `disasm_cpu`,
`enable_mem_trace`, `freeze_add`, `freeze_remove`) also accept a `symbol` name in place of the numeric
address — e.g. `peek_memory {symbol: "monster_x", count: 2}`, or a
symbol-bounded watch range `bp_add {kind: "mem", symbol: "buf_start",
hi_symbol: "buf_end"}`.

The symbol table carries **two address spaces**: the
24-bit CPU bus and the SPC700's 16-bit ARAM. `load_symbols` /
`load_symbols_str` take `space: "aram"` for a wla-spc700 driver's `.sym`
(loading one space never clobbers the other), `resolve_symbol` /
`symbol_for_addr` take the same `space` argument, and the ARAM tools
(`disasm_spc`, `peek_aram`) accept `symbol` names resolved in the ARAM
space — `disasm_spc` output is annotated from it. WLA-DX
`[definitions]` constants also resolve by name (they never annotate
addresses — a constant is not a location).

### Reading the SDK assert/log channels over MCP

An agent debugging an SDK-built ROM watches the two debug channels the
same way the CLI's `--nocash-out` / `--wdm-out` flags do — enable, run,
drain:

```text
enable_nocash_log {}   # $21FC TTY — SNES_NOCASH("...") text output
enable_wdm_log {}      # WDM (opcode $42) — SNES_ASSERT is `WDM $00`, "fired here" events
run {}                 # or step / step_until_frame / run_until_break
pause {}
take_nocash_log {}     # → {text: "hello\n", base64: "aGVsbG8K"}
take_wdm_log {}        # → {events: [{pc: 32779, operand: 0, symbol: "assert_fail+0x02"}]}
```

An empty `take_wdm_log` after a run is the "no assertions fired" green
light a CI-style probe wants; the Nocash text is the ROM's own printf
channel. Draining resets each channel, so successive takes return only
new output.

### Stopping on a Super FX job

A Super FX renderer works in jobs: the game writes the GSU's program
counter, the chip runs until its `STOP`, the game copies the result out.
Two tools stop on those edges, so a job's inputs and its result can be
read without searching a 200 000-line opcode trace for them:

```text
load_rom {path: "Star Fox (USA) (Rev 2).sfc"}
run_until_gsu_go {max_steps: 20000000}     # → {hit: true}   a job has just started
state {}                                   # state.gsu.running = true, state.gsu.r = the job's arguments
run_until_gsu_stop {max_steps: 20000000}   # → {hit: true}   that job has just finished
peek_coproc_ram {offset: 0, count: 64}     # what it left in Game Pak RAM
```

`hit: false` means `max_steps` ran out first. On a cartridge without the
chip both tools return the error `this cartridge has no Super FX`.

### Every memory by one name

`memory_kinds`, `read_memory` and `write_memory` reach every memory of the
machine by the name Mesen2's tools and Lua API give it, so one call shape
inspects work RAM, the cartridge's RAMs, the PPU's and the APU's. A
CPU-bus read is a debugger's read: it goes to the memory behind the
address, so peeking an S-DD1 or DSP-1 cartridge's windows changes
nothing in the run.

```text
load_rom {path: "game.sfc"}
memory_kinds {}                                   # → [{kind: "SnesMemory", size: 16777216}, {kind: "SnesPrgRom", …}, …]
absolute_address {address: 0xC00000}              # HiROM → {kind: "SnesPrgRom", offset: 0}
read_memory {kind: "SnesWorkRam", offset: 0x100, count: 16}
write_memory {kind: "SnesPrgRom", offset: 0x1234, data: [0xEA, 0xEA]}   # patch two NOPs into ROM
read_memory {kind: "SnesMemory", offset: 0xC01234, count: 2}            # → [234, 234]
read_memory {kind: "Sa1InternalRam", offset: 0, count: 64}              # an SA-1 cart's I-RAM
```

A memory the cartridge lacks is an error (`GsuWorkRam: this cartridge has
none`), and so is a range past its end; the two buses (`SnesMemory`,
`SpcMemory`) wrap around instead.

### The debugger's panes

What the GUI's Debugger window shows beside the listing is a tool too,
so an agent reads the same values the window draws.

```text
label_set {memory: "SnesWorkRam", address: 0x10, length: 2, name: "counter"}
watch {entries: ["{counter}", "[counter], B", "1 / 0"]}
                       # → {values: ["$1234", "%0011.0100", "<division by zero>"]}
goto {text: "counter"}                       # → {address: 8257552}   $7E:0010
goto {text: "8000"}                          # → {address: 32768}     bare digits are hex
goto_all {query: "inidisp"}                  # → {results: [{name: "INIDISP", address: 8448, …}]}
find {text: "STA $2100"}                     # → {pos: {addr, row}} of the next such row from the PC
find_all {text: "counter", whole_word: true} # every instruction row naming it
functions {}                                 # needs enable_code_log
debugger_status {}                           # registers, IRQs, H clock, scanline, stack…
```

- **`watch`** formats as Mesen2's Watch pane: a suffix `, H`, `, S`,
  `, U`, `, B` and a byte count 1-4, `[addr, count]` for bytes; `style`
  (`hex`, `decimal`, `binary`) when an entry names no format, a `{word}`
  entry two bytes wide.
- **`goto`** reads a label first, then bare hex digits, then an
  expression; **`goto_all`** wants every term of the query (split at
  spaces and capitals), those in view listed first.
- **`find`** starts at `from` (default: the PC's row) and wraps round the
  space; **`find_all`** walks the whole listing, the ROM's mirrors
  included.
- **`workspace_save` / `workspace_load`** write and read the ROM's
  debugger workspace: the labels set and every breakpoint (condition,
  mark, state). The file is the one the GUI's Debugger window loads with
  the ROM (`<settings>/debugger/<rom name>.json`, `~/.config/luna/…` on
  Linux), so what an agent sets the window shows, and the other way
  round; a save keeps the watches and window settings the GUI wrote
  there. `path` names another file.

### Labels and comments

A debugger label names bytes of a **memory**, not a bus address, as in
Mesen2: a label on ROM offset `$0000` shows at `$00:8000`, `$80:8000` and
every other mirror, and a work-RAM label at `$00:0010` as at `$7E:0010`.
Labels come from three places:

- **The defaults.** The SNES I/O registers (`INIDISP` at `$2100`,
  `NMITIMEN`, the DMA registers `DMAP0` … `NTLR7`) and the SPC700's
  (`CPUIO0` at `$F4`, `T2OUT`…), each with a one-line comment. They are
  ordinary labels: a label of yours on the same byte or with the same name
  replaces one.
- **The files next to the ROM**, in Mesen2's order, each `<rom>.<ext>`
  else `<rom>.sfc.<ext>`: a ca65 / cc65 **`.dbg`** (it then stands for the
  others), a **`.sym`** (WLA-DX's into the symbol table, bass's into the
  labels), a Mesen2 **`.mlb`**. At each ROM load the labels go back to the
  defaults first; `rom.labels_loaded` in the `load_rom` result counts what
  the label file brought.
- **What you set or import** with the tools below.

```text
label_set {memory: "SnesWorkRam", address: 0x10, length: 2, name: "counter", comment: "frames since reset"}
resolve_symbol {name: "counter"}                       # → {addr: 8257552}   $7E:0010
labels_import {text: "SnesPrgRom:0000:start\nbroken\n", merge: true}
                                                       # → {imported: 1, errors: 1, skipped: 0}
labels_export {}                                       # → {text: "…SnesWorkRam:0010-0011:counter:frames since reset\n…", count: …}
label_list {user_only: true}
```

- **Names** are letters, digits, `_` and `@`, not starting with a digit,
  and keep their case. One name, one place; one byte, one label: a new
  label replaces the one with its name and the ones on its bytes, whole.
  Byte N of a multi-byte label reads `name+N`.
- **Where they show.** Every tool that takes a `symbol` accepts a label
  (after the `.sym`'s own names); `eval` and breakpoint conditions read it
  as its address on the CPU's bus; `disasm_cpu` / `disasm_spc` lines carry
  `label` and the `comment` of a label starting there.
- **Which CPU sees which.** The 65C816 and the SPC700 see every label of
  a memory behind their bus. The SA-1 and the Super FX see the labels
  given on their own bus (`Sa1Memory`, `GsuMemory`), the DSP-1 those on
  its program ROM; a label they cannot place is out of scope.
- **`.mlb` lines** read `Memory:ADDR[-END]:Name[:Comment]`, as Mesen2
  writes them: the memory by its name, the offset in hex with no prefix,
  both ends of a range included, `\n` for a line break in the comment.
  Mesen-S prefixes (`PRG`, `WORK`, `SAVE`, `REG`, `SPCRAM`…) are read too.
  A line that does not parse counts as an error and the rest goes on; a
  line for a memory the cartridge lacks is skipped. An export writes every
  label, the defaults included, sorted by memory then address, so Mesen2
  reads it back.
- `import` without `merge` puts the defaults back first, as Mesen2 does.
- **A ca65 `.dbg`** (`labels_import {path: "game.dbg"}`) is read as
  Mesen2 reads it for a SNES cartridge: a symbol in a segment with an
  output offset is a program-ROM label at `val - start + ooffs`; one in a
  segment without (BSS, zero page, SRAM) is a CPU address placed through
  the memory map, kept in work RAM, save RAM or ROM; an `rw` segment with
  an output offset is taken for SPC700 code. Labels (`type=lab`, or any
  symbol with a `size`) keep their name as it is — `@loop` stays `@loop`,
  a name met again becomes `name_2` — and their size (code labels are one
  byte). The comments come from the source files the `.dbg` names, found
  next to it or in a folder above: the text after `;` on the line and the
  comment lines just above it (`//` for C). The labels go back to the
  defaults first.
- **A bass `.sym`** (`AAAAAAAA name` lines) merges into the labels: the
  address is a CPU address placed through the memory map, registers
  included, and every character a label cannot hold becomes `_`
  (`main.loop` → `main_loop`).
- **SDCC `.cdb` and ELF files** are not read: Mesen2 has no SNES reader
  for them either (its ELF reader is for the GBA).

### The code listing

`listing` returns the rows Mesen2's Disassembly pane shows, the same
listing the GUI's debugger draws. It knows code from the **code log**:
turn `enable_code_log` on, run, then ask. An instruction is known once it
ran, with the accumulator and index widths it ran with; what follows a
first run is decoded ahead until a `PLP`, `XCE` or `RTI`, and those rows
say `predecoded`. Everything else, bank by bank, collapses into a block.

```text
enable_code_log {on: true}
step_until_frame {}                          # … run a while
listing {before: 20, rows: 40}               # around the 65C816's PC
listing {symbol: "NmiHandler", rows: 30}
listing {cpu: "spc", rows: 20}
listing {address: 0x7E8000, options: {unidentified: "show"}}
```

Rows from Super Mario World after 600 frames with the log on from power-on:

```text
058A57 Instruction LDA [$6B],Y   [$7ECBAA] = $2525
058A59 Instruction AND #$00FF
058A6E Instruction LDA [$0A],Y   [$0D8172] = $1838
108001 Instruction STZ $4200     [NMITIMEN] = $0000
7E0000 BlockStart  unidentified
7E0000 BlockBody
7E0000 BlockEnd
```

- **Each row** has its `pos` (`{addr, row}`: an address and the row of
  what starts there), its `kind` (`instruction`, `label`, `comment`,
  `function_start`, `function_end`, `block_start`, `block_body`,
  `block_end`), the `absolute` memory behind it, its `bytes`, its `text`,
  `effective` and `value`, a one-line `comment`, `code` (`executed`,
  `predecoded`, `data`, `unidentified`, `unmapped`), `is_pc`, and the
  `breakpoint` drawn on it.
- **`effective`** is the address the instruction would reach **if it
  ran now**, from the live registers (D, DB, X, Y, S, the widths), as
  Mesen2 computes it for every visible row: `[$7ECBAA]`, or the label
  there, `[NMITIMEN]`. `value` is what is there, 8 or 16 bits as the
  instruction would access it. A `JMP` / `JSR` through a 16-bit operand
  shows its target, with no value; a branch and a long operand show their
  full 24-bit target, or its label, in the text itself.
- **Labels and comments** shape the rows: a `name:` row before a
  labelled instruction, one `;` row per line of a multi-line comment, a
  one-line comment at the end of its row, a `function_start` separator
  (titled with the label, else `sub start`) before a subroutine entry and
  a `function_end` after a return.
- **`options`** are Mesen2's: `data` / `unidentified` (`hide`, one empty
  row, the default; `show`, `.db` rows of 8 bytes), `memory_values`
  (default on), `jump_labels` (a `$ADDR:` row before an unlabelled jump
  target), `lower_case`.
- **Where it stops**: the SA-1, the Super FX and the DSP-1 have no
  listing yet, and bytes read as data are not logged yet (no `data` block
  until the code/data log reads them).

### Expressions

`eval` evaluates a debugger expression in Mesen2's language, the one its
breakpoint conditions, watches and trace filters use, so a condition
written for one emulator means the same in the other. It is the language
luna's own conditions build on.

```text
eval {expression: "a"}                                  # → {value: 4660, kind: "numeric"}   the 16-bit accumulator
eval {expression: "[player_hp] < 10 && scanline > 200"} # a .sym label stands for its address
eval {expression: "{$7E0010} == $1234"}                 # {…} reads a little-endian word
eval {expression: ":$C08000"}                           # the ROM offset behind a HiROM address
eval {expression: "[$F2]", cpu: "spc"}                  # the SPC700's DSP register index
eval {expression: "a", cpu: "sa1"}                      # the SA-1's accumulator
eval {expression: "r15", cpu: "gsu"}                    # the Super FX's program counter
eval {expression: "dp", cpu: "dsp"}                     # the DSP-1's data pointer
eval {expression: "10 / x"}                             # → {value: 0, kind: "divide_by_zero"} when X is 0
```

- **Values.** Decimal, `$hex` / `0xhex` and `%binary` literals. The
  registers of the CPU: `a x y sp d db pc ps`, the `ps…` flags (`pscarry`,
  `psnegative`…), `nmi`, `irq`, `frame`, `scanline`, `hclock`, `cycle`;
  on the SPC700 `a x y sp ps pc dspreg`. The SA-1 has the 65C816's, its
  own registers and bus, the console's PPU position. The GSU has
  `r0`-`r15`, `srcreg`, `dstreg`, `sfr`, `pbr`, `rombr`, `rambr` (no `pc`:
  `r15`) and its bus. The DSP-1 has `a b tr trb rp dp dr sr k l m n sp pc`
  (`pc` a word index), and its memory operators read its program ROM.
- **Memory.** `[a]` reads a byte, `{a}` a word, `#a` a long, without side
  effects. `:a` gives the offset of the memory behind `a`.
- **Operators.** C's, with C's precedence: `x & 1 == 1` is `x & (1 == 1)`.
- **Arithmetic.** Signed 64-bit, with the result clamped to −2³¹ … 2³²−1.
  `&&` and `||` evaluate both sides.
- **Mesen2's quirks**, which luna keeps:
  - spaces are removed first, so `1 2` is 12;
  - right after `[`, an operator is binary: `[-1]` reads `[0-1]`, and
    `[~1]` is invalid (write `[(~1)]`).
- **Where luna departs from Mesen2:** a closing bracket must match the
  innermost open one, and a label may contain `.`, as WLA-DX labels do.

### Conditional breakpoints

A breakpoint can carry a condition in the [expression](#expressions)
language: it fires only when the condition is non-zero, the way Mesen2's
breakpoints do, so a condition carries over between the two emulators.

```text
bp_add {kind: "exec", symbol: "enemy_spawn", condition: "x == 5"}   # the sixth slot
bp_add {kind: "mem", symbol: "player_hp", condition: "value > [player_hp_max]"}
bp_add {kind: "mem", addr: 0x002100, condition: "value & $80"}      # forced blank switched on
bp_add {kind: "exec", symbol: "nmi_handler", mark: true}            # no halt: one dot per NMI on the Event Viewer
run_until_break {max_steps: 50000000}                              # → {hit: {id, kind, pc, addr, value}}
bp_list {}                                                          # condition, mark, hit_count of each
```

- **What the condition sees.**
  - On an exec breakpoint, `value` is the opcode and `address` the PC, and
    the registers are those before the instruction runs.
  - On a watchpoint, `value` and `address` are the byte and address of the
    access, and `iswrite` / `isdma` tell what it was.
  - Registers and memory are those of the end of the accessing instruction:
    luna runs an instruction whole, where Mesen2 checks in the middle of
    it. A condition on `[address]` therefore sees the byte already written.
- **Failures.**
  - A condition that cannot be computed (`1/0`, a label out of reach)
    never fires its breakpoint, as in Mesen2.
  - One that does not parse is refused when it is set.
- **Counting.** The hit count counts firings, marks included; a match whose
  condition is false does not count.

**VRAM, CGRAM, OAM.** With `memory`, a `mem` watchpoint watches a PPU
memory instead of the CPU bus. It fires when the CPU or a DMA reads or
writes there through the PPU's ports, at the address the PPU actually
uses: after VMAIN's remap, and on the entry the picture redirects a
mid-frame access to:

```text
bp_add {kind: "mem", memory: "SnesVideoRam", addr: 0x4000, hi: 0x41FF}   # who writes this tile?
run_until_break {max_steps: 50000000}  # → {kind: "write", space: "vram", addr: 16386, value: 190, dma: true, pc: …}
bp_add {kind: "mem", memory: "SnesCgRam", addr: 0, hi: 1, condition: "value != 0"}   # the backdrop goes non-black
bp_add {kind: "mem", memory: "SnesSpriteRam", addr: 0x200, hi: 0x21F, on_read: true, on_write: false}   # who reads the high table back?
```

- `addr` and `address` in a condition are the byte address in that
  memory. `pc` is the instruction that made the access, or that started
  the DMA.
- **A VRAM read** is the word the read buffer fetches: on a VMADD write,
  and on the `$2139` / `$213A` read VMAIN increments on. That read fetches
  the word at the address before it moves, as on the console; the CPU
  receives the buffer's previous contents.
- During the picture the PPU owns VRAM and OAM: a port write is dropped
  or redirected, and a VRAM read fetches nothing, so no read fires.

**The SPC700.** `cpu: "spc"` puts a breakpoint on an SPC700
instruction (`kind: "spc_exec"` says the same):

```text
bp_add {kind: "exec", cpu: "spc", addr: 0x0400, condition: "a == $10"}   # the sound driver's command loop
run_until_break {max_steps: 50000000}   # → {kind: "exec", cpu: "spc", pc: 1024, …}
disasm_spc {}                           # the SPC700 stands on $0400
```

- **Where each CPU stops.** The SPC700 stops at the start of that
  instruction, exactly as in Mesen2. The 65C816 finishes the instruction
  it was running.
- **The condition** is evaluated inside the APU at that instruction's
  start, for the SPC700: `a x y sp ps pc dspreg` and its 64 KB bus.
  A condition that does not hold leaves the run's timing untouched.
- **Outside a breakpoint run.** These breakpoints are armed only for
  `run_until_break` / `run`, so a plain `step` never stops the SPC700.
- **Marks** are for the 65C816 only.

**The Super FX.** `cpu: "gsu"` and `memory: "GsuMemory"` (ROM at
`$00-$5F`, Game Pak RAM at `$70-$71`) do the same for the GSU, which also
runs behind the S-CPU and freezes on a hit. An exec breakpoint's address
is `PBR:R15`, the opcode the GSU is about to run. A watchpoint sees the
GSU's data accesses (the ROM buffer, RAM loads and stores, the plot
cache), not its code fetches. A store lands through the RAM buffer a few
clocks after `STW`, so its hit's `pc` is the instruction after the store.

```text
bp_add {kind: "exec", cpu: "gsu", addr: 0x008104, condition: "r1 == $45"}
bp_add {kind: "mem", memory: "GsuMemory", addr: 0x700010, condition: "value == $48"}
run_until_break {max_steps: 10000}   # → {kind: "write", cpu: "gsu", pc: 33031, addr: 7340048, value: 72}
```

**The DSP-1.** `cpu: "dsp"` stops the NEC uPD7725 at the start of a
microcode instruction. Its address is a byte of `DspProgramRom`, the word
× 3, as in Mesen2. `memory: "DspDataRam"` watches the microcode's reads
and writes of its data RAM, 2 bytes per word, low first. The DSP runs
behind the S-CPU and freezes on a hit, like the others.

```text
bp_add {kind: "exec", cpu: "dsp", addr: 6, condition: "a == $1234"}   # word 2
bp_add {kind: "mem", memory: "DspDataRam", addr: 0x20, hi: 0x21}       # data RAM word $10
```

The SPC700's memories take watchpoints too: `SpcMemory` (its 16-bit bus,
the `$F0-$FF` registers included), `SpcRam` (its 64 KB, the registers
left out) and `SpcRom` (the IPL ROM, read only). They see the SPC700's
data accesses; its code fetches are not reads. The SPC700 stops just
after the access, in the middle of its instruction if need be, and `pc`
is that instruction's address. The IPL ROM clearing page zero, from `$EF`
down:

```text
bp_add {kind: "mem", memory: "SpcRam", addr: 0x80}
run_until_break {max_steps: 100000}  # → {kind: "write", cpu: "spc", pc: 65477, addr: 128, value: 0, space: "aram"}
eval {expression: "x", cpu: "spc"}   # → 128: `MOV (X),A` at $FFC5 has written, `DEC X` has not run
bp_add {kind: "mem", memory: "SpcMemory", addr: 0xF4, on_read: true, on_write: false}   # the SPC700 reads mailbox 0
```

`symbol` and `hi_symbol` are then ARAM labels. A condition sees the
access (`value`, `address`, `iswrite`), and the SPC700's registers at
that point of the instruction.

**The SA-1.** `cpu: "sa1"` puts a breakpoint on the SA-1's 65C816, and
`memory: "Sa1Memory"` a watchpoint on its bus (its I-RAM at `$0000-$07FF`
and `$3000-$37FF`, BW-RAM, ROM). The SA-1 runs behind the S-CPU and
catches up with it; a hit freezes it until the next run, the time it
stood still caught up then.

```text
bp_add {kind: "exec", cpu: "sa1", addr: 0x008105, condition: "a == $45"}
run_until_break {max_steps: 10000}   # → {kind: "exec", cpu: "sa1", pc: 33029, …}
bp_add {kind: "mem", memory: "Sa1Memory", addr: 0x3000}   # who in the SA-1 writes this I-RAM byte?
```

- **Where it stops.** An exec breakpoint at the start of the
  instruction. A watchpoint after the instruction that made the access,
  as on the 65C816; reads include its code fetches, as on the 65C816.
- **The condition** is evaluated inside the chip, for the SA-1: its
  registers and its bus. `scanline` and `hclock` are the position its
  H/V timer follows; `frame` reads 0 there.
- **Marks** are for the 65C816 only.

#### Forbid ranges

A *forbid* range is Mesen2's forbid breakpoint: a stretch of a CPU's code
where it does not halt. While that CPU runs an instruction that starts
inside the range, its breakpoints and watchpoints do not stop the run,
nor, on the 65C816, a `BRK` / `COP` / `WDM` / `STP` break source. It
keeps a noisy routine (a decompressor, a memory clear, an interrupt
handler) out of a watchpoint's way.

```text
bp_add {kind: "mem", addr: 0x7E0010, hi: 0x7E001F}                     # who writes these bytes?
bp_add {kind: "forbid", addr: 0x008008, hi: 0x00800B}                    # … except the routine at $8008
bp_add {kind: "forbid", memory: "SnesPrgRom", addr: 0x8, hi: 0xB}        # the same, by its ROM bytes: every mirror
bp_add {kind: "forbid", cpu: "spc", addr: 0xFFC0, hi: 0xFFFF}            # never stop the SPC700 in the IPL ROM
bp_add {kind: "forbid", addr: 0x008008, hi: 0x00800B, condition: "[$10] == 1"}
```

- **The test** is the address of the instruction running, the opcode's,
  on the CPU that would halt. Each CPU's ranges protect that CPU only. On
  the 65C816 `memory` can name the memory the address resolves to
  (`SnesPrgRom`, `SnesWorkRam`, `SnesSaveRam`) instead of the bus.
- **A condition** limits the range to when it holds; it sees the access
  the halting breakpoint saw.
- **What still stops:** a step (`step_kind`), the NMI / IRQ and scanline
  sources, `pause`. **Marks** still mark: only the halt is forbidden.
- A range cannot be a mark. `bp_list` shows it with `kind: "forbid"`; a
  disabled one does nothing.

Mesen2's "break on" options and its "run to NMI / IRQ" are break sources,
set once for every run:

```text
set_break_sources {brk: true, wdm: true}   # a crash into BRK, or an SDK assert's WDM
run_until_break {max_steps: 50000000}      # → {hit: true, kind: "wdm", pc: …}, before the WDM runs
set_break_sources {nmi: true}
run_until_break {max_steps: 1000000}       # → {kind: "nmi"}: PC is the handler's first instruction
set_break_sources {scanline: 225}          # the first VBlank line
set_break_sources {}                       # all off again
```

`step_kind` is the debugger's step buttons:

```text
step_kind {kind: "over"}                   # past the JSR under PC, back in this routine
step_kind {kind: "out"}                    # to the instruction after the call that got here
step_kind {kind: "frames", count: 60}      # one second of an NTSC game
```

### Loading homebrew straight from the assembler

A build loop that never touches the filesystem, including a
checksum-invalid work-in-progress image and a pointer device:

```text
load_rom_bytes {rom_base64: "<the .sfc bytes>", force_mapper: "lorom"}
set_port_device {port: 0, device: "mouse"}
set_mouse {dx: 5, dy: 0, buttons: 1}
step_until_frame {}
```

`force_mapper` / `force_region` accept exactly the CLI's
`--force-mapper` / `--force-region` values, so a recipe translates
between the two transports verbatim.
