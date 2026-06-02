---
description: Run the SMRPG / SMW visual regression smoke test via luna-cli
argument-hint: "[smrpg|smw|both]"
allowed-tools: Bash(cargo *), Bash(./target/release/luna *), Read
---

Drive the release `luna` binary against the test ROMs and screenshot
the result for visual regression checks. Default target if no argument
is passed is `smrpg`.

Requires the release binary at `./target/release/luna` — run `/rebuild`
first if it isn't built.

## Targets

### `smrpg` (default) — Super Mario RPG

> SMRPG plays an intro and then **waits at its title/demo screen for a
> Start press**. With no input it sits there forever (forced-blank →
> black) and reads as a hang — that is NOT a bug (it cost us several
> sessions; see `.claude/rules/coproc-testing.md` and the
> `project_smrpg_sa1_deadlock` memory). Two checkpoints:

```bash
# 1. Intro cinematic (no input): Peach-in-the-garden scene at ~frame 392.
./target/release/luna state -n 12000000 --screenshot /tmp/smrpg_intro.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"

# 2. Past the title: pulse Start ($1000) to reach New Game → name entry.
./target/release/luna state -n 55000000 \
  --input "1600:0x1000,1610:0,1700:0x1000,1710:0,2000:0x1000,2010:0,2500:0x1000,2510:0" \
  --screenshot /tmp/smrpg_name.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

Expected: **#1** the intro cinematic (Peach in the garden — bird,
treehouse, bushes); **#2** the **"Your name?"** name-entry screen (Mario
+ alphabet grid), with `nmis_serviced` climbing past the title (≥ ~5000,
NMI rate ≥ 80%). A no-input run freezing at `nmis_serviced` ≈ 1598 is the
title wait, not a deadlock.

### `smw` — Super Mario World Yoshi's House intro

Useful for PPU compositor / color-math regressions. Scripted Start
presses get past the title + file select into the intro cutscene.

```bash
./target/release/luna state -n 30000000 \
  --input "300:0x1000,315:0,500:0x1000,515:0,700:0x1000,715:0" \
  --screenshot /tmp/smw.png \
  --peek 7E:0200:20 \
  "tests/roms/Super Mario World (U) [!].smc"
```

Expected: Yoshi's House intro scene with the welcome dialog. Sub-screen
sky + clouds visible on the sides of the dialog box. The `--peek` dump
of `$7E:0200..0220` shows the shadow OAM — a non-park-Y-240 pattern
means Mario is in OAM and should render.

### `both`

Run both targets sequentially. Each leaves its PNG in `/tmp/`.

## Reporting

Read the resulting screenshot(s) and describe what you see (sprites
present, BG layers visible, palette correctness). Compare against the
last-known-good if there's a baseline image in `tests/screenshots/`.

If $ARGUMENTS is empty, default to `smrpg`.
