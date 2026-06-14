# SMW2 Yoshi's Island intro "barcode" — investigation log (2026-06)

**Status: RESOLVED** (commit `7f9f728`, `fix(dma): honor mid-frame HDMA
enable`). The intro story text is on **BG4**, shown via a per-line HDMA
Mode-0 split that the game enables **mid-frame**; luna ignored mid-frame
HDMA enable, so the split never ran and the text never composited.

## Symptom

In SMW2 Yoshi's Island's intro (Nintendo-logo storybook, then the stork
scene), the top of every screen was correct but the **bottom story-text
band rendered as a row of uniform blue vertical "barcode" bars** instead of
the readable text (e.g. "A stork hurries across the dusky, pre-dawn sky.").
ares and Mesen2 render it perfectly. Reproduce (pre-fix):

```
./target/release/luna state -n 20400000 --screenshot /tmp/yi.png \
  "tests/roms/Super Mario World 2 - Yoshi's Island (U) (V1.1).smc"
```

## Root cause

The text band is **BG4**. Mode 1 (the scene's mode) has no BG4, so the band
switches to **Mode 0** via a per-line HDMA split — BGMODE (`$2105`) → Mode 0
and TM (`$212C`) → BG4-only — for the bottom scanlines. The game sets this
up by enabling HDMA **mid-frame**, every frame:

```
$420C = $F0  (enable ch4-7)  at scanline ~12
$420C = $00  (disable)       at scanline ~198
```

So HDMAEN is `0x00` at the V=0 HDMA-init point (the previous frame's `$00`
persists). luna only set channels up at V=0 init and gated per-scanline
HDMA on that latched `hdma_active` — so the **mid-frame enable was ignored**:
the channels never ran, the band stayed Mode 1 (no BG4), BG4's text was
never composited, and the visible bars are the **BG3 filler tile** (which is
correct, and identical in both emulators).

ares gates `hdmaRun` on the **live** `hdmaActive() = hdmaEnable &&
!hdmaCompleted`, not a V=0-only latch. **The fix** (`dma/channel.rs` +
`dma/controller.rs`): a per-frame `hdma_started` latch, re-armed in
`hdma_init`; a channel whose HDMAEN bit is set after init is lazily set up
from its source address on its first active line. Mid-frame *re-enables*
within a frame keep their state (the latch clears only at V=0); channels
enabled at V=0 are unchanged. Regression test:
`hdma_enabled_mid_frame_starts_from_source`.

## What cracked it — and the lesson

The decisive observation was a **side-by-side per-BG-layer comparison in
Mesen** (the user's own check): *"BG1/2/3 look identical to luna; the bars
are present in BG3 in both; the difference is BG4 — it has the text in
Mesen, empty in luna."* That single comparison collapsed days of
misdirection.

**Lesson** (cf. [[feedback_pivot_to_reference_architecture]]): when a
composited frame "looks wrong," **compare the individual BG/OBJ layers
against the reference FIRST** — it's cheap (a layer viewer / a tilemap
render) and localises the bug to one layer before any deep differential.
Here, every per-line HDMA *input* (the BGMODE/TM/scroll/window tables in
WRAM) was byte-identical between luna and Mesen — the bug was purely whether
the HDMA *ran*, which the layer comparison would have pointed at immediately.

## Misdirection — hypotheses chased and eliminated (confound-free)

These were all genuinely ruled out, but none was the bug — the real cause
was a layer/HDMA issue, not the GSU. Kept as a record of what *not* to
re-investigate:

| Hypothesis | Verdict | How it was disproven |
|---|---|---|
| PPU per-scanline renderer | ❌ | luna already renders per-scanline; HDMA register writes route live through `ppu.write()`. |
| GSU **engine** logic | ❌ | `gsu_trajectory_vs_mesen` replayed 19 999 instrs **byte-exact**; work RAM 99.63 % identical. |
| GSU **RAM size** hardcode | ❌ (real latent bug, fixed separately) | luna hardcoded 128 KB for all Super FX; correct is the `$FFBD` expansion byte (YI 32 KB, Doom/Stunt Race 64 KB). Forcing 32 KB → byte-identical YI output. Fixed in its own PR. |
| Timing **lag** ("~38 % slow") | ❌ | luna is **frame-synced** with Mesen (Nintendo logo ~800–1200, stork ~1600 in both). The "lag" was a measurement error (instruction-derived frame vs scene-*start* frame). |
| CPU / GSU **starvation** | ❌ | per-frame work comparable: CPU 12 658 vs 11 764; GSU 46 443 vs 38 032 instrs/frame. |
| DMA **sampling-phase** / GSU stall | ❌ | drain-GSU-before-MDMA was a no-op — the GSU is STOPPED at every framebuffer MDMA. |
| GSU **renders the text** | ❌ | the GSU never writes the text region; the text is BG4, composited by the PPU. |

The earlier conclusion in this doc — a "transient CPU→WRAM→DMA→GSU-RAM→VRAM
font pipeline" — was **wrong**. It over-fit the BG3 char data (which is the
*filler* tile, correct in both) and missed that the missing content was a
whole separate layer (BG4) gated off by un-run HDMA.

## Reusable tools produced

- **`tools/snes-gsu-trajectory-capture.lua`** — Mesen → the four
  `gsu_trajectory_vs_mesen` fixture files (the GSU engine oracle).
- **`tools/snes-wram-perframe-hash.lua`** — Mesen side of the NMI-aligned
  WRAM differential (matches `luna wram-trace`).
- **`tools/snes-gsu-state-keys.lua`** — dumps Mesen `cart.coprocessor.*`
  getState keys (Mesen→luna GSU register mapping reference).
- luna CLI used heavily: `--dma-trace`, `--dump-coproc-ram`, `--dump-vram`,
  `--superfx-trace`, `wram-trace`.
- Mesen2 at `~/bin/Mesen` (`--testRunner <lua> <rom>`); per-BG-layer state
  via getState `ppu.layers[n].*`; HDMAEN write timing via a `$420C` write
  callback reading `ppu.scanline`.

## Related

- `docs/cooperative_scheduler_reference.md` — the GSU↔CPU timing model
  (confirmed faithful here; the bug was not there).
- The 128 KB Super FX RAM hardcode (`snes.rs`) — a genuine latent bug
  surfaced during this hunt, fixed separately to the `$FFBD` expansion size.
