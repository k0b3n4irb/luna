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

> **A black / forced-blank smoke screenshot is NOT proof of a bug.**
> Commercial titles play an intro and then **wait at a title/demo
> screen for a Start press**. The CLI injects no input by default, so a
> game that is working perfectly will sit there forever (often
> forced-blank → black) and read as a "hang." Before suspecting the
> emulator, **inject Start** with `--input` (see below). This is the
> inverse of the [[feedback_audit_deviations_test_in_gui]] gotcha: there
> a CLI pass hid a real bug; here a CLI "fail" hid a working emulator.
> (Cost us several sessions chasing a phantom "SA-1 deadlock" in SMRPG
> that was just the title screen waiting for Start — see the
> `project_smrpg_sa1_deadlock` memory.)

When DMA / PPU / SA-1 logic changes, screenshot Super Mario RPG via the
CLI as a quick visual regression check:

```
/smoke-test
```

Two checkpoints — the no-input intro **and** the post-Start path,
because SA-1 graphics are exercised by both:

```
# 1. Intro cinematic (no input): the Peach-in-the-garden scene at ~frame 392.
./target/release/luna state -n 12000000 --screenshot /tmp/smrpg_intro.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"

# 2. Past the title: pulse Start to reach New Game → the "Your name?"
#    name-entry screen (Mario + alphabet grid). Start = $1000.
./target/release/luna state -n 55000000 \
  --input "1600:0x1000,1610:0,1700:0x1000,1710:0,2000:0x1000,2010:0,2500:0x1000,2510:0" \
  --screenshot /tmp/smrpg_name.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

A working build:
- **#1** shows the intro cinematic (Peach in the garden — bird, treehouse,
  bushes), rendered cleanly.
- **#2** reaches the **"Your name?"** name-entry screen, and crucially
  `nmis_serviced` keeps climbing past the title (≥ ~5000 at `-n
  55000000`, NMI service rate ≥ 80%). Without the `--input` the run
  **freezes at `nmis_serviced` ≈ 1598** — that plateau is the title
  wait, not a deadlock.

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
