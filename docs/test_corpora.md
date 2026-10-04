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

`LUNA_TOM_HARTE_REQUIRE=1` turns the run into a gate (otherwise the test
just prints a report). It then fails on a state mismatch, on a cycle
mismatch, on a core that panics, and on a dataset that is missing, empty or
shorter than the counts below — a run that compared nothing is not a pass.
CI runs both in the `tom-harte` workflow
(`.github/workflows/tom-harte.yml`), not in the main `ci.yml`. The datasets
are gitignored.

Measured 2026-10-04 on the full datasets:

| Core | State | Cycles |
|---|---|---|
| 65C816 | 5,080,000 / 5,080,000 (508 files × 10,000; the 4 MVN/MVP files are not run) | count: 5,040,000 / 5,040,000 (WAI, STP excluded). Per-cycle bus trace: 4,740,000 / 5,040,000 — the rest is exactly the 30 files of `TRACE_RESIDUAL` in the harness (emulation-mode read-modify-write opcodes and `WDM`), every case of each. The gate fails on a divergence outside that list **and** on a listed file that stops diverging. |
| SPC700, production core (`step_cycle`) | 256,000 / 256,000 | per-cycle bus trace: 254,000 / 254,000 (SLEEP, STOP excluded) |
| SPC700, atomic core (`step`) | 256,000 / 256,000 | 254,000 / 254,000 |

The SPC700 harness runs every case on both cores: the cycle-stepped one
that `luna-apu` drives, and the atomic interpreter kept as the equivalence
oracle.

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
  `MosaicMode5` holds **R** too since 2026-10-04 (it used to hold nothing
  and so rendered `ppu_interlace_moogle`'s frame, same hash), but its
  golden is a candidate and the test is `#[ignore]`d: at the largest
  mosaic size luna's frame does not match the corpus' `MosaicMode5.png`
  (see the comment on the test). Mode 5 mosaic therefore still has **no
  blessed coverage**.
- **`ppu_interlace_font_native_512x448`** — the native 512×448 capture
  path, hashed on its own buffer.
- **`input_controller_latency` — `INPUT/ControllerLatency`**: holds **A**
  and expects the white screen (joypad auto-read end to end).
- **`spc700_test!` — `CPUTest/SPC700/*`** (ADC, AND, DEC, EOR, INC, ORA,
  SBC): these are validated by the ROM's own verdict, not a hash. The
  SPC700 program reports each sub-test on port 0
  (`$2140`): `k` when sub-test `k` passes, `$80 | k` when it fails (and it
  then halts). The test (`run_spc700_mailbox`) asserts that the emulator
  did not panic, that no fail code appeared, and that port 0 ends on the
  **last** sub-test's pass value — i.e. the ROM ran to completion.
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

`LUNA_SNES_TEST_RECORD=1` prints the new hashes and **skips every assertion**
— a run with it proves nothing. Only the value `1` enables it, and it is
refused (the test panics) while `LUNA_SNES_TEST_REQUIRE` or
`LUNA_GAME_TEST_REQUIRE` is set.

## 3. Commercial titles — developer-local goldens

`game_test!` in the same harness: one eyeball-validated scene per hardware
feature (mapper, coprocessor, PPU effect). The ROM is auto-detected, booted
with no input, run to a fixed **frame** (`run_game_to_frame`) and its
framebuffer hash asserted. The ROMs are copyrighted and never committed:
they live in `tests/roms/` (gitignored), and each test skips when its ROM is
missing — CI never runs them. Set `LUNA_GAME_TEST_REQUIRE=1` locally before
a release so a missing dump fails instead of skipping.

Also developer-local, and under the same `LUNA_GAME_TEST_REQUIRE` switch:

- the `smoke` test (`crates/luna-api/tests/smoke.rs`, screenshot + audio
  statistics on three titles, goldens under `tests/golden/`);
- `reset_repro` (`crates/luna-api/tests/`, three titles reboot after a
  reset);
- `mouse` and `superscope` (`crates/luna-api/tests/`), which drive two
  OpenSNES example ROMs named by `LUNA_MOUSE_ROM` / `LUNA_SUPERSCOPE_ROM`;
- `dsp1_port_differential` (`crates/luna-core/tests/`), a manual
  `#[ignore]`d harness: it compares against a Mesen2 trace
  (`tools/mesen-dsp1-port-trace.lua` writes `/tmp/mesen_dsp1_port.csv`) and
  runs only with `-- --ignored`.

Two harnesses are **manual** (`#[ignore]`, run with `--ignored`): the GSU
differentials in `crates/luna-bus/src/superfx.rs`
(`gsu_differential_vs_mesen`, `gsu_trajectory_vs_mesen`). They need a
Mesen2 capture of Star Fox (`tools/snes-gsu-trajectory-capture.lua`) and
fail — never skip — when it is absent; with it they assert zero divergence.

The HDMA corpus sweep (`tools/validate-hdma-corpus.sh`) is eyeballed — see
`.claude/rules/hdma-dma-faithful-audit.md`. It is frame-anchored and exits
non-zero when a ROM is present but its screenshot was not written.

## 4. Environment variables read by the tests

No `LUNA_*` variable is read by production code; all of these are test-only.
Each row was checked in the file named.

| Variable | Read in | Effect |
|---|---|---|
| `LUNA_SNES_TEST_DIR` | `crates/luna-core/tests/snes_test_roms.rs` (`corpus_root`), `tools/fetch-snes-test-roms.sh` | Corpus root. Default: the sibling `../luna_tests`. |
| `LUNA_SNES_TEST_REQUIRE` | same file (`CORPUS_REQUIRE`, `skip`) | Set: a missing corpus or corpus ROM **fails** instead of skipping. Set by CI's `snes-test-roms` job. |
| `LUNA_GAME_TEST_REQUIRE` | same file (`GAMES_REQUIRE`, `skip`); `crates/luna-api/tests/{smoke,reset_repro,mouse,superscope}.rs` and `crates/luna-core/tests/dsp1_port_differential.rs` (`skip`) | Same, for everything the repository cannot ship: the commercial ROMs under `tests/roms/`, the OpenSNES example ROMs, the DSP-1 firmware and Mesen2 trace. Set by no workflow — local pre-release check. |
| `LUNA_SNES_TEST_RECORD` | same file (`record_mode`) | `=1` (only that value): print the freshly computed hash and **return without asserting**. Panics if a `*_REQUIRE` variable above is set. |
| `LUNA_SNES_TEST_PNG` | same file | With `…_RECORD`: directory that receives a PNG per display test and a WAV per audio test. |
| `LUNA_SNES_TEST_HOLD` | same file (`run_to_stable`, `run_audio`) | Hex pad mask that **overrides** the test's own held buttons — it silently changes the test input; for ad-hoc experiments only. |
| `LUNA_SNES_TEST_AUDIO_N` | same file (`run_audio`) | Number of stereo samples to capture instead of `AUDIO_SAMPLES` — changes the hash. |
| `LUNA_SNES_TEST_PPUDIAG` | same file (`run_to_stable`) | Set: print PPU register state after the run. Diagnostic only. |
| `LUNA_SNES_TEST_APUDIAG` | same file (`run_audio`) | Set: print APU / DSP state after the run. Diagnostic only. |
| `LUNA_TOM_HARTE_DIR` | `crates/luna-cpu-65c816/tests/tom_harte.rs` (`dataset_path`) | 65C816 dataset directory (the `v1/` level). Default `tests/tom-harte/v1`. |
| `LUNA_TOM_HARTE_SPC700_DIR` | `crates/luna-cpu-spc700/tests/tom_harte.rs` (`dataset_path`) | SPC700 dataset directory. Default `tests/tom-harte-spc700/v1`. |
| `LUNA_TOM_HARTE_REQUIRE` | both `tom_harte.rs` (`require`, `enforce_baseline`) | Set: the run is a gate — a state or cycle mismatch, a panic of the core, a missing dataset or one with fewer files / cases than expected fails the test. Unset: report only. Set by the `tom-harte` workflow. |
| `LUNA_TOM_HARTE_SAMPLE` | 65C816 `tom_harte.rs` | Run only the first N cases of each opcode file (fast iteration). |
| `UPDATE_GOLDENS` | `crates/luna-api/tests/smoke.rs` | `=1` (only that value): rewrite the smoke screenshots / audio statistics instead of comparing. Panics if `LUNA_GAME_TEST_REQUIRE` is set. |
| `LUNA_MOUSE_ROM`, `LUNA_SUPERSCOPE_ROM` | `crates/luna-api/tests/mouse.rs`, `superscope.rs` | Path of the OpenSNES example ROM the test drives (`<opensnes>/examples/input/mouse/mouse.sfc`, `…/superscope/superscope.sfc`). No default: unset, or not a file, the test skips — or fails under `LUNA_GAME_TEST_REQUIRE`. |
| `LUNA_GSU_DIFF_CSV` | `crates/luna-bus/src/superfx.rs` (`gsu_differential_vs_mesen`, manual) | Mesen2 GSU trace to diff against. Default `/tmp/mesen_gsu_full.csv`; the test fails when it is absent. |
| `LUNA_GSU_DIFF_DIR` | same file (`gsu_trajectory_vs_mesen`, manual) | Directory holding the trajectory capture (`mesen_gsu_full.csv`, `mesen_gsu_init.txt`, `mesen_gsu_ram_start.bin`, `mesen_gsu_ram_stop1.bin`). Default `/tmp`; the test fails when a file is absent. |
| `LUNA_SF_ROM` | same file, both GSU harnesses | Star Fox ROM path. Default under `tests/roms/`. |
| `LUNA_DSP1_PORT_CSV` | `crates/luna-core/tests/dsp1_port_differential.rs` | Mesen2 DSP-1 port trace. Default `/tmp/mesen_dsp1_port.csv`. The test is `#[ignore]`d (manual); run explicitly, an absent trace is a failure. |
| `LUNA_SPC_RESET_TIMER` | `crates/luna-core/tests/spc_trajectory.rs` | Set: zero the APU timer phase before the (ignored, manual) SPC trajectory run. |
