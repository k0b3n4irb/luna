# ares SNES PPU — Compositor/Window/DAC Reference Notes

Source pulled fresh from `https://raw.githubusercontent.com/ares-emulator/ares/master/ares/sfc/` into `/tmp/ares/`.

Note on missing files: the request listed `window.hpp`, `dac.hpp`, `background.hpp`, `object.hpp`, and `cpu/joypad.cpp`. None of these exist as standalone files. Per `gh api repos/ares-emulator/ares/contents/ares/sfc/ppu`, the PPU directory has only `*.cpp` plus one umbrella `ppu.hpp`; every PPU subsystem (Background, Object, Window, DAC, OAM, Mosaic) is defined as a nested struct inside the `PPU` class in `ppu.hpp`. Joypad/auto-poll logic lives in `cpu/timing.cpp` (`joypadEdge()`) and `cpu/io.cpp` ($4200, $4016, $4017, $4218..$421F handlers). I fetched the surrounding files (`ppu/main.cpp`, `ppu/color.cpp`, `ppu/mosaic.cpp`, `cpu/irq.cpp`, `cpu/cpu.hpp`) to cover everything actually referenced.

Hereafter all line citations are `<file>:<line>`.

---

## 1. The above/below mixer (`dac.cpp`)

### 1.1 Scanline reset (`dac.cpp:1-29`)

```
auto PPU::DAC::scanline() -> void {
  line = nullptr;
  ...
  math.above.color = paletteColor(0);
  math.below.color = math.above.color;
  math.above.colorEnable = false;
  math.below.colorEnable = false;
  math.transparent = true;
  math.blendMode   = false;
  math.colorHalve  = io.colorHalve && !io.blendMode && math.above.colorEnable;
}
```

Important: at scanline start `math.transparent` is initialised true and `math.below.color` is preloaded with palette entry 0 (the backdrop colour). This matters because `below()` only overwrites `math.below.color` when a non-backdrop layer wins. The "first hires pixel of each scanline is transparent" comment at `dac.cpp:18` is the rationale for the explicit init.

### 1.2 Per-dot dispatch (`dac.cpp:31-41`)

```
auto PPU::DAC::run() -> void {
  if(self.vcounter() == 0) return;

  bool hires      = self.io.pseudoHires || self.io.bgMode == 5 || self.io.bgMode == 6;
  auto belowColor = below(hires);
  auto aboveColor = above();

  if(!line) return;
  *line++ = self.io.displayBrightness << 15 | (hires ? belowColor : aboveColor);
  *line++ = self.io.displayBrightness << 15 | (aboveColor);
}
```

Two contiguous u32 words are written per dot. In hires modes the buffer stores `[below, above]` and the screen rescaler emits 512 effective dots per line; in non-hires it stores `[above, above]` (the first pixel is overwritten by the second slot's "above" copy when sampled at 256-wide). The 564-pixel line span comes from `dac.cpp:9`.

`displayBrightness` is OR'd in as bits 15..18 of each u32 — that is the path the screen palette callback (`color.cpp`) uses to apply INIDISP brightness scaling.

### 1.3 `below()` — sub-screen winner selection (`dac.cpp:43-80`)

```
auto PPU::DAC::below(bool hires) -> n16 {
  if(self.io.displayDisable || (!self.io.overscan && self.vcounter() >= 225)) return 0;

  u32 priority = 0;
  if(bg1.output.below.priority) {
    priority = bg1.output.below.priority;
    if(io.directColor && (self.io.bgMode == 3 || self.io.bgMode == 4 || self.io.bgMode == 7)) {
      math.below.color = directColor(bg1.output.below.palette, bg1.output.below.paletteGroup);
    } else {
      math.below.color = paletteColor(bg1.output.below.palette);
    }
  }
  if(bg2.output.below.priority > priority) {
    priority = bg2.output.below.priority;
    math.below.color = paletteColor(bg2.output.below.palette);
  }
  ...
  if(obj.output.below.priority > priority) {
    priority = obj.output.below.priority;
    math.below.color = paletteColor(obj.output.below.palette);
  }
  if(math.transparent = (priority == 0)) math.below.color = paletteColor(0);

  if(!hires) return 0;
  if(!math.below.colorEnable) return math.above.colorEnable ? math.below.color : (n15)0;

  return blend(
    math.above.colorEnable ? math.below.color : (n15)0,
    math.blendMode ? math.above.color : fixedColor()
  );
}
```

Key points:

- **All five layers (BG1..4 + OBJ) compete on the sub-screen** by `>` priority comparison. The TS register (`$212D`, ppu_io.cpp:567) sets `bg*.io.belowEnable / obj.io.belowEnable`, and those gates are checked inside `Background::run` / `Object::run` to populate `output.below`. Whether the layer participates is determined upstream — by the time `below()` runs, only layers TS-enabled have non-zero `output.below.priority`.
- **Backdrop**: when no layer wins (`priority == 0`), `math.below.color = paletteColor(0)` and `math.transparent` is set true. This is the sub-screen's backdrop.
- **Direct color** only applies to BG1 in the below path, gated on `io.directColor && (bgMode==3|4|7)` (`dac.cpp:49`). BG2..4 and OBJ never go through `directColor()` even in mode 3.
- **`math.transparent`** is the assignment expression on `dac.cpp:71` (`math.transparent = (priority == 0)`) — yes, that's an assignment, so it equals the parenthesised value. This flag is consumed by `above()` later to decide whether the math operand should be force-zeroed for transparent sub pixels.
- **Non-hires return**: `if(!hires) return 0;` — `below()`'s return value is only meaningful in hires modes (5, 6, or pseudoHires). In non-hires it simply updates `math.*` state for `above()` to consume; the returned 0 is discarded by `run()` because it stores `aboveColor` into both slots.
- **Hires return**: when the sub doesn't participate in colour math (`!math.below.colorEnable`), it returns the raw `math.below.color` if main is on the screen, else 0 (force-main-black case). When math is enabled, it `blend`s the sub against either the above pixel (`blendMode==1`, i.e. CGWSEL bit 1 = "add main") or the fixed color (`blendMode==0`).

### 1.4 `above()` — main-screen winner and colour math (`dac.cpp:82-136`)

```
auto PPU::DAC::above() -> n16 {
  if(self.io.displayDisable || (!self.io.overscan && self.vcounter() >= 225)) return 0;

  u32 priority = 0;
  if(bg1.output.above.priority) {
    priority = bg1.output.above.priority;
    if(io.directColor && (self.io.bgMode == 3 || self.io.bgMode == 4 || self.io.bgMode == 7)) {
      math.above.color = directColor(bg1.output.above.palette, bg1.output.above.paletteGroup);
    } else {
      math.above.color = paletteColor(bg1.output.above.palette);
    }
    math.below.colorEnable = io.bg1.colorEnable;
  }
  if(bg2.output.above.priority > priority) {
    priority = bg2.output.above.priority;
    math.above.color = paletteColor(bg2.output.above.palette);
    math.below.colorEnable = io.bg2.colorEnable;
  }
  ...
  if(obj.output.above.priority > priority) {
    priority = obj.output.above.priority;
    math.above.color = paletteColor(obj.output.above.palette);
    math.below.colorEnable = io.obj.colorEnable && obj.output.above.palette >= 192;
  }
  if(priority == 0) {
    math.above.color = paletteColor(0);
    math.below.colorEnable = io.back.colorEnable;
  }

  if(!window.output.below.colorEnable) math.below.colorEnable = false;
  math.above.colorEnable = window.output.above.colorEnable;
  if(!math.below.colorEnable) return math.above.colorEnable ? math.above.color : (n15)0;

  if(io.blendMode && math.transparent) {
    math.blendMode  = false;
    math.colorHalve = false;
  } else {
    math.blendMode  = io.blendMode;
    math.colorHalve = io.colorHalve && math.above.colorEnable;
  }

  return blend(
    math.above.colorEnable ? math.above.color : (n15)0,
    math.blendMode ? math.below.color : fixedColor()
  );
}
```

This is *the* compositor. Step-by-step:

1. **Lines 82-83 — force-blank** (and over-VBlank short-circuit). Returns hardware-black 0 (which, after the brightness OR in `run()`, is still black). Note overscan affects the cutoff: 225 if overscan disabled, else 240 (`updateVideoMode`, ppu_io.cpp:641). The DAC always uses 225 as the hard limit when overscan is off, not vdisp().
2. **Lines 85-114 — winner pick across BG1..4, OBJ** by `>` priority. Priorities come from `bg*.io.priority[]` (`updateVideoMode`, ppu_io.cpp:644-740) — these are the 4..12 numbers ares uses internally to fold sub-priority and layer-priority into one scalar.
3. **For each winning layer, `math.below.colorEnable` is set to the corresponding CGADSUB layer bit** (`io.bg1.colorEnable`, `io.obj.colorEnable && palette>=192`, etc., from $2131 — ppu_io.cpp:607-616). This is *the* place ares decides "did the math-on-this-pixel flag fire" — by inspecting whether the **main-screen winner** is a layer marked in $2131.
4. **OBJ math gate (line 113)**: `math.below.colorEnable = io.obj.colorEnable && obj.output.above.palette >= 192;`. Sprites only participate in colour math if their palette index is in the upper quarter (palettes 4..7, indices 128+0x40 .. 255). This is the famous "sprite palettes 4..7 only" hardware rule.
5. **Backdrop winner (lines 115-118)**: `math.above.color = paletteColor(0)` and `math.below.colorEnable = io.back.colorEnable` (CGADSUB bit 5).
6. **Window gate (lines 120-121)**:
   - `if(!window.output.below.colorEnable) math.below.colorEnable = false;` — the color-math-region window can mask off math.
   - `math.above.colorEnable = window.output.above.colorEnable;` — the force-main-black window directly assigns to `math.above.colorEnable`. **This is the critical bit.** Note this is `math.above.colorEnable`, not whether the layer drew — it tracks "is main screen visible at this dot". When this is false at the final return, the function returns 0 (lines 122 and 133).
7. **Early return when math is off (line 122)**: `if(!math.below.colorEnable) return math.above.colorEnable ? math.above.color : (n15)0;` — no math, just emit the winning above colour, or black if force-main-black is active.
8. **Math fixup for transparent sub (lines 124-130)**: if `blendMode` (CGWSEL bit 1, "use sub-screen as math operand") is on AND `math.transparent` is true (the sub had no layer winner this dot, falling to backdrop), `blendMode` and `colorHalve` are cleared — so an "add sub" with a transparent sub does *not* halve. Otherwise the configured `blendMode` and `colorHalve` are latched, with `colorHalve` gated by `math.above.colorEnable` (only halve when main is on the screen).
9. **Blend (lines 132-135)**: `blend(main_or_0, blendMode ? below_color : fixedColor)`. So:
   - blendMode=0 → math vs **fixed color** (COLDATA $2132).
   - blendMode=1 → math vs **sub-screen winning color**.

### 1.5 `blend()` add/sub with halve (`dac.cpp:138-156`)

```
auto PPU::DAC::blend(u32 x, u32 y) const -> n15 {
  if(!io.colorMode) {  //add
    if(!math.colorHalve) {
      u32 sum = x + y;
      u32 carry = (sum - ((x ^ y) & 0x0421)) & 0x8420;
      return (sum - carry) | (carry - (carry >> 5));
    } else {
      return (x + y - ((x ^ y) & 0x0421)) >> 1;
    }
  } else {  //sub
    u32 diff = x - y + 0x8420;
    u32 borrow = (diff - ((x ^ y) & 0x8420)) & 0x8420;
    if(!math.colorHalve) {
      return   (diff - borrow) & (borrow - (borrow >> 5));
    } else {
      return (((diff - borrow) & (borrow - (borrow >> 5))) & 0x7bde) >> 1;
    }
  }
}
```

`io.colorMode` is CGADSUB bit 7 (0=add, 1=sub, ppu_io.cpp:614). `colorHalve` is CGADSUB bit 6. Standard 15bpp parallel-channel saturating add and clamping sub.

### 1.6 `paletteColor()` / `directColor()` / `fixedColor()` (`dac.cpp:158-174`)

`paletteColor()` reads `cgram[palette]` and also writes `self.latch.cgramAddress` — this is the "CGRAM read latch" the CPU sees when reading $213B during active display (`readCGRAM`, ppu_io.cpp:47-53 forces address to `latch.cgramAddress` during hcounter 88..1096 of an active scanline).

`directColor()` does the mode-3/4/7 BG1 direct mapping:
```
//palette = -------- BBGGGRRR
//group   = -------- -----bgr
//output  = 0BBb00GG Gg0RRRr0
```

`fixedColor()` packs $2132's COLDATA R/G/B into a 15bpp value.

---

## 2. Window pipeline (`window.cpp`, `ppu.hpp:431-484`)

### 2.1 Per-dot window evaluation (`window.cpp:5-39`)

```
auto PPU::Window::run() -> void {
  bool one = (x >= io.oneLeft && x <= io.oneRight);
  bool two = (x >= io.twoLeft && x <= io.twoRight);
  x++;

  if(test(io.bg1.oneEnable, one ^ io.bg1.oneInvert, io.bg1.twoEnable, two ^ io.bg1.twoInvert, io.bg1.mask)) {
    if(io.bg1.aboveEnable) bg1.output.above.priority = 0;
    if(io.bg1.belowEnable) bg1.output.below.priority = 0;
  }
  ...
  bool value = test(io.col.oneEnable, one ^ io.col.oneInvert, io.col.twoEnable, two ^ io.col.twoInvert, io.col.mask);
  bool array[] = {true, value, !value, false};
  output.above.colorEnable = array[io.col.aboveMask];
  output.below.colorEnable = array[io.col.belowMask];
}
```

For each of BG1..4 + OBJ, if the per-layer window mask evaluates true:
- `aboveEnable` (TMW $212E) being set → zero out the layer's `output.above.priority` (kill main screen at this dot).
- `belowEnable` (TSW $212F) being set → zero out the layer's `output.below.priority` (kill sub at this dot).

So the per-layer window in ares is *destructive*: it nukes the layer's contribution before `DAC::above`/`below` see it.

For the color-math window the result is non-destructive — it sets `output.above.colorEnable` / `output.below.colorEnable` which `DAC::above` consumes (`dac.cpp:120-121`).

### 2.2 The `{true, value, !value, false}` array (`window.cpp:36-38`)

```
bool array[] = {true, value, !value, false};
output.above.colorEnable = array[io.col.aboveMask];
output.below.colorEnable = array[io.col.belowMask];
```

`io.col.aboveMask` is CGWSEL bits 7:6 (ppu_io.cpp:601). `io.col.belowMask` is CGWSEL bits 5:4 (ppu_io.cpp:600). The two-bit value indexes into:

| index | meaning of array entry | `aboveMask` ("force main black" region) | `belowMask` ("color-math enable" region) |
|---:|---|---|---|
| 0 | `true`  | main **always on** → never force black | math **always on**  |
| 1 | `value` | main on **iff `value` is true** | math on **iff `value` is true** |
| 2 | `!value`| main on **iff `value` is false** | math on **iff `value` is false** |
| 3 | `false` | main **always off** → always force black | math **always off** |

Here `value` is the result of `test()` on the **colour window** registers (oneEnable/oneInvert/twoEnable/twoInvert/mask drawn from $2125 bits 4..7 and $212B bits 2..3, ppu_io.cpp:509-512 and 552).

The convention `output.above.colorEnable == true` means **main screen is visible at this dot** (the DAC will emit the winning above colour). The convention is therefore *inverted* from what the register-name suggests — the bits in CGWSEL 7:6 are nocash's "clip-to-black" bits, where the documented values are "never / outside / inside / always force-black". In ares:

- aboveMask=0 → `array[0]=true` → main visible (nocash "never force black"). MATCH.
- aboveMask=1 → `array[1]=value` → main visible **inside** the colour window region (where `value` is true). Nocash says aboveMask=1 = "outside window". DIVERGENCE in nocash naming but they describe the same observable. Note: `value` here is "are we inside the configured colour window combination"; if the configured window is just W1 with no invert, then `value` is true *inside* W1. Nocash documents the bit-1 as "outside" because of how it composes with the typical configuration. Look at the resulting behaviour, not the label.

The four `aboveMask` values map cleanly to nocash's "clip colour to black before math" semantics once you carry through what `value` evaluates to per configuration.

### 2.3 Per-layer mask combiner (`window.cpp:41-47`)

```
auto PPU::Window::test(bool oneEnable, bool one, bool twoEnable, bool two, u32 mask) -> bool {
  if(!oneEnable) return two && twoEnable;
  if(!twoEnable) return one;
  if(mask == 0) return (one | two);
  if(mask == 1) return (one & two);
                return (one ^ two) == 3 - mask;
}
```

Decoded:
- One off, Two on → result = two.
- Two off, One on → result = one.
- Both off → result = `two && twoEnable` = false (short-circuit on first branch).
- Both on:
  - mask=0 → OR
  - mask=1 → AND
  - mask=2 → `(one^two)==1` → XOR
  - mask=3 → `(one^two)==0` → XNOR

This matches the SNES "mask logic" register WBGLOG/WOBJLOG ($212A/$212B) semantics: 00 OR, 01 AND, 10 XOR, 11 XNOR.

`one` and `two` are passed in already XOR'd with the per-layer invert bits (`window.cpp:10`), and the invert toggles match the in-register convention where the "invert" bit flips whether the window means "inside" or "outside".

### 2.4 Register mapping (ppu_io.cpp:478-554)

- **$2123 W12SEL** (ppu_io.cpp:479-487): BG1/BG2 win1Invert, win1Enable, win2Invert, win2Enable per nibble.
- **$2124 W34SEL** (ppu_io.cpp:492-500): BG3/BG4.
- **$2125 WOBJSEL** (ppu_io.cpp:505-513): OBJ in low nibble, **COLOR-MATH window** in upper nibble (bits 4-7).
- **$2126..$2129 WH0..WH3** (ppu_io.cpp:517-538): window 1 L/R, window 2 L/R.
- **$212A WBGLOG** (ppu_io.cpp:541-547): BG1..4 mask combiner.
- **$212B WOBJLOG** (ppu_io.cpp:550-554): OBJ low nibble, COLOR-MATH mask combiner upper nibble (bits 2-3).
- **$212E TMW** (ppu_io.cpp:577-584): per-layer window-on-main enable.
- **$212F TSW** (ppu_io.cpp:587-594): per-layer window-on-sub enable.
- **$2130 CGWSEL** (ppu_io.cpp:597-602):
  ```
  dac.io.directColor      = data.bit(0);  // bit 0
  dac.io.blendMode        = data.bit(1);  // bit 1
  window.io.col.belowMask = data.bit(4,5); // bits 5..4
  window.io.col.aboveMask = data.bit(6,7); // bits 7..6
  ```

---

## 3. Force-main-screen-black behavior

The whole story lives in `DAC::above` (`dac.cpp:120-122`):

```
if(!window.output.below.colorEnable) math.below.colorEnable = false;
math.above.colorEnable = window.output.above.colorEnable;
if(!math.below.colorEnable) return math.above.colorEnable ? math.above.color : (n15)0;
```

And the value of `math.above.colorEnable` is taken straight from `window.output.above.colorEnable`, which is `array[io.col.aboveMask]` (`window.cpp:37`).

### Per-aboveMask behaviour table

Let `value = test(col.oneEnable, one^col.oneInvert, col.twoEnable, two^col.twoInvert, col.mask)`. This is the *colour-math window* test (driven by $2125 bits 4..7, $212B bits 2..3).

| `aboveMask` ($2130 bits 7:6) | `output.above.colorEnable` | Per-dot behaviour |
|:---:|:---|:---|
| 0 | always `true` | Main screen never forced black. |
| 1 | `value` | Main screen visible where `value`==true; **forced black** where `value`==false. |
| 2 | `!value` | Main screen visible where `value`==false; **forced black** where `value`==true. |
| 3 | always `false` | Main screen always forced black. |

Then in `above()`:
- If math is off (no math layer hit): return `math.above.color` if `colorEnable`, else `0`. So values 1/2/3 produce hardware-black on the forced dots.
- If math is on: `blend(math.above.colorEnable ? math.above.color : 0, ...)` — so the **main operand** of the blend is zeroed in the forced-black region. The math still runs (so the sub or fixed colour still appears in the blend result), but the main operand contributes nothing.

So "force main black" is really "force the main operand of color math to 0". When math is disabled at that pixel, this reads as plain black; when math is enabled, it reads as "sub colour" (or "halve of sub" if colorHalve), or "fixed color".

### Cross-check vs nocash

Nocash documents CGWSEL bits 7:6 as:

```
0 = Always (never force-black)
1 = Outside Color Window
2 = Inside Color Window
3 = Always (always force-black)
```

ares' table above matches this **only if you treat `value` as "we are inside the colour window"** under the typical "one-enable, no invert" configuration. The implementation is in fact more general: `value` is whatever the colour window's per-window enable / invert / mask combiner evaluates to. Under a configuration where `oneInvert` is set, the very same `aboveMask=1` cell will read "outside" because the inversion flips `one`. ares' implementation is *the* truth; nocash's "outside/inside" labels are a shorthand for the no-invert case.

For luna: the polarity to verify is `output.above.colorEnable == true ⇒ main visible`. If luna instead treats the bit as "is forced black", then 0 means "never forced" → main visible (matches), 3 means "always forced" → main visible (WRONG). The bug surface is exactly this.

---

## 4. Color-math region (`belowMask` / CGWSEL bits 5:4)

Same `array[]` indirection (`window.cpp:38`):

| `belowMask` ($2130 bits 5:4) | `output.below.colorEnable` | Per-dot behaviour |
|:---:|:---|:---|
| 0 | always `true` | Math enabled everywhere. |
| 1 | `value` | Math enabled where `value`==true. |
| 2 | `!value` | Math enabled where `value`==false. |
| 3 | always `false` | Math disabled everywhere. |

Consumed at `dac.cpp:120`:
```
if(!window.output.below.colorEnable) math.below.colorEnable = false;
```

Note: this only **disables** math. If the window says "math here" but the winning above layer's CGADSUB bit ($2131) is off (or, for OBJ, palette<192), `math.below.colorEnable` is already false from the layer-pick step and the window's `true` does nothing. The AND order is `colorEnable_from_layer AND colorEnable_from_window`.

### Interaction with `cgadsub` ($2131)

CGADSUB bits (ppu_io.cpp:607-616):
```
bit 0: bg1 colorEnable
bit 1: bg2 colorEnable
bit 2: bg3 colorEnable
bit 3: bg4 colorEnable
bit 4: obj colorEnable
bit 5: back colorEnable (backdrop participates in math)
bit 6: colorHalve
bit 7: colorMode (0=add, 1=sub)
```

In `dac.cpp:86-118` the per-layer `colorEnable` is stamped into `math.below.colorEnable` *only for the winning above layer*. Subsequent losers are not consulted. So enabling CGADSUB bit 0 (BG1) only makes math fire on pixels where BG1 happens to be the visible main layer.

Implications:

- A pixel where **BG3 wins above** with CGADSUB bit-2 disabled: math is off, even if the sub has a BG1 colour ready, even if CGWSEL bit-1 says "use sub as operand".
- A pixel where **OBJ wins above** with palette 0..3 (palette index 128..191): `palette >= 192` is false → math off, regardless of CGADSUB bit 4.
- A pixel where **backdrop is the winner** (no layer drew): CGADSUB bit 5 controls. SMW dialog boxes often work this way — the dialog interior is the backdrop (palette 0) on the main screen, with BG3 text on the main screen, and the sub-screen has the world behind. The "translucent blue dialog interior" effect is exactly: main = backdrop (some palette-0 colour), sub = world scene, CGADSUB bit-5 set so backdrop participates in math, CGWSEL bit-1 set so the sub is the math operand (instead of fixed colour), CGADSUB bit-6 set for halve.

---

## 5. Sub-screen rendering

### 5.1 ares renders a real sub-screen

`DAC::below(hires)` (`dac.cpp:43-80`) iterates the full BG1..4 + OBJ winners using their `output.below` Pixel records. These are populated by `Background::run` (`background.cpp:172-211`) and `Object::run` (`object.cpp:57-89`) any time the respective `io.belowEnable` (TS $212D) flag is set. There's no "fixed colour only on sub" shortcut; the sub-screen is a fully composited screen.

```
//background.cpp:209-210
if(!hires || screen == Screen::Above) if(io.aboveEnable) output.above = pixel;
if(!hires || screen == Screen::Below) if(io.belowEnable) output.below = pixel;
```

```
//object.cpp:78-86
if(io.aboveEnable) {
  output.above.palette = tile.palette + color;
  output.above.priority = io.priority[tile.priority];
}
if(io.belowEnable) {
  output.below.palette = tile.palette + color;
  output.below.priority = io.priority[tile.priority];
}
```

So the sub-screen composition is identical to the main, gated by TS instead of TM. The window stage then zeroes out `output.below.priority` for windowed layers (`window.cpp:11-33`) before the DAC sees them.

### 5.2 CGWSEL bit 1 (`io.blendMode`) — fixed-colour vs real-sub

`io.blendMode` is named confusingly — it's NOT the math add/sub mode. CGWSEL bit 1 is the "use sub-screen as math operand" flag.

- `io.blendMode == 0`: the math operand is `fixedColor()` (COLDATA $2132). This is the "fixed colour math" mode used by, e.g., F-Zero road fading.
- `io.blendMode == 1`: the math operand is the actual sub-screen pixel. This is the path used for translucency overlays.

Consumption sites:
- `dac.cpp:78`: `math.blendMode ? math.above.color : fixedColor()` — in the **below** return when math is enabled, the math operand for the *sub* path's blend is the above colour (when blendMode==1) or fixed (when blendMode==0). This handles the hires "blend the two halves" case.
- `dac.cpp:134`: `math.blendMode ? math.below.color : fixedColor()` — in the **above** return, the second operand is the sub colour (when blendMode==1) or fixed.
- `dac.cpp:124-130`: when blendMode==1 AND `math.transparent` (sub was empty backdrop this dot), ares **clears `blendMode` and `colorHalve`** for this dot. Quote:

```
if(io.blendMode && math.transparent) {
  math.blendMode  = false;
  math.colorHalve = false;
} else {
  math.blendMode  = io.blendMode;
  math.colorHalve = io.colorHalve && math.above.colorEnable;
}
```

Why: nocash documents that when CGWSEL.bit1=1 (add sub) and the sub backdrop is the only sub thing, hardware substitutes the fixed colour AND disables halve. ares implements exactly that — `math.transparent` is the "sub had no winning layer" flag set by `below()`. **luna must implement this transparent-backdrop fallback** or sub-screen math against an empty sub will produce wrong colours and unwanted halving.

### 5.3 Sub-screen backdrop

When no layer wins on the sub, `math.below.color = paletteColor(0)` (`dac.cpp:71`) and `math.transparent = true`. So the sub backdrop **is just CGRAM[0]** — the same backdrop as the main screen. There is no separate sub-backdrop register; the fixed colour COLDATA is *not* the sub backdrop, it's the math operand alternative.

### 5.4 Direct-color mode interaction

`io.directColor` (CGWSEL bit 0). Only applies to BG1 in modes 3, 4, 7 (`dac.cpp:49, 88`). When set, the BG1 palette byte is decoded into a 15bpp colour directly rather than indexing CGRAM. This affects the BG1 contribution to both `math.above.color` and `math.below.color`. It has no interaction with the sub-screen mixer beyond changing which colour the BG1 layer reports.

---

## 6. OBJ rendering (`object.cpp`, `oam.cpp`)

### 6.1 Per-scanline sprite evaluation timing

`PPU::main()` (`main.cpp`) drives the timing:

- Evaluation: `cycleObjectEvaluate()` (main.cpp:102-104) calls `obj.evaluate(hcounter() >> 3)` once every 8 cycles between cycle 0 and 1016 (`main.cpp:214`). So 128 sprites are visited at one per 8 cycles. `evaluate()` (object.cpp:35-49):
  ```
  if(t.itemCount > 32) return;
  ...
  if(!onScanline(oam.object[sprite])) return;
  self.latch.oamAddress = sprite;
  if(t.itemCount++ < 32) {
    oamItem[t.itemCount - 1] = {true, sprite};
  }
  ```
  Up to 32 sprites are kept per scanline (`itemCount` capped on the `< 32` check). The 32nd-overflow sets `rangeOver` later.
- Fetch: `obj.fetch()` at H=1080 (main.cpp:93) reverse-iterates the items, fetching tile data, capping at 34 tiles. `t.itemCount > 32 → io.rangeOver = 1`, `t.tileCount > 34 → io.timeOver = 1` (object.cpp:159-160), exposed via $213E STAT77 bits 6/7 (ppu_io.cpp:161-162).
- Render: `obj.run()` (object.cpp:57-89) runs every dot via `cycleRenderPixel` (main.cpp:206-210).

### 6.2 Sprite-zero handling

ares does NOT implement a sprite-zero hit register in the SNES sense (the NES had it; on the SNES the equivalents are the OAM range/time-over flags and the M7HOFS/VOFS latches, which aren't a sprite-zero hit). There's no special-case for sprite #0 anywhere in `object.cpp`. The "first sprite" referenced in code (`io.firstSprite`, `setFirstSprite`, ppu.hpp:385, object.cpp:6-9) is OAM-priority rotation — when OAMADDH bit 7 (`io.oamPriority`) is set, sprite evaluation starts at the high half of OAMADD instead of sprite 0. That's the "priority rotation" feature, unrelated to sprite-zero hits.

### 6.3 OAMADDR reload at VBlank ("OAM address reset")

`object.cpp:31-32` in `Object::scanline()`:
```
if(t.y == self.vdisp() && !self.io.displayDisable) addressReset();
if(t.y >= self.vdisp() - 1 || self.io.displayDisable) return;
```

`addressReset()` (object.cpp:1-4):
```
inline auto PPU::Object::addressReset() -> void {
  self.io.oamAddress = self.io.oamBaseAddress;
  setFirstSprite();
}
```

So at the start of VBlank (vcounter == vdisp, i.e. line 225 NTSC non-overscan), if force-blank is NOT active, `io.oamAddress` is reset from the latched `io.oamBaseAddress` (which was set via $2102/$2103, ppu_io.cpp:209-220).

Also: a write to $2100 (INIDISP) that exits force-blank at vcounter==vdisp triggers the same `addressReset()` (ppu_io.cpp:194). And $2102/$2103 themselves call `addressReset()` immediately (ppu_io.cpp:211, 219). The pattern is exactly nocash's "OAMADDR resets at the start of VBlank if not in force-blank".

### 6.4 OBJ priority interaction with compositor

`updateVideoMode` (ppu_io.cpp:640-741) stamps `obj.io.priority[]` (4 entries) with the scalar priority numbers used by the DAC comparator. For BG mode 1 with `io.bgPriority` (BGMODE bit 3) set:
```
memory::assign(obj.io.priority, 2, 3, 6, 9);
```

So a sprite with OAM `priority` field 0→DAC priority 2, 1→3, 2→6, 3→9. The BG3 priority for that mode is `{1, 10}` — so BG3 priority-1 tiles win over EVERY sprite when `bgPriority==1` (the famous "BG3-on-top" mode for status bars). Sprite priority 3 (DAC 9) beats BG3 priority-0 (DAC 1) but loses to BG3 priority-1 (DAC 10).

This is the *whole* layer-priority resolution mechanism: pack (layer, sub-priority) into one scalar and compare with `>` in `DAC::above()`/`below()`. There are no separate "OBJ priority 0/1/2/3" comparisons — they're interleaved into the same scalar space as the BGs.

---

## 7. DMA / HDMA timing (`cpu/dma.cpp`, `cpu/timing.cpp`)

### 7.1 HDMA service time

`cpu/timing.cpp:31-46`:
```
if(!status.hdmaSetupTriggered && hcounter() >= status.hdmaSetupPosition) {
  status.hdmaSetupTriggered = 1;
  hdmaReset();
  if(hdmaEnable()) {
    status.hdmaPending = 1;
    status.hdmaMode = 0;
  }
}

if(!status.hdmaTriggered && hcounter() >= status.hdmaPosition) {
  status.hdmaTriggered = 1;
  if(hdmaActive()) {
    status.hdmaPending = 1;
    status.hdmaMode = 1;
  }
}
```

- **HDMA setup** fires once per frame at `status.hdmaSetupPosition`, set in `CPU::scanline()` (cpu/timing.cpp:64): `12 + 8 - dmaCounter()` on rev1, `12 + dmaCounter()` on rev2. So early in vline 0.
- **HDMA transfer** fires once per visible scanline at `status.hdmaPosition = 1104` (cpu/timing.cpp:76), which is **after** the visible-pixel region (visible dots end around hcounter 1078). So HDMA runs during HBlank.
- Only on visible scanlines: `if(vcounter() < ppu.vdisp())` (timing.cpp:75) — no HDMA during VBlank.

The pending flag is consumed by `dmaEdge` (timing.cpp:100-140) which actually performs `hdmaSetup()` or `hdmaRun()`.

### 7.2 General DMA on write to $420B

`cpu/io.cpp:204-207`:
```
case 0x420b:  //DMAEN
  for(u32 n : range(8)) channels[n].dmaEnable = data.bit(n);
  if(data) status.dmaPending = true;
  return;
```

Sets the per-channel `dmaEnable` and raises `status.dmaPending`. The actual transfer happens in the next `dmaEdge()` (timing.cpp:100-140), which is called from `step()` at every DMA-clock boundary.

`dmaEdge` aligns to an 8-cycle boundary first (`step(counter.dma = 8 - dmaCounter())`), then runs the channels via `dmaRun()` (dma.cpp:16-22):

```
auto CPU::dmaRun() -> void {
  counter.dma += 8;
  step(8);
  dmaEdge();
  for(auto& channel : channels) channel.dmaRun();
  status.irqLock = true;
}
```

`Channel::dmaRun()` (dma.cpp:108-122) walks the transfer-size word, calling `transfer()` per index. Each `readA`/`readB`/`writeA`/`writeB` costs 8 clocks (dma.cpp:64-68 — `step(4); ...; step(4);`).

So yes — the CPU is fully stalled during the DMA. The write to $420B doesn't return until `dmaEdge` completes, which steps the bus through all the transfer cycles.

### 7.3 Auto-joypad read ($4200 bit 0)

The polling state machine lives in `CPU::joypadEdge()` (cpu/timing.cpp:143-195), called every 128 clocks from `CPU::step()` (timing.cpp:18).

```
if(vcounter() == ppu.vdisp() && (counter.cpu & 255) == 0 && hcounter() >= 130 && hcounter() <= 384) {
  status.autoJoypadCounter = 0;
}
```

So polling **begins at vcounter == vdisp (line 225 NTSC, 240 PAL), within hcounter 130..384** — that is, just after entering VBlank.

The state machine (timing.cpp:154-194):
- counter==0: latch state, set `autoJoypadLatch = io.autoJoypadPoll`, drive both ports' latch pin, **zero out io.joy1..joy4 shift registers** if polling is enabled.
- counter==1: release latch.
- counter>=2: on even counters sample controller data, on odd counters shift into `io.joy{1-4}` MSB-first. 32 cycles of even/odd → 16 bits per pad.

The 16 bits accumulate in `io.joy1..io.joy4` (cpu/cpu.hpp / io fields), exposed via $4218..$421F (cpu/io.cpp:46-53):
```
case 0x4218: return io.joy1.byte(0);   //JOY1L
case 0x4219: return io.joy1.byte(1);   //JOY1H
...
```

The autoJoypadCounter ranges 0..33 (counter==33 = done). HVBJOY bit 0 is "auto-joypad-busy" — true while counter < 33 (cpu/io.cpp:34): `data.bit(0) = io.autoJoypadPoll && status.autoJoypadCounter < 33;`.

---

## 8. NMI + frame timing (`timing.cpp`, `io.cpp`, `irq.cpp`)

### 8.1 NMI fires at vdisp

`cpu/irq.cpp:6-16`:
```
alwaysinline auto CPU::nmiPoll() -> void {
  if(status.nmiHold.lower() && io.nmiEnable) {
    status.nmiTransition = 1;
  }

  if(status.nmiValid.flip(vcounter(2) >= ppu.vdisp())) {
    if(status.nmiLine = status.nmiValid) status.nmiHold = 1;
  }
}
```

`nmiPoll()` is called every 4 clocks (timing.cpp:17: `if(hcounter() & 2) nmiPoll(), irqPoll();`). `vcounter(2)` is "vcounter 2 clocks ago" — emulating the bus delay. So when `vcounter >= vdisp` (NTSC 225 / PAL 240, or 240/240 if overscan), `nmiValid.flip` raises `nmiLine`, which triggers NMI delivery if `io.nmiEnable` ($4200 bit 7) is set.

The line stays held for 4 cycles (`status.nmiHold`), then lowers, generating a falling-edge transition that's the actual NMI signal to the CPU.

`rdnmi()` (irq.cpp:52-58) reads $4210 — returns the current line state and clears it (unless still held).

### 8.2 HVBJOY register ($4212) — `cpu/io.cpp:33-37`

```
case 0x4212:  //HVBJOY
  data.bit(0) = io.autoJoypadPoll && status.autoJoypadCounter < 33;
  data.bit(6) = hcounter() <= 2 || hcounter() >= 1096;  //Hblank
  data.bit(7) = vcounter() >= ppu.vdisp();              //Vblank
  return data;
```

- **bit 0**: auto-joypad busy. Set only when polling is enabled AND counter < 33.
- **bit 6**: H-blank. **Live** (hcounter-driven), not latched. True when `hcounter() <= 2 || hcounter() >= 1096`. Notice this includes the brief window at the very start of the scanline (dots 0..2). HCounter range is 0..1363, visible/render region roughly 22..1096. So bit-6 is "outside the visible-dot zone". This matches the project's recent fix commit (`9d801f8 fix(core): $4212 HVBJOY bit 6 = live Hblank (ares)`).
- **bit 7**: V-blank. True when `vcounter() >= vdisp()`.

Bits 1..5 are unimplemented (returned as input `data`, i.e. open-bus from previous read).

### 8.3 PPU register open-bus

`readIO` (`ppu_io.cpp:63-185`): every unhandled register returns `data` (the input parameter, which is the bus MDR). The handled cases ($2134..$213F) carefully return either `ppu1.mdr` or `ppu2.mdr` after updating those latches. Write-only registers ($2104, $2105, ...) return `ppu1.mdr` unchanged (ppu_io.cpp:68-74).

So ares models the dual PPU1/PPU2 open-bus latches properly. Writes to write-only registers update the corresponding MDR via the bus, then a subsequent read of an unmapped reg returns that MDR.

---

## 9. Mode-7 + EXTBG (`mode7.cpp`)

`Background::runMode7()` is invoked from `Background::run()` (background.cpp:181) when `io.mode == Mode::Mode7`. It produces a `Pixel` and writes it to `output.above` and/or `output.below`:

```
//mode7.cpp:54-64
if(io.aboveEnable) {
  output.above.priority = priority;
  output.above.palette = palette;
  output.above.paletteGroup = 0;
}
if(io.belowEnable) {
  output.below.priority = priority;
  output.below.palette = palette;
  output.below.paletteGroup = 0;
}
```

So Mode 7 BG1 (and EXTBG BG2) plug into the *same* DAC mixer via `bg1.output` / `bg2.output`. The compositor doesn't know it's Mode 7.

**EXTBG (BG2 in mode 7)**: `mode7.cpp:46-50`:
```
} else if(id == ID::BG2) {
  priority = io.priority[palette.bit(7)];
  palette.bit(7) = 0;
}
```

In Mode 7 EXTBG, the high bit of the Mode-7 pixel's palette index is used as the BG2 per-pixel priority selector (palette.bit(7) → priority[0] or priority[1]), then masked off. The two priorities come from `obj/bg priority[]` set in `updateVideoMode` mode 7 case (ppu_io.cpp:732-739):

```
memory::assign(bg1.io.priority, 3);
memory::assign(bg2.io.priority, 1, 5);
memory::assign(obj.io.priority, 2, 4, 6, 7);
```

So an EXTBG BG2 pixel with palette bit 7 set wins over OBJ priority 0 (DAC 2) but loses to OBJ priority 3 (DAC 7). EXTBG with bit-7 clear loses to even OBJ priority 0.

Direct-color mode (CGWSEL bit 0) applies to BG1 in mode 7 (`dac.cpp:49, 88`), decoding the 8bpp palette index directly as `BBGGGRRR` + paletteGroup-derived low bits.

---

## 10. Things directly relevant to the SMW dialog box bug

### 10.1 The translucent-blue dialog interior recipe

For SMW Yoshi's House intro, the dialog interior should be a translucent blue overlay on the level scene. The ingredients ares' DAC will mix into that effect:

1. **Main screen** contains BG3 (the dialog text+border) drawn over palette-0 backdrop in the interior region. BG1/BG2 (the level) are TM-disabled, OR are masked out by a window-on-main, OR the interior is just outside their drawn area — practically, SMW sets up the dialog so the **backdrop wins** on the main screen inside the dialog.
2. **Sub screen** contains BG1/BG2 (the level scene). TS bits for BG1/BG2 are set ($212D).
3. **CGADSUB ($2131)**:
   - bit 5 (back colorEnable) = 1 — backdrop participates in math.
   - bit 6 (colorHalve) = 1 — halve the result.
   - bit 7 (colorMode) = 0 — add (not subtract).
4. **CGWSEL ($2130)**:
   - bit 1 (blendMode) = 1 — use sub-screen as math operand (not fixed color).
   - bits 5:4 (belowMask) typically = 0 (math enabled everywhere) or `value` controlled by colour window for the dialog region.
   - bits 7:6 (aboveMask) typically = 0 — main never forced black (we WANT to see the BG3 text and the blue-tinted backdrop).
5. **Backdrop colour** (CGRAM[0]) might be set to a dark blue or near-black. The translucent blue effect comes from `(backdrop + sub_level_pixel) >> 1`.

If luna's compositor:

- **(a) Forces main black** when CGWSEL bits 7:6 == 0 (inverted polarity bug) → entire interior renders 0 (the "dialog box is pure black" symptom).
- **(b) Doesn't render the sub-screen with real BG layers**, only fixed-color → the math operand is COLDATA, not the level pixel. With CGADSUB bit 5 + 6 set and a blue/dark backdrop + e.g. COLDATA = grey, you'd get a flat halved colour instead of a translucent overlay; closer to a uniform colour washout than "you can see the level through the dialog".
- **(c) Doesn't apply the `math.transparent` clear-blendMode-and-halve fallback** (`dac.cpp:124-130`) — sub pixels where the sub backdrop wins would still halve, darkening them spuriously.

The dac.cpp implementation specifically does NOT short-circuit math when `io.blendMode==1` and the sub backdrop is the winner — it falls back to fixed colour with halve disabled. Luna needs both branches.

### 10.2 The "no Mario sprite" symptom

OBJ rendering and the OAMADDR reset (object.cpp:1-4, 31-32) are critical. If luna's PPU does not perform the VBlank `addressReset()` (or worse, performs it during force-blank, which ares specifically guards against on line 31), the OAM walk during sprite evaluation may start at a stale address and miss the Mario sprite entries.

Also: `Object::scanline()` (object.cpp:16-33) sets `t.active = !t.active` and the **OBJ run consumes the OTHER buffer** (`auto oamTile = t.tile[!t.active];` at object.cpp:61). The double-buffering means a scanline renders tiles fetched on the *previous* scanline. If luna does not double-buffer the OAM tile cache the same way, sprites will be rendered against this scanline's tile fetch data (which is wrong because fetch happens at H=1080, after pixels start being drawn).

For dialog phase specifically: SMW rebuilds OAM each frame via DMA. If luna's general DMA ($420B) write completes synchronously but the *actual byte-by-byte writeOAM path* is buggy (e.g. ignores `io.oamAddress` increment or doesn't honour the odd/even latch behaviour at ppu_io.cpp:224-236), the shadow OAM upload silently corrupts and Mario disappears. Note ares' $2104 handler does a write-pair latch:

```
//ppu_io.cpp:223-236
case 0x2104: {
  n1 latchBit = io.oamAddress.bit(0);
  n10 address = io.oamAddress++;
  if(latchBit == 0) latch.oam = data;
  if(address.bit(9)) {
    writeOAM(address, data);
  } else if(latchBit == 1) {
    writeOAM((address & ~1) + 0, latch.oam);
    writeOAM((address & ~1) + 1, data);
  }
  obj.setFirstSprite();
  return;
}
```

Notes:
- The low byte of an even-address pair is **latched** and only committed when the high byte arrives — luna must replicate this or every OAM DMA gets one byte right and one byte wrong.
- Writes to the upper OAM table (addr bit 9) bypass the latch and write directly.
- Every $2104 write calls `setFirstSprite()` (object.cpp:6-9) — refreshes the OAM-priority rotation start point.

### 10.3 The "artifacts during level load" symptom

Level-load frames typically:
- Set force-blank on ($2100 bit 7) for the whole frame while uploading VRAM/CGRAM/OAM.
- Run several large DMAs to $2118/$2119 (VRAM), $2122 (CGRAM), $2104 (OAM).
- Release force-blank.

Potential ares-vs-luna gaps:

- **CGRAM mid-active-display fixup**: `readCGRAM`/`writeCGRAM` (ppu_io.cpp:47-61) **force the address to `latch.cgramAddress`** during dots 88..1096 of a visible scanline when not in force-blank. The latch is updated every pixel by `DAC::paletteColor()` (dac.cpp:159). If luna doesn't model this, a CPU CGRAM write during active display will go to the address you asked for instead of the latched one — and you'll see colour glitches on the dot at which the write landed.
- **VRAM gate**: similarly, `readVRAM`/`writeVRAM` (ppu_io.cpp:19-29) return 0 / discard writes when in active display, unless force-blank is on. Luna must replicate this to avoid corrupting tile data with timing-sensitive level-load uploads.
- **OAM mid-active-display fixup**: `readOAM`/`writeOAM` (ppu_io.cpp:31-45) during active display force the address through `latch.oamAddress` (which the OBJ evaluator stamps). This is the same family of address-mux quirks; missing it causes OAM corruption during the brief moments level-load NMI windows are too short.

### 10.4 SMW dialog: TM/TS bits at a glance

The compositor reaches "dialog interior = blue translucent" only if:
- TM ($212C) for BG3 = 1 (text on main).
- TM for BG1/BG2 = 0 inside the dialog region (or windowed off).
- TS ($212D) for BG1/BG2 = 1 (level on sub).
- CGADSUB.5 = 1 (backdrop math).
- CGWSEL.1 = 1 (sub operand).
- CGWSEL.7:6 = 0 (main not forced black).

If luna inverts CGWSEL.7:6 polarity (e.g. treats 0 as "force black always"), the interior collapses to pure black — exactly the reported symptom. The `array[]` semantics at window.cpp:36-38 are unambiguous: `aboveMask=0 → array[0] = true → output.above.colorEnable = true → main visible`. Verify luna agrees.

---

## Appendix A: Pixel pipeline summary

For every visible dot (vcounter 1..vdisp-1, hcounter the rendering region driven by `cycle<>` template instantiations in `main.cpp:60-95`):

1. `cycleObjectEvaluate()` — once per 8 dots, walks one of 128 sprites (`evaluate`).
2. `cycleBackgroundFetch<N>()` — once per 4 dots, the N=0..7 cycle determines what is fetched (nametable, character, offset-per-tile) for the current bgMode.
3. `cycleBackgroundBegin()` — once at H=56, scrolls the pre-rendered tile data by `pixelCounter`.
4. `cycleBackgroundBelow()` then `cycleBackgroundAbove()` (alternating, every 4 dots) — emit `Background::run(screen)` for the current dot, populating `output.above` or `output.below`.
5. `cycleRenderPixel()` — sequence is:
   1. `obj.run()` — composite sprites at this x into `obj.output.above/below`.
   2. `window.run()` — evaluate windows, zero out windowed layer priorities, set color-math window enables.
   3. `dac.run()` — `below(hires)` then `above()`, write u32 to the screen buffer.

OBJ tile fetch happens after rendering, at `obj.fetch()` (main.cpp:93), and populates the *other* OAM buffer (`t.active ^= 1` at object.cpp:24) — so what was fetched last scanline is what `obj.run()` sees this scanline.

---

## Appendix B: All register → field map (the regs that matter for compositing)

| Reg | bits | field | meaning |
|---|---|---|---|
| $2100 INIDISP | 0..3 | `io.displayBrightness` | brightness, OR'd into output u32 |
| $2100 INIDISP | 7 | `io.displayDisable` | force-blank; `DAC::below/above` return 0 |
| $2101 OBSEL | 0..2 | `obj.io.tiledataAddress` | obj tile base shifted to 13 |
| $2101 OBSEL | 3..4 | `obj.io.nameselect` | obj name-table select |
| $2101 OBSEL | 5..7 | `obj.io.baseSize` | sprite size table index |
| $2102/3 OAMADDL/H | | `io.oamBaseAddress`, `io.oamPriority` | base OAM addr + priority rotation flag |
| $2105 BGMODE | 0..2 | `io.bgMode` | which mode 0..7 |
| $2105 BGMODE | 3 | `io.bgPriority` | mode-1 BG3-on-top |
| $212C TM | 0..4 | `bg*.io.aboveEnable`, `obj.io.aboveEnable` | per-layer main-screen enable |
| $212D TS | 0..4 | `bg*.io.belowEnable`, `obj.io.belowEnable` | per-layer sub-screen enable |
| $212E TMW | 0..4 | `window.io.bg*.aboveEnable`, `window.io.obj.aboveEnable` | per-layer window-on-main |
| $212F TSW | 0..4 | `window.io.bg*.belowEnable`, `window.io.obj.belowEnable` | per-layer window-on-sub |
| $2130 CGWSEL | 0 | `dac.io.directColor` | direct-color mode (BG1 in modes 3/4/7) |
| $2130 CGWSEL | 1 | `dac.io.blendMode` | 0=fixed-color math operand, 1=sub-screen math operand |
| $2130 CGWSEL | 4..5 | `window.io.col.belowMask` | color-math-region selector |
| $2130 CGWSEL | 6..7 | `window.io.col.aboveMask` | force-main-black region selector |
| $2131 CGADSUB | 0..4 | `dac.io.bg1..bg4,obj.colorEnable` | per-layer math enable |
| $2131 CGADSUB | 5 | `dac.io.back.colorEnable` | backdrop math enable |
| $2131 CGADSUB | 6 | `dac.io.colorHalve` | halve result |
| $2131 CGADSUB | 7 | `dac.io.colorMode` | 0=add, 1=sub |
| $2132 COLDATA | 5/6/7 | `dac.io.colorRed/Green/Blue` | per-channel fixed color (gated by which of R/G/B select bits is set) |
| $2133 SETINI | 0 | `io.interlace` | |
| $2133 SETINI | 1 | `obj.io.interlace` | |
| $2133 SETINI | 2 | `io.overscan` | vdisp 225 or 240 |
| $2133 SETINI | 3 | `io.pseudoHires` | enables hires path in DAC |
| $2133 SETINI | 6 | `io.extbg` | mode-7 BG2 overlay |

---

## Appendix C: Subtleties luna might have missed

1. **`math.transparent` fallback in CGWSEL.blendMode==1** (`dac.cpp:124-130`). When sub backdrop wins and add-sub-screen is enabled, hardware substitutes fixed-color **and disables halve** for that dot.
2. **OBJ palette gate ≥ 192** (`dac.cpp:113`). Sprites only participate in math from palette 4..7. This is `palette index >= 128 + 64 = 192`.
3. **CGRAM write-fixup on active display** (`ppu_io.cpp:55-61`). Writes during dots 88..1096 of a visible scanline get redirected to the latched address.
4. **VRAM/OAM write-discard on active display** (`ppu_io.cpp:19-45`). These return 0 / discard unless force-blank.
5. **OAM $2104 even-byte latch** (`ppu_io.cpp:223-236`). The low byte of an even-aligned write is held until the high byte arrives.
6. **OAM addressReset on entering VBlank** (`object.cpp:31`). And on `INIDISP` write exiting force-blank at vcounter==vdisp (`ppu_io.cpp:194`).
7. **HVBJOY bit 6 is live Hblank, not latched** (`cpu/io.cpp:35`) — luna's recent fix commit `9d801f8` already addresses this; just confirming the ares behaviour.
8. **The `window.run` clobbers layer priority to 0** to mask (`window.cpp:11-33`) — luna's window may be implementing a separate "mask" bit, which would still work but be a different shape. Either is correct; verify the data path.
9. **DAC scanline init sets `math.above.colorEnable = false`** then `above()` overwrites it from `window.output.above.colorEnable`. So the *initial* state for the first dot of a line has main forced black — this is the "first hires pixel is transparent" hardware quirk.
10. **`math.below.color` defaults to palette[0] on each scanline**, then is updated by `below()` only when a layer wins. So if luna initialises `math.below.color` to zero, a sub-backdrop dot in math will use 0 instead of CGRAM[0] — wrong colour by however much CGRAM[0] differs from 0.
