# SNES PPU compositor + DMA/OAM pipeline — reference spec

**Sources cross-checked**:
- ares: `ares/sfc/ppu/*.cpp`, `ares/sfc/cpu/dma.cpp,timing.cpp,io.cpp,irq.cpp`
- Mesen2: `Core/SNES/SnesPpu.cpp`, `Core/SNES/SnesPpuTypes.h`, `Core/SNES/DmaController.cpp`, `Core/SNES/InternalRegisters.cpp`

Per CLAUDE.md, claims here have agreement from both refs unless explicitly flagged "ARES-ONLY", "MESEN2-ONLY", or "DIVERGENCE". File-line cites use `<emu>:<file>:<line>` to disambiguate.

---

## 1. Pixel mixer overview

Per dot, both refs produce a final pixel by:

1. Selecting a **main-screen winner** (BG1..4 / OBJ / backdrop) via priority comparison.
2. Selecting a **sub-screen winner** the same way using TS instead of TM.
3. Applying the **window pipeline** to zero out windowed layers and to compute the per-region "above.colorEnable" / "below.colorEnable" flags from CGWSEL bits 7:6 and 5:4.
4. Applying **force-main-black** to the main pixel when above.colorEnable is false.
5. Applying **color math** (add/sub, optional halve) when below.colorEnable is true AND the winning above layer's CGADSUB enable bit is set AND (for OBJ) the palette index is ≥ 192.
6. Applying display brightness ($2100 bits 0-3).

Hi-res (mode 5/6) and pseudo-hi-res (SETINI bit 3) double the horizontal resolution to 512 pixels by separately emitting the sub-pixel and main-pixel on alternating dot positions. The non-hi-res case discards the sub-pixel and emits the main-pixel twice.

---

## 2. The above/below mixer

Both refs treat above and below symmetrically — same priority resolution, same per-layer enable (TM vs TS), same per-pixel winner stamping. The main–vs-sub asymmetry is *only* in:

- TM ($212C) gates above, TS ($212D) gates below.
- The DAC consumes them differently at the end: main is the displayed pixel; sub is one of two possible math operands.

### 2.1 Main-screen winner

Per ares `dac.cpp:82-118` and Mesen2 `SnesPpu.cpp:920-1066`:

For each layer in priority order, if the layer wrote a non-transparent pixel at this x, stamp:
- `math.above.color` = the layer's pixel color (CGRAM-indexed, except BG1 direct-color in modes 3/4/7).
- `math.below.colorEnable` = CGADSUB bit for that layer kind (BG1=bit0, BG2=bit1, BG3=bit2, BG4=bit3, OBJ=bit4 — and for OBJ, ALSO require palette ≥ 192 i.e. CGRAM index ≥ 192).

If no layer drew, the winner is the backdrop:
- `math.above.color` = CGRAM[0].
- `math.below.colorEnable` = CGADSUB bit 5 (back colorEnable).

Priority resolution uses a scalar comparator. Both refs flatten the layer×sub-priority space into one scalar so OBJ priority and BG priority are interleaved (e.g. ares' `obj.io.priority[] = {2,3,6,9}` for BG mode 1, BG3-priority — see ares:`ppu_io.cpp:640-741`).

### 2.2 Sub-screen winner

Same algorithm, gated by TS instead of TM. Both refs render a real sub-screen.

- ares: `dac.cpp:43-80` (`DAC::below`).
- Mesen2: same `RenderTilemap`/`RenderSprites` functions, with `drawSub == true` branch (`SnesPpu.cpp:993, 1012, 1020`).

If no sub-layer drew, the sub backdrop is CGRAM[0] (same as main backdrop) — there is **no separate sub-backdrop register**.

Mesen2 marks "sub had a real winner" as `_subScreenPriority[x] > 0`. ares uses a `math.transparent` boolean stamped by `below()` (see §4.3).

### 2.3 Final pixel selection

```
// pseudo-code combining ares dac.cpp:120-136 with Mesen2 SnesPpu.cpp:1302-1376

if (!window.above.colorEnable[x])  math.above.color = 0;   // force-main-black
if (!window.below.colorEnable[x])  math.below.colorEnable = false; // window-disables-math

if (!math.below.colorEnable) {
    // no math this dot
    return math.above.colorEnable ? math.above.color : 0;
}

// math enabled
operand_b = blendMode ? math.below.color : fixedColor;     // CGWSEL bit 1 selector
if (blendMode && math.transparent) {                       // sub had no real winner
    operand_b = fixedColor;
    colorHalve = false;                                    // disable halve
}
out = add_or_sub(math.above.color, operand_b, subtract, halve);
return out;
```

Order of operations is exact in both references: the force-main-black happens before the AllowColorMath/`math.below.colorEnable` gate, so even pixels where math is disabled can be blanked by the force-black rule.

---

## 3. Window pipeline

### 3.1 Per-window evaluation

For each x, evaluate Window1 (`x ∈ [WH0..WH1]`) and Window2 (`x ∈ [WH2..WH3]`). Hardware semantic per both refs: when `left > right` the window is empty, never matches.

### 3.2 Per-layer combiner

Each of the 6 entities (BG1, BG2, BG3, BG4, OBJ, math) has 4 bits in its `*SEL` nibble:
- bit 0: W1 invert (true → use `!w1_in`).
- bit 1: W1 enable.
- bit 2: W2 invert (true → use `!w2_in`).
- bit 3: W2 enable.

ares `window.cpp:41-47` (`Window::test`):
```
if(!oneEnable) return two && twoEnable;
if(!twoEnable) return one;
if(mask == 0) return (one | two);   // OR
if(mask == 1) return (one & two);   // AND
return (one ^ two) == 3 - mask;     // mask=2 XOR (== 1), mask=3 XNOR (== 0)
```

Mesen2 has equivalent logic in `ProcessMaskWindow<>`, branching on `(activeWindowCount, mask)`.

The combiner output for a layer is then masked against TMW/TSW to gate per-layer rendering on each screen.

### 3.3 Color-math window

The math window is a dedicated entry in the same array (ares: `io.col`, Mesen2: layer index 5 of the window state). Its enable/invert bits live in **`$2125` WOBJSEL high nibble** (math is the "logically 6th" layer alongside OBJ).

Both refs compute the math-window `value` per dot, then expand to two enable flags via the `array[] = {true, value, !value, false}` lookup at ares:`window.cpp:36-38`:

| CGWSEL[7:6] aboveMask | output.above.colorEnable | meaning |
|:---:|:---:|:---|
| 0 | true            | main never forced black |
| 1 | value           | main visible inside window, FORCED BLACK outside |
| 2 | !value          | main visible outside window, FORCED BLACK inside |
| 3 | false           | main always forced black |

| CGWSEL[5:4] belowMask | output.below.colorEnable | meaning |
|:---:|:---:|:---|
| 0 | true            | math enabled everywhere |
| 1 | value           | math enabled inside window only |
| 2 | !value          | math enabled outside window only |
| 3 | false           | math disabled everywhere |

Mesen2's `ColorWindowMode` enum (`SnesPpuTypes.h:13-19`) names the values:
- `Never = 0`, `OutsideWindow = 1`, `InsideWindow = 2`, `Always = 3`.

For force-black (`ColorMathClipMode`, bits 7:6):
- `OutsideWindow` (1) means "force black OUTSIDE the window" → main visible inside.
- `InsideWindow` (2) means "force black INSIDE the window" → main visible outside.

For math-region (`ColorMathPreventMode`, bits 5:4):
- `OutsideWindow` (1) means "math DISABLED outside the window" → math fires inside only.
- `InsideWindow` (2) means "math DISABLED inside the window" → math fires outside only.

Mesen2's enum names describe the *operand region* (where the rule is active); ares' `array[]` lookup describes the *flag value at that x*. Both decode to identical per-x behavior — verify by reading Mesen2 `SnesPpu.cpp:1302-1326` against ares `dac.cpp:120-122`.

---

## 4. Color math semantics

### 4.1 CGADSUB ($2131)

Both refs identical bit layout:
- bit 0..3: BG1..BG4 colorEnable.
- bit 4: OBJ colorEnable (gated by palette ≥ 192 at pixel-pick time).
- bit 5: backdrop colorEnable.
- bit 6: colorHalve.
- bit 7: subtract (0=add, 1=sub).

### 4.2 Blend semantics

Add (ares `dac.cpp:140-144`):
```
sum = x + y;
carry = (sum - ((x ^ y) & 0x0421)) & 0x8420;
result = (sum - carry) | (carry - (carry >> 5));
```
The `0x0421` / `0x8420` magic isolates inter-channel carries for the BGR555 packed representation. With halve: `(x + y - ((x^y) & 0x0421)) >> 1`.

Sub: `diff = x - y + 0x8420; borrow = (diff - ((x^y) & 0x8420)) & 0x8420; ...` — same channel-isolation trick going the other way.

Mesen2 expresses the same math but operates per-channel after unpacking — equivalent results on valid inputs.

### 4.3 The `math.transparent` fallback

**DIVERGENCE WORTH CALLING OUT** — both implement the same rule, but the trigger differs:

ares (`dac.cpp:124-130`):
```
if(io.blendMode && math.transparent) {
  math.blendMode  = false;          // operand becomes fixedColor
  math.colorHalve = false;          // halve disabled this dot
}
```
where `math.transparent = (priority == 0)` set at `dac.cpp:69` after sub-winner pick — i.e. true when the sub backdrop was the only sub thing.

Mesen2 (`SnesPpu.cpp:1354-1364`):
```
if(_subScreenPriority[x] > 0) {
    otherPixel = pixelB;            // real sub pixel
} else {
    otherPixel = _state.FixedColor; // empty sub → fixed
    halfShift = 0;                  // halve disabled
}
```

Same outcome. luna MUST implement this fallback or any dialog-box-style math against an empty sub region will halve a non-zero pixel and look wrong.

---

## 5. Direct color (CGWSEL bit 0)

When set, BG1 in modes 3/4/7 reinterprets its palette byte as a packed BGR triplet rather than indexing CGRAM:
- `palette = bbgggrr` (low 8 bits from tilemap).
- Optional `paletteGroup = -----bgr` (low 3 bits from tilemap's palette field).
- Both refs combine these to produce a 15-bit color: see ares `dac.cpp:159-167`.

luna implements this: `direct_color_to_bgr5(palette_index, group)`
(renderer.rs:924) decodes the 8-bit `BBGGGRRR` palette byte AND folds in
the 3-bit tilemap palette group (R←g0, G←g1, B←g2), so the paletteGroup
low bits are NOT dropped.

---

## 6. OBJ rendering

### 6.1 Per-scanline evaluation

Both refs walk OAM once per scanline checking sprite Y vs current line and OBSEL size. Up to 32 sprites per line, 34 tiles per line. Sprite overflow flags ($213E bits 6/7) set when caps are exceeded.

Both refs evaluate from sprite N where N is the OAM-priority rotation index (refreshed by OAM `$2104` writes via `setFirstSprite()` — ares `object.cpp:6-9`, Mesen2 `SnesPpu.cpp:1672-1675` `UpdateOamAddress` path).

### 6.2 OAM address reset at VBlank

**Both refs identical** — `InternalOamAddress = OamRamAddress << 1` at vcounter == vdisp, **only when force-blank is OFF**.

ares (`object.cpp:31-32`):
```
if(t.y == self.vdisp() && !self.io.displayDisable) addressReset();
```

Mesen2 (`SnesPpu.cpp:464-472`):
```
if(_scanline == _nmiScanline) {
    if(!_state.ForcedBlank) {
        _state.InternalOamAddress = (_state.OamRamAddress << 1);
    }
}
```

Additionally: a write to **$2100** that exits force-blank while at the vblank line triggers the same reset (ares `ppu_io.cpp:194`, Mesen2 `SnesPpu.cpp:1889-1896`).

This is critical for SMW: the game expects every NMI to start OAM-streaming at index 0. If the reset is missing, the OAM DMA lands at whatever the last-written `$2102/$2103` address was — which for many games is fine because they explicitly write `$2103=0` before the DMA, but games that rely on the auto-reset will end up with garbled or empty OAM.

### 6.3 OAM `$2104` write

Even-address writes are LATCHED until the odd-address write commits both atomically:

ares `ppu_io.cpp:223-236`:
```
n1 latchBit = io.oamAddress.bit(0);
n10 address = io.oamAddress++;
if(latchBit == 0) latch.oam = data;
if(address.bit(9)) {
  writeOAM(address, data);              // high table = direct byte
} else if(latchBit == 1) {
  writeOAM((address & ~1) + 0, latch.oam);
  writeOAM((address & ~1) + 1, data);
}
obj.setFirstSprite();                   // refresh OAM-priority rotation
```

luna's `memory.rs:425-454` (`Oam::write_gated`) implements the even-byte
latch correctly. luna does not maintain a *cached* firstSprite refreshed
on every `$2104` write the way ares' `setFirstSprite()` does; instead it
derives it on demand each scanline from the priority-rotation flag and
`$2103` word address — `Oam::first_sprite()` (memory.rs:407) returns
`(word_address >> 2) & 0x7F` when priority rotation is on, else 0. Same
observable result; the firstSprite index is just recomputed lazily rather
than stamped per write.

### 6.4 Sprite double-buffering

ares double-buffers the per-line tile cache: `t.active ^= 1` at start of each scanline, `obj.run()` consumes `tile[!t.active]` (filled the previous scanline), `obj.fetch()` fills `tile[t.active]` for next scanline. See ares `object.cpp:16-33, 61, 93`.

Mesen2 evaluates sprites for line N at the end of line N-1 — same effective behavior, different code shape.

luna decodes the sprite set **once per scanline** and shares it across the
whole line: `render_current_scanline` (ppu.rs:493) evaluates sprites once
and threads the decode into `render_scanline_partial_into_from` via the
`precomp` argument (renderer.rs:460, ppu.rs:511), so per-pixel composition
does not re-walk OAM. What luna does NOT do is ares' cross-scanline
double-buffer (fetch line N's tiles while running line N-1); it evaluates
the current line's sprites against live OAM at the start of that line. The
practical effect is the same for static OAM; only a game that rewrites OAM
mid-scanline expecting the previous line's fetched tiles to already be
latched would differ — and no commercial title in the corpus relies on it.

---

## 7. DMA / HDMA timing

### 7.1 General DMA ($420B)

Write to `$420B` with a 1-bit set per active channel:
- Both refs stall the CPU for the transfer duration.
- Transfers happen one channel at a time, in channel-number order.
- 8 cycles per byte transferred + overhead.

### 7.2 HDMA enable / service ($420C)

`$420C` enables HDMA channels for the *next* HDMA setup at the start of the next frame.

HDMA setup runs at H=6 of scanline 0 (visible frame start), resetting per-channel state. HDMA transfer runs on every visible scanline at H=278 (just before HBlank), performing one transfer per enabled channel based on the channel's repeat counter.

Both refs implement event-driven HDMA dispatch.

### 7.3 Auto-joypad-read ($4200 bit 0)

Fires at scanline `vdisp + 2.5` (NTSC line 227.5) when bit 0 of `$4200` is set. Reads `$4016/$4017` 16 times, writes the resulting bit-shifted-in values to `$4218..$421F`.

ares `cpu/timing.cpp` calls `joypadEdge()`; Mesen2 `InternalRegisters.cpp:ProcessAutoJoypadRead`.

While the auto-read is in progress, `$4212` bit 0 reads 1 ("auto-joypad-read busy").

### 7.4 HVBJOY ($4212)

- bit 0: auto-joypad-read busy.
- bit 6: HBlank — TRUE during HBlank (hcounter ≥ 274) of *every* line including non-visible. The recent luna commit (a802112-prev: `9d801f8 fix(core): $4212 HVBJOY bit 6 = live Hblank (ares)`) was about this.
- bit 7: VBlank — TRUE from scanline vdisp until line 261/311 (NTSC/PAL).

---

## 8. NMI / VBlank

- NMI fires at the start of vblank (scanline 225 NTSC non-overscan) when `$4200.7` is set.
- `$4210` (RDNMI) read returns the NMI flag in bit 7 and clears it (open-bus bits 0-6).
- IRQ is gated by `$4200.5` (V-IRQ) or `$4200.4` (H-IRQ).

---

## 8b. Read/write latches and write-twice quirks

### 8b.1 STAT78 ($213F) read resets the OPHCT/OPVCT byte flip-flop

OPHCT ($213C) and OPVCT ($213D) are 9-bit latched counters read one byte
at a time via a shared low/high **byte flip-flop**: the first read returns
the low byte and arms the flip-flop, the second returns bit 0 of the high
byte and disarms it.

Reading **STAT78 ($213F)** resets that flip-flop as a side effect, so the
*next* OPHCT/OPVCT read is guaranteed to return the LOW byte:

ares (`io.cpp:167-169`, the `$213f` case):
```
latch.hcounter = 0;
latch.vcounter = 0;
```

A handler that does not re-sync via $213F can desync the toggle and read
the high byte (0 for lines < 256) when it expected the low byte. This is
the **Doom-flicker root cause**: Doom's raster IRQ read V≈0, mis-dispatched
to its no-ack branch, and re-fired the H/V IRQ ~200×/frame.

luna implements this: reading STAT78 clears `ophct_hi_pending` /
`opvct_hi_pending` (ppu.rs:635-636) — the same read also clears the shared
BG-scroll write-twice latch and the external-latch-hit status bit
(ppu.rs:623-625).

### 8b.2 BG scroll ($210D-$2114) write-twice — TWO shared latches

The regular-BG scroll registers are write-twice into a pair of shared
8-bit latches `bgofs_ppu1` / `bgofs_ppu2` (ares `io.cpp:312-324`):

- **H-scroll write ($210D/$210F/$2111/$2113):** the composed 10-bit offset
  takes bits 3-9 from PPU1's *previous* byte (`bgofs_ppu1 & ~7`), bits 0-2
  from PPU2's *byte-before-that* (`bgofs_ppu2 & 7`), and bits 8-9 from the
  newly written high byte. **Both** latches then take the new byte. The
  cross-latch on the low 3 bits is the real hardware quirk — it only
  manifests when scroll registers interleave; a single latch mis-scrolls
  the sub-tile H offset.
- **V-scroll write ($210E/$2110/$2112/$2114):** uses the FULL previous-write
  latch (`bgofs_ppu1`, no PPU2 cross) and updates ONLY `bgofs_ppu1`.

luna implements both: `write_bg_h_scroll` (ppu.rs:678-691, dual-latch
cross) and `write_bg_v_scroll` (ppu.rs:695-699, PPU1-only). The Mode-7
M7HOFS/M7VOFS write-twice uses a *separate* `m7_latch` (ppu.rs:707-726),
not these BG-scroll latches.

---

## 9. Mid-frame register write latching

Both refs treat PPU register writes as **instantaneous, mid-scanline**. The pixel up to the write x position uses the OLD state; the rest of the scanline uses the NEW state. Implementation:
- ares dispatches per-cycle, so writes naturally interleave with rendering.
- Mesen2 calls `RenderScanline()` before applying a register write that affects rendering (`SnesPpu.cpp:1712-1714, 1884-1886`).

luna renders **per scanline**, not in one end-of-frame pass: the scheduler
calls `Ppu::render_current_scanline` (ppu.rs:493) at the end of every
visible line, committing it to the persistent framebuffer. So a register
write on line N is already seen by lines N+1.. — mid-frame tilemap/palette
changes for status-bar split, parallax, and HDMA-driven effects render
correctly at scanline granularity.

luna additionally models **mid-scanline** writes: a rendering-affecting
register write flushes the in-progress line up to the current dot via
`Ppu::flush_partial_scanline` (ppu.rs:526) so pixels left of the write
keep the OLD state and pixels right of it use the NEW state — matching
Mesen2's `RenderScanline()`-before-write model. The remaining gap vs ares
is purely sub-dot ordering, not whole-frame staleness.

---

## 10. Force-display / VRAM / CGRAM / OAM access gating

Both refs implement (ares `ppu_io.cpp:19-61`, Mesen2 also enforced):
- VRAM read/write during active display (forced-blank OFF, vcounter < vdisp) returns 0 / discards the write.
- CGRAM write during active display lands at `latch.cgramAddress` (the address-mux updated by `DAC::paletteColor()` per pixel), not the address the game programmed.
- OAM read/write during active display routes through `latch.oamAddress` (updated by the OBJ evaluator).

luna implements the VRAM and OAM gates: it tracks `Ppu::active_display`
(true when not forced-blank AND on a visible scanline) and routes the data
ports through gated writers — VRAM via `Vram::write_lo_gated` /
`write_hi_gated` (ppu.rs:825-826) and OAM via `Oam::write_gated`
(ppu.rs:764). When `active_display` is true the byte is dropped but the
address counter (and OAM even/odd latch) still advance, matching ares
(`ppu_io.cpp:40-45`) and Mesen2 (`SnesPpu.cpp:1916-1927`).

CGRAM is **deliberately ungated** (`Cgram::write`, memory.rs:261): on real
hardware an active-display CGRAM write still commits, just at the
DAC's `latch.cgramAddress` rather than the programmed address. luna commits
at the programmed address — it does not yet model the per-pixel
address-mux, so the *value* lands but at the wrong slot only in the rare
case a game writes CGRAM mid-active-line. This is the one remaining
sub-quirk; the blanket "implements none of these gates" was stale.

---

## 11. Mode-7 and EXTBG

Mode 7 (BGMODE=7): BG1 is a 1024×1024 affine-transformed 8bpp tilemap. M7A/M7B/M7C/M7D matrix (signed 8.8), M7X/M7Y center (signed 13-bit).

EXTBG (SETINI bit 6): BG2 reuses the Mode-7 framebuffer with priority bits
from the high tile-byte — used by F-Zero, Pilotwings.

luna implements EXTBG: when `BGMODE == 7 && (setini & 0x40) != 0`
(renderer.rs:497) it exposes the affine plane as BG2, deriving the colour
from the low 7 bits of the 8bpp pixel and the priority from bit 7
(renderer.rs:536-538), and composites it via `MODE7_EXTBG_TABLE`
(renderer.rs:1165). Mode 7 is no longer BG1-only.

---

## Appendix: CGWSEL / CGADSUB / SETINI bit-level cross-check

| Reg.bits | Meaning | ares field | Mesen2 field |
|:---|:---|:---|:---|
| $2130.0 | direct color | `io.directColor` | `_state.DirectColorMode` |
| $2130.1 | sub-as-math-operand | `io.blendMode` | `_state.ColorMathAddSubscreen` |
| $2130.5:4 | math-region (4 values) | `window.io.col.belowMask` | `_state.ColorMathPreventMode` (`Never/OutsideWindow/InsideWindow/Always`) |
| $2130.7:6 | force-main-black region (4 values) | `window.io.col.aboveMask` | `_state.ColorMathClipMode` (same enum) |
| $2131.0..3 | BG1..BG4 colorEnable | `dac.io.bgN.colorEnable` | `_state.ColorMathEnabled & (1<<N)` |
| $2131.4 | OBJ colorEnable (+ pal≥192) | `dac.io.obj.colorEnable` | `_state.ColorMathEnabled & 0x10` |
| $2131.5 | back colorEnable | `dac.io.back.colorEnable` | `_state.ColorMathEnabled & 0x20` |
| $2131.6 | colorHalve | `dac.io.colorHalve` | `_state.ColorMathHalveResult` |
| $2131.7 | subtract | `dac.io.colorMode` | `_state.ColorMathSubtractMode` |
| $2132 | fixed color (R/G/B select bits 7/6/5, value low 5) | per-channel | `_state.FixedColor` |
| $2133.6 | EXTBG | `io.extbg` | `_state.ExtBgEnabled` |
| $2133.5 | hi-res | | `_state.HiResMode` |
| $4200.0 | auto-joypad-read enable | | `_state.EnableAutoJoypadRead` |
| $4200.7 | NMI enable | | `_state.EnableNmi` |
| $4212.0 | auto-joypad busy | | per-frame flag |
| $4212.6 | live HBlank | | live hcounter check |
| $4212.7 | VBlank | | `_state.InVblank` |
