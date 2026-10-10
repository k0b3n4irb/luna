# I want to…

The guide's other pages are ordered by subject. This one is ordered by
what you came to do: find the line, take the command or the tool, follow
the link to the section that shows it running.

The **CLI** column is an option of the `luna` binary; the **MCP** column
is the tool an agent calls on `luna mcp`. Both drive the same
`luna-api`, so a recipe written for one translates to the other. A dash
means that transport has no form of it.

## Play

| I want to… | How | Shown in |
|---|---|---|
| start a game | `luna-gui "game.sfc"` | [Install & first run](using/install.md#prebuilt-binaries-recommended) |
| know whether my game runs | the coprocessor list, and what is refused at load | [Compatibility](using/compatibility.md) |
| remap the keys | *Settings → Input* | [Remap dialog](using/controls.md#remap-dialog) |
| use a gamepad | plug it in; the layout is fixed | [Gamepads](using/controls.md#gamepads) |
| save anywhere | `F5` / `F9`, nine slots | [Saves & save states](using/saves.md#save-states--full-snapshots-9-slots) |
| find my saves and screenshots on disk | two fixed folders per platform | [Where Luna keeps its files](using/saves.md#where-luna-keeps-its-files) |
| run a DSP-1 game | supply `dsp1b.rom` once | [Firmware](using/install.md#firmware-dsp-1-games) |

## See what the machine shows

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| a screenshot at a given frame | `luna run --until-frame N --screenshot out.png` | `step_until_frame`, `screenshot` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| one background layer alone | `--bg <1..4>` | `screenshot {bg}` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| the picture behind a forced blank | `--force-display` | `screenshot {force_display}` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| the true 512-wide hi-res frame | `--native-res` | `set_native_capture`, `screenshot {native}` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| a hash of the frame, to compare runs | `--print-fbhash` | `frame_hash` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| consecutive frames, to catch a flicker | `luna frames --from-frame N -c K` | — | [`luna frames`](using/cli-analysis.md#luna-frames--consecutive-frame-capture-temporal-artefacts) |
| the tiles, tilemaps, palette and sprites as PNGs | `luna assets-dump` | `render_vram_tiles`, `render_tilemap`, `render_palette`, `render_sprite_sheet` | [`luna assets-dump`](using/cli-analysis.md#luna-assets-dump--export-the-loaded-graphics-as-pngs) |
| the sprites as data, not pixels | `oam.json` from `luna assets-dump` | `decode_sprites`, `peek_oam` | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| every register at once | `luna state --out -` | `state` | [The state JSON](using/state-json.md) |

## Drive the game

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| press buttons on a schedule | `--input "frame:mask,…"` | `set_joypad` | [Scripted joypad input](using/input-scripts.md) |
| a second player, or five | `--input2` … `--input5`, `--port2 multitap` | `set_port_device`, `set_joypad {port}` | [Super Multitap](using/input-scripts.md#super-multitap-3-5-players) |
| a Mouse or a Super Scope | `--port1`, `--mouse`, `--superscope` | `set_port_device`, `set_mouse`, `set_superscope` | [Pointer devices](using/input-scripts.md#pointer-devices-mouse--super-scope) |
| record what I played and replay it | `--input @file` | `start_input_capture`, `take_input_capture` | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| resume from a save state | `luna state --load-state` | `save_state`, `load_state` | [Where Luna keeps its files](using/saves.md#where-luna-keeps-its-files) |
| carry the battery RAM across a power cycle | `--srm-out`, `--srm-in` | `sram_get`, `sram_set` | [Asserting on memory](using/cli-run-state.md#asserting-on-memory---assert---srm-in----srm-out) |
| boot a ROM with a blank header | `--force-mapper`, `--force-region` | `load_rom {force_mapper, force_region}` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| boot on the garbage RAM a console has | `--power-on random` | `load_rom {power_on}` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |

## Test my homebrew

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| a test suite my CI runs | `luna test tests/` | — | [Developing homebrew](using/homebrew-ci.md#the-manifest) |
| a ready GitHub Actions job | — | — | [A GitHub Actions recipe](using/homebrew-ci.md#a-github-actions-recipe) |
| assert before and after a button press | `[[checkpoint]]` in a manifest | — | [Checkpoints](using/homebrew-ci.md#checkpoints--beforeafter-assertions) |
| read or assert state at the end of a game tick, not at a frame boundary | `luna state --until-pc tickEnd --hit N`, or a symbol checkpoint in a manifest | `run_until_pc` | [Stopping on a routine](using/cli-run-state.md#stopping-on-a-routine-not-on-a-frame) |
| make a scripted press land in the same game tick on every build | `--input-at tickStart --input "81:0x0800"`, or the same key in a manifest | — | [A script clocked by the game](using/input-scripts.md#a-script-clocked-by-the-game-not-by-the-frame) |
| sample variables at every tick, in one run | `--peek-at tickEnd --peek var --peek-at-out ticks.csv` | — | [Stopping on a routine](using/cli-run-state.md#stopping-on-a-routine-not-on-a-frame) |
| assert on a byte without a manifest | `luna state --assert`, `--assert-aram`, `--assert-vram`, `--assert-cgram` | `peek_memory`, `peek_aram`, `peek_vram`, `peek_cgram` | [Asserting on memory](using/cli-run-state.md#asserting-on-memory---assert---srm-in----srm-out) |
| read my ROM's printf and its assertions | `--nocash-out`, `--wdm-out` | `enable_nocash_log`, `enable_wdm_log` and their `take_*` | [The SDK channels over MCP](using/mcp.md#reading-the-sdk-assertlog-channels-over-mcp) |
| know that a rebuild draws the same thing | `luna diff a.sfc b.sfc --frames …` | — | [`luna diff`](using/cli-analysis.md#luna-diff--two-roms-at-equal-ppu-frame-match--diff) |
| know that a rebuild shows the same pictures at another pace | `luna diff a.sfc b.sfc --sequence --to …` | — | [`luna diff --sequence`](using/cli-analysis.md#luna-diff---sequence--the-same-pictures-another-cadence) |
| recapture a memory block after an intended change | `luna test --update` | — | [Recapturing a block](using/homebrew-ci.md#recapturing-a-block) |
| know that a rebuild sounds the same | `luna diff --audio` | — | [`luna diff --audio`](using/cli-analysis.md#luna-diff---audio--the-same-sound-a-few-samples-apart) |
| what each exit code means | `0` pass, `1` assert, `2` usage | — | [Exit codes](using/cli-api-mcp.md#exit-codes-the-ci-contract) |

## Find the bug

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| read memory by address or by label | `--peek`, `--sym` | `peek_memory`, `load_symbols` | [`luna state`](using/cli-run-state.md#luna-state--json-snapshot--diagnostics-the-workhorse) |
| know who wrote this register | `--mem-trace` with `--trace-writes` | `enable_mem_trace`, `run_until_mem_write` | [Who wrote this register?](using/cli-run-state.md#who-wrote-this-register---trace-writes) |
| stop when an address is reached, read or written | — | `bp_add`, `run_until_break`, `run_until_pc`, `run_until_mem_read` | [Breakpoints & stepping](using/debugging.md#breakpoints--stepping) |
| see where the CPU is and how it got there | `--call-stack` | `enable_call_stack`, `call_stack` | [`--call-stack`](using/cli-run-state.md#where-is-the-cpu-and-how-did-it-get-there---call-stack) |
| trace a window of instructions | `--cpu-trace`, `--cpu-trace-from` | `enable_cpu_trace`, `take_cpu_trace` | [Tracing a window](using/cli-run-state.md#tracing-a-window-of-cpu-execution) |
| read the code at an address | — | `disasm_cpu`, `disasm_spc` | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| tell a hang from a wait | `scheduler.last_nmi_frame` in the state JSON | `loop_probe` | [The state JSON](using/state-json.md) |
| find the address of a variable I can only see on screen | — | `search_begin`, `search_refine`, `search_results` | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| pin a byte, or change one | — | `freeze_add`, `poke_memory`, `set_cpu_register` | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| see when in the frame a register was written | the GUI's Event Viewer | `enable_mem_trace` | [The Event Viewer](using/debugging.md#the-event-viewer) |
| see what a DMA sent to VRAM | `--dma-trace`, `--dump-vram` | `enable_dma_trace`, `peek_vram` | [`luna state`](using/cli-run-state.md#luna-state--json-snapshot--diagnostics-the-workhorse) |

## Measure what it costs

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| master cycles per function | `luna profile --sym game.sym` | `enable_profile`, `take_profile` | [`luna profile`](using/cli-analysis.md#luna-profile--real-master-cycles-per-symbol) |
| fail the build when the NMI handler is too slow | `--budget NmiHandler=6000` | — | [`luna profile`](using/cli-analysis.md#luna-profile--real-master-cycles-per-symbol) |
| find which frame overran, and what ran in it | `luna profile --frames-out frames.csv --worst 3` | — | [Frame by frame](using/cli-analysis.md#frame-by-frame--which-frame-overran-and-what-ran-in-it) |
| fail the build when a tick spills into one frame too many | `--max-lag-run 1`, `--max-lag-frames`, `--max-frame-mclk` | — | [Frame by frame](using/cli-analysis.md#frame-by-frame--which-frame-overran-and-what-ran-in-it) |
| know how deep the stack went | `--stack-floor` | — | [How deep the stack went](using/cli-analysis.md#how-deep-the-stack-actually-went) |
| know which code ran at all | `--pc-set` | — | [How deep the stack went](using/cli-analysis.md#how-deep-the-stack-actually-went) |
| how much of the frame is left | `stats.last_frame` in the state JSON | `state` | [The state JSON](using/state-json.md) |

## Hear it, and look inside the sound

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| the audio as a WAV | `--audio-out out.wav` | `drain_audio` | [`luna run`](using/cli-run-state.md#luna-run--quick-render--audio-dump) |
| the music as a `.spc` file | `luna spc-dump` | `export_spc` | [`luna spc-dump`](using/cli-analysis.md#luna-spc-dump--export-a-spc-sound-file) |
| the DSP voices and the driver's memory | `--peek APU:…`, `--dump-aram` | `dsp_registers`, `peek_aram` | [Audio-side visibility](using/cli-run-state.md#audio-side-visibility) |
| the order of the key-on and key-off writes | `--dsp-trace` | `enable_dsp_trace` | [Audio-side visibility](using/cli-run-state.md#audio-side-visibility) |
| what the CPU and the sound chip said to each other | `--apu-log`, `--spc-trace` | `enable_mailbox_log`, `enable_spc_trace` | [Audio-side visibility](using/cli-run-state.md#audio-side-visibility) |

## Look inside a coprocessor

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| stop on a Super FX job, read its result | `--superfx-trace` | `run_until_gsu_go`, `run_until_gsu_stop`, `peek_coproc_ram` | [Stopping on a Super FX job](using/mcp.md#stopping-on-a-super-fx-job) |
| what a Super FX job cost | `luna profile` | — | [What a Super FX job cost](using/cli-analysis.md#what-a-super-fx-job-cost) |
| who touched the cartridge while the GSU owned it | `--gsu-bus-trace` | — | [Who owns the cartridge](using/cli-analysis.md#who-owns-the-cartridge-super-fx) |
| how fast the SA-1 runs in this scene | `luna profile` | — | [How fast the SA-1 runs](using/cli-analysis.md#how-fast-the-sa-1-actually-runs) |
| what the SA-1 did on its side | `--sa1-side-log`, `--sa1-log`, `--sa1-trace` | `enable_sa1_side_log`, `enable_sa1_log`, `enable_sa1_trace` | [`--sa1-side-log`](using/cli-run-state.md#what-the-sa-1-itself-did---sa1-side-log) |
| read a DSP-1 command and its answer | `--dsp1-trace`, `--dsp1-trace-ports`, `--dsp1-trace-commands` | `enable_dsp1_trace`, `take_dsp1_trace` | [The DSP-1 handshake](using/cli-run-state.md#coprocessor-liveness-and-the-dsp-1-handshake) |

## Compare with another emulator, or a whole corpus

| I want to… | CLI | MCP | Shown in |
|---|---|---|---|
| the first frame where luna and a reference disagree | `luna wram-trace` | `wram_page_hashes`, `wram_snapshot` | [`luna wram-trace`](using/cli-analysis.md#luna-wram-trace--cross-emulator-state-differential) |
| the method behind that comparison | — | — | [The differential harness](method/differential.md) |
| a report over a folder of ROMs | `luna bench` | — | [`luna bench`](using/cli-analysis.md#luna-bench--whole-corpus-compatibility-report) |
| know which outputs are stable across runs | — | — | [Determinism guarantees](method/determinism.md#the-table) |

## Let an agent drive

| I want to… | How | Shown in |
|---|---|---|
| connect an MCP client with the ROM already loaded | `luna mcp --rom game.sfc` | [`luna mcp`](using/mcp.md#luna-mcp--mcp-server-over-stdio) |
| the list of tools | `capabilities`, or the catalogue | [MCP catalogue](using/mcp.md#mcp-tool-catalogue) |
| load a ROM that exists only in memory | `load_rom_bytes`, `load_symbols_str` | [Loading homebrew from the assembler](using/mcp.md#loading-homebrew-straight-from-the-assembler) |
| run until something happens, without a step budget | `run`, then `pause` from another call | [Breakpoints & stepping](using/debugging.md#breakpoints--stepping) |
| use the same surface from Rust | `luna_api::Emulator` | [The `luna-api` surface](using/rust-api.md) |
