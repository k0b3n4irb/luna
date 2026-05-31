# Coprocessor / DMA / PPU Test Discipline (auto-loaded)

Anything touching these paths has tight cross-coupling and silent
regressions elsewhere are common:

- `crates/luna-bus/src/sa1.rs` (the bus-side SA-1 mapper)
- `crates/luna-core/src/coproc/sa1.rs` (the chip-side SA-1 state)
- `crates/luna-core/src/dma/` (DMA + HDMA controllers)
- `crates/luna-ppu/`

A change in any of these requires a full `cargo test --workspace --lib`
sweep, not just the per-crate test set. Run after the rebuild discipline
sequence has passed.

## ROM smoke test

When DMA / PPU / SA-1 logic changes, also screenshot Super Mario RPG
via the CLI as a quick visual regression check:

```
/smoke-test
```

Or run the underlying command directly:

```
./target/release/luna state -n 30000000 --screenshot /tmp/smrpg.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

A working build should reach the sky-coloured title scene (frame
~2000+, NMI service rate ≥ 80%).

For PPU compositor / color-math changes the equivalent SMW Yoshi's
House intro repro is also useful — see `/smoke-test` for the
scripted-input variant.

## Interlace smoke test (RPM Racing)

For any change to interlace, hi-res (mode 5/6), or sprite rendering,
screenshot **RPM Racing** — the canonical commercial interlace title
(runs Mode 5 hi-res + BG interlace, `SETINI=$01`, the whole time):

```
./target/release/luna state -n 12000000 --screenshot /tmp/rpm.png \
  "tests/roms/RPM Racing (U).smc"
```

A working build shows the **R.P.M. RACING / Interplay** title with a
crisp, readable sprite logo (no horizontal banding, no ghosted/doubled
letters). RPM Racing exposed a real bug the homebrew `PPU/Interlace/*`
ROMs could not: it sets BG interlace (SETINI bit 0) **without** OBJ
interlace (bit 1), so gating OBJ interlace on the wrong bit garbled its
sprites (fixed `0cc6da4`). No public corpus covers that combination, so
this commercial smoke is worth keeping.

The ROM is copyrighted and **not committed** (`tests/roms/*` is
gitignored — dump your own cart); the command skips if it is absent. The
homebrew `ppu_interlace_*` goldens in `snes_test_roms.rs` are the
committed, CI-run interlace coverage.
