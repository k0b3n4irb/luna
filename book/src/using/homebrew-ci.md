# Developing homebrew with luna — `luna test`

Homebrew developers have had no serious CI story: the classic loop is
"build, open an emulator, eyeball it". `luna test` (issue #181) turns
that into a declarative suite a pipeline can run in seconds — one TOML
manifest per test, executed in-process against the same `luna-api`
surface the GUI and MCP use, with the CLI's exit-code contract:

| Exit | Meaning |
|---|---|
| `0` | Every manifest's asserts passed. |
| `1` | At least one assert failed. |
| `2` | Manifest / usage error (bad TOML, missing ROM, no manifests found). |

```
luna test [PATHS...] [--update] [--only SUBSTR] [--report json]
```

`PATHS` are manifest files, or directories scanned recursively for
`*.toml` (default: `./tests`).

## The manifest

```toml
# tests/boot.toml — "the game boots and reports ready"
rom = "../build/game.sfc"      # relative to this manifest
sym = "../build/game.sym"      # optional (a beside-ROM .sym auto-loads)
force_mapper = "lorom"         # optional — headerless/WIP images
frames = 600                   # run bound: `frames` or `steps` (or checkpoints)
input = "300:0x1000,310:0"     # optional joypad script, or "@inputs/boot.txt"
screenshot = "artifacts/boot.png"  # optional artifact, written after the run

[asserts]
wdm_empty = true               # SNES_ASSERT never fired (WDM channel silent)
nocash_contains = "BOOT OK"    # the $21FC TTY printed this
fbhash = "7429bf441a1c7d6c"    # displayed-frame hash — see below
audio_rms_min = 100.0          # the music is audibly playing

[asserts.values]               # loaded symbol (or "BANK:OFFSET") = expected
r_game_state = 0x02            # bare int = eq; ≤ 0xFF checks one byte…
r_score = { ge = 0x1000 }      # …and tables give ge/gt/le/lt/ne thresholds

[asserts.blocks]               # byte-range equality, any memory space
"0000" = { space = "vram", hex = "7cc6cede..." }

[asserts.trace]                # coprocessor liveness
superfx = { min = 1 }
```

What each assert means:

- **`wdm_empty`** — the SDK's `SNES_ASSERT` macro executes `WDM $00`;
  an empty log after the run is the "no assertions fired" green light.
- **`nocash_contains`** — the `$21FC` Nocash TTY is the ROM's printf
  channel (`SNES_NOCASH("...")`); assert on any marker text it prints.
- **`fbhash`** — the 64-bit displayed-frame hash (the same
  cross-arch-stable value `luna state --print-fbhash` emits — *not*
  the golden suite's SHA-256). After an **intended** render change, run
  `luna test --update` to regenerate every manifest's `fbhash` in
  place; formatting and comments are preserved.
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

  With an explicit `offset`, the key becomes a **free label** — so two
  spaces at the same offset can share a manifest (#210):

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

`at_frame` values must increase; `steps` cannot be combined with
checkpoints (use `frames`, which may extend past the last checkpoint —
with checkpoints alone, the last one ends the run). The final
`[asserts]` block still evaluates at the very end.

## The final capabilities (#212)

- **Peripheral input** — top-level or per-checkpoint `mouse =
  "frame:dx,dy,buttons"` (`;`-separated, the `--mouse` grammar; plugs a
  SNES Mouse into port 1) and `superscope = "frame:x,y,buttons"`
  (port 2). Mix freely with joypad `input`.
- **`[asserts.dsp]`** — the S-DSP register file, by name (`FLG`, `EDL`,
  `KON`, `MVOL_L`, `V0_VOLL`…`V7_GAIN`, `FIR0`…`FIR7`) or raw hex index
  (`"7D"`), with the `[asserts.values]` comparator grammar (registers
  are bytes).
- **`[asserts.footprint]`** — `vram = { nonzero_min = 5000 }`: at least
  N non-zero bytes in `wram`/`vram`/`cgram`/`oam`/`aram` — proof an
  upload happened without pinning exact bytes.
- **`[asserts.dma]`** — DMA-discipline ceilings from the trace luna
  records: `unsafe_writes = 0` (max DMA→VRAM bytes written outside
  VBlank/forced-blank — the writes real hardware drops) and
  `max_vblank_bytes = 4096` (max bytes in any single frame's burst).
- **Battery SRAM round-trip** — `srm_out = "save.srm"` writes SRAM
  after the run; a later manifest (sorted order!) reloads it with
  `srm_in` and asserts the value persisted:

  ```toml
  # a_write.toml: play, then persist    # b_read.toml: power-cycle
  srm_out = "save.srm"                  # srm_in = "save.srm"
                                        # [asserts.values]
                                        # "70:0000" = 0x5A
  ```

- **`firmware = "dsp1b.rom"`** — SKIP (not fail) when the named blob is
  absent from luna's firmware folder, so a DSP-1 test stays green in CI
  where Sony firmware can't ship. Skips print `SKIP <name> (reason)`,
  count separately, and never affect the exit code.

Input scripts use exactly the `--input` grammar (`frame:mask`, `#`
comments, `@file`), so a recording exported from the GUI or captured
over MCP (`take_input_capture`) replays verbatim. Checkpoints spend
from the same budget as the run bound (issue #126 semantics).

## A GitHub Actions recipe

Copy this into a homebrew repo — it builds the ROM, fetches a pinned
luna release binary (no Rust toolchain), and runs the suite:

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
            https://github.com/k0b3n4irb/luna/releases/latest/download/luna-linux-amd64.tar.gz
          tar xzf luna.tar.gz && sudo install luna /usr/local/bin/
      - name: Run the test suite
        run: luna test tests --report json
      - name: Upload screenshots
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: luna-artifacts
          path: tests/artifacts/
```

`--report json` appends a machine-readable summary (per-test pass/fail,
failure details, measured `fbhash`) to stdout for dashboards or PR
comments.

## Tips

- Keep one manifest per behaviour ("boots", "menu reachable", "level 1
  completable") — `--only level1` runs a subset while iterating.
- A **black screenshot is not a failed test**: commercial-style intros
  sit in forced blank waiting for Start. Drive them with `input` (the
  same lesson as the smoke-test corpus).
- For deeper debugging of a failing test, replay the same ROM + input
  under `luna state` with traces, or over MCP with the interactive
  tools — every assert here reads the same `luna-api` state they do.
