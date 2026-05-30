# luna Background subsystem — correctness gaps vs ares

Meticulous audit of luna's BG pipeline against the ares references
(`ares/sfc/ppu/background.cpp`, `io.cpp`, `mode7.cpp`, `dac.cpp`),
covering all 8 BG modes. Scope: the **runtime** path
(`render_bg_scanline_indexed_with` + the compositor in
`render_scanline_partial_into`), not the legacy single-BG helpers.

Authored 2026-05-30. Mode 7 affine + M7SEL already fixed (commit
`35be343`); this list tracks the remainder.

## Severity legend

- 🔴 visible bug, correct ROMs render wrong
- 🟠 whole feature missing — a class of games is affected
- 🟡 precision / edge deviation

---

## 🔴 1. Mode 0 — missing per-BG palette offset (`id << 5`)

ares `background.cpp:111-113`:

```cpp
u32 paletteOffset = self.io.bgMode == 0 ? id << 5 : 0;
u32 paletteSize   = 2 << io.mode;
tile.palette = paletteOffset + (tile.paletteGroup << paletteSize);
```

In Mode 0 each BG occupies its **own 32-colour CGRAM region**:
BG1→0, BG2→32, BG3→64, BG4→96. luna
(`renderer.rs:1235-1238`) computes `palette_off * 4 + idx` for **all**
BGs with no `id*32` term, so Mode 0 BG2/BG3/BG4 draw with BG1's
palette region — wrong colours. Mode 0 is common in menus / status
bars / early titles.

**Fix:** add `(bg_idx as u8) << 5` to the 2bpp CGRAM index when
`bgmode & 7 == 0`. Max index = 96 + 7*4 + 3 = 127, stays inside the
BG half of CGRAM. **(Being fixed now.)**

---

## ✅ 2. Offset-per-tile (Modes 2 / 4) — DONE (Mode 6 deferred)

ares `background.cpp:52-69` + `fetchOffset()` reinterpret BG3's tilemap
as per-column H/V offset words for BG1/BG2. Implemented in
`opt_scroll` + `bg3_tilemap_word`:

- Column D≥1 reads BG3 one tile to the left (the fetch-pipeline lag);
  column 0 always uses the plain scroll.
- H replaces scroll bits 3-9 keeping fine `&7`; V replaces fully.
  Mode 4 = one word (bit 15 picks H/V); Modes 2/6 = two words. Enable
  bit `0x2000` (BG1) / `0x4000` (BG2). Per-column, BG1/BG2 only.
- The two refs diverge on the BG3 lookup for 64-wide tilemaps (Mesen2
  linear vs ares quadrant); followed **ares** (matches `sample_bg_pixel`).
- Gated on modes 2/4 — modes 0/1/3/5/7 byte-identical (98 tests).
- Test `offset_per_tile_shifts_bg1_column`; **GUI-validated** on the
  Chrono Trigger title (the "TRIGGER" logo now waves per-column instead
  of rendering flat).

**Mode 6 deferred**: it is hi-res *and* OPT, so OPT must move into the
512-sampling path — rare (≈no commercial game uses Mode 6). Tracked
in 🟡 below.

## ✅ 3. Hi-res Modes 5/6 (512 px) — DONE (Option A, downsample 512→256)

Implemented as a per-dot two-subpixel render (left = sub screen, right
= main screen) averaged down into the 256-wide framebuffer — the CRT
horizontal blend. Faithful to ares `dac.cpp:39-40` and Mesen2
`SnesPpu.cpp:984,1010-1016`:

- `render_bg_scanline_indexed_hires` samples 512 columns (scroll
  doubled, 8-px hires tiles) → `(above[x]=col 2x+1, below[x]=col 2x)`.
- Compositor: hires `main`←`bgs_above`, `sub`←`bgs_below`, then
  `out[x] = average_bgr5(sub, main)`.
- `MODE56_TABLE` (Mode-1 order minus BG3) wired into `priority_table`.
- Shared `BgGeom` + `sample_bg_pixel` keep lores/hires in lockstep.
- Gated on `is_hires` → modes 0-4/7 are byte-identical to before.
- Tests: `hires_samples_two_distinct_subpixels_per_dot`; full suite
  (96) green. Not GUI-validated — no Mode 5/6 test ROM available.

**Pseudo-hires** (`$2133` bit 3) is also done: it reuses the same
main/sub interleave-and-average but with lores (256) BG content — the
transparency trick (Kirby waterfalls, Jurassic Park). Gated on the bit,
so the existing test ROMs (all `setini & 8 == 0`) are unchanged.

Still pending (follow-ups, see 🟡 below): **mosaic in hi-res** and
exact color-math on the sub subpixel.

## ✅ 4. Mode 7 EXTBG (BG2 overlay) — DONE

ares `mode7.cpp:44-52` + `io.cpp:733-738`. When `$2133` bit 6 is set in
Mode 7, the affine plane is also exposed as BG2: colour = pixel bits
0-6, priority = bit 7 (transparent when the low 7 bits are zero); BG1
keeps the full 8-bit colour. luna derives BG2 from the same rendered
plane in the compositor and selects a dedicated `MODE7_EXTBG_TABLE`
(numeric priorities OBJ3=7, OBJ2=6, BG2hi=5, OBJ1=4, BG1=3, OBJ0=2,
BG2lo=1).

Gated on `setini & 0x40` in Mode 7 — every other case is byte-identical
(99 tests). Test `mode7_extbg_splits_plane_into_bg1_and_bg2`. Not
GUI-validated (no test ROM enables EXTBG).

---

## 🟡 Precision / edge deviations

| # | Issue | ares ref | luna |
|---|---|---|---|
| 5 | Mode 7 mosaic not applied | `mode7.cpp:12-21` | absent |
| 6 | Direct-colour ignores tilemap palette-group low bits | `dac.cpp` | `renderer.rs:893` drops them |
| 7 | No per-mode character-address mask (VRAM wrap) | `background.cpp:104-106` | wraps only at 64 KB |
| 8 | Brightness 0 ≠ black — uses `(b+1)/16`, hw is `b/15` | dac LUT | `tile.rs:78` (all layers) |
| 9 | Legacy `render_bg1_scanline_with` is 32×32-only, diverged from runtime path | — | `renderer.rs:93` (trap for API consumers) |
| 11 | Mosaic not applied in the hi-res path | Mesen2 `SnesPpu.cpp:1026-1044` | `render_bg_scanline_indexed_hires` skips it |
| 12 | Hi-res sub-subpixel uses raw winner, not its own color-math | `dac.cpp:43-80` | approximated |
| 13 | Offset-per-tile in Mode 6 (hi-res + OPT) | `background.cpp:52-69` | OPT only wired into the lores path (modes 2/4) |

---

## ✅ Verified correct (do not regress)

- Tilemap base/size decode incl. 64×32 / 32×64 / 64×64 quadrant
  offsets (`+0x800/0x1000/0x1800`) — matches `background.cpp:84-89`.
- 16×16 tiles + flip-aware quadrant selection (`+1/+16/+17`).
- 2/4/8 bpp planar decode (`tile.rs`).
- 10-bit scroll; char base `(v&0xF)<<12` words; tilemap base
  `(v&0xFC)<<8` words (= ares `data.bit(2,7)<<10`).
- Priority tables for Modes 0/1 (both BG3-prio variants)/2/3.
- Direct-colour gating (8bpp + BG + CGWSEL bit 0); window / colour-math
  (already audited in `ppu_compositor_reference.md`).
- Mode 7 affine (fixed `35be343`).

## Suggested order

1. **#1 Mode 0 palette** — done (`f4e3d9b`).
2. **#3 hi-res 5/6 + pseudo-hires** — done (Option A downsample).
3. **#2 offset-per-tile (modes 2/4)** — done, GUI-validated on CT title.
4. **#4 Mode 7 EXTBG** — done.

All 🟠 gaps closed. Remaining work is the 🟡 tail: #6 direct-colour
palette-group bits, #7 character-address VRAM mask, #8 brightness
0=black, #5/#11 mosaic (Mode 7 / hi-res), #12 hi-res sub color-math,
#13 Mode-6 OPT, #9 legacy `render_bg1_scanline_with` cleanup.
