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


## MCP tool catalogue

Each tool is a thin wrapper over the matching `luna_api::Emulator`
method, so the MCP transport adds reach, not capability.

| Tool | Maps to | Purpose |
|---|---|---|
| `load_rom` | `load_rom` / `load_rom_forced` | Load a `.sfc`/`.smc` from a host path. Optional `force_mapper` (`lorom`, `hirom`, `exhirom`, `sa1`, `superfx`, `dsp1`, `sdd1`; `spc7110` is recognised but not emulated, so forcing it fails with an unsupported-mapper error) and `force_region` (`ntsc`, `pal`) bypass header auto-detection — same vocabulary as the CLI `--force-mapper` / `--force-region`. `power_on` (`zero` default, `ones`, `random`, `random=<seed>`) is the CLI `--power-on`; a random load returns the seed as `power_on_seed`. A WLA-DX `<rom>.sym` next to the ROM is loaded automatically (count in `rom.symbols_loaded`). |
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
| `bp_add` | `bp_add_exec` / `bp_add_mem` | Register an exec breakpoint or a read/write watchpoint range. `mirror: false` makes a mem watch bank-exact (default follows WRAM/MMIO mirrors); `name` (defaulting to the `symbol` used) labels it in `bp_list`. |
| `bp_set_enabled` | `bp_set_enabled` | Disable/re-enable without removing — id, name and hit count survive. |
| `bp_remove` / `bp_clear_all` / `bp_list` | `bp_remove` / `bp_clear` / `bp_list` | Manage the registry. `bp_list` rows now carry `enabled`, `hit_count` (mem: at most one per instruction), `mirror` and `name`. |
| `run_until_break` | `run_until_break` | Run at full speed until a breakpoint fires (or a step budget). |
| `run` / `pause` | `run_until_break_interruptible` | Unbounded interruptible run: `run` goes until a breakpoint / `STOP` / `pause`; `pause` stops it (returns `interrupted: true`). No mandatory step budget. `pause` also ends every other run tool early — `step`, `step_until_frame`, `run_until_pc`, `run_until_break`, `run_until_mem_read` / `_write` — so a huge `max_steps` can never wedge the session. |
| `peek_oam` | `peek_oam` | All 544 OAM bytes (512 low table + 32 high table). |
| `capabilities` | — | luna `version` + the live tool catalogue, for client feature-detection (the handshake `serverInfo` also reports luna's name and version). |
| `start_input_capture` / `take_input_capture` | `start_input_capture` / `take_input_capture` | Record joypad changes and export a `frame:mask` script (replay with `--input @file`). |
| `load_symbols` | `load_symbols` | Load a WLA-DX `.sym`; disasm + traces become annotated. |
| `load_symbols_str` | `load_symbols_str` | Load `.sym` text directly (no host file — e.g. an in-memory build's output). Replaces the table. |
| `clear_symbols` | `clear_symbols` | Drop the loaded table. |
| `resolve_symbol` | `resolve_symbol` | Label name → 24-bit address. |
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
