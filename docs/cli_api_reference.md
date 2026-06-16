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
consumers of the one `luna_api::Emulator` contract (see
`.claude/rules/api-first.md`). What `luna state` measures is exactly what
the GUI shows — coherence by construction.

---

## 1. The `luna` CLI

```
luna <COMMAND>

Commands:
  run         Load a ROM, step N instructions, optionally dump a screenshot.
  state       Run through luna-api and emit a JSON state snapshot (+ dumps/traces).
  frames      Capture EXACTLY-consecutive PPU frames as PNGs (temporal artefacts).
  wram-trace  Per-frame vblank-aligned WRAM page hashes (cross-emulator differential).
  bench       Run a whole ROM directory headless and write a compatibility report.
  mcp         Serve the luna MCP server on stdio.

Global options:
  -h, --help     Print help
  -V, --version  Print version
```

Build it with `cargo build --release -p luna-cli`; the binary is
`./target/release/luna`.

### `luna run` — quick render / audio dump

```
luna run [OPTIONS] <ROM>
```

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the `.sfc` / `.smc` ROM. |
| `-n, --steps <N>` | `64` | CPU instructions to execute before dumping. |
| `--screenshot <PATH>` | — | Render a 256×224 PNG of the framebuffer to `PATH`. |
| `--force-display` | off | Bypass INIDISP forced-blank so you see whatever is in VRAM/CGRAM. |
| `--bg <1..=4>` | composited | Render ONLY that BG layer instead of the composited frame. |
| `--audio-out <PATH>` | — | Capture the APU's 32 kHz stereo output to a WAV. |

```bash
luna run -n 12000000 --screenshot /tmp/title.png "game.sfc"
```

### `luna state` — JSON snapshot + diagnostics (the workhorse)

```
luna state [OPTIONS] <ROM>
```

Emits the same `EmulatorState` JSON the MCP `state` tool returns (§2),
and is the hub for every headless diagnostic.

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the ROM. |
| `-n, --steps <N>` | `1000` | CPU instructions before snapshotting. |
| `--out <PATH>` | `-` | Where to write the JSON (`-` = stdout). |
| `--force-mapper <M>` | auto | Force a mapper for headerless ROMs: `lorom`, `hirom`, `exhirom`, `sa1`, `superfx`. |
| `--dsp1-rom <PATH>` | — | Install `dsp1b.rom` firmware then load (Mario Kart, Pilotwings). Persists. |
| `--load-state <PATH>` | — | Load a `.luna` save-state right after ROM load, before warm-up (resume a GUI-captured scene). |
| `--input <SCRIPT>` | — | Scripted joypad-1 input (§3). |
| `--screenshot <PATH>` | — | Also write a PNG. |
| `--audio-out <PATH>` | — | Also write a 32 kHz stereo WAV. |
| `--peek <B:O:C>` | — | Hex-dump `COUNT` bytes at `BANK:OFFSET` to stderr (repeatable). |
| `--dump-vram <PATH>` | — | Dump all 64 KB PPU VRAM (raw). |
| `--dump-aram <PATH>` | — | Dump all 64 KB APU ARAM (raw). |
| `--dump-coproc-ram <PATH>` | — | Dump coprocessor work RAM (Super FX Game Pak RAM), ungated. |
| `--apu-log <PATH>` | — | CSV of every `$2140-$2143` CPU↔APU mailbox access. |
| `--sa1-log <PATH>` | — | CSV of every `$2200-$23FF` SA-1 MMIO access. |

```bash
# JSON snapshot to stdout, plus a peek at SMW shadow-OAM
luna state -n 1000000 --peek 7E:0200:220 "game.sfc"

# Reach the name-entry screen by pulsing Start, then screenshot
luna state -n 55000000 \
  --input "1600:0x1000,1610:0,2000:0x1000,2010:0" \
  --screenshot /tmp/name.png "game.sfc"
```

### `luna frames` — consecutive-frame capture (temporal artefacts)

```
luna frames [OPTIONS] <ROM>
```

Captures a run of consecutive PPU frames as PNGs through the same render
path the GUI uses — for flicker / page-flip-desync bugs a single
screenshot can't show. Each PNG is tagged with its frame number and the
forced-blank flag.

| Option | Default | Purpose |
|---|---|---|
| `-n, --steps <N>` | `1000` | Warm-up instructions before capture begins. |
| `-c, --count <N>` | `8` | Number of consecutive frames to capture. |
| `--out-dir <DIR>` | `/tmp/luna_frames` | Output directory (created if absent). |
| `--force-mapper <M>` | auto | As in `state`. |
| `--input <SCRIPT>` | — | Joypad-1 script applied during warm-up (§3). |

### `luna wram-trace` — cross-emulator state differential

```
luna wram-trace [OPTIONS] <ROM>
```

Emits per-frame (vblank-aligned) FNV-1a hashes of each WRAM page. With no
input, WRAM-at-vblank-N is the **same game-frame** in luna and a
reference emulator, so the first differing frame pins the first real
state divergence (THE method's confound-free oracle). Line format:
`<ppu_frame> <h0> <h1> … <hN>`.

| Option | Default | Purpose |
|---|---|---|
| `-n, --steps <N>` | `0` | Warm-up instructions before frame 0. |
| `-c, --count <N>` | `300` | Consecutive frames to hash. |
| `--page-size <BYTES>` | `4096` | Page size (power of two dividing `0x20000`). |
| `--out <PATH>` | `/tmp/luna_wram_hashes.txt` | Hash-table output. |
| `--dump-frame <N>` | — | Also dump the full 128 KiB WRAM as raw `.bin` at frame `N`. |
| `--dump-out <PATH>` | `/tmp/luna_wram_frame.bin` | Where the `--dump-frame` snapshot goes. |
| `--force-mapper <M>` | auto | As in `state`. |
| `--input <SCRIPT>` | — | Joypad-1 script (§3). |

### `luna bench` — whole-corpus compatibility report

```
luna bench [OPTIONS] [DIR]
```

Runs every `.sfc`/`.smc` in `DIR` headless, detects anomalies (crashes,
freezes, dead APU, missing firmware) panic-safely, and writes a
compatibility report + one markdown bug file per finding. Reports stay
local (gitignored under `--out`).

| Option | Default | Purpose |
|---|---|---|
| `[DIR]` | `tests/roms` | Directory of ROMs to scan. |
| `--out <DIR>` | `tests/roms/bench` | Output dir for `report.md`, `bugs/*`, `screenshots/*`. |
| `-f, --frames <N>` | `600` | Frames to run per ROM. |
| `--input <SCRIPT>` | Start-pulse | Override the default title-clearing input (§3). |

### `luna mcp` — MCP server over stdio

```
luna mcp
```

Serves the tool catalogue in §4 to any connected MCP client (Claude
Desktop, Claude Code, custom). Stays alive until the client closes the
stream. No options — configure the client to launch `luna mcp`.

---

## 2. The state JSON (`EmulatorState`)

`luna state` / the MCP `state` tool serialise this top-level shape:

| Field | Contents |
|---|---|
| `rom` | `RomInfo`: `title`, `mapper`, `rom_bytes`, `header_rom_size_kb`, `sram_kb`, `region`, `fast_rom`, `version`, `checksum{,_complement,_valid}`, `missing_firmware`. |
| `cpu` | 65c816 registers `a/x/y/sp/pc/pb/db/dp/p` + flags. |
| `cpu_regs` | Decoded MMIO/CPU register block. |
| `ppu` | PPU registers + VRAM/CGRAM/OAM occupancy. |
| `scheduler` | Master-clock / line / frame scheduler state. |
| `apu` | SPC700 + S-DSP state (`spc_stopped`, etc.). |
| `dma` | DMA/HDMA channel state. |
| `stats` | Counters: `nmis_serviced`, frame count, instruction count, NMI rate, … |

(See `crates/luna-api/src/lib.rs` for the full nested field set.)

---

## 3. Scripted joypad input (`--input`)

Shared by `state`, `frames`, `wram-trace`, `bench`. Format:
comma-separated `frame:hex` checkpoints — frame number in decimal, mask
in hex (optional `0x`). The mask is latched at the **start** of the named
PPU frame and held until the next checkpoint overrides it.

```
--input "100:0x1000,110:0"   # hold Start for frames 100..=109, then release
```

**JOY1 bit layout:** `B(15) Y(14) Select(13) Start(12) Up(11) Down(10)
Left(9) Right(8) A(7) X(6) L(5) R(4)`. So Start = `$1000`, A = `$80`.

> Most commercial titles sit at a title/demo screen waiting for Start —
> a black/forced-blank screenshot with no input is **not** a bug. Pulse
> Start to get past it (see `.claude/rules/coproc-testing.md`).

---

## 4. MCP tool catalogue (`luna mcp`)

Each tool is a thin wrapper over the matching `luna_api::Emulator`
method, so the MCP transport adds reach, not capability.

| Tool | Maps to | Purpose |
|---|---|---|
| `load_rom` | `load_rom` | Load a `.sfc`/`.smc` from a host path. |
| `reset` | `reset` | Reset to power-on state. |
| `set_joypad` | `set_joypad` | Set the button bitmask for `port` (0 = P1, 1 = P2). |
| `step` | `step` | Step `count` instructions (stops early if the CPU halts). |
| `step_until_frame` | `step_until_frame` | Run until one PPU frame completes (bounded). |
| `state` | `state` | Full observable-state JSON snapshot (§2). |
| `screenshot` | `render_frame_png` | Render the 256×224 composited framebuffer to PNG. |
| `drain_audio` | `drain_audio` | Drain up to `max` stereo samples from the APU. |
| `peek_memory` | `peek_memory` | Read `count` bytes from the CPU bus at `bank:offset`. |
| `peek_aram` | `peek_aram` | Read `count` bytes from the SPC700's 64 KB ARAM. |

---

## 5. The `luna-api` Rust surface (`Emulator`)

Add `luna-api` as a dependency and drive the emulator directly. Every
method returns `Result<_, ApiError>` unless noted. Grouped by purpose:

**Lifecycle / loading**
- `load_rom(path)` → `RomInfo`, `load_rom_bytes(bytes)`,
  `load_rom_bytes_forced(bytes, mapper)`
- `reset()`
- `firmware_dir()`, `install_firmware(src, target)` — DSP-1 etc.

**Driving**
- `step(count)` → instructions executed
- `step_until_frame(max_steps)`, `loop_probe(max_steps)` → `LoopProbe`
- `set_joypad(port, mask)`

**Observation**
- `state()` → `EmulatorState` (the whole snapshot)
- `cpu_state()`, `spc700_state()`
- `frame_count()`, `forced_blank()`, `frame_showed_content()`,
  `framebuffer_hash()`

**Rendering**
- `render_frame_png(force_display)`, `render_frame_rgba(force_display)`
- `render_frame_bg_png(bg, force_display)`
- `render_tilemap_rgba(bg_idx)` → `TilemapImage`
- `decode_sprites()` → `Vec<SpriteInfo>`

**Save-states**
- `save_state()` → bytes, `load_state(bytes)`

**Audio**
- `audio_queue_len()`, `drain_audio(max)` → `Vec<(i16, i16)>`

**Memory / register peeking**
- `peek_memory(bank, offset, count)`, `peek_aram(offset, count)`,
  `peek_vram(offset, count)`, `peek_cgram()`, `peek_pc_bytes(count)`
- `vram_bytes()`, `aram_bytes()`, `wram_snapshot()`,
  `wram_page_hashes(page_size)`, `coproc_ram()`

**Disassembly**
- `disassemble_cpu(start, …)` (M/X-aware), `disassemble_spc(start, count)`

**Tracing / diagnostics** (enable, run, then take the buffered log)
- mailbox: `enable_mailbox_log` / `take_mailbox_log`
- SA-1: `enable_sa1_log` / `take_sa1_log`, `…_side_log`, `…_trace`
- Super FX: `enable_superfx_trace` / `take_superfx_trace`
- DMA: `enable_dma_trace` / `take_dma_trace`
- CPU: `enable_cpu_trace` / `take_cpu_trace_log`
- memory: `enable_mem_trace` / `take_mem_trace_log`

---

## 6. Controls & firmware

- **GUI keyboard bindings + hotkeys:** `docs/CONTROLLER_BINDINGS.md`.
- **Coprocessor firmware (DSP-1, …):** `docs/firmware.md`; install via
  `luna state --dsp1-rom <path>` or `Emulator::install_firmware`.
