# luna Sprite / OBJ subsystem — correctness gaps vs ares + Mesen2

Meticulous audit of luna's sprite path against ares
(`ares/sfc/ppu/object.cpp`, `oam.cpp`, `io.cpp`) and Mesen2
(`SnesPpu.cpp` sprite fetch/eval), companion to `luna_bg_gaps.md`.

Scope: the runtime sprite path — `decode_all_sprites`,
`render_sprites_scanline_indexed_with`, `sprite_size_pair`,
`sprite_tile_byte_offset` (all in `renderer.rs`) and the OAM module.

Authored 2026-05-30.

## Severity legend

- 🔴 visible bug, correct ROMs render wrong
- 🟠 whole feature missing
- 🟡 precision / rare

---

> **Status:** #1 and #2 fixed (commit pending). The remaining open
> items are #3, #4 (🟠) and #5, #6 (🟡).

## ✅ 1. Sprite tile index per-nibble wrap (`& 0x0F`), page fixed — DONE

Both refs (ares `object.cpp:130-146`, Mesen2 `SnesPpu.cpp:735-741`)
build the component tile index by wrapping the **low and high nibbles
of the OAM character byte independently** within the 16×16 name page,
with the page (`nameselect`) held fixed:

```
row    = (charHigh + rowOffset)    & 0x0F      // vertical wrap
column = (charLow  + columnOffset) & 0x0F      // horizontal wrap
tileIndex = (row << 4) | column                // page never changes
```

luna (`renderer.rs:1514`) instead does a **linear 9-bit add**:

```rust
let tile_id = (sp.tile.wrapping_add((tile_y * 16 + tile_x) as u16)) & 0x01FF;
```

with `sp.tile` already folding `nameselect` into bit 8. So a sprite
whose tiles sit at the right or bottom edge of the name page carries
into the next tile **row** (or flips the page via bit 8) instead of
wrapping within the page. **Fixed**: nibbles now wrap independently
(`(charLow + tile_x) & 0x0F`, `(charHigh + tile_y) & 0x0F`) with the
page held fixed. Test `sprite_tile_index_wraps_within_name_page`.

## ✅ 2. Vertical Y-wrap at 256 — DONE

ares `onScanline` (`object.cpp:51-55`) uses `within<0,255>` — the
row-in-sprite is computed **mod 256**, so a sprite near the bottom
wraps to the top of the screen. luna (`renderer.rs:1493`):

```rust
let row_in_sprite = y.wrapping_sub(sp.y as u16);
if usize::from(row_in_sprite) >= sp.h as usize { continue; }
```

used `u16` wrapping (mod 65536), not mod 256, so `y < sp.y` yielded a
huge value and the wrapped rows were never drawn. **Fixed**: mask
`& 0xFF`. Test `sprite_wraps_from_bottom_to_top`. (Also: the legacy
`render_sprites_scanline` now delegates to the indexed renderer, so the
two no longer diverge.)

---

## ✅ 3. Per-line 32-sprite / 34-tile limits (+ range/time over) — DONE

Hardware evaluates at most **32 sprites** and fetches at most **34
8×8 tiles** per scanline; excess is dropped (the source of sprite
flicker) and the overflow sets `$213E` bit 6 (range over) / bit 7
(time over) — ares `object.cpp:159-160`, Mesen2 `SnesPpu.cpp:610-614,
695-698`. Implemented in `evaluate_sprite_line`:

- Pass 1 keeps the first 32 on-line sprites from `firstSprite`
  (priority order) → range over.
- Pass 2 fetches **back-to-front** and keeps sprites while the
  cumulative on-screen tile count stays within 34 — so the *front*
  sprites overflow the budget and drop (the SNES tile-over quirk). Tile
  total > 34 → time over.
- `$213E` bits 6/7 now reflect the flags (accumulated per frame, reset
  at line 0). The renderer draws only the survivors.

Tests `sprite_range_over_drops_33rd_sprite`, `sprite_time_over_drops_
front_sprite`. (Drop is at sprite granularity, not ares' per-tile — a
boundary sprite drops whole rather than partial; negligible and only
in already-overflowing scenes.)

## ✅ 4. OAM priority rotation (`$2103` bit 7) — DONE

When OAM priority is set, the first sprite evaluated rotates to
`word_address >> 2` (ares `object.cpp:6-9`, `setFirstSprite`).
Implemented via `Oam::first_sprite()`, feeding `evaluate_sprite_line`.
Test `oam_priority_rotation_changes_winner`. Previously luna always
evaluates sprite 0..127 in fixed order. Coupled with #3 — it changes
which sprites survive the per-line cap.

---

## 🟡 Precision / rare

| # | Issue | refs | status |
|---|---|---|---|
| ~~5~~ | ~~Non-square (16×32 / 32×64) vflip~~ — **DONE**: rectangular sprites now use the buggy hardware flip (top/bottom halves mirror separately, `3*w-1-row`); square sprites keep the plain `h-1-row` | ares `object.cpp:111-119`, Mesen2 `SnesPpu.cpp:716-732` | ✅ |
| ~~6~~ | ~~OBJ interlace~~ — **DONE (Phase D)**: screen height halved (`height>>1`, `object.cpp:53`); a screen row samples sprite row `screen_row*2+field`, vflip-aware (`object.cpp:109,121-122`); `baseSize≥6 → height 16` quirk (`oam.cpp:67`). **Gated on SETINI bit 1** (`obj.io.interlace = data.bit(1)`, ares `io.cpp`) — SEPARATE from BG/screen interlace (bit 0). The first cut wrongly gated on bit 0, which garbled RPM Racing's sprite logo (sets bit 0 only, no OBJ interlace); fixed to bit 1. Validated: InterlaceRPG hero (sets `%11`, both bits) half-height ✓; RPM Racing logo crisp (GUI-confirmed) ✓. Guard `obj_interlace_gates_on_setini_bit1_not_bit0`. | ares `oam.cpp:67`, `object.cpp:109,121-123`, `io.cpp` | ✅ |

---

## ✅ Verified correct (do not regress)

- **Size-pair table** for all 8 `OBSEL[7:5]` codes — matches ares
  `oam.cpp:55-73` / Mesen2 exactly, including code 7's large = 32×32.
- **9-bit X** sign + off-screen/wrap visibility: luna's signed
  `x -= 512` is equivalent to ares' 9-bit wrap + the
  `x>256 && x+w-1<512` dead-zone skip (same visible pixels).
- **Name-page select**: page1 = `(nameselect+1) << 12` words
  (`sprite_tile_byte_offset`), = ares `object.cpp:129`.
- **Palette** `128 + palette*16 + idx`; **priority** via the per-mode
  priority tables; **OBJ colour-math ≥ 192** gate (already audited).
- OAM read/write decode, high-table X-bit-8 + size bit.

## Suggested order

1. ~~#1 tile `&0x0F` wrap~~ — **done**.
2. ~~#2 Y-wrap~~ — **done**.
3. ~~#3 per-line limits + range/time over~~ — **done**.
4. ~~#4 OAM priority rotation~~ — **done**.
5. ~~#5 non-square vflip~~ — **done**.

All items are now done, including #6 (OBJ interlace, Phase D). The entire
sprite audit is complete; interlace is implemented end-to-end (`bg_gaps`
#16 Phases A-C for BG/blend, this #6 for OBJ).

## Salvaged PPU-compositor residuals (from the retired `luna_ppu_gaps.md`)

The old SMW-Yoshi's-House worksheet (`luna_ppu_gaps.md`) was deleted — its
marquee bugs (force-black polarity, sub-screen compositor, OAM auto-reset,
empty-sub fallback, direct-color group bits, EXTBG, hi-res) are all FIXED and
verified in `renderer.rs`. Three genuinely-open, minor items are preserved here
so they aren't lost:

- **OBJ cross-scanline sprite fetch-ahead** — ares evaluates line N+1's sprites
  during line N; luna decodes once per scanline with no fetch-ahead. Cosmetic
  at most (affects only exact mid-OAM-write timing).
- **PPU register read open-bus** — reads of write-only / unmapped `$21xx` return
  a fixed value, not the PPU MDR open-bus latch (`ppu.rs` read fallthrough).
- **General mid-scanline register latching** — per-scanline render + a partial
  mid-scanline flush exist (`flush_partial_scanline`), but not every register is
  latched at its exact dot.
