# SMW2 Yoshi's Island intro "barcode" — investigation log (2026-06)

**Status: OPEN.** Root cause narrowed to the per-frame **text font upload
pipeline** (CPU → WRAM → DMA → GSU work RAM → DMA → VRAM). The Super FX
**GSU engine is exonerated** for this bug — it renders the *scene*
(clouds/stork, which are correct); it does **not** render the text glyphs.

## Symptom

In SMW2 Yoshi's Island's intro (Nintendo-logo storybook, then the stork
scene), the top of every screen is correct but the **bottom story-text band
renders as a row of uniform blue vertical "barcode" bars** instead of the
readable text (e.g. "A stork hurries across the dusky, pre-dawn sky.").
ares and Mesen2 render it perfectly. Reproduce:

```
./target/release/luna state -n 28000000 --screenshot /tmp/yi.png \
  "tests/roms/Super Mario World 2 - Yoshi's Island (U) (V1.1).smc"
```

The bottom band shows ~52 identical `30 01 30 01 …` tiles in the BG3 char
region of VRAM where Mesen has varied letter glyphs.

## What it is NOT (eliminated, confound-free)

| Hypothesis | Verdict | How it was disproven |
|---|---|---|
| PPU per-scanline renderer | ❌ | luna already renders per-scanline; HDMA register writes route live through `ppu.write()`. |
| GSU **engine** logic | ❌ | `gsu_trajectory_vs_mesen` replayed 19 999 instrs **byte-exact** (control-flow + all R0–R15); work RAM 99.63 % identical. |
| GSU **RAM size** hardcode | ❌ (latent bug, not this) | luna hardcodes 128 KB for all Super FX (`snes.rs` `SuperFxMapper::new(cart.rom, 0x2_0000)`); correct is the `$FFBD` expansion byte (YI 32 KB, Doom/Stunt Race 64 KB, Star Fox 64 KB default). Forcing 32 KB → **byte-identical** output. Worth fixing separately. |
| Timing **lag** ("~38 % slow") | ❌ | luna is **frame-synced** with Mesen (Nintendo logo ~800–1200, stork ~1600 in both). The "lag" was a measurement error (instruction-derived frame vs scene-*start* frame). |
| CPU / GSU **starvation** | ❌ | per-frame work is comparable: CPU 12 658 vs 11 764 instrs/frame; GSU 46 443 vs 38 032. |
| DMA **sampling-phase** (DMA reads GSU mid-render) | ❌ | a drain-GSU-before-MDMA hack was a **no-op** — the GSU is **STOPPED** at every framebuffer MDMA. |
| GSU **bus-stall park** | ❌ | a force-through-stalls drain fired **0 times** — the GSU is genuinely stopped, not parked, at the DMA. |
| GSU **renders the text** | ❌ | the GSU writes the glyph buffer `$70:$4000–$5FFF` **0 times** over frames 1400–1700 (write callbacks, both `cpuType.gsu` and `cpuType.snes`). |

## What it IS (current frontier)

The text glyphs reach VRAM through a **CPU/DMA font pipeline**, not the GSU:

```
CPU builds glyph data in WRAM ──DMA──▶ GSU RAM $4000-$5FFF (staging)
                                          │
                                          └──DMA──▶ VRAM BG3 char region
```

The divergence is **transient and mid-frame**:

- At **frame boundaries**, luna's GSU RAM `$4000–$5FFF` is **byte-identical
  to Mesen** (real letters) — verified at frames 800 and 1592.
- At the **DMA-transfer instant** (mid-frame), luna's staging buffer holds
  the `30 01` **bars**, which is what lands in VRAM (confirmed via
  `--dma-trace`: source `$70:4C58…` at transfer time, value `$30`).

So luna's glyph buffer is *bars at the DMA, letters by frame end*, while
nobody (GSU or CPU instruction) writes that region during the scene → it is
written by a **DMA** the exec/write callbacks don't observe. The bug is in
**what that font-upload DMA copies (or its WRAM source) at the moment the
glyphs are staged**, one or two stages upstream of VRAM.

This is why every frame-boundary tool (the WRAM differential, GSU-RAM
dumps, screenshots) reports "matches": they are **structurally blind to a
transient mid-frame state**.

## Why it's hard

A multi-stage per-frame pipeline where the divergence is transient
(correct at frame boundaries) and carried by **DMA** writes (invisible to
instruction-level callbacks). Each ad-hoc probe pushes the divergence one
stage upstream and hits a fresh confound: exact-frame alignment (luna and
Mesen drift a few frames), transient buffer state, and which-unit-wrote-it.

## The instrument the next attempt needs

A **per-frame, transfer-time pipeline tracer** that, at the exact instant of
each font-upload DMA, snapshots all three stages in both emulators and
diffs them:

1. the **WRAM glyph source** the upload DMA reads,
2. the **GSU-RAM staging** buffer `$4000–$5FFF`,
3. the **VRAM** char bytes written.

Concretely: in Mesen, trigger on the DMA that writes the BG3 char VRAM
region (or the DMA that *fills* GSU RAM `$4000–$5FFF`), dump its A-bus
source + the staging buffer at that instant; do the same in luna
(`--dma-trace` already captures source→VMADD→byte for the VRAM leg; a
companion capture is needed for the WRAM→GSU-RAM leg). The first stage that
differs at the transfer instant is the bug.

**Precise open question:** what populates GSU RAM `$4000–$5FFF` with the
`30 01` bars in luna (vs letters in Mesen), given neither the GSU nor a CPU
*instruction* writes it during the scene? Trace the upload DMA's source and
content at transfer time.

## Reusable tools produced

- **`tools/snes-gsu-trajectory-capture.lua`** — Mesen → the four
  `gsu_trajectory_vs_mesen` fixture files (the engine oracle).
- **`tools/snes-wram-perframe-hash.lua`** — Mesen side of the NMI-aligned
  WRAM differential (matches `luna wram-trace`).
- **`tools/snes-gsu-state-keys.lua`** — dumps Mesen `cart.coprocessor.*`
  getState keys (the Mesen→luna GSU register mapping reference).
- **`crates/luna-bus/src/superfx.rs`** `gsu_trajectory_vs_mesen` /
  `gsu_differential_vs_mesen` — the harnesses (env: `LUNA_GSU_DIFF_DIR`,
  `LUNA_SF_ROM`; pass a **headerless** `.sfc`).
- luna CLI: `--dma-trace` (+`--dma-trace-from`/`--dma-trace-max`),
  `--dump-coproc-ram`, `--dump-vram`, `--superfx-trace`, `wram-trace`.

## Related

- `docs/cooperative_scheduler_reference.md` — the GSU↔CPU timing model
  (which this investigation confirms is faithful; the bug is not there).
- The 128 KB Super FX RAM hardcode (`snes.rs`) is a genuine latent bug
  surfaced here — fix it to the `$FFBD` expansion-RAM size independently.
