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
clone of only the test-relevant subdirs (not the multi-GB whole repo).
Harness: `crates/luna-core/tests/snes_test_roms.rs`. Each test boots a
ROM with a forced LoROM mapper (`Cartridge::from_bytes_forced` — these
homebrew ROMs have no valid header checksum), runs it until the 256×224
framebuffer settles, and asserts a SHA-256 of the framebuffer against a
committed golden hash.

```bash
tools/fetch-snes-test-roms.sh                  # sparse-clone → ../luna_tests
cargo test -p luna-core --test snes_test_roms
```

Or point `LUNA_SNES_TEST_DIR` at a corpus root. If the corpus is absent
the tests skip with a notice and pass. CI runs them in the
`snes-test-roms` job.

### Golden hashes are luna's own output

Unlike the Tom Harte vectors (hardware truth), these hashes are captured
from **luna's renderer**, so they are **regression baselines**, not an
independent correctness oracle. Each ROM ships a reference `CPU<NAME>.png`
(real-hardware output) — eyeball luna's render against it when blessing a
baseline. Regenerate after an intended render change:

```bash
LUNA_SNES_TEST_RECORD=1 LUNA_SNES_TEST_PNG=/tmp/snes \
  cargo test -p luna-core --test snes_test_roms -- --nocapture
```

### Known gaps surfaced by this suite

- **`BRA` / `JMP` / `PSR` / `RET` render blank.** luna runs these four
  CPUTest ROMs for 30M instructions without ever drawing the result
  table (the reference PNGs show a full PASS table). Tracked as
  `#[ignore]`d `known_blank` tests — the committed hash characterises the
  current broken (blank) output, so when luna is fixed the hash changes
  and the `--ignored` run goes red. Notably the *instructions* themselves
  are perfect under Tom Harte, so this is a full-system issue (display
  kernel / control-flow loop), not an ALU bug.
- The 19 passing CPU tests render the correct all-PASS data but luna
  drops the top title line + column-header separators vs the reference
  (a minor PPU compositing gap; the regression hash still holds).
