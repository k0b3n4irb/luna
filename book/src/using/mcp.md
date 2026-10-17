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
| `memory_kinds` | `memory_kinds` | The memories this cartridge has, by Mesen2's name, with their size: `SnesMemory` (the CPU bus), `SpcMemory`, `SnesPrgRom`, `SnesWorkRam`, `SnesSaveRam`, `SnesVideoRam`, `SnesSpriteRam`, `SnesCgRam`, `SpcRam`, `SpcRom`, `SpcDspRegisters`, `DspProgramRom` / `DspDataRom` / `DspDataRam`, `Sa1InternalRam`, `GsuWorkRam`. |
| `read_memory` / `write_memory` | `read_memory` / `write_memory` | `count` bytes of any of them from `offset` (or `symbol`), and back, without side effects: a `SnesMemory` address goes to the memory behind it, never through the bus, so a read starts no S-DD1 decompression and drains no DSP-1 port (a register reads 0, open bus `$FF`). Writes reach ROM too. See the example below. |
| `eval` | `evaluate` | A debugger expression in Mesen2's language, evaluated as a watch: `{"value": 1, "kind": "boolean"}`. See [Expressions](#expressions) below. |
| `absolute_address` | `absolute_address` | The memory and offset behind a CPU-bus address or label — `{"kind": "SnesPrgRom", "offset": 16}` for `$80:8010` on a 32 KB `LoROM` — as the mapper's banking selects it now; `null` for a register. |
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
| `bp_add` | `bp_add_exec` / `bp_add_mem`, `bp_set_condition`, `bp_set_mark` | Register an exec breakpoint or a read/write watchpoint range. `mirror: false` makes a mem watch bank-exact (default follows WRAM/MMIO mirrors); `name` (defaulting to the `symbol` used) labels it in `bp_list`. `condition` is an [expression](#expressions) that must be non-zero for it to fire; `mark: true` puts a mark on the Event Viewer instead of halting. `kind: "spc_exec"` stops with the SPC700 at the start of an instruction (`addr` in ARAM, or an ARAM `symbol`). See [Conditional breakpoints](#conditional-breakpoints). |
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
eval {expression: "10 / x"}                             # → {value: 0, kind: "divide_by_zero"} when X is 0
```

- **Values.** Decimal, `$hex` / `0xhex` and `%binary` literals. The
  registers of the CPU: `a x y sp d db pc ps`, the `ps…` flags (`pscarry`,
  `psnegative`…), `nmi`, `irq`, `frame`, `scanline`, `hclock`, `cycle`;
  on the SPC700 `a x y sp ps pc dspreg`.
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

**The SPC700.** `kind: "spc_exec"` puts a breakpoint on an SPC700
instruction:

```text
bp_add {kind: "spc_exec", addr: 0x0400, condition: "a == $10"}   # the sound driver's command loop
run_until_break {max_steps: 50000000}   # → {kind: "spc_exec", pc: 1024, …}
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
