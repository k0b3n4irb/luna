# External test corpora

luna is validated against two large external test sets — the Tom Harte
single-instruction vectors and the Peter Lemon hardware-test ROMs — plus a
developer-local net of commercial titles (§3). None is vendored into this
repo: the first two are fetched on demand, the third needs your own cartridge
dumps, and every test skips when its input is absent, so a plain `cargo test`
works without them. **A skip is not a pass** — the `*_REQUIRE` variables in
§4 turn a missing input into a failure.

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

`LUNA_TOM_HARTE_REQUIRE=1` turns any state mismatch into a hard failure
(otherwise the test just prints a report). Current state: **both pass
100%** (65C816 5,080,000/5,080,000; SPC700 256,000/256,000). CI runs them
in the `tom-harte` workflow (`.github/workflows/tom-harte.yml`), not in the
main `ci.yml`. The datasets are gitignored. The 65C816 harness also counts
cycles; that comparison only fails the run when `LUNA_TOM_HARTE_CYCLES` is
set as well (§4).

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

Source: [PeterLemon/SNES](https://github.com/PeterLemon/SNES) — a sparse
checkout, pinned to one upstream commit, of only the test-relevant
directories (the `SPARSE_PATHS` list in `tools/fetch-snes-test-roms.sh`:
`CPUTest`, `PPU`, `SPC700`, `INPUT`; not the multi-GB whole repo). Harness:
`crates/luna-core/tests/snes_test_roms.rs`.

Each display test boots a ROM with a forced LoROM mapper
(`Cartridge::from_bytes_forced` — these homebrew ROMs have no valid header
checksum), runs it until the framebuffer (256×224, or 256×239 under
overscan) **settles**, and asserts a SHA-256 of the framebuffer against a
committed golden hash.

### The capture point is a frame, never an instruction count

`run_to_stable` samples the framebuffer hash every `SAMPLE_FRAMES` frames and
stops after `STABLE_SAMPLES` identical samples, or at `FRAME_CAP` for a scene
that animates forever. `STEP_CAP` is only a hang guard. The harness used to
count instructions; it no longer does, for a reason its source spells out: a
ROM that busy-waits on VBlank executes *more* instructions per frame as the
emulation becomes more cycle-accurate, so an instruction-indexed capture
slides backwards through the ROM every time timing improves. The commercial
goldens (`run_game_to_frame`, §3) are frame-anchored for the same reason.
Only the SPC700 ALU runner still counts instructions — it reads a mailbox,
not the screen.

### Coverage

The families are the test macros of the harness; count them with
`grep -c '^<macro>!(' crates/luna-core/tests/snes_test_roms.rs` rather than
trusting a number written here.

- **`cpu_test!` — `CPUTest/CPU/*`**: every opcode-group result screen
  (ADC … TRN), each an all-PASS table.
- **`ppu_test!` — `PPU/*`**: BG maps (`BGMAP/8x8`, 2/4/8 bpp, the four
  tilemap sizes, tile flip), hi-colour blend (`Blend/HiColor*`), the HDMA
  family (`HiColor64/128PerTileRow`, `RedSpace*`, `WaveHDMA`, `Mode7HDMA`),
  windows (`WindowHDMA`, `WindowMultiHDMA`), Mode 7 (`RotZoom`,
  `Perspective`, `StarWars`), `Rings`, `GreenSpace`, the `Interlace/*`
  family (Mode 5 hi-res + interlace) and `Mosaic` (Mode 3 / Mode 5).
  `MosaicMode3` holds **R** so the demo ramps the `$2106` mosaic size.
  `MosaicMode5` is run **without** input, so today it covers Mode 5 hi-res
  + interlace but not the mosaic itself (its hash equals
  `ppu_interlace_moogle`'s) — a known hole, recorded by the 2026-10-04
  audit.
- **`ppu_interlace_font_native_512x448`** — the native 512×448 capture
  path, hashed on its own buffer.
- **`input_controller_latency` — `INPUT/ControllerLatency`**: holds **A**
  and expects the white screen (joypad auto-read end to end).
- **`spc700_test!` — `CPUTest/SPC700/*`** (ADC, AND, DEC, EOR, INC, ORA,
  SBC): these are validated by the ROM's own verdict, not a hash — on the
  first failing case the SPC700 writes `$81` to port 0 (`$2140`) and halts;
  the test asserts that value never appears (`run_spc700_fail_port`). It
  proves "did not fail", not "ran to completion".
- **`spc_test!` — `SPC700/*`** audio ROMs: these play music / sounds rather
  than draw a screen, so they assert a SHA-256 of the APU's **PCM output**
  (the first `AUDIO_SAMPLES` stereo samples, about 3 s) instead of the
  framebuffer (`test_audio`). They were auditioned by ear before blessing.
  One, `spc_pitchmod`, is `#[ignore]`d: its SPC program executes `STOP`
  (Mesen2 halts on it as well; see [`luna_apu_gaps.md`](luna_apu_gaps.md)
  #8 for what luna does after a `STOP`). `PlayTwoSong` only plays on a
  button press, so its test holds **A** until the driver boots, then
  releases (`hold = PAD_A`). NB: these surfaced the IPL-ROM multi-block bug
  (the `$FFEE` byte) — see `luna_spc700_gaps.md`.

```bash
tools/fetch-snes-test-roms.sh                  # sparse checkout → ../luna_tests
cargo test -p luna-core --test snes_test_roms
```

Or point `LUNA_SNES_TEST_DIR` at a corpus root. If the corpus is absent
the tests skip with a notice and pass — unless `LUNA_SNES_TEST_REQUIRE` is
set, which is what CI's `snes-test-roms` job does.

### Region: CPUTest runs as NTSC, everything else as PAL

The region is a per-family decision, passed to `run_to_stable`:

- The **PPU, INPUT and audio** ROMs run as **PAL** (`Region::Pal`), matching
  the `twvd/siena` convention and the corpus' reference captures.
- The **`CPUTest/CPU`** family runs as **NTSC** (`cpu_test!` passes
  `Region::Ntsc`). It used to be PAL too, on the theory that its result
  table only fits inside PAL's longer V-blank. That was calibrated against
  an older, too-fast boot (the DRAM refresh was not charged during DMA).
  With cycle-exact timing the opposite holds, and Mesen2 agrees on both
  counts: in PAL the write burst overruns V-blank and the table is
  truncated mid-row, while in NTSC both emulators render the full all-PASS
  table. A truncated table asserts nothing, so NTSC it is. The reasoning is
  in the doc comment of `run_to_stable`.

### Golden hashes are luna's own output

Unlike the Tom Harte vectors (hardware truth), these hashes are captured
from **luna's renderer**, so they are **regression baselines**, not an
independent correctness oracle. Each ROM ships a reference `*.png`
(real-hardware output) — eyeball luna's render against it when blessing a
baseline; the test itself never reads that PNG. Regenerate after an intended
render change:

```bash
LUNA_SNES_TEST_RECORD=1 LUNA_SNES_TEST_PNG=/tmp/snes \
  cargo test -p luna-core --test snes_test_roms -- --nocapture
```

`LUNA_SNES_TEST_RECORD` prints the new hashes and **skips every assertion**
— a run with it set proves nothing.

## 3. Commercial titles — developer-local goldens

`game_test!` in the same harness: one eyeball-validated scene per hardware
feature (mapper, coprocessor, PPU effect). The ROM is auto-detected, booted
with no input, run to a fixed **frame** (`run_game_to_frame`) and its
framebuffer hash asserted. The ROMs are copyrighted and never committed:
they live in `tests/roms/` (gitignored), and each test skips when its ROM is
missing — CI never runs them. Set `LUNA_GAME_TEST_REQUIRE=1` locally before
a release so a missing dump fails instead of skipping.

Also developer-local: the `smoke` test (`crates/luna-api/tests/smoke.rs`,
screenshot + audio statistics on three titles, goldens under `tests/golden/`)
and the HDMA corpus sweep (`tools/validate-hdma-corpus.sh`, eyeballed — see
`.claude/rules/hdma-dma-faithful-audit.md`).

## 4. Environment variables read by the tests

No `LUNA_*` variable is read by production code; all of these are test-only.
Each row was checked in the file named.

| Variable | Read in | Effect |
|---|---|---|
| `LUNA_SNES_TEST_DIR` | `crates/luna-core/tests/snes_test_roms.rs` (`corpus_root`), `tools/fetch-snes-test-roms.sh` | Corpus root. Default: the sibling `../luna_tests`. |
| `LUNA_SNES_TEST_REQUIRE` | same file (`CORPUS_REQUIRE`, `skip`) | Set: a missing corpus or corpus ROM **fails** instead of skipping. Set by CI's `snes-test-roms` job. |
| `LUNA_GAME_TEST_REQUIRE` | same file (`GAMES_REQUIRE`, `skip`) | Same, for the commercial ROMs under `tests/roms/`. Set by no workflow — local pre-release check. |
| `LUNA_SNES_TEST_RECORD` | same file (`test_display`, `test_audio`, `game_test!`, the native-capture test) | Set: print the freshly computed hash and **return without asserting**. |
| `LUNA_SNES_TEST_PNG` | same file | With `…_RECORD`: directory that receives a PNG per display test and a WAV per audio test. |
| `LUNA_SNES_TEST_HOLD` | same file (`run_to_stable`, `run_audio`) | Hex pad mask that **overrides** the test's own held buttons — it silently changes the test input; for ad-hoc experiments only. |
| `LUNA_SNES_TEST_AUDIO_N` | same file (`run_audio`) | Number of stereo samples to capture instead of `AUDIO_SAMPLES` — changes the hash. |
| `LUNA_SNES_TEST_PPUDIAG` | same file (`run_to_stable`) | Set: print PPU register state after the run. Diagnostic only. |
| `LUNA_SNES_TEST_APUDIAG` | same file (`run_audio`) | Set: print APU / DSP state after the run. Diagnostic only. |
| `LUNA_TOM_HARTE_DIR` | `crates/luna-cpu-65c816/tests/tom_harte.rs` (`dataset_path`) | 65C816 dataset directory (the `v1/` level). Default `tests/tom-harte/v1`. |
| `LUNA_TOM_HARTE_SPC700_DIR` | `crates/luna-cpu-spc700/tests/tom_harte.rs` (`dataset_path`) | SPC700 dataset directory. Default `tests/tom-harte-spc700/v1`. |
| `LUNA_TOM_HARTE_REQUIRE` | both `tom_harte.rs` (`enforce_baseline`) | Set: a state mismatch fails the test. Unset: report only. |
| `LUNA_TOM_HARTE_CYCLES` | 65C816 `tom_harte.rs` (`enforce_baseline`) | With `…_REQUIRE`: a cycle-count mismatch fails too. Set by no workflow. |
| `LUNA_TOM_HARTE_SAMPLE` | 65C816 `tom_harte.rs` | Run only the first N cases of each opcode file (fast iteration). |
| `UPDATE_GOLDENS` | `crates/luna-api/tests/smoke.rs` | Set: rewrite the smoke screenshots / audio statistics instead of comparing. |
| `LUNA_MOUSE_ROM`, `LUNA_SUPERSCOPE_ROM` | `crates/luna-api/tests/mouse.rs`, `superscope.rs` | Path of the OpenSNES example ROM the test drives. The default is a path on the maintainer's machine; the test skips when the file is absent. |
| `LUNA_GSU_DIFF_CSV` | `crates/luna-bus/src/superfx.rs` (`gsu_differential_vs_mesen`) | Mesen2 GSU trace to diff against. Default `/tmp/mesen_gsu_full.csv`; skips when absent. |
| `LUNA_GSU_DIFF_DIR` | same file (`gsu_trajectory_vs_mesen`) | Directory holding the trajectory capture (`mesen_gsu_full.csv`, `mesen_gsu_init.txt`, `mesen_gsu_ram_start.bin`). Default `/tmp`. |
| `LUNA_SF_ROM` | same file, both GSU harnesses | Star Fox ROM path. Default under `tests/roms/`. |
| `LUNA_DSP1_PORT_CSV` | `crates/luna-core/tests/dsp1_port_differential.rs` | Mesen2 DSP-1 port trace. Default `/tmp/mesen_dsp1_port.csv`; skips when absent. |
| `LUNA_SPC_RESET_TIMER` | `crates/luna-core/tests/spc_trajectory.rs` | Set: zero the APU timer phase before the (ignored, manual) SPC trajectory run. |
