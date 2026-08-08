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
  spc-dump    Export the live APU state as a playable .spc sound file.
  assets-dump Dump the loaded graphics (VRAM tiles, tilemaps, palette, sprites) as PNGs.
  mcp         Serve the luna MCP server on stdio.

Global options:
  -h, --help     Print help
  -V, --version  Print version
```

Build it with `cargo build --release -p luna-cli`; the binary is
`./target/release/luna`.

#### Exit codes (the CI contract)

| Code | Meaning |
|---|---|
| `0` | Run completed; every `--assert*` spec passed. (A ROM hitting an unimplemented core path still exits 0 — emulation gaps are reported, not treated as CLI failures.) |
| `1` | Runtime failure (ROM load, I/O, trace enable) **or** at least one `--assert*` spec failed (each failing spec prints a `FAIL …` line on stdout). |
| `2` | Usage error — a malformed `--input` / `--mouse` / `--superscope` script (any subcommand). Fix the invocation, not the ROM. |

A test harness should treat `1` as "the ROM regressed" and `2` as "the
harness itself is broken".

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
| `--force-mapper <M>` | auto | Force a mapper (`lorom`/`hirom`/`exhirom`/`sa1`/`superfx`) for a headerless / checksum-invalid ROM. |
| `--force-region <R>` | header | Force the video standard (`ntsc`/`pal`) — changes the scanline count (262/312) and frame rate. |
| `--native-res` | off | Emit the native **512×448** frame for `--screenshot`/`--print-fbhash`: hi-res modes 5/6 & pseudo-512 keep both horizontal subpixels, interlace keeps both fields as lines. |
| `--wdm-out <PATH>` | — | Write captured `WDM $xx` executions (the `SNES_ASSERT` channel) — a non-empty file means an assertion fired. |
| `--print-fbhash` | off | Print `fbhash=<16-hex>`, a cross-arch-stable key for the displayed frame. |

```bash
luna run -n 12000000 --screenshot /tmp/title.png "game.sfc"

# Visual baseline for a reference ROM with a bad header (e.g. a PeterLemon
# test ROM): force the mapper so it renders, and print the hash key.
luna run -n 3000000 --force-mapper lorom --print-fbhash "WaveHDMA.sfc"
# → fbhash=7429bf441a1c7d6c   (record this as the test's expected value)
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
| `--force-region <R>` | header | Force the video standard: `ntsc` or `pal`. |
| `--native-res` | off | As in `run` — native 512×448 output for `--screenshot` and `--print-fbhash`. |
| `--sym <PATH>` | auto-detect `<rom>.sym` | Load a WLA-DX symbol file (annotated disasm, named addresses). |
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
| `--dsp1-trace <PATH>` | — | DSP-1 (µPD77C25) trace: microcode execution **and** CPU-side DR/SR traffic in one stream — `seq,kind,pc,opcode,value,a,b,dr,sr,rqm` (`kind` = E/W/R/S). |
| `--dsp1-trace-ports` | off | Restrict the above to the DR/SR transactions (the stock firmware idles in an RQM loop, so a full trace is mostly idle spin). |
| `--dsp1-trace-max <N>` | `200000` | Cap on captured DSP-1 events. |
| `--dsp-trace <PATH>` | — | CSV of every DSP register write: `spc_cycles,reg,name,value`, with `name` decoded (`V0_ADSR1`, `KON`, `FLG`, …). |
| `--dsp-trace-max <N>` | `100000` | Cap on captured DSP writes. |
| `--sa1-log <PATH>` | — | CSV of every `$2200-$23FF` SA-1 MMIO access. |
| `--print-fbhash` | off | Print `fbhash=<16-hex>` for the displayed frame — the same key as `run`, so an `--input`-driven test can carry a visual baseline. |
| `--wdm-out <PATH>` | — | Write captured `WDM $xx` (`SNES_ASSERT`) executions — keeps the assertion oracle on an `--input` test. |

```bash
# JSON snapshot to stdout, plus a peek at SMW shadow-OAM
luna state -n 1000000 --peek 7E:0200:220 "game.sfc"

# Reach the name-entry screen by pulsing Start, then screenshot
luna state -n 55000000 \
  --input "1600:0x1000,1610:0,2000:0x1000,2010:0" \
  --screenshot /tmp/name.png "game.sfc"

# A self-contained gameplay regression test: drive input, then emit BOTH a
# visual baseline (fbhash) and the assertion oracle (WDM) in one run.
luna state -n 55000000 --input @repro.input \
  --print-fbhash --wdm-out /tmp/asserts.txt "game.sfc"
```

Reproduce the golden harness's configuration exactly — it runs the homebrew
corpus as **PAL** to match krom's reference captures, which a blank test-ROM
header can't say:

```bash
luna state -n 5000000 --force-mapper lorom --force-region pal \
  --screenshot /tmp/bra.png "CPUTest/CPU/BRA/CPUBRA.sfc"
```

Exact-resolution regression for the hi-res / interlace demos (issue #115): the
PPU really computes 512 horizontal subpixels and two interlace fields, then
averages them into the displayed 256×224 — `--native-res` keeps them:

```bash
luna state -n 8000000 --force-mapper lorom --force-region pal --native-res \
  --screenshot /tmp/font.png --print-fbhash \
  "PPU/Interlace/InterlaceFont/InterlaceFont.sfc"   # → a 512×448 PNG
```

#### Coprocessor liveness and the DSP-1 handshake

`--superfx-trace` and `--sa1-trace` let a harness prove those chips ran;
`--dsp1-trace` closes the gap for the DSP-1 (µPD77C25). Note the two
distinct flags: `--dsp-trace` is the **audio** S-DSP, `--dsp1-trace` the
**cart coprocessor**.

```bash
# Liveness without any trace: state JSON carries the instruction count.
luna state "Super Mario Kart (USA).sfc" -n 5000000 --out - \
  | jq '.dsp1.instructions_executed'    # assert >= 1
# -> 41780039

# The command handshake. --dsp1-trace-ports is what makes it readable:
# the stock firmware idles in a two-instruction RQM wait loop, so a full
# trace spends its whole budget on idle spin before your command lands.
luna state "Super Mario Kart (USA).sfc" -n 5000000 \
  --dsp1-trace dsp1.csv --dsp1-trace-ports
# seq,kind,pc,opcode,value,a,b,dr,sr,rqm
# 0,W,$0004,$000000,$80,$0000,$00C0,$0080,$0400,0   <- command byte in
# 143,S,$0185,$000000,$00,$7FFF,$0000,$3400,$0000,0 <- poll: RQM clear, busy
# 195,R,$034D,$000000,$00,$003E,$0000,$0000,$9000,1 <- result byte out

# Drop --dsp1-trace-ports to see the microcode between the transactions.
```

On a port row `pc` is *not* a CPU address: it is where the DSP-1 microcode
was sitting when the CPU touched the port. That is the column that turns a
handshake into something readable — group by it and the firmware's structure
falls out. On the run above:

```console
$ awk -F, 'NR>1 {print $2, $3}' dsp1.csv | sort | uniq -c | sort -rn | head -5
  21462 R $038F
  21462 R $038D
  21462 R $038A
  21462 R $0387
  19224 S $0387
```

Four read sites hit an identical number of times is the firmware handing back
a four-word result, one word per site — and the `S` rows at `$0387` are the
CPU polling the same site until `RQM` comes up. An off-by-one in a command's
result length shows up here as a fifth site, or as one count that does not
match the others.

##### Command transactions

`--dsp1-trace-commands` groups that byte stream into one row per command —
the command byte, the input words it consumed, the output words it produced:

```bash
luna state "Super Mario Kart (USA).sfc" -n 5000000 \
  --dsp1-trace-commands dsp1_cmds.csv
# seq,cmd,name,pc,in_words,out_words,expected_in,expected_out,confidence,status,in,out
# 128,$02,Parameter,$0004,7,4,7,4,provisional,ok,$0880|$27A0|…,$0000|$FFB2|…
# 203,$0A,Raster,$0004,5,384,-,-,unbounded,unbounded,$FFB6|$8000|…,$05FF|…
```

Boundaries come from the **protocol** — an 8-bit (`DRC`) write opens a
command, and every word until the next one belongs to it — never from the
word-count table. The table only supplies the `expected_*` columns, so a
stale entry surfaces as `status=mismatch` on that single row, with both
counts side by side, while every other transaction stays correctly grouped.
That is deliberate: a word count is documentation, and documentation must
never be able to make an emulator look broken.

Read the two verdict columns together:

| Column | Meaning |
|---|---|
| `confidence` | `verified` (checked on hardware-grade traces) → `documented` → `provisional`. How much the `expected_*` figures are worth. |
| `status` | `ok`, `mismatch`, `unbounded` (open-ended output — observed length reported, nothing asserted), `truncated` (capture hit its cap mid-transaction), `unknown` (command not in the table). |

A `mismatch` on a `provisional` row is far more likely a stale table entry
than an emulator defect. A `mismatch` on a `verified` row is worth chasing.

#### Audio-side visibility

Three views for driver debugging, when a WAV capture alone cannot say
what the SPC actually did:

```bash
# 1. Structured DSP state: per-voice registers + live decode state.
luna state game.sfc -n 3000000 --out - \
  | jq '.apu.dsp | {mvol_l, kon, dir, voices: [.voices[] | select(.keyed_on)
        | {index, srcn, pitch, envx, outx, envelope_phase}]}'

# 2. Peek ARAM directly — verify an uploaded driver image or the
#    $F0-$FF register page (hex offset:count, like a CPU-bus peek).
luna state game.sfc -n 3000000 --peek APU:0200:40 --peek APU:00F0:10

# 3. DSP register-write trace — the sequencing oracle: did the
#    KON/KOFF pulses reach the chip in the intended order?
luna state game.sfc -n 2000000 --dsp-trace dsp.csv
# spc_cycles,reg,name,value
# 0,$6C,FLG,$20
# 0,$5C,KOFF,$FF     <- driver mutes every voice before setup
# 0,$5D,DIR,$0A      <- sample directory at $0A00
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
| `[DIR]` | the bundled ROM dir | Directory of ROMs to scan. |
| `--out <DIR>` | a `bench` subdir | Output dir for `report.md`, `bugs/*`, `screenshots/*`. |
| `-f, --frames <N>` | `600` | Frames to run per ROM. |
| `--input <SCRIPT>` | Start-pulse | Override the default title-clearing input (§3). |

### `luna spc-dump` — export a `.spc` sound file

```
luna spc-dump [OPTIONS] <ROM>
```

Runs the ROM until its music driver is playing, then writes the live APU
state as a `.spc` file (`SNES-SPC700 Sound File Data v0.30`): SPC700
registers + 64 KB ARAM + 128 DSP registers + IPL ROM, playable in any SPC
player. Step far enough in — and pulse Start via `--input` — that the
music has started before the snapshot.

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the ROM. |
| `-n, --steps <N>` | `5000000` | CPU instructions before the snapshot. |
| `-o, --out <PATH>` | `<rom-stem>.spc` | Output path for the `.spc`. |
| `--force-mapper <M>` | auto | As in `state`. |
| `--dsp1-rom <PATH>` | — | Install `dsp1b.rom` then load (DSP-1 games). |
| `--input <SCRIPT>` | — | Joypad-1 script applied before the snapshot (§3). |

```bash
luna spc-dump "game.sfc" -n 8000000 -o /tmp/song.spc
```

### `luna assets-dump` — export the loaded graphics as PNGs

```
luna assets-dump [OPTIONS] <ROM>
```

Runs the ROM to a scene, then writes every graphics asset **currently
loaded** as PNGs: `screen.png`, `vram_tiles.png` (the whole 64 KB VRAM as
a tile sheet), `bg1..4_tilemap.png` (only the layers enabled in the
current mode; Mode 7 → one `bg1_tilemap_mode7.png`), `palette.png`, and
`sprites.png` (the 128 OAM sprites at native size, transparent). Also
raw `vram.bin` / `cgram.bin` and `oam.json` (sprite metadata).

> **This captures only what is loaded at that instant** (already
> decompressed by the game). Snapshot several scenes — different `-n`,
> or `--input` to reach them — to cover a whole game. A static
> whole-ROM rip is **not** possible: SNES graphics are
> game-specific-compressed with no standard layout.

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the ROM. |
| `-n, --steps <N>` | `5000000` | CPU instructions before the snapshot. |
| `--out <DIR>` | `/tmp/luna_assets` | Output directory (created if absent). |
| `--bpp <2\|4\|8>` | auto (BG1 mode) | Bit-depth for the VRAM tile sheet. |
| `--palette <N>` | `0` | CGRAM sub-palette row for the tile sheet (2/4bpp). |
| `--force-mapper <M>` | auto | As in `state`. |
| `--dsp1-rom <PATH>` | — | Install `dsp1b.rom` then load (DSP-1 games). |
| `--input <SCRIPT>` | — | Joypad-1 script applied before the snapshot (§3). |

```bash
luna assets-dump "game.sfc" -n 8000000 --out /tmp/assets
```

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
| `dma` | Per-channel DMA/HDMA registers (see below). |
| `stats` | Counters: `nmis_serviced`, frame count, instruction count, NMI rate, … |

(See the `luna-api` rustdoc for the full nested field set.)

The `dma` block is the headless surface for the `$43xx` DMA/HDMA registers —
which read `0` through `--peek` because they are **write-only on hardware**.
Each of `dma.channels[0..8]` gives `params` (DMAP), `bbad` (BBAD, target
`$2100+bbad`), `a_addr` (A1T, table start), `a_bank`, `das`, `a2a` (HDMA
indirect / table pointer) and `ntlr` (HDMA line counter):

```bash
# Watch an HDMA table pointer advance per frame (e.g. a scanline wave effect)
luna state -n 3000000 --force-mapper lorom --out - "WaveHDMA.sfc" \
  | jq '.dma.channels[0] | {bbad, a_addr, ntlr}'
```

---

## 3. Scripted joypad input (`--input`)

Shared by `state`, `frames`, `wram-trace`, `bench`. Format:
comma-separated `frame:hex` checkpoints — frame number in decimal, mask
in hex (optional `0x`). The mask is latched at the **start** of the named
PPU frame and held until the next checkpoint overrides it.

```
--input "100:0x1000,110:0"   # hold Start for frames 100..=109, then release
```

`--input` also accepts **`@<file>`** to read the script from a file, and the
grammar allows `#` comments and newlines — so a recording exported from
`luna-gui` (*Emulation ▸ ● Record input*) or the MCP `take_input_capture`
tool replays straight back:

```bash
luna state -n 60000000 --input @gameplay.input "game.sfc"
```

**JOY1 bit layout:** `B(15) Y(14) Select(13) Start(12) Up(11) Down(10)
Left(9) Right(8) A(7) X(6) L(5) R(4)`. So Start = `$1000`, A = `$80`.

> Most commercial titles sit at a title/demo screen waiting for Start —
> a black/forced-blank screenshot with no input is **not** a bug. Pulse
> Start to get past it.

### Pointer devices (Mouse / Super Scope)

A port can hold a **Mouse** or **Super Scope** instead of a pad. Select the
device with `--port1`/`--port2` (`pad` · `mouse` · `superscope`), then script
its motion:

```
# Super Scope on port 2, fire at screen pixel (128, 112) on frame 120
--port2 superscope --superscope "120:128,112,1"

# Mouse on port 1: move +5/-3 and press the left button on frame 60
--port1 mouse --mouse "60:5,-3,1"
```

`--mouse` takes signed `dx,dy` motion (`buttons` bit0 = left, bit1 = right);
`--superscope` takes absolute screen `x,y` pixels (`buttons` bit0 = trigger,
bit1 = cursor, bit2 = turbo, bit3 = pause). In the GUI these map to the host
mouse cursor automatically once a port is set to the device under
**Settings → Devices**.

---

## 4. MCP tool catalogue (`luna mcp`)

Each tool is a thin wrapper over the matching `luna_api::Emulator`
method, so the MCP transport adds reach, not capability.

| Tool | Maps to | Purpose |
|---|---|---|
| `load_rom` | `load_rom` | Load a `.sfc`/`.smc` from a host path. |
| `reset` | `reset` | Reset to power-on state. |
| `set_joypad` | `set_joypad` | Set the button bitmask for `port` (0 = P1, 1 = P2). |
| `set_mouse` | `set_mouse` | Feed SNES Mouse `dx`/`dy`/buttons for the next auto-read. |
| `set_superscope` | `set_superscope` | Feed Super Scope aim (`x`, `y`) + buttons. |
| `step` | `step` | Step `count` instructions (stops early if the CPU halts). |
| `step_until_frame` | `step_until_frame` | Run until one PPU frame completes (bounded). |
| `run_until_pc` | `run_until_pc` | Step until PB:PC hits a 24-bit target (bounded). |
| `run_until_mem_write` | `run_until_mem_write` | Step until an address is written; returns PC + value. |
| `run_until_mem_read` | `run_until_mem_read` | Step until an address is read; returns PC + value. |
| `state` | `state` | Full observable-state JSON snapshot (§2). |
| `screenshot` | `render_frame_png` | Render the 256×224 composited framebuffer to PNG. |
| `drain_audio` | `drain_audio` | Drain up to `max` stereo samples from the APU. |
| `peek_memory` | `peek_memory` | Read `count` bytes from the CPU bus at `bank:offset`. |
| `peek_aram` | `peek_aram` | Read `count` bytes from the SPC700's 64 KB ARAM. |
| `peek_vram` | `peek_vram` | Read `count` bytes from the 64 KB VRAM. |
| `peek_cgram` | `peek_cgram` | All 256 CGRAM palette entries as BGR555 words. |
| `poke_memory` | `poke_memory` | Write bytes into WRAM (state injection). |
| `search_memory` | `search_memory` | Find a byte pattern in `$7E-$7F` WRAM. |
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
| `enable_mem_trace` / `take_mem_trace` | `enable_mem_trace` / `take_mem_trace_log` | Per-bus-access trace with bank/offset-range filters. |
| `bp_add` | `bp_add_exec` / `bp_add_mem` | Register an exec breakpoint or a read/write watchpoint range. |
| `bp_remove` / `bp_clear_all` / `bp_list` | `bp_remove` / `bp_clear` / `bp_list` | Manage the breakpoint registry. |
| `run_until_break` | `run_until_break` | Run at full speed until a breakpoint fires (or a step budget). |
| `run` / `pause` | `run_until_break_interruptible` | Unbounded interruptible run: `run` goes until a breakpoint / `STOP` / `pause`; `pause` stops it (returns `interrupted: true`). No mandatory step budget. |
| `peek_oam` | `peek_oam` | All 544 OAM bytes (512 low table + 32 high table). |
| `capabilities` | — | luna `version` + the live tool catalogue, for client feature-detection (the handshake `serverInfo.version` is rmcp's, not luna's). |
| `start_input_capture` / `take_input_capture` | `start_input_capture` / `take_input_capture` | Record joypad changes and export a `frame:mask` script (replay with `--input @file`). |
| `load_symbols` | `load_symbols` | Load a WLA-DX `.sym`; disasm + traces become annotated. |
| `resolve_symbol` | `resolve_symbol` | Label name → 24-bit address. |
| `enable_nocash_log` / `take_nocash_log` | `enable_nocash_log` / `take_nocash_log` | The `$21FC` Nocash TTY (`SNES_NOCASH` text): drain returns `{text, base64}`. |
| `enable_wdm_log` / `take_wdm_log` | `enable_wdm_log` / `take_wdm_log` | The `WDM` assert channel (`SNES_ASSERT` → `WDM $00`): drain returns `[{pc, operand, symbol}]`. |

With a symbol table loaded, the address-taking tools (`peek_memory`,
`poke_memory`, `run_until_pc`, `run_until_mem_*`, `bp_add`) also accept a
`symbol` name in place of the numeric address — e.g.
`peek_memory {symbol: "monster_x", count: 2}`.

#### Reading the SDK assert/log channels over MCP

An agent debugging an SDK-built ROM watches the two debug channels the
same way the CLI's `--nocash-out` / `--wdm-out` flags do — enable, run,
drain:

```text
enable_nocash_log {}   # $21FC TTY — SNES_NOCASH("...") text output
enable_wdm_log {}      # WDM $42 — SNES_ASSERT "fired here" events
run {}                 # or step / step_until_frame / run_until_break
pause {}
take_nocash_log {}     # → {text: "hello\n", base64: "aGVsbG8K"}
take_wdm_log {}        # → {events: [{pc: 32779, operand: 0, symbol: "assert_fail+0x02"}]}
```

An empty `take_wdm_log` after a run is the "no assertions fired" green
light a CI-style probe wants; the Nocash text is the ROM's own printf
channel. Draining resets each channel, so successive takes return only
new output.

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
- `render_tilemap_rgba(bg_idx)` → `TilemapImage`, `render_tilemap_png(bg_idx)`
- `render_vram_tiles_png(bpp, palette_row)`, `render_palette_png(cell)`, `render_sprite_sheet_png()`
- `bg_bpp(bg_idx)` → 2/4/8 (0 if disabled), `decode_sprites()` → `Vec<SpriteInfo>`

**Save-states & export**
- `save_state()` → bytes, `load_state(bytes)`
- `export_spc()` → a 66 048-byte `.spc` sound file (SPC700 regs + ARAM + DSP regs + IPL ROM)

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

- **GUI keyboard bindings + hotkeys:** see the Controls chapter.
- **Coprocessor firmware (DSP-1, …):** install via
  `luna state --dsp1-rom <path>` or `Emulator::install_firmware`.
