# Mesen2 parity — what luna takes from Mesen2, and where it stands

The maintainer wants luna to offer everything Mesen2 offers for the SNES.
Debugging comes first, the cosmetic right after (decision of 2026-10-17).
This page is the living roadmap: one row per feature, flipped when a commit
lands it.

✅ done · 🟡 in progress · ⬜ to do

## Rules every lot follows

- **API first.** Each capability lands in `luna-api`, then as an MCP tool
  and a CLI option where they make sense, then as a GUI panel
  (`.claude/rules/api-first.md`).
- **Licence.** Mesen2 is GPL-3 and luna MPL-2.0.
  - luna re-implements Mesen2's tools from their behaviour: what they show
    and do, their documentation, their file formats, the names of the Lua
    API.
  - An exact algorithm (a video filter) goes through a clean-room
    description written by a separate agent.
  - ares (ISC) is ported directly.
- **Proof.**
  - Debugger panels are tested through the API.
  - Whatever changes a picture, a sound or a GUI control waits for the
    maintainer's eyes or ears.
  - An output Mesen2 also produces (a trace line, a `.cdl`, an event list,
    a picture) is compared with `Mesen --testRunner` on the same ROM.

## D — Debugging (priority 1)

| Lot | Feature | State |
|---|---|---|
| D1a | Every memory by Mesen2's name (`MemoryKind`): read / write / size, a CPU-bus address resolved to the memory behind it, without side effects; MCP `memory_kinds`, `read_memory`, `write_memory`, `absolute_address` | ✅ |
| D1b | Expression evaluator, Mesen2's grammar (`[byte]`, `{word}`, `#long`, `:abs`, labels, `value`, `address`, `scanline`…), checked on Mesen2's 87 reference cases; 65C816 and SPC700 contexts; MCP `eval` | ✅ |
| D1b | SA-1 expression context (`ExprCpu::Sa1`) | ✅ |
| D1b | GSU expression context (`ExprCpu::Gsu`) | ✅ |
| D1b | DSP-1 expression context (`ExprCpu::Dsp`) | ✅ |
| D1c | Conditions (Mesen2's language) and marks on the 65C816's exec breakpoints and watchpoints; every match of an instruction checked; DMA flagged | ✅ |
| D1c | Break sources: BRK, COP, WDM, STP, NMI and IRQ handler entry, a scanline; MCP `set_break_sources` | ✅ |
| D1c | SPC700 exec breakpoints, with conditions, stopping at the instruction start; MCP `bp_add {kind: "spc_exec"}` | ✅ |
| D1c | VRAM, CGRAM, OAM read and write watchpoints (CPU and DMA, through the ports, at the PPU's own address); MCP `bp_add {memory}` | ✅ |
| D1c | SPC700 watchpoints on `SpcMemory`, `SpcRam`, `SpcRom` (data accesses, stopping just after the access) | ✅ |
| D1c | SA-1 exec breakpoints and `Sa1Memory` watchpoints, frozen in the chip; MCP `bp_add {cpu: "sa1"}` | ✅ |
| D1c | GSU exec breakpoints and `GsuMemory` watchpoints; MCP `bp_add {cpu: "gsu"}` | ✅ |
| D1c | DSP-1 exec breakpoints (program ROM bytes) and `DspDataRam` watchpoints; MCP `bp_add {cpu: "dsp"}` | ✅ |
| D1c | *Forbid* ranges on every CPU, by its address or (65C816) the memory behind it, with a condition; marks and steps unaffected; MCP `bp_add {kind: "forbid"}` | ✅ |
| D1d | Step types: into, over, out, scanline, frame, *break in N* instructions / scanlines / frames (to NMI / IRQ are break sources; run to here is `run_until_pc`; move PC is `set_cpu_register`); MCP `step_kind` | ✅ |
| D1d | CPU-cycle and PPU-cycle steps — not possible while luna runs instructions whole | ⬜ |
| D2a | Labels and comments by memory and offset, Mesen2's model: default register labels, `.mlb` import / export (Mesen2's grammar, measured on Mesen2), `<rom>.mlb` loaded with the ROM; seen by disassembly, expressions and every `symbol` argument; `SnesRegister` memory; MCP `label_set`, `label_remove`, `label_list`, `labels_import`, `labels_export` | ✅ |
| D2a | `ui.rs` split into one module per panel (no behaviour change) | ✅ |
| D2b | Code log (instructions run, widths, jump targets, subroutine entries, decoded ahead) for the 65C816 and the SPC700 — the code half of the CDL, brought forward | ✅ |
| D2d | Debugger window (GUI) for the 65C816: code view, Status, Labels / Functions / Find Results / Controllers, Watch / Breakpoints / Call Stack, dialogs, shortcuts, settings panel, workspace import / export | ✅ |
| D2g | SPC Debugger window (listing, Status, Labels, Find Results, Watch, Breakpoints); SPC700 steps into / over / out and register writes in the API and the MCP. The SPC700's call stack is still to come | ✅ |
| D2f | Label imports: ca65 `.dbg` (segments, sizes, source comments) and bass `.sym`, Mesen2's auto-load order (`.dbg`, `.sym`, `.mlb`). SDCC `.cdb` and ELF: Mesen2 reads neither for SNES, so neither does luna | ✅ |
| D2e | Workspace per ROM (labels, breakpoints, watches, window settings), shared by the GUI and the MCP | ✅ |
| D2c | Panes' content in `luna-api`: watch formats, Go To, Go To All, Find, Find All Occurrences, function list, label list, call stack rows | ✅ |
| D2b | Code listing, Mesen2's Disassembly pane: whole address space, blocks, labels, comments, function separators, effective address and value from the live registers; MCP `listing` | ✅ |
| D2 | Debugger window per CPU (disassembly with labels, comments, effective addresses; editable status; breakpoint, watch, label, function, call-stack lists; go to / find all); labels and comments in `.mlb`, ca65 `.dbg`, bass, SDCC `.cdb`, ELF; default register labels; workspace per ROM | ⬜ |
| D3 | Per-byte access counters; Memory Tools (every memory, edit, highlight by access / code / data, freeze, find, `.tbl`); Memory Search on every memory | ⬜ |
| D4 | Trace logger (per CPU, Mesen2's format tags, condition, to file); profiler with calls and inclusive / exclusive time; Event Viewer completed and exposed to MCP and CLI | ⬜ |
| D5 | Code/Data Logger (`.cdl` compatible with Mesen2), function list; Tile, Tilemap (main / sub screen), Sprite and Palette viewers; Tile Editor; decoded Register Viewer | ⬜ |
| D6 | Rewind in `luna-api`; step back by instruction, scanline, frame | ⬜ |
| D7 | 65C816 assembler, ROM / IPS save; Lua with Mesen2's `emu.*` API (`mlua`), script window, `luna script` | ⬜ |

## C — Cosmetic (priority 2)

| Lot | Feature | State |
|---|---|---|
| C1 | Video Size, Aspect Ratio, Video Filter: Prescale, Scale2x-4x, LCD Grid, xBRZ, 2xSaI family, NTSC (blargg), HQx — each Mesen2's picture to the pixel | ✅ |
| C1 | NTSC options panel and presets; scanlines; picture settings; overscan; integer fullscreen scaling; rotation; hide BG layers / sprites; hi-res blend; native 512-wide picture to the filters; OSD (frame, lag, timer, input display) | ⬜ |
| C2 | Audio: 32 040 Hz; per-voice volume; interpolation choice; equaliser, reverb, crossfeed; device, latency, rate; WAV recorder; SPC player | ⬜ |
| C3 | Video recording from the GUI | ⬜ |
| C4 | Theme, recent files, game selection screen, pause in background, confirmations, folders, full shortcut set, zip / 7z, IPS / BPS / UPS, settings overrides on the command line | ⬜ |

## F — Emulation comfort (priority 3)

| Lot | Feature | State |
|---|---|---|
| F1 | Save states: 10 slots + auto, dialog with previews, last session, auto-save | ⬜ |
| F2 | Rewind key; History Viewer with movie export | ⬜ |
| F3 | Speed 0-5000 %, fast-forward, slow motion, run-ahead | ⬜ |
| F4 | Reset vs power cycle; forced region; power-on RAM; overclocking | ⬜ |
| F5 | Movies (`.mmo`) record and play | ⬜ |
| F6 | Cheats (Game Genie, Pro Action Replay), cheat list and database | ⬜ |
| F7 | Controllers: turbo, presets, gamepad remap, dead zone, per-game settings | ⬜ |

## P — Missing SNES chips (priority 4)

Faithful ports of ares, cross-checked with Mesen2, in this order: Cx4,
OBC1, S-RTC, SPC7110 (with its clock), MSU-1, BS-X, Sufami Turbo. Each
then gets its debugger tools. All ⬜.

## N — Netplay (last) · ⬜

## Out of scope

Super Game Boy (another system), NTSC (Bisqwit) (NES-only in Mesen2), the
NES / FDS / VS tools, automatic updates.
