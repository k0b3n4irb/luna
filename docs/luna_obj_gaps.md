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

## 🔴 1. Sprite tile index must wrap per-nibble (`& 0x0F`), page fixed

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
wrapping within the page. Visible on multi-tile sprites placed at page
edges. **Real bug — the headline finding.**

## 🔴 2. Vertical Y-wrap at 256 dropped

ares `onScanline` (`object.cpp:51-55`) uses `within<0,255>` — the
row-in-sprite is computed **mod 256**, so a sprite near the bottom
wraps to the top of the screen. luna (`renderer.rs:1493`):

```rust
let row_in_sprite = y.wrapping_sub(sp.y as u16);
if usize::from(row_in_sprite) >= sp.h as usize { continue; }
```

uses `u16` wrapping (mod 65536), not mod 256, so `y < sp.y` yields a
huge value and the wrapped rows are never drawn. Fix: mask `& 0xFF`.

---

## 🟠 3. No per-line 32-sprite / 34-tile limits (+ range/time over)

Hardware evaluates at most **32 sprites** and fetches at most **34
8×8 tiles** per scanline; excess is dropped (the source of sprite
flicker) and the overflow sets `$213E` bit 6 (range over) / bit 7
(time over) — ares `object.cpp:159-160`, Mesen2 `SnesPpu.cpp:610-614,
695-698`. luna renders **all 128 sprites** every line with no cap, so
games that rely on intentional flicker show none, and the status bits
read 0.

## 🟠 4. OAM priority rotation (`$2103` bit 7)

When OAM priority is set, the first sprite evaluated rotates to
`OAMADDR >> 2` (ares `object.cpp:6-9`, `setFirstSprite`). luna always
evaluates sprite 0..127 in fixed order. Coupled with #3 — it changes
which sprites survive the per-line cap.

---

## 🟡 Precision / rare

| # | Issue | refs | luna |
|---|---|---|---|
| 5 | Non-square (16×32 / 32×64) vflip is the *buggy* hardware flip — top/bottom halves mirror separately (`pos = W*3-1-yGap`) | ares `object.cpp:111-119`, Mesen2 `SnesPpu.cpp:716-732` | does a clean full-height flip |
| 6 | OBJ interlace — `height >> 1`, y-doubling, and the `baseSize≥6 → height 16` quirk | ares `oam.cpp:67`, `object.cpp:109,121-123` | not modelled |

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

1. **#1 tile `&0x0F` wrap** — real visible bug, the priority.
2. **#2 Y-wrap** — one-line fix.
3. **#3 per-line limits + range/time over**, then **#4 OAM rotation**.
4. 🟡 tail (#5 non-square vflip, #6 interlace).
