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
> sessions chasing a phantom "SA-1 deadlock"; see
> `.claude/rules/coproc-testing.md`). Two checkpoints:

```bash
# 1. Intro cinematic (no input): Peach-in-the-garden scene
#    (`frame_count` ≈ 909, `nmis_serviced` ≈ 417).
mkdir -p /tmp/luna-smoke
./target/release/luna state -n 12000000 --screenshot /tmp/luna-smoke/smrpg_intro.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"

# 2. Past the title: pulse Start ($1000) to reach New Game → name entry.
./target/release/luna state -n 55000000 \
  --input "1600:0x1000,1610:0,1700:0x1000,1710:0,2000:0x1000,2010:0,2500:0x1000,2510:0" \
  --screenshot /tmp/luna-smoke/smrpg_name.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

Expected: **#1** the intro cinematic (Peach in the garden — bird,
treehouse, bushes); **#2** the **"Your name?"** name-entry screen (Mario
+ alphabet grid), with `nmis_serviced` climbing past the title (≈ 3335 at
frame ≈ 3988 for `-n 55000000` since #126 — the `--input` checkpoints now
spend from the `-n` budget; NMI rate ≥ 80%). A no-input run never
reaches that screen: it stays on the title / attract-demo loop, with
`nmis_serviced` still climbing but slowly (≈ 2147 at frame ≈ 4171 for
`-n 55000000`, NMI rate ≈ 50%; ≈ 1651 at frame ≈ 2219 and a black
forced-blank screenshot for `-n 30000000`). That is the title wait, not a
deadlock.

### `smw` — Super Mario World Yoshi's House intro

Useful for PPU compositor / color-math regressions. Scripted Start
presses get past the title + file select into the intro cutscene. The
title only appears around frame 400, so the first press waits until
frame 700 (re-measured 2026-10-24: a press at frame 300 lands before the
title and the run ends on the 1/2 PLAYER menu).

```bash
mkdir -p /tmp/luna-smoke
./target/release/luna state -n 30000000 \
  --input "700:0x1000,715:0,900:0x1000,915:0,1100:0x1000,1115:0,1300:0x1000,1315:0" \
  --screenshot /tmp/luna-smoke/smw.png \
  --peek 7E:0200:20 \
  "tests/roms/Super Mario World (U) [!].smc"
```

Expected: Yoshi's House intro scene with the welcome dialog. Sub-screen
sky + clouds visible on the sides of the dialog box. The `--peek` dump
of `$7E:0200..0220` shows the shadow OAM (Mario at `$7E:021C`) — a non-park-Y-240 pattern
means Mario is in OAM and should render.

### `both`

Run both targets sequentially. Each leaves its PNG in `/tmp/luna-smoke/`
(`mkdir -p` it first; `tools/clean-tmp.sh --apply` removes it).

## Reporting

Read the resulting screenshot(s) and describe what you see (sprites
present, BG layers visible, palette correctness). Compare against the
last-known-good if there's a baseline image in `tests/golden/smoke/`.

If $ARGUMENTS is empty, default to `smrpg`.
