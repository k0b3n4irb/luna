# Developing homebrew with luna — `luna test`

Homebrew developers have had no serious CI story: the classic loop is
"build, open an emulator, eyeball it". `luna test` turns
that into a declarative suite a pipeline can run in seconds — one TOML
manifest per test, executed in-process against the same `luna-api`
surface the GUI and MCP use, with the CLI's exit-code contract:

| Exit | Meaning |
|---|---|
| `0` | Every manifest's asserts passed. |
| `1` | At least one assert failed. |
| `2` | Manifest / usage error (bad TOML, missing ROM, no manifests found). |

```
luna test [PATHS...] [--update] [--only SUBSTR] [--report json] [--jobs N]
```

`PATHS` are manifest files, or directories scanned recursively for
`*.toml` (default: `./tests`).

**`--jobs N`** runs up to N manifests at once, each on its own emulator
(`0` = one per CPU; the default `1` is serial). The report is unchanged:
the same `PASS` / `FAIL` lines in the same order, the same `--report json`,
the same exit code. A corpus of 121 manifests takes 73 s serially and 16 s
with `--jobs 0` on six cores:

```bash
luna test --jobs 0 tests/
```

Only stderr (symbol-loading notes, warnings) interleaves across manifests.
A malformed manifest still exits 2 with its error, but in parallel the
other manifests have already run by then.

**A power-cycle pair keeps its order.** A manifest whose `srm_in` is another
one's `srm_out` (or two that write the same `srm_out`) runs after it, in
path order, exactly as a serial run would; `luna test` finds these chains
before starting and runs each one as a unit, in parallel with the rest.
Paths are compared by where the file lands, so `save.srm` and
`./sub/../save.srm` are the same file. Nothing else is ordered between
manifests: two that talk through anything but a battery file — a file a
script writes, a shared screenshot path — should not rely on `--jobs`.

## The manifest

```toml
# tests/boot.toml — "the game boots and reports ready"
rom = "../build/game.sfc"      # relative to this manifest
power_on = "random"            # optional: zero (default) | ones | random
seed = 1                       # optional: fixes the random machine (default 1)
sym = "../build/game.sym"      # optional (a beside-ROM .sym auto-loads)
force_mapper = "lorom"         # optional — headerless/WIP images
region = "pal"                 # optional: ntsc | pal — overrides the header
frames = 600                   # run bound: `frames` or `steps` (or checkpoints)
input = "300:0x1000,310:0"     # optional joypad script, or "@inputs/boot.txt"
input2 = "300:0x0080"          # optional joypad-2 script, same grammar
screenshot = "artifacts/boot.png"  # optional artifact, written after the run

[asserts]
wdm_empty = true               # SNES_ASSERT never fired (WDM channel silent)
nocash_contains = "BOOT OK"    # the $21FC TTY printed this
fbhash = "7429bf441a1c7d6c"    # displayed-frame hash — see below
audio_rms_min = 100.0          # the music is audibly playing

[asserts.values]               # loaded symbol (or "BANK:OFFSET") = expected
r_game_state = 0x02            # bare int = eq; ≤ 0xFF checks one byte…
r_score = { ge = 0x1000 }      # …and tables give ge/gt/le/lt/ne thresholds
"results+16" = 0xFFFF          # symbol+N: N bytes in — decimal, 0x for hex

[asserts.blocks]               # byte-range equality, any memory space
"0000" = { space = "vram", hex = "7cc6cede..." }

[asserts.trace]                # a trace recorded at least `min` events
dma = { min = 1 }              # the boot uploaded something to VRAM by DMA

[asserts.ppu]                  # PPU registers, named as `luna state` prints them
inidisp = 0x0F                 # the screen is on at full brightness
bgmode = 5                     # …and the mode the example claims to demo
"windows.0" = 0x20             # `.` indexes arrays and nested tables
```

A Super FX cartridge adds one more table, `[asserts.gsu]`, with the same
vocabulary idea (the field names `luna state` prints under `gsu`). Keep it
out of a manifest for any other cartridge: with no GSU on board the table
fails instead of passing vacuously.

```toml
# tests/render.toml — a Super FX game: "the GSU ran, and nothing read under it"
rom = "../build/fxgame.sfc"
frames = 300

[asserts.trace]
superfx = { min = 1 }          # the GSU executed at least one opcode

[asserts.gsu]                  # Super FX state, named as `luna state` prints it
bus_violations = 0             # the CPU never read the cartridge while the GSU held it
instructions_executed = { gt = 10000 }
```

**Which console the ROM runs on** is set by three optional keys, all in
the manifest so a run never depends on a command-line flag:

- **`region`** — `"ntsc"` or `"pal"`, the video standard, whatever the
  cartridge header's country byte says (the CLI's `--force-region`; the
  older spelling `force_region` still works). It changes what the ROM can
  observe: 262 or 312 scanlines, 60 or 50 frames a second, and the PAL
  flag in `$213F`. Use it to check that a game behaves on the other
  standard **without building it twice**, including the case no header
  can express: an NTSC cartridge on a PAL console.
- **`power_on`** / **`seed`** — what RAM holds before the ROM boots.
- **`force_mapper`** — for headerless or work-in-progress images.

```toml
# tests/boot_pal.toml — the same ROM, on a PAL console
rom = "../build/game.sfc"
region = "pal"
frames = 500                   # 10 s at 50 Hz

[asserts]
wdm_empty = true

[asserts.ppu]
stat78 = 0x13                  # $213F: bit 4 set, the console is PAL
```

What each assert means:

- **`wdm_empty`** — the SDK's `SNES_ASSERT` macro executes `WDM $00`;
  an empty log after the run is the "no assertions fired" green light.
- **`nocash_contains`** — the `$21FC` Nocash TTY is the ROM's printf
  channel (`SNES_NOCASH("...")`); assert on any marker text it prints.
- **`fbhash`** — the 64-bit displayed-frame hash (the same value
  `luna state --print-fbhash` emits — *not* the golden suite's SHA-256).
  Since v1.21.0 it is **fbhash v2**: FNV-1a 64 over the raw RGBA bytes,
  a pinned function that is stable across toolchains and architectures by
  construction (v1 used the standard library's hasher, which is not
  guaranteed to stay the same between Rust releases). After an
  **intended** render change — or when moving a corpus from a pre-v1.21.0
  luna — run `luna test --update` to regenerate every manifest's `fbhash`
  in place; formatting and comments are preserved.
- **`[asserts.values]`** — read memory through the loaded symbol table
  (or a literal `"7E:0100"` hex pair) and compare. A bare integer means
  `eq`; a table gives comparators — any of `eq`/`ne`/`ge`/`gt`/`le`/`lt`
  plus an optional `width = 1|2` (default: 1 byte if every bound fits,
  else a little-endian u16):

  ```toml
  [asserts.values]
  r_lives = 3                      # exact
  r_score = { ge = 0x1000 }        # threshold
  r_timer = { gt = 0, le = 0x63, width = 1 }
  ```

- **`audio_rms_min`** — RMS over the drained sample ring must reach
  this floor: the "music is actually playing" oracle. The ring holds
  the most recent audio, so this asserts on the state at the end of
  the run.
- **`[asserts.blocks]`** — arbitrary-length byte-range equality in any
  space. A bare hex string reads the CPU bus, and so does
  `space = "wram"` — both use **symbol or `BANK:OFFSET` keys** (a bare
  hex offset is only valid for `vram`/`cgram`/`oam`/`aram`, whose keys
  are 16-bit offsets). Failures report the first mismatching offset.
- **Keys relative to a symbol** — anywhere a symbol or `BANK:OFFSET` key
  is accepted (`values`, checkpoint `values`, `blocks`), `symbol+N` and
  `symbol-N` name an address relative to it: `"results+16"` is sixteen
  bytes into `results`. Name an array element this way and it survives the
  array moving in RAM; a raw address breaks the moment the linker shifts
  anything above it.

  `N` is **decimal** unless prefixed `0x` or `$`, as an offset reads in
  assembly. That differs on purpose from `BANK:OFFSET` and from the
  `--peek` count, which are hex: `+16` quietly meaning twenty-two bytes
  would be a trap nobody would suspect. `BANK:OFFSET+N` works too.

  With an explicit `offset`, the key becomes a **free label** — so two
  spaces at the same offset can share a manifest:

  ```toml
  [asserts.blocks]
  font_tiles = "7cc6ce...00"                    # symbol key, CPU bus
  "0000" = { space = "vram", hex = "7cc6ce" }   # key-as-offset form
  font = { space = "vram",  offset = "0000", hex = "7cc6ce" }  # labelled
  pal  = { space = "cgram", offset = "0000", hex = "0028ff7f" }
  ```

- **`[asserts.trace]`** — the named trace recorded at least `min`
  events: `dma`, `dsp` (S-DSP writes), `mailbox`, `sa1`, `superfx`,
  `dsp1`, `spc`. `superfx = { min = 1 }` is the "the GSU actually ran"
  liveness check.

## Checkpoints — before/after assertions

`[[checkpoint]]` tables measure *along* the run, in order: each leg
runs to its `at_frame` (applying that leg's `input` entries), then
evaluates its `values` and `delta` asserts. `delta` compares against
the previous checkpoint (the run start for the first one):

```toml
# "pressing RIGHT moves the player right"
rom = "../build/game.sfc"
force_mapper = "lorom"

[[checkpoint]]                 # settle: establish the baseline
at_frame = 60

[[checkpoint]]                 # press RIGHT for 3 frames
at_frame = 90
input = "62:0x0100,65:0"
[checkpoint.delta]
xloc = "increased"             # increased | decreased | changed | unchanged
yloc = "unchanged"
r_mode = { dir = "unchanged", width = 1 }
```

A `delta` entry compares a **little-endian u16** unless it says
`width = 1` — unlike `[asserts.values]`, which reads one byte when the
expected value fits in one. A counter that is a single byte next to an
unrelated one needs the table form, as `r_mode` above.

`at_frame` values must increase; `steps` cannot be combined with
checkpoints (use `frames`, which may extend past the last checkpoint —
with checkpoints alone, the last one ends the run). The final
`[asserts]` block still evaluates at the very end.

## More asserts and inputs

- **Peripheral input** — top-level or per-checkpoint `mouse =
  "frame:dx,dy,buttons"` (`;`-separated, the `--mouse` grammar; plugs a
  SNES Mouse into port 1) and `superscope = "frame:x,y,buttons"`
  (port 2). Mix freely with joypad `input`.
- **Port devices** — `port1` / `port2` take the `--port1` vocabulary
  (`pad`, `mouse`, `superscope`, `multitap`, `none`). Left unset, a port
  keeps the inference above; set, it wins, which is the only way a
  manifest can **unplug** a port — the case a "is a controller
  connected?" routine needs:

  ```toml
  port1 = "none"          # nothing in either port
  port2 = "none"
  [asserts.values]
  pad_connected = 0
  ```

  A script whose device no port carries is a manifest error (exit 2)
  rather than a silent pass: `port1 = "none"` beside a `mouse` script would
  otherwise feed the mouse input into nothing. The scripts follow their
  device, so `port2 = "mouse"` with a `mouse` script is fine.
- **Joypad 2** — `input2` beside `input`, top-level or per leg, same
  `frame:hex` grammar: the second half of a two-player probe, or the
  replay of an MCP capture's `script_p2`.

  ```toml
  [[checkpoint]]                 # P2 joins: pad 2 presses Start
  at_frame = 120
  input2 = "100:0x1000,105:0"
  [checkpoint.values]
  players = 2
  ```
- **`[asserts.dsp]`** — the S-DSP register file, by name (`FLG`, `EDL`,
  `KON`, `MVOL_L`, `V0_VOLL`…`V7_GAIN`, the per-voice read-backs
  `V0_ENVX`…`V7_ENVX` / `V0_OUTX`…`V7_OUTX`, `FIR0`…`FIR7`) or raw hex
  index (`"7D"`). The names the `--dsp-trace` CSV prints are accepted as
  they are (`V3_PL` is `V3_PITCHL`, `KOFF` is `KOF`, `MVOLL` is
  `MVOL_L`), so a register can be copied from a trace. Asserts use the
  `[asserts.values]` comparator grammar (registers are bytes). ENVX is the voice's envelope, OUTX its last
  output — the pair that answers "is this voice actually sounding?".
- **`[asserts.ppu]`** — the PPU registers, keyed by the field names
  `luna state --out -` prints under `ppu`: `inidisp`, `bgmode`, `tm`,
  `ts`, `tmw`, `tsw`, `w12sel`, `w34sel`, `wobjsel`, `wbglog`, `cgwsel`,
  `cgadsub`, `coldata_r/g/b`, `mosaic`, `setini`, `obsel`, `m7sel`,
  `m7a`…`m7d`, `m7x`, `m7y`, and the counts. A `.` steps into the arrays
  and tables the same JSON prints — `windows.0` is WH0,
  `bgs.1.h_scroll` BG2's scroll, `cgram.16` palette 1's colour 0. The
  vocabulary **is** the state JSON, so it cannot drift from what the
  runner can observe; values are compared with the `[asserts.values]`
  grammar and may be signed (`m7a = -256`).

  This is the handle for an example whose only frame-boundary
  observable is a register: an HDMA gradient that rewrites the backdrop
  and INIDISP per scanline, a Mode-7 matrix written straight to the
  registers with no RAM shadow, or windows programmed as raw `$2123`
  writes — where asserting the library's shadow would assert your own
  bookkeeping rather than the PPU.

  ```toml
  # "the gradient really is running with the screen on"
  [asserts.ppu]
  inidisp = 0x0F
  "bgs.0.tilemap_addr_words" = 16384

  [[checkpoint]]               # per-leg too
  at_frame = 120
  [checkpoint.ppu]
  m7a = -256
  ```

  `[checkpoint.gsu]` is the same per-leg form for the Super FX table
  below.
- **`[asserts.gsu]`** — the Super FX, keyed by the field names
  `luna state --out -` prints under `gsu`, same grammar and the same `.`
  into arrays: `running`, `pbr`, `cbr`, `scbr`, `colr`, `por`, `clsr`,
  `instructions_executed`, `r.15` for the GSU program counter, and the
  two that say who owns the cartridge, `scmr_ron` / `scmr_ran`.

  `SCMR` is given decoded as well as raw because its wire layout is
  scrambled; assert on `scmr_ron`, not on a bit of `scmr`.

  A cartridge with no GSU **fails** an `[asserts.gsu]` table rather than
  passing it vacuously — an assert that cannot fail is worse than none.

  ```toml
  # "the renderer ran, and the CPU never read the cartridge under it"
  [asserts.gsu]
  instructions_executed = { gt = 10000 }
  bus_violations = 0

  [[checkpoint]]               # per-leg: the GSU has started by frame 200
  at_frame = 200
  [checkpoint.gsu]
  instructions_executed = { gt = 0 }
  ```

  `bus_violations` counts CPU reads that got a dummy byte or open bus
  because the GSU held the bus. Vector-page fetches are counted separately
  (`bus_vector_fetches`) and are *not* faults — the busy vector is shaped
  so they land on `$0108` / `$010C` in WRAM, which is how Super FX titles
  take interrupts during a job. When a violation does appear,
  `luna state --gsu-bus-trace` names the instruction.
- **`[asserts.footprint]`** — `vram = { nonzero_min = 5000 }`: at least
  N non-zero bytes in `wram`/`vram`/`cgram`/`oam`/`aram` — proof an
  upload happened without pinning exact bytes.
- **`[asserts.dma]`** — DMA-discipline ceilings from the trace luna
  records: `unsafe_writes = 0` (max DMA→VRAM bytes written during
  active display — the writes real hardware drops) and
  `max_vblank_bytes = 4096` (max screen-on VRAM bytes in any single
  frame's burst window). Both count only the VRAM data ports
  (`$2118`/`$2119`) — OAM/CGRAM/scroll-register (H)DMA writes never
  race the VRAM deadline and are ignored — and forced-blank bytes are
  excluded from `max_vblank_bytes`: under forced blank there is no
  VBlank deadline, which is exactly why big boot uploads use it. A
  failing `unsafe_writes` names the first offending write (frame,
  line, channel, VRAM word, source address). The counts are kept frame
  by frame as the run goes, not from a stored trace, so a run of any
  length fits.
- **`[asserts.oam]`** — decoded sprite structure, no raw-OAM golden
  needed: `visible = 1` counts on-screen sprites (the standard
  predicate `0 <= y < 224`, `-32 < x < 256`; comparator tables like
  `{ ge = 1 }` work), and `[asserts.oam.sprites.N]` (hardware OAM
  index 0-127) asserts decoded fields — `x`, `y`, `tile`, `palette`,
  `priority`, `w`, `h` with the comparator grammar, `hflip`/`vflip`
  as booleans:

  ```toml
  [asserts.oam]
  visible = 1

  [asserts.oam.sprites.0]
  x = 112
  y = 95
  tile = 16
  priority = 3
  w = 32
  h = 32
  ```

- **Battery SRAM round-trip** — `srm_out = "save.srm"` writes SRAM
  after the run; a later manifest (sorted order!) reloads it with
  `srm_in` and asserts the value persisted:

  ```toml
  # a_write.toml: play, then persist    # b_read.toml: power-cycle
  srm_out = "save.srm"                  # srm_in = "save.srm"
                                        # [asserts.values]
                                        # "70:0000" = 0x5A
  ```

  What the `.srm` holds depends on the board, as on a real cartridge: the
  SRAM of a LoROM / HiROM / S-DD1 / DSP-1 cart; the part of BW-RAM an SA-1
  header declares; the **whole GSU work RAM** of a Super FX cart whose
  header has a battery (chipset byte `$15` or `$1A`, e.g. Yoshi's Island,
  Stunt Race FX). A board without a battery (Star Fox `$13`, Doom `$14`)
  has no save: `srm_out` writes an empty file and `srm_in` is ignored.

- **`firmware = "dsp1b.rom"`** — SKIP (not fail) when the named blob is
  absent from luna's firmware folder, so a DSP-1 test stays green in CI
  where Sony firmware can't ship. Skips print `SKIP <name> (reason)`,
  count separately, and never affect the exit code.

Input scripts use exactly the `--input` grammar (`frame:mask`, `#`
comments, `@file`), so a recording exported from the GUI or captured
over MCP (`take_input_capture`) replays verbatim. Script entries
spend from the run bound: under `steps = N` the run is exactly N
instructions long, and an entry whose frame it never reaches does not
fire.

## A GitHub Actions recipe

Copy this into a homebrew repo — it builds the ROM, fetches the latest
luna release binary (no Rust toolchain), and runs the suite. To pin a
version instead, swap `latest/download/luna-linux-x86_64.tar.gz` for
`download/vX.Y.Z/luna-vX.Y.Z-linux-x86_64.tar.gz` (the folder inside is
then `luna-vX.Y.Z-linux-x86_64/`). Only the five most recent versions keep
a release with binaries, so a pin has to be moved forward from time to
time; an older version is built from its tag instead (every tag is kept —
see [Installing](install.md)):

```yaml
name: test
on: [push, pull_request]
jobs:
  luna-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build the ROM
        run: make        # your wla-dx build
      - name: Install luna
        run: |
          curl -sL -o luna.tar.gz \
            https://github.com/k0b3n4irb/luna/releases/latest/download/luna-linux-x86_64.tar.gz
          tar xzf luna.tar.gz && sudo install luna-linux-x86_64/luna /usr/local/bin/
      - name: Run the test suite
        run: luna test tests --report json
      - name: Upload screenshots
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: luna-artifacts
          path: tests/artifacts/
```

`--report json` appends a machine-readable summary to stdout, after the
`PASS` / `FAIL` lines, for dashboards or PR comments: the four counters,
then one object per test (pass/fail, failure details, measured `fbhash`,
the `manifest` path, and `skipped` — the skip reason, or `null`). Each
test also carries the machine it ran on — `"power_on":
"random", "seed": 12345` (a deterministic run reports `"zero"` / `"ones"`
with `"seed": null`) — so a red `random` run is reproducible from the
report alone — and the `"region"` its manifest forced (`null` when the
cartridge header decided):

```json
{
  "passed": 0, "failed": 1, "skipped": 0, "total": 1,
  "tests": [
    { "name": "boot", "manifest": "tests/boot.toml", "passed": false,
      "skipped": null, "failures": ["…"], "fbhash": "…",
      "power_on": "random", "seed": 12345, "region": "pal" }
  ]
}
```

## Tips

- Keep one manifest per behaviour ("boots", "menu reachable", "level 1
  completable") — `--only level1` runs a subset while iterating.
- A **black screenshot is not a failed test**: commercial-style intros
  sit in forced blank waiting for Start. Drive them with `input` (the
  same lesson as the smoke-test corpus).
- For deeper debugging of a failing test, replay the same ROM + input
  under `luna state` with traces, or over MCP with the interactive
  tools — every assert here reads the same `luna-api` state they do.
