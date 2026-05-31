# External test corpora

luna is validated against two large external test sets. Neither is
vendored into this repo — both are fetched on demand and the tests are
opt-in / skip-if-absent, so a plain `cargo test` works without them.

## 1. Tom Harte / SingleStepTests — CPU instruction semantics

Exhaustive single-instruction state-transition tests (≈10k cases per
opcode) for both CPU cores.

| Core | Source | Harness | Fetch |
|---|---|---|---|
| 65C816 | [SingleStepTests/65816](https://github.com/SingleStepTests/65816) (= [TomHarte/ProcessorTests](https://github.com/TomHarte/ProcessorTests) `/65816`) | `crates/luna-cpu-65c816/tests/tom_harte.rs` | `tools/fetch-tom-harte.sh` → `tests/tom-harte/v1` |
| SPC700 | [SingleStepTests/spc700](https://github.com/SingleStepTests/spc700) (= ProcessorTests `/spc700`) | `crates/luna-cpu-spc700/tests/tom_harte.rs` | `tools/fetch-tom-harte-spc700.sh` → `tests/tom-harte-spc700/v1` |

Both are `#[ignore]`d (large dataset, ~8 min for the 65C816). Run:

```bash
tools/fetch-tom-harte.sh
tools/fetch-tom-harte-spc700.sh
LUNA_TOM_HARTE_REQUIRE=1 cargo test -p luna-cpu-65c816 --test tom_harte -- --ignored
LUNA_TOM_HARTE_REQUIRE=1 cargo test -p luna-cpu-spc700 --test tom_harte -- --ignored
```

`LUNA_TOM_HARTE_REQUIRE=1` turns any mismatch into a hard failure
(otherwise the test just prints a report). Current state: **both pass
100%** (65C816 5,080,000/5,080,000; SPC700 256,000/256,000). CI gates
them in the `tom-harte` job. The datasets are gitignored.

## 2. Peter Lemon SNES — full-system golden display tests

End-to-end hardware-test ROMs that exercise the whole emulator (CPU +
PPU + bus). Following the [`twvd/siena`](https://github.com/twvd/siena)
convention, the ROM corpus is **not vendored** and is checked out **at
the same directory level** as this repo (a sibling), then referenced
from there:

```
<parent>/
├── luna/          ← this repo
└── luna_tests/    ← the sibling corpus  (../luna_tests)
```

Source: [PeterLemon/SNES](https://github.com/PeterLemon/SNES) — sparse
clone of only the test-relevant subdirs (`CPUTest`, `PPU`; not the
multi-GB whole repo). Harness: `crates/luna-core/tests/snes_test_roms.rs`.
Each test boots a ROM with a forced LoROM mapper
(`Cartridge::from_bytes_forced` — these homebrew ROMs have no valid
header checksum), runs it until the 256×224 framebuffer settles (or a
fixed instruction cap for continuously-animated scenes — deterministic
by instruction count), and asserts a SHA-256 of the framebuffer against a
committed golden hash.

Coverage:

- **`CPUTest/CPU/*`** (23): every opcode-group result screen (ADC … TRN),
  each an all-PASS table.
- **`PPU/*`** (13, the twvd/siena selection): BG maps (2BPP BG1-4, 4BPP),
  hi-colour blend (`HiColor*`), windows (`WindowHDMA`, `WindowMultiHDMA`),
  and Mode 7 (`RotZoom`, `Perspective`, `Rings`). Each luna render was
  eyeballed against the bundled reference `*.png` before blessing.
- **`SPC700/*`** (9): audio ROMs — these play music / sounds rather than
  draw a screen, so they assert a SHA-256 of the APU's **32 kHz PCM
  output** (first 3 s) instead of the framebuffer (`test_audio` /
  `spc_test!`). 8 play (auditioned by ear before blessing); `PlayTwoSong`
  is `known_silent` (the 65816 never starts the upload — a separate open
  gap). `LUNA_SNES_TEST_PNG=<dir>` in record mode dumps a `.wav` to
  audition. NB: these surfaced the IPL-ROM multi-block bug (the `$FFEE`
  byte) — see `luna_spc700_gaps.md`.

```bash
tools/fetch-snes-test-roms.sh                  # sparse-clone → ../luna_tests
cargo test -p luna-core --test snes_test_roms
```

Or point `LUNA_SNES_TEST_DIR` at a corpus root. If the corpus is absent
the tests skip with a notice and pass. CI runs them in the
`snes-test-roms` job.

All 23 CPUTest ROMs render the correct all-PASS result screen and are
committed as golden tests.

### Loaded as PAL

The harness forces **`Region::Pal`** (`run_to_stable`), matching the
`twvd/siena` convention. Peter Lemon's suite is PAL-timed: some tests
(BRA, JMP, PSR, RET) do a single `WaitNMI` then write the entire result
table in one burst that only fits inside PAL's longer V-blank (~72 lines
vs NTSC's 37). Run as NTSC, luna *correctly* drops the writes that
overflow into active display (`active_display` gating, ares-accurate) and
those screens stay blank. The other 19 re-sync to V-blank per row (e.g.
ADC issues 33 `WaitNMI`s) and render on any region. luna's NTSC timing is
correct — these ROMs simply target PAL, so the suite loads them as PAL to
reproduce the reference output.

### Golden hashes are luna's own output

Unlike the Tom Harte vectors (hardware truth), these hashes are captured
from **luna's renderer**, so they are **regression baselines**, not an
independent correctness oracle. Each ROM ships a reference `CPU<NAME>.png`
(real-hardware output) — eyeball luna's render against it when blessing a
baseline (verified: e.g. BRA shows the full BCC…BRL PASS table). Regenerate
after an intended render change:

```bash
LUNA_SNES_TEST_RECORD=1 LUNA_SNES_TEST_PNG=/tmp/snes \
  cargo test -p luna-core --test snes_test_roms -- --nocapture
```
