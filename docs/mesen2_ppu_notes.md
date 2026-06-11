# Mesen2 SNES PPU reference notes

Source: `https://raw.githubusercontent.com/SourMesen/Mesen2/master/Core/SNES/...`
Files fetched into `/tmp/mesen2/` (line numbers throughout reference local snapshots
at `master` commit at fetch time):

- `SnesPpu.h` (265 lines)
- `SnesPpu.cpp` (2471 lines)
- `SnesPpuTypes.h` (195 lines)
- `InternalRegisters.h` (161 lines)
- `InternalRegisters.cpp` (414 lines)
- `InternalRegisterTypes.h` (29 lines)
- `SnesDmaController.h` (62 lines), `SnesDmaController.cpp` (648 lines)
- `DmaControllerTypes.h` (33 lines)
- `SnesMemoryManager.h/.cpp` (event dispatch for HDMA)

No separate `PpuRenderer.h/.cpp` exists in Mesen2 — the renderer is fully inlined
in `SnesPpu.cpp`. There is no `Ppu.h/.cpp` (only `SnesPpu.*`); the request URLs
for `Ppu.h`, `Ppu.cpp`, `PpuTypes.h`, `MemoryMappings.h`, `RegisterHandlerB.h`,
`Spc7110.h` would 404 against `Core/SNES/` (Mesen2 uses Snes-prefixed names,
`SnesPpuTypes.h`, etc.). Mapped equivalents:
`Ppu.h` -> `SnesPpu.h`, `Ppu.cpp` -> `SnesPpu.cpp`,
`PpuTypes.h` -> `SnesPpuTypes.h`,
`DmaController.h/.cpp` -> `SnesDmaController.h/.cpp`,
`MemoryMappings.h` -> exists in `Core/SNES/MemoryMappings.h`.

---

## 1. The pixel composition pipeline

### 1.1 Persistent per-scanline state

Mesen2 keeps **two parallel 256-pixel scratch buffers** for every scanline
(declared in `SnesPpu.h:81-85`):

```
uint8_t  _mainScreenFlags[256];   // bits 0..3: winning priority; bit 7 (PixelFlags::AllowColorMath): ColorMath gate
uint16_t _mainScreenBuffer[256];  // BGR555 pixel
uint8_t  _subScreenPriority[256]; // priority of current sub-screen winner (or 0 = backdrop)
uint16_t _subScreenBuffer[256];   // BGR555 pixel
```

Sprite ahead-of-time pixel arrays for the *next* line are also kept
(`SnesPpu.h:112-117`):

```
uint8_t _spritePriority[256]; uint8_t _spritePalette[256]; uint8_t _spriteColors[256];
uint8_t _spritePriorityCopy[256]; uint8_t _spritePaletteCopy[256]; uint8_t _spriteColorsCopy[256];
```

`PixelFlags` is a single bit, `AllowColorMath = 0x80` (`SnesPpuTypes.h:193-196`).
Priority is encoded as a small integer where **higher = wins**; backdrop
priority is 0 (set in `RenderBgColor`), normal/high priorities for tilemaps
come from per-mode constants in the eight `RenderMode*` calls
(`SnesPpu.cpp:781-859`), and sprite priorities come from the constant arrays
`spritePriorities[4]` per mode also in the same block.

### 1.2 Per-scanline driver

`SnesPpu::RenderScanline()` (`SnesPpu.cpp:861-920`) — the main driver. Once
`hPos > 22` and the scanline is renderable:

```c++
// SnesPpu.cpp:891-902
switch(_state.BgMode) {
    case 0: RenderMode0(); break;
    ...
    case 7: RenderMode7(); break;
}
RenderBgColor();
ApplyColorMath();
ApplyBrightness<true>();
ApplyHiResMode();
```

So for every chunk of pixels the order is:
1. Sprites first (`RenderSprites`, called from each `RenderModeX`).
2. Each BG layer for that mode in **lowest-to-highest** layer index. The
   priority comparisons (`_mainScreenFlags[x] & 0x0F < priority`) decide who
   actually wins.
3. `RenderBgColor()` fills the backdrop where no layer wrote.
4. `ApplyColorMath()` mixes main and sub.
5. Brightness scaling, then copy to the output framebuffer.

### 1.3 How main/sub pixels are populated per layer kind

#### Sprites (`RenderSprites`, SnesPpu.cpp:938-972`)

```c++
// SnesPpu.cpp:957-969
if(_spritePriority[x] <= 3) {
    uint8_t spritePrio = priority[_spritePriority[x]];
    if(drawMain && ((_mainScreenFlags[x] & 0x0F) < spritePrio) && !ProcessMaskWindow<...>(...)) {
        uint16_t paletteRamOffset = 128 + (_spritePalette[x] << 4) + _spriteColors[x];
        _mainScreenBuffer[x] = _cgram[paletteRamOffset];
        _mainScreenFlags[x] = spritePrio
            | (((_state.ColorMathEnabled & 0x10) && _spritePalette[x] > 3) ? PixelFlags::AllowColorMath : 0);
    }
    if(drawSub && (_subScreenPriority[x] < spritePrio) && !ProcessMaskWindow<...>(...)) {
        _subScreenBuffer[x] = _cgram[paletteRamOffset];
        _subScreenPriority[x] = spritePrio;
    }
}
```

This shows **three load-bearing facts**:

1. The sub-screen really is rendered with its own per-layer priorities and its
   own window masks (`drawSub`, `subWindowCount`); not a copy of main.
2. The OBJ "palette ≥ 4" gating is applied only to the *AllowColorMath bit*
   on the main screen winner — not to whether the OBJ pixel is drawn.
   (`_spritePalette[x] > 3` means OAM palette index 4..7, i.e. CGRAM
   $90..$FF.) Subscreen sprite write does not carry an AllowColorMath bit
   because the sub-screen has no per-pixel color-math gate; the bit lives on
   the main-screen pixel that math is applied to.
3. The sub-screen will accept a sprite winner over backdrop (initial
   `_subScreenPriority[x] == 0` < any visible sprite priority).

#### Tilemaps (`RenderTilemap<>`, SnesPpu.cpp:974-1066`)

```c++
// SnesPpu.cpp:993
uint8_t pixelFlags = (((_state.ColorMathEnabled >> layerIndex) & 0x01) ? PixelFlags::AllowColorMath : 0);
...
// SnesPpu.cpp:1047-1056
if(color > 0) {
    uint16_t rgbColor = GetRgbColor<bpp, directColorMode, basePaletteOffset>(paletteIndex, color);
    if(drawMain && (_mainScreenFlags[x] & 0x0F) < priority && !ProcessMaskWindow<layerIndex>(mainWindowCount, x)) {
        DrawMainPixel(x, rgbColor, priority | pixelFlags);
    }
    if constexpr(!hiResMode) {
        if(drawSub && _subScreenPriority[x] < priority && !ProcessMaskWindow<layerIndex>(subWindowCount, x)) {
            DrawSubPixel(x, rgbColor, priority);
        }
    }
}
```

`DrawMainPixel` / `DrawSubPixel` (`SnesPpu.cpp:1260-1270`) just unconditionally
overwrite buffer + flags / priority. The decision of whether to call them lives
in the priority comparison and the window test above.

#### Mode-7 tilemap (`RenderTilemapMode7<>`, SnesPpu.cpp:1126-1258`)

Same shape as the regular tilemap path (`SnesPpu.cpp:1249-1255`), except
priority comes from `(color & 0x80)` for the EXTBG layer (BG2 / layer=1), see
`SnesPpu.cpp:1218-1225`.

#### Backdrop (`RenderBgColor`, SnesPpu.cpp:922-936`)

```c++
// SnesPpu.cpp:924-934
uint8_t pixelFlags = (_state.ColorMathEnabled & 0x20) ? PixelFlags::AllowColorMath : 0;
for(int x = _drawStartX; x <= _drawEndX; x++) {
    if((_mainScreenFlags[x] & 0x0F) == 0) {       // nothing wrote main
        _state.InternalCgramAddress = 0;
        _mainScreenBuffer[x] = _cgram[0];
        _mainScreenFlags[x] = pixelFlags;
    }
    if(_subScreenPriority[x] == 0) {              // nothing wrote sub
        _state.InternalCgramAddress = 0;
        _subScreenBuffer[x] = _cgram[0];
    }
}
```

Two notable polarities:

- Backdrop priority is **0**, and the comparison `(_mainScreenFlags[x] & 0x0F) == 0` is
  what gates it. A sub-screen pixel from the backdrop carries priority 0, which
  means a later sprite/BG with priority ≥ 1 still wins.
- The backdrop's AllowColorMath bit is `(_state.ColorMathEnabled & 0x20)`,
  which is the bit-5 of `$2131` (CGADSUB). That bit "enables color math for the
  backdrop". This is the same bit that ares calls `bg5`.

So the backdrop **is** the sub-screen value when nothing else is drawn on the
sub-screen — there is no separate "sub-screen backdrop" register; it shares
CGRAM entry 0 with the main backdrop. Sub-screen-only fixed color comes from
the math path, not from the backdrop fill.

### 1.4 Final pixel selection (color-math disabled)

When color math is disabled for a pixel (per layer, per backdrop, or per OBJ
palette ≥ 4) the `AllowColorMath` flag bit is **never set** for that pixel.
`ApplyColorMathToPixel` then early-exits at `SnesPpu.cpp:1328-1331`:

```c++
if(!(_mainScreenFlags[x] & PixelFlags::AllowColorMath)) {
    //Color math doesn't apply to this pixel
    return;
}
```

But — and this is critical — **the force-main-black step at the top of
`ApplyColorMathToPixel` runs unconditionally regardless of AllowColorMath**.
This means a pixel where color math is "disabled" can still be forced to
black by CGWSEL[7:6]. See section 3.

### 1.5 Final pixel selection (color-math enabled)

`ApplyColorMath` (`SnesPpu.cpp:1272-1300`) walks the row:

```c++
// SnesPpu.cpp:1278-1299
uint8_t activeWindowCount = (uint8_t)_state.Window[0].ActiveLayers[SnesPpu::ColorWindowIndex]
                          + (uint8_t)_state.Window[1].ActiveLayers[SnesPpu::ColorWindowIndex];
bool hiResMode = _state.HiResMode || _state.BgMode == 5 || _state.BgMode == 6;
if(hiResMode) {
    for(int x = _drawStartX; x <= _drawEndX; x++) {
        bool isInsideWindow = ProcessMaskWindow<SnesPpu::ColorWindowIndex>(activeWindowCount, x);
        uint16_t subPixel = _subScreenBuffer[x];
        uint16_t prevMainPixel = x > 0 ? _mainScreenBuffer[x - 1] : 0;
        int prevX = x > 0 ? x - 1 : 0;
        ApplyColorMathToPixel(_subScreenBuffer[x], prevMainPixel, prevX, isInsideWindow);
        ApplyColorMathToPixel(_mainScreenBuffer[x], subPixel, x, isInsideWindow);
    }
} else {
    for(int x = _drawStartX; x <= _drawEndX; x++) {
        bool isInsideWindow = ProcessMaskWindow<SnesPpu::ColorWindowIndex>(activeWindowCount, x);
        ApplyColorMathToPixel(_mainScreenBuffer[x], _subScreenBuffer[x], x, isInsideWindow);
    }
}
```

So:

- In normal-res mode: per-pixel `ApplyColorMathToPixel(main, sub)`.
- In hi-res (mode 5/6 or pseudo-hires `$2133.3`): two sub-pixels are produced;
  the *left* subpixel is sub-screen-math-of-previous-main; the *right*
  subpixel is main-screen-math-of-sub. This emits left/right halves of the
  doubled 512-wide line. That detail isn't directly relevant to SMW's dialog
  bug (mode 1, not hi-res), but matters for hi-res games.

### 1.6 Hi-res / pseudo-hires

`ApplyHiResMode` (`SnesPpu.cpp:1429-1463`):

- Non-hi-res path: `memcpy` the 256-pixel main buffer into the framebuffer.
- Hi-res path: emit `[sub, main, sub, main, ...]` interleaved per pixel at
  512-wide output. `ApplyBrightness<false>()` (the sub-screen one) is called
  only when `IsDoubleWidth()` is true (mode 5/6/hires).
- Interlace: doubles vertically.

`ConvertToHiRes` (`SnesPpu.cpp:1396-1427`) is the upscaler for the case where
hi-res turns on mid-frame.

---

## 2. Window pipeline

### 2.1 Per-layer window mask

`WindowConfig::PixelNeedsMasking<layerIndex>` (`SnesPpuTypes.h:108-124`):

```c++
template<uint8_t layerIndex>
bool PixelNeedsMasking(int x)
{
    if(InvertedLayers[layerIndex]) {
        if(Left > Right) {
            return true;
        } else {
            return x < Left || x > Right;
        }
    } else {
        if(Left > Right) {
            return false;
        } else {
            return x >= Left && x <= Right;
        }
    }
}
```

Key polarities:

- `ActiveLayers[layerIndex]` (`$2123/4/5`) means "this layer participates in
  this window" — read in the per-layer `ProcessMaskWindow` lookup below.
- `InvertedLayers[layerIndex]` toggles the in/out polarity.
- Empty-window convention: `Left > Right` ⇒ in non-inverted mode it's "no
  pixels masked" (window degenerates to empty); in inverted mode it's "all
  pixels masked". This matches nocash docs.

### 2.2 Active-window-count branching

`ProcessMaskWindow<layerIndex>(activeWindowCount, x)` (`SnesPpu.cpp:1465-1485`):

```c++
switch(activeWindowCount) {
    case 1:
        if(_state.Window[0].ActiveLayers[layerIndex]) {
            return _state.Window[0].PixelNeedsMasking<layerIndex>(x);
        }
        return _state.Window[1].PixelNeedsMasking<layerIndex>(x);
    case 2:
        switch(_state.MaskLogic[layerIndex]) {
            default:
            case WindowMaskLogic::Or:   return _state.Window[0]...|  _state.Window[1]...;
            case WindowMaskLogic::And:  return _state.Window[0]...&  _state.Window[1]...;
            case WindowMaskLogic::Xor:  return _state.Window[0]...^  _state.Window[1]...;
            case WindowMaskLogic::Xnor: return !(_state.Window[0]... ^ _state.Window[1]...);
        }
}
return false;
```

Note: `activeWindowCount == 0` hits the `return false` at the bottom — no
masking. This is the equivalent of ares' "no window" early-out.

The caller computes `activeWindowCount` per-layer per-line by summing the two
booleans:

```c++
// e.g. SnesPpu.cpp:980-981 for tilemap
uint8_t mainWindowCount = _state.WindowMaskMain[layerIndex] ?
    (uint8_t)_state.Window[0].ActiveLayers[layerIndex] +
    (uint8_t)_state.Window[1].ActiveLayers[layerIndex] : 0;
uint8_t subWindowCount  = _state.WindowMaskSub[layerIndex]  ? ... : 0;
```

So the gating is:

1. Per-layer **enable for main** (`$212E`) or **enable for sub** (`$212F`). If
   the layer's window is not enabled for that screen, `windowCount = 0` → no
   masking at all.
2. Each of the two windows participates only if `ActiveLayers[layerIndex]`
   says so (`$2123/4/5` for the layer in question).
3. With 1 active window: use that window's result directly.
4. With 2 active windows: combine via `MaskLogic[layerIndex]` (`$212A` for
   BG, `$212B` for OBJ/Color).

### 2.3 luna's "4-value aboveMask/belowMask" equivalent

ares uses a packed 4-value array `[main, sub] × [layer]`. Mesen2 doesn't —
each call to `ProcessMaskWindow` computes the result on the fly, and the
`activeWindowCount` short-circuits when both windows are off. The `WindowMaskMain`
/ `WindowMaskSub` arrays (`SnesPpuTypes.h:148-149`) are simple booleans that
say "this layer's window applies to main / sub", indexed 0..4 (BG1..4, OBJ).
There is no main/sub flag for the color window — only `Window[0/1].ActiveLayers[5]`
and `MaskLogic[5]`.

### 2.4 Write paths for `$2123-$212B`

```c++
// SnesPpu.cpp:2127-2173

case 0x2123: ProcessWindowMaskSettings(value, 0); break; // W12SEL
case 0x2124: ProcessWindowMaskSettings(value, 2); break; // W34SEL
case 0x2125: ProcessWindowMaskSettings(value, 4); break; // WOBJSEL

case 0x2126: _state.Window[0].Left  = value; break;
case 0x2127: _state.Window[0].Right = value; break;
case 0x2128: _state.Window[1].Left  = value; break;
case 0x2129: _state.Window[1].Right = value; break;

case 0x212A:                                  // WBGLOG
    _state.MaskLogic[0] = (WindowMaskLogic)(value & 0x03);
    _state.MaskLogic[1] = (WindowMaskLogic)((value >> 2) & 0x03);
    _state.MaskLogic[2] = (WindowMaskLogic)((value >> 4) & 0x03);
    _state.MaskLogic[3] = (WindowMaskLogic)((value >> 6) & 0x03);
    break;

case 0x212B:                                  // WOBJLOG
    _state.MaskLogic[4] = (WindowMaskLogic)((value >> 0) & 0x03);
    _state.MaskLogic[5] = (WindowMaskLogic)((value >> 2) & 0x03);
    break;
```

`ProcessWindowMaskSettings` (`SnesPpu.cpp:1487-1498`):

```c++
_state.Window[0].ActiveLayers[0 + offset]   = (value & 0x02) != 0;  // W1 enable for first  layer
_state.Window[0].ActiveLayers[1 + offset]   = (value & 0x20) != 0;  // W1 enable for second layer
_state.Window[0].InvertedLayers[0 + offset] = (value & 0x01) != 0;
_state.Window[0].InvertedLayers[1 + offset] = (value & 0x10) != 0;
_state.Window[1].ActiveLayers[0 + offset]   = (value & 0x08) != 0;
_state.Window[1].ActiveLayers[1 + offset]   = (value & 0x80) != 0;
_state.Window[1].InvertedLayers[0 + offset] = (value & 0x04) != 0;
_state.Window[1].InvertedLayers[1 + offset] = (value & 0x40) != 0;
```

So per byte:

```
bit 7: W2 enable  second
bit 6: W2 invert  second
bit 5: W1 enable  second
bit 4: W1 invert  second
bit 3: W2 enable  first
bit 2: W2 invert  first
bit 1: W1 enable  first
bit 0: W1 invert  first
```

Standard.

### 2.5 Per-window combination semantics

For each value of CGWSEL[7:6] and [5:4], section 3 and 4 cover behavior.
For the per-layer **W1/W2 combination** in the BG/OBJ case:

| `MaskLogic[layer]` | Behavior |
|---|---|
| `Or` (`0`)   | mask if W1 covers OR W2 covers. |
| `And` (`1`)  | mask if W1 AND W2 both cover. |
| `Xor` (`2`)  | mask if exactly one of W1/W2 covers. |
| `Xnor` (`3`) | mask if both or neither cover. |

The `PixelNeedsMasking` inversion is applied **per-window** before logic, so
inversion is "this window's masking region is the complement of [Left,Right]".
That matches ares.

---

## 3. Force-main-screen-black (CGWSEL[7:6])

### 3.1 Where it's decoded

```c++
// SnesPpu.cpp:2199-2205
case 0x2130:
    //CGWSEL - Color Addition Select
    _state.ColorMathClipMode    = (ColorWindowMode)((value >> 6) & 0x03);  // bits 7..6
    _state.ColorMathPreventMode = (ColorWindowMode)((value >> 4) & 0x03);  // bits 5..4
    _state.ColorMathAddSubscreen = (value & 0x02) != 0;                    // bit 1
    _state.DirectColorMode       = (value & 0x01) != 0;                    // bit 0
    break;
```

`ColorWindowMode` enum (`SnesPpuTypes.h:13-19`):

```
0: Never
1: OutsideWindow
2: InsideWindow
3: Always
```

So **CGWSEL[7:6]** is `ColorMathClipMode` — "force main screen to black".
This **agrees with nocash**: 0 = never; 1 = outside color window; 2 = inside
color window; 3 = always.

### 3.2 Where it's applied

```c++
// SnesPpu.cpp:1302-1326
void SnesPpu::ApplyColorMathToPixel(uint16_t &pixelA, uint16_t pixelB, int x, bool isInsideWindow)
{
    uint8_t halfShift = (uint8_t)_state.ColorMathHalveResult;

    //Set color to black as needed based on clip mode
    switch(_state.ColorMathClipMode) {
        default:
        case ColorWindowMode::Never: break;

        case ColorWindowMode::OutsideWindow:
            if(!isInsideWindow) {
                pixelA = 0;
                halfShift = 0;
            }
            break;

        case ColorWindowMode::InsideWindow:
            if(isInsideWindow) {
                pixelA = 0;
                halfShift = 0;
            }
            break;

        case ColorWindowMode::Always: pixelA = 0; break;
    }

    if(!(_mainScreenFlags[x] & PixelFlags::AllowColorMath)) {
        //Color math doesn't apply to this pixel
        return;
    }
    ...
```

### 3.3 Exhaustive table — CGWSEL[7:6] for the *main pixel*

`isInsideWindow` is true iff `ProcessMaskWindow<ColorWindowIndex>` returns
true (color window 5).

| CGWSEL[7:6] | Outside color window | Inside color window |
|---|---|---|
| `00` Never          | pixel unchanged                     | pixel unchanged |
| `01` OutsideWindow  | pixelA = 0 (force black), halve→0   | pixel unchanged |
| `10` InsideWindow   | pixel unchanged                     | pixelA = 0, halve→0 |
| `11` Always         | pixelA = 0                          | pixelA = 0 |

The force-black runs **before** the AllowColorMath gate. So even a pixel
flagged "no color math" (e.g. an OBJ with palette < 4) will be blanked when
the clip rule says so. Side effect: when forced to black, `halfShift` is
cleared so the later add/sub math (if it runs) doesn't accidentally do
`(0 >> 1) + sub/2`; instead it would do `0 + sub`. That detail probably
doesn't apply because the AllowColorMath bit usually wasn't set if force-black
was triggered anyway, but the code is defensive.

**Polarity to watch in luna**: when `CGWSEL[7:6] == 01`, the main pixel is
forced black **outside the color window**, i.e. where `isInsideWindow == false`.
If luna's polarity is flipped (force-black inside the window for mode 01) it
will paint exactly the wrong region. Look hard at the `PixelNeedsMasking`
return-value convention and how it's combined for the color window — the
color window's `ActiveLayers[5]` / `InvertedLayers[5]` polarity is the same
as for BG/OBJ; an "inverted" window region is the complement of `[Left,Right]`.

### 3.4 Agrees with nocash?

Yes. Mesen2's mapping is exactly the nocash semantic:

- bits 7..6 of `$2130`: "Force Main Screen Black" (0=Never, 1=NotMath aka
  outside color window, 2=Math aka inside color window, 3=Always).
- The "Color Math Enable Region" bits 5..4 use the same enum with the same
  meaning, but gate the math operation instead of forcing black.

---

## 4. Color-math region (CGWSEL[5:4])

### 4.1 Decoded as `ColorMathPreventMode`

Same enum (`SnesPpuTypes.h:13-19`). Same table polarity:

| CGWSEL[5:4] | Outside color window | Inside color window |
|---|---|---|
| `00` Never           | math runs            | math runs |
| `01` OutsideWindow   | **math skipped**     | math runs |
| `10` InsideWindow    | math runs            | **math skipped** |
| `11` Always          | math skipped         | math skipped |

### 4.2 Where applied

```c++
// SnesPpu.cpp:1333-1351
//Prevent color math as needed based on mode
switch(_state.ColorMathPreventMode) {
    default:
    case ColorWindowMode::Never: break;
    case ColorWindowMode::OutsideWindow:
        if(!isInsideWindow) { return; }
        break;
    case ColorWindowMode::InsideWindow:
        if(isInsideWindow) { return; }
        break;
    case ColorWindowMode::Always: return;
}
```

This runs **after** the AllowColorMath check, so reaching it implies the
per-layer/per-OBJ math enable is on.

### 4.3 CGADSUB layer-enable (`$2131`) and per-pixel winner

`$2131` write decoder:

```c++
// SnesPpu.cpp:2207-2212
case 0x2131:
    //CGADSUB - Color math designation
    _state.ColorMathEnabled = value & 0x3F;                 // bits 0..5
    _state.ColorMathSubtractMode = (value & 0x80) != 0;
    _state.ColorMathHalveResult  = (value & 0x40) != 0;
    break;
```

Where bits 0..5 of `$2131` mean: bit 0=BG1, bit 1=BG2, bit 2=BG3, bit 3=BG4,
bit 4=OBJ, bit 5=Backdrop.

Each layer's renderer stamps the AllowColorMath flag onto **the winning main
pixel** *at the moment that pixel is drawn*:

- Tilemap (`SnesPpu.cpp:993`):
  ```c++
  uint8_t pixelFlags = (((_state.ColorMathEnabled >> layerIndex) & 0x01) ? PixelFlags::AllowColorMath : 0);
  ```
  then `DrawMainPixel(x, rgbColor, priority | pixelFlags)`.

- Mode-7 (`SnesPpu.cpp:1190`): same shape, parameterized over `layerIndex`.

- Sprites (`SnesPpu.cpp:962`):
  ```c++
  _mainScreenFlags[x] = spritePrio
      | (((_state.ColorMathEnabled & 0x10) && _spritePalette[x] > 3) ? PixelFlags::AllowColorMath : 0);
  ```
  **The "OBJ palette ≥ 4" gating IS applied here**. Sprites with OAM palette
  index 0..3 (CGRAM offsets 128..191) never get color math, even if `$2131.4`
  is set. Sprites with palette 4..7 (CGRAM offsets 192..255) do.

- Backdrop (`SnesPpu.cpp:924`):
  ```c++
  uint8_t pixelFlags = (_state.ColorMathEnabled & 0x20) ? PixelFlags::AllowColorMath : 0;
  ```

So "which layer's color-math enable bit applies" is determined by *which
layer drew the winning pixel*. If sprite wins, sprite's bit (with the palette
gate) applies; if BG2 wins, BG2's bit applies. This is exactly the SNES
behavior.

### 4.4 Subtract / halve / fixed-color path

```c++
// SnesPpu.cpp:1353-1379
uint16_t otherPixel;
if(_state.ColorMathAddSubscreen) {                    // CGWSEL bit 1
    if(_subScreenPriority[x] > 0) {                   // anything actually drawn on sub
        otherPixel = pixelB;                          // the real sub-pixel
    } else {
        //there's nothing in the subscreen at this pixel, use the fixed color and disable halve operation
        otherPixel = _state.FixedColor;
        halfShift = 0;                                // <- halve gets bypassed
    }
} else {
    otherPixel = _state.FixedColor;
}

constexpr unsigned int mask = 0x1F;
if(_state.ColorMathSubtractMode) {
    uint16_t r = std::max((int)((pixelA & mask) - (otherPixel & mask)), 0) >> halfShift;
    uint16_t g = std::max((int)(((pixelA >> 5U) & mask) - ((otherPixel >> 5U) & mask)), 0) >> halfShift;
    uint16_t b = std::max((int)(((pixelA >> 10U) & mask) - ((otherPixel >> 10U) & mask)), 0) >> halfShift;
    pixelA = r | (g << 5U) | (b << 10U);
} else {
    uint16_t r = std::min(((pixelA & mask) + (otherPixel & mask)) >> halfShift, mask);
    uint16_t g = std::min((((pixelA >> 5U) & mask) + ((otherPixel >> 5U) & mask)) >> halfShift, mask);
    uint16_t b = std::min((((pixelA >> 10U) & mask) + ((otherPixel >> 10U) & mask)) >> halfShift, mask);
    pixelA = r | (g << 5U) | (b << 10U);
}
```

Important polarities:

- `_subScreenPriority[x] > 0` is the test for "subscreen actually has a
  non-backdrop pixel". This is `> 0`, not `≥ 0`. A backdrop pixel painted via
  `RenderBgColor` sets `_subScreenPriority[x]` to **0** (the function only
  paints when the slot was 0 to begin with — see `SnesPpu.cpp:931-934`). So
  the test correctly says "if subscreen winner is a real layer/sprite, use
  it; if it's just backdrop, fall back to fixed color and skip halving."
- Subtract is clamp-to-0 per component, add is clamp-to-31 per component, both
  performed in BGR555 with 5-bit components.
- Halving (CGADSUB bit 6) is done **after** the add/sub, applied to each
  component, and is suppressed when the "subscreen has nothing" fallback kicks
  in.
- Subtract mode (CGADSUB bit 7) is *unconditional* sign — both clamp-to-0 and
  the same per-component halving.

---

## 5. Sub-screen rendering

### 5.1 The sub-screen is real

Mesen2 fully renders the sub-screen with its own per-layer winners. Every
tilemap/sprite render path has parallel `drawMain` / `drawSub` write blocks
(see `SnesPpu.cpp:944-945, 977-978, 1049-1056, 1132-1133, 1249-1255` plus the
RenderSprites quote in section 1.3).

- `_state.MainScreenLayers` = `$212C & 0x1F` (`SnesPpu.cpp:2175-2178`).
- `_state.SubScreenLayers`  = `$212D & 0x1F` (`SnesPpu.cpp:2180-2183`).
- `_state.WindowMaskMain[5]` = `$212E` low 5 bits, per-layer (`SnesPpu.cpp:2185-2190`).
- `_state.WindowMaskSub[5]`  = `$212F` low 5 bits, per-layer (`SnesPpu.cpp:2192-2197`).

So sub-screen rendering uses:

- Different layer enables (`$212D`).
- Different per-layer window enables (`$212F`).
- Same per-layer window definitions (`$2123/4/5`, `$2126-9`, `$212A/B`).
- Same per-layer priorities.

### 5.2 CGWSEL bit 1 = "ColorMathAddSubscreen"

```c++
// SnesPpu.cpp:2203
_state.ColorMathAddSubscreen = (value & 0x02) != 0;
```

Bit 1 of `$2130`. **0 = use fixed color always**, **1 = use sub-screen pixel
if it's non-backdrop, else fall back to fixed color**.

That is — Mesen2 does NOT treat bit 1 as the *enable* of the sub-screen
rendering itself; the sub-screen is *always rendered* regardless of this bit.
Bit 1 only chooses what the math second-operand is. This is consistent with
ares and with real hardware.

### 5.3 Sub-screen backdrop

There is no separate sub-screen backdrop register. CGRAM entry 0 (`$00/$01`
in CGRAM) is the sole backdrop. `RenderBgColor` writes
`_subScreenBuffer[x] = _cgram[0]` with priority 0 whenever no sub-screen
winner exists. The "fixed color" register (`$2132`/`COLDATA`) only acts as
the alternate add/sub operand in color math, not as a sub-screen backdrop.

### 5.4 Direct-color mode (CGWSEL bit 0)

`_state.DirectColorMode = (value & 0x01) != 0;` (`SnesPpu.cpp:2204`).

Used in `GetRgbColor<bpp=8, directColorMode=true, ...>` (`SnesPpu.cpp:1071-1076`)
and in `RenderTilemapMode7<>` (`SnesPpu.cpp:1241-1247`):

```c++
if(directColorMode) {
    paletteColor = ((colorIndex & 0x07) << 2)
                 | ((colorIndex & 0x38) << 4)
                 | ((colorIndex & 0xC0) << 7);
} else {
    paletteColor = _cgram[colorIndex];
}
```

The 4bpp mode-7 direct-color formula:
- bits 0..2 -> R bits 2..4 (i.e. shifted to upper R)
- bits 3..5 -> G bits 5..9 (with low/high spread)
- bits 6..7 -> B bits 12..13

For 8bpp + direct-color (`SnesPpu.cpp:1071-1076`), an extra palette-index
modulation also adds palette bits 0..2 into the lowest bit of each
component.

Direct color mode only takes effect for layers using 8bpp tiles (mode 3 BG1,
mode 4 BG1, mode 7) and is ignored otherwise. Mesen2 templates pick it
through `RenderTilemap<...>` at `SnesPpu.cpp:2414-2418` (recursive template
expansion adds the `directColorMode` parameter from `_state.DirectColorMode`)
and through `RenderTilemapMode7<...>` at `SnesPpu.cpp:2466-2470` (which
guards `if(_state.DirectColorMode && layerIndex == 0)` so only mode-7 BG1
ever uses direct color; mode-7 BG2/EXTBG never does).

---

## 6. OBJ rendering

### 6.1 OAM layout / sizes

`FetchSpritePosition` (`SnesPpu.cpp:669-691`):

```c++
static constexpr uint8_t oamWidth[16]  = { 8,8,8,16,16,32,16,16, 16,32,64,32,64,64,32,32 };
static constexpr uint8_t oamHeight[16] = { 8,8,8,16,16,32,32,32, 16,32,64,32,64,64,64,32 };
...
uint8_t highTableValue = _oamRam[0x200 | (spriteIndex >> 2)] >> ((spriteIndex << 1) & 0x06);
_currentSprite.X = (int16_t)(sign[highTableValue & 0x01] | _oamRam[(spriteIndex << 2)]);
_currentSprite.Y = _oamRam[(spriteIndex << 2) + 1];

uint8_t mode = _state.OamMode | ((highTableValue & 0x02) << 2);
_currentSprite.Width  = oamWidth[mode];
_currentSprite.Height = oamHeight[mode];
```

OAM layout: 512 bytes of low table at $000-$1FF, 32 bytes of "high" table at
$200-$21F. The high-table byte for sprite N is `0x200 | (N >> 2)`, shifted
by `(N << 1) & 6` to get the 2 bits for that sprite — low bit is X-MSB sign,
high bit is large/small selector.

Sprite mode (OamMode 0..7) and large/small combine to index `oamWidth/Height`.

### 6.2 Per-scanline evaluation (sprite-in-range)

`EvaluateNextLineSprites` (`SnesPpu.cpp:595-623`):

```c++
if(_spriteEvalStart == 0) {
    _spriteCount = 0;
    _oamEvaluationIndex = _state.EnableOamPriority ? ((_state.InternalOamAddress & 0x1FC) >> 2) : 0;
}

if(_state.ForcedBlank) {
    return;
}

for(int i = _spriteEvalStart; i <= _spriteEvalEnd; i++) {
    if(i & 0x01) {
        if(_currentSprite.IsVisible(_scanline, _state.ObjInterlace)) {
            if(_spriteCount < 32) {
                _spriteIndexes[_spriteCount] = _oamEvaluationIndex;
                _spriteCount++;
            } else {
                _rangeOver = true;
            }
        }
        _oamEvaluationIndex = (_oamEvaluationIndex + 1) & 0x7F;
    } else {
        FetchSpritePosition(_oamEvaluationIndex);
    }
}
```

`IsVisible` (`SnesPpuTypes.h:38-51`):

```c++
bool IsVisible(uint16_t scanline, bool interlace)
{
    if(X != -256 && (X + Width <= 0 || X > 255)) {
        //Sprite is not visible (and must be ignored for time/range flag calculations)
        //Sprites at X=-256 are always used when considering Time/Range flag calculations, but not actually drawn.
        return false;
    }
    uint16_t endY = Y + (interlace ? (Height >> 1) : Height);
    return (
        (scanline >= Y && scanline < endY) ||
        ((uint8_t)endY < Y && scanline < (uint8_t)endY) //wrap-around occurs after 256 scanlines
    );
}
```

Notable details:

- **OBJ priority rotation** (`$2103.7` = `EnableOamPriority`): when set, sprite
  evaluation starts at `(InternalOamAddress & 0x1FC) >> 2` instead of 0
  (`SnesPpu.cpp:599`). This is the standard "first sprite = current OAM
  address" priority shift.
- `X == -256` (i.e. `0x100` with high bit set) is the "ignore X-range" magic
  — sprite is invisible but still counts for time/range flag accounting. Mesen2
  preserves this quirk explicitly.
- Vertical wraparound: a sprite whose Y+Height crosses 256 wraps; `IsVisible`
  uses the truncated `(uint8_t)endY` for the wrap-around check.
- 32-sprite-per-line range cap (vs 34-tile cap below) sets `_rangeOver` (the
  range-over status bit in `$213E.6`).

### 6.3 Sprite tile fetching + per-pixel write into sprite buffers

`FetchSpriteData` (`SnesPpu.cpp:625-667`) walks the evaluated sprite list at
H=270..339, calling `FetchSpriteAttributes` to read tile/flags + `FetchSpriteTile`
to read two CHR words per sprite.

`FetchSpriteTile` (`SnesPpu.cpp:754-779`):

```c++
uint16_t chrData = _vram[_currentSprite.FetchAddress];
_currentSprite.ChrData[secondCycle] = chrData;

if(!secondCycle) {
    _currentSprite.FetchAddress = (_currentSprite.FetchAddress + 8) & 0x7FFF;
} else {
    int16_t xPos = _currentSprite.DrawX;
    for(int x = 0; x < 8; x++) {
        if(xPos + x < 0 || xPos + x > 255) continue;
        uint8_t xOffset = _currentSprite.HorizontalMirror ? ((7 - x) & 0x07) : x;
        uint8_t color = GetTilePixelColor<4>(_currentSprite.ChrData, 7 - xOffset);
        if(color != 0) {
            _spriteColorsCopy[xPos + x]   = color;
            _spritePriorityCopy[xPos + x] = _currentSprite.Priority;
            _spritePaletteCopy[xPos + x]  = _currentSprite.Palette;
        }
    }
}
```

So sprites are evaluated in **OAM order** during the fetch phase, and the
*last* sprite to write a non-zero pixel to a given column wins — this is the
"lower-OAM-index sprite wins" priority rule, implemented in reverse: the
loop walks `_spriteIndexes[_spriteCount-1]` down to `_spriteIndexes[0]`
(decrement at line `_spriteCount--` in `FetchSpriteAttributes`), so the first
sprite in OAM order is the last to write, and wins.

Sprite tile-count over 34 sets `_timeOver` (`$213E.7`):

```c++
// SnesPpu.cpp:695-698
_spriteTileCount++;
if(_spriteTileCount > 34) {
    _timeOver = true;
}
```

### 6.4 Sprite-zero — no special case

Mesen2 does **not** treat sprite 0 specially. There's no sprite-0 hit
register on the SNES (unlike NES), so this is correct.

### 6.5 OAM auto-write / OAMADDR latch reload at VBlank

The OAM address gets reloaded from `$2102/$2103` at the start of vblank
(`SnesPpu.cpp:464-472`):

```c++
if(_scanline == _nmiScanline) {
    ProcessLocationLatchRequest();
    _latchRequest = false;

    //Reset OAM address at the start of vblank?
    if(!_state.ForcedBlank) {
        //TODO, the timing of this may be slightly off? should happen at H=10 based on anomie's docs
        _state.InternalOamAddress = (_state.OamRamAddress << 1);
    }
    ...
}
```

So:

1. When entering vblank (scanline = `_nmiScanline`, which is 225 NTSC / 240
   PAL by default — see `UpdateNmiScanline` at `SnesPpu.cpp:538-561`):
   `InternalOamAddress` is reset to `OamRamAddress << 1` **only when force
   blank is OFF**.
2. Writes to `$2100` while force blank is on at scanline `_nmiScanline` also
   trigger the reset (`SnesPpu.cpp:1889-1896`):
   ```c++
   case 0x2100:
       if(_state.ForcedBlank && _scanline == _nmiScanline) {
           //"writing this register on the first line of V-Blank (225 or 240, depending on overscan) when force blank is currently active causes the OAM Address Reset to occur."
           UpdateOamAddress();
       }
       _state.ForcedBlank = (value & 0x80) != 0;
       _state.ScreenBrightness = value & 0x0F;
       break;
   ```
3. Writes to `$2102` and `$2103` also call `UpdateOamAddress()` immediately
   (`SnesPpu.cpp:1905-1914`), which is just `_state.InternalOamAddress = (_state.OamRamAddress << 1);`
   (`SnesPpu.cpp:1672-1675`).

`UpdateOamAddress` doesn't check force-blank or scanline; only the vblank-
auto-reload at `_nmiScanline` does. So the standard programming idiom — write
`$2102/$2103 = 0` once, then stream OAM via `$2104` during vblank — relies on
the per-vblank auto-reload to start fresh at index 0 each frame, **only if
force blank is off when the vblank line begins** (or the game does its own
`$2100` write at vblank with force blank already on).

### 6.6 Priority handling

Per-scanline OBJ priority is one of 4 values (0..3), the high nybble of the
OAM flags byte (`SnesPpu.cpp:700-703`):

```c++
uint8_t flags = _oamRam[oamAddress + 1];
_currentSprite.Palette = (flags >> 1) & 0x07;
_currentSprite.Priority = (flags >> 4) & 0x03;
_currentSprite.HorizontalMirror = (flags & 0x40) != 0;
```

And then per-mode the priority is remapped to a 1..16 scale in `RenderSprites`
via the `spritePriorities[4]` constant arrays (`SnesPpu.cpp:783, 794, 808,
817, 826, 835, 844, 852`). For example mode-1 (SMW's main mode) — `RenderMode1`,
`SnesPpu.cpp:792-804`:

```c++
constexpr uint8_t spritePriorities[4] = { 2, 4, 7, 10 };
RenderSprites(spritePriorities);
RenderTilemap<0, 4, 6, 9>();          // BG1 with normal=6, high=9
RenderTilemap<1, 4, 5, 8>();          // BG2 with normal=5, high=8
if(!_state.Mode1Bg3Priority) {
    RenderTilemap<2, 2, 1, 3>();      // BG3 normal=1, high=3
} else {
    RenderTilemap<2, 2, 1, 11>();     // BG3 with high promoted above OBJ-3
}
```

So in mode 1 with `Mode1Bg3Priority=true` (the typical "windows shows above
sprites" trick), BG3's high-priority tiles get priority 11, above the
highest sprite (10). This is the standard SNES priority table.

OBJ priority 0..3 maps to `spritePriorities[i]`, so an OBJ pixel's effective
priority in the comparison is `priority[_spritePriority[x]]`.

### 6.7 OAM addressing & `$2138` quirks

The standard rules:

- Internal OAM address is `(OamRamAddress << 1)` and addresses bytes
  (`SnesPpu.cpp:1672-1675`).
- During rendering, reads/writes to OAM use the **PPU's currently-fetching
  index** (`SnesPpu.cpp:1677-1689`):
  ```c++
  uint16_t SnesPpu::GetOamAddress()
  {
      if(_state.ForcedBlank || _scanline >= _vblankStartScanline) {
          return _state.InternalOamAddress;
      } else {
          _emu->BreakIfDebugging(...);
          if(_memoryManager->GetHClock() <= 255 * 4) {
              return _oamEvaluationIndex << 2;
          } else {
              return _oamTimeIndex << 2;
          }
      }
  }
  ```
- The high table is mirrored as `0x200 | (oamAddr & 0x1F)` — `$2104` writes
  with the high byte buffer pair semantics (`SnesPpu.cpp:1916-1948`).

---

## 7. DMA / HDMA timing

### 7.1 Event-driven dispatch

Memory manager has a per-scanline event sequence (`SnesMemoryManager.cpp:224-265`):

```
HdmaInit   (at scanline 0, after end-of-scanline)
DramRefresh
HdmaStart  (only outside vblank)
EndOfScanline (every line)
```

`Exec()` (`SnesMemoryManager.cpp:206-222`) ticks the master clock 2 cycles at
a time; when `_hClock == _nextEventClock`, `ProcessEvent()` fires.

```c++
// SnesMemoryManager.cpp:227-262
case SnesEventType::HdmaInit:
    _console->GetDmaController()->BeginHdmaInit();
    _nextEvent = SnesEventType::DramRefresh;
    _nextEventClock = _dramRefreshPosition;
    break;

case SnesEventType::DramRefresh:
    IncMasterClock40();
    if(_ppu->GetScanline() < _ppu->GetVblankStart()) {
        _nextEvent = SnesEventType::HdmaStart;
        _nextEventClock = 276 * 4;        // hclock 1104
    } else {
        _nextEvent = SnesEventType::EndOfScanline;
        _nextEventClock = 1360;
    }
    break;

case SnesEventType::HdmaStart:
    _console->GetDmaController()->BeginHdmaTransfer();
    _nextEvent = SnesEventType::EndOfScanline;
    _nextEventClock = 1360;
    break;
```

So:

- **HDMA init** happens once per frame, scheduled after end-of-scanline for
  scanline 0 (`SnesMemoryManager.cpp:253-255`), at hclock `12 + (masterClock & 7)`.
- **HDMA transfer** happens at hclock 1104 (dot 276) for every visible
  scanline (scanline < vblank-start).
- **End-of-scanline** is at hclock 1360.

This matches ares: HDMA fires near the end of every visible scanline.

### 7.2 `$420B` (MDMAEN) — start general DMA

```c++
// SnesDmaController.cpp:411-425
case 0x420B: {
    //MDMAEN - DMA Enable
    for(int i = 0; i < 8; i++) {
        if(value & (1 << i)) {
            _state.Channel[i].DmaActive = true;
        }
    }
    if(value) {
        _dmaPending = true;
        _dmaStartDelay = true;
        UpdateNeedToProcessFlag();
    }
    break;
}
```

`_dmaPending` and `_dmaStartDelay` are then processed in
`ProcessPendingTransfers` (`SnesDmaController.cpp:369-406`) — there's a one-
cycle start delay (`_dmaStartDelay` cleared on its own pass), then
`SyncStartDma`, then 8 master cycles overhead, then each active channel runs
to completion in order 0..7 via `RunDma` (`SnesDmaController.cpp:68-102`).

`RunDma` uses 8 cycles per byte per channel, plus the 8-cycle channel-startup
overhead — so 8 + 8*N total. (One-byte-at-a-time, with `ProcessPendingTransfers`
called between every byte to allow HDMA to preempt — `SnesDmaController.cpp:96`.)
Synchronization with the CPU clock is done via `SyncStartDma` (`SnesDmaController.cpp:206-211`)
and `SyncEndDma` (`SnesDmaController.cpp:213-218`).

### 7.3 `$420C` (HDMAEN) — set HDMA channel enable bits

```c++
// SnesDmaController.cpp:427-430
case 0x420C:
    //HDMAEN - HDMA Enable
    _state.HdmaChannels = value;
    break;
```

The actual HDMA init / per-scanline transfer doesn't fire from this write —
it fires from the per-scanline `HdmaInit` and `HdmaStart` events above. The
`$420C` write only sets the channel mask.

### 7.4 `$4200` NMITIMEN — auto-joypad-read fire timing

Auto-joypad-read is driven by master-clock scheduling, NOT scanline.

`SetAutoJoypadReadClock` (`InternalRegisters.cpp:53-60`):

```c++
//Auto-read starts at the first multiple of 256 master clocks after dot 32.5 (hclock 130)
uint64_t rangeStart = _console->GetMasterClock() + 130;
_autoReadClockStart = rangeStart + ((rangeStart & 0xFF) ? (256 - (rangeStart & 0xFF)) : 0) - 128;
_autoReadNextClock = _autoReadClockStart;
_autoReadDisabled = false;
```

`ProcessAutoJoypad` (`InternalRegisters.cpp:62-147`) runs a 35-step state
machine clocked at 128 master cycles per step:

- step 0: strobe high (`SetAutoReadStrobe(EnableAutoJoypadRead)`).
- step 1: latch enable check; reset controller data to 0.
- step 2: strobe low.
- steps 3..33: odd = read `$4016`/`$4017`; even = shift+insert.
- step ≥ 34: done.

```c++
// InternalRegisters.cpp:114-128
} else if((step & 0x01) == 1) {
    //First half of the 256-clock cycle reads the ports
    _autoReadPort1Value = _controlManager->Read(0x4016, true);
    _autoReadPort2Value = _controlManager->Read(0x4017, true);
} else {
    //Second half shifts the data and inserts the new bit at position 0
    _state.ControllerData[0] <<= 1;
    _state.ControllerData[1] <<= 1;
    _state.ControllerData[2] <<= 1;
    _state.ControllerData[3] <<= 1;
    _state.ControllerData[0] |= (_autoReadPort1Value & 0x01);
    _state.ControllerData[1] |= (_autoReadPort2Value & 0x01);
    _state.ControllerData[2] |= (_autoReadPort1Value & 0x02) >> 1;
    _state.ControllerData[3] |= (_autoReadPort2Value & 0x02) >> 1;
}
```

`ProcessAutoJoypad` is called once per scanline at end-of-scanline (`SnesPpu.cpp:462`)
plus on every relevant register read or `$4200`/`$4201` write to keep state
in sync.

The "in progress" bit at `$4212.0` is `(_autoReadActive ? 0x01 : 0)`
(`InternalRegisters.cpp:200, 266`).

What's written to `$4218..$421F`: 16-bit shift register per port, MSB-first
shifted. After 16 shifts the standard SNES gamepad word layout is:

```
$4218.7 = B   $4219.7 = A   $421A.7 = B (port2)  ...
$4218.6 = Y   $4219.6 = X
$4218.5 = Sel $4219.5 = L
$4218.4 = Sta $4219.4 = R
$4218.3 = U   $4219.3 = '0'
$4218.2 = D   $4219.2 = '0'
$4218.1 = L   $4219.1 = '0'
$4218.0 = R   $4219.0 = '0' (controller-type ID bit)
```

The actual bit ordering is built by the controller's `Read(0x4016, true)` and
the shift-left-then-insert step above; the resulting word lives in
`_state.ControllerData[port]`. Reads are bit-swizzled in `ReadControllerData`
(`InternalRegisters.cpp:149-168`).

`SetAutoJoypadReadClock()` is called from somewhere in the per-frame
machinery (search history — likely `SnesPpu::ProcessEndOfScanline` at the
scanline that begins the latch window; not visible directly in the files
fetched but the call exists). The key behavior is: auto-joypad-read **runs
during early vblank** (starting at the first 256-mclk multiple after dot 32.5
of the latch scanline) and completes well before the next frame begins.

### 7.5 `$4200` bit 0 — when the latch fires

```c++
// InternalRegisters.cpp:298-329  (case 0x4200)
bool autoRead = (value & 0x01) != 0;
if(_state.EnableAutoJoypadRead != autoRead) {
    ProcessAutoJoypad();
    if(_autoReadClockStart <= _console->GetMasterClock() &&
       (_console->GetMasterClock() - _autoReadClockStart) < 256) {
        //If EnableAutoJoypadRead changes at any point in the first 256 clocks of this process,
        //the value sent to OUT0 can be changed and immediately strobe the controllers, etc.
        _controlManager->SetAutoReadStrobe(autoRead);
    }
}
...
_state.EnableAutoJoypadRead = autoRead;
```

So if a game writes `$4200 = 0x01` mid-frame and is within the latch window,
the strobe will fire. The actual gamepad latch happens *during* the state-
machine steps, not at `$4200` write time itself.

---

## 8. NMI + frame timing

### 8.1 NMI fire timing

NMI is set in `ProcessIrqCounters` (`InternalRegisters.h:104-161`):

```c++
} else if(hClock == 6) {
    _hCounter = 0;
    if(_ppu->GetScanline() > 0 && !_ppu->IsInOverclockedScanline()) {
        _vCounter++;
    }

    if(_state.EnableNmi && _ppu->GetScanline() == _ppu->GetNmiScanline()) {
        _cpu->SetNmiFlag(1);
    }
} else if(hClock == 2) {
    _hCounter++;
    if(_ppu->GetScanline() == _ppu->GetNmiScanline()) {
        _nmiFlag = true;
    } else if(_ppu->GetScanline() == 0) {
        _nmiFlag = false;
        _vCounter = 0;
    }
}
```

So at the start of the NMI scanline (`_nmiScanline`):
- At hclock 2: the readable `$4210.7` NMI flag (`_nmiFlag`) goes high.
- At hclock 6: if `$4200.7 = 1` (`EnableNmi`), the CPU's NMI line is asserted
  (one tick later — `SetNmiFlag(1)`).

`UpdateNmiScanline` (`SnesPpu.cpp:538-561`) decides the line:
- NTSC: 225 normal / 240 overscan.
- PAL: 311 / 312 base end-of-vblank.
- `_vblankStartScanline = _state.OverscanMode ? 240 : 225;`
- `_nmiScanline = _vblankStartScanline + ppuExtraScanlinesBeforeNmi;`

`$4210` read clears the NMI flag, with a guard against reading-the-set-cycle
(`InternalRegisters.cpp:229-244`):

```c++
//Reading $4210 on any cycle clears the NMI flag,
//Except between from cycles 2 to 5 on the NMI scanline...
if(_nmiFlag && (_memoryManager->GetHClock() >= 6 || _ppu->GetScanline() != _ppu->GetNmiScanline())) {
    SetNmiFlag(false);
}
return value | (_memoryManager->GetOpenBus() & 0x70);
```

### 8.2 `$4212` HVBJOY semantics

```c++
// InternalRegisters.cpp:256-269
case 0x4212: {
    ProcessAutoJoypad();
    uint16_t hClock = _memoryManager->GetHClock();
    uint16_t scanline = _ppu->GetScanline();
    uint16_t nmiScanline = _ppu->GetNmiScanline();
    //TODO TIMING (set/clear timing)
    return (
        (scanline >= nmiScanline ? 0x80 : 0) |
        ((hClock >= 1*4 && hClock <= 274*4) ? 0 : 0x40) |
        (_autoReadActive ? 0x01 : 0) |
        (_memoryManager->GetOpenBus() & 0x3E)
    );
}
```

So **bit 7** = scanline ≥ NMI scanline (i.e. we are in vblank).
**Bit 6** = hclock outside `[4, 1096]` (the visible-dot window) — i.e. it's
high during hblank, including the front porch before dot 1, AND the back
porch after dot 274 (=hclock 1096). Important: this is a **live, per-mclk**
HBlank signal, not just a vblank-derived value. Per-pixel sample.
**Bit 0** = auto-joypad-read in progress.
**Bits 1..5** = open-bus (`0x3E` mask).

luna's recent fix in commit `9d801f8` ("HVBJOY bit 6 = live Hblank") is
exactly what Mesen2 does here.

### 8.3 Open-bus behavior for PPU reads

PPU registers `$2134..$213F` and aliases tracked in two open-bus latches:

```c++
// SnesPpu.cpp:1873-1879
uint16_t reg = addr & 0x210F;
if((reg >= 0x2104 && reg <= 0x2106) || (reg >= 0x2108 && reg <= 0x210A)) {
    //Registers matching $21x4-6 or $21x8-A (where x is 0-2) return the last value read from any of the PPU1 registers $2134-6, $2138-A, or $213E.
    return _state.Ppu1OpenBus;
}
return _console->GetMemoryManager()->GetOpenBus();
```

- `_state.Ppu1OpenBus` is updated by reads of `$2134-6, $2138-A, $213E`.
- `_state.Ppu2OpenBus` is updated by reads of `$213B-D, $213F`.
- Specific reads use these in partial returns (e.g. `$213C` returns 7 bits of
  PPU2 open-bus in the MSB high byte path — `SnesPpu.cpp:1801-1815`).

---

## 9. Mode-7 + EXTBG

### 9.1 Mode-7 BG1 integration with the DAC

`RenderTilemapMode7<layerIndex=0, ...>` (`SnesPpu.cpp:1126-1258`) renders BG1
of mode 7. The pixel goes through `_cgram[colorIndex]` lookup (or direct
color via the formula in section 5.4), then through `DrawMainPixel` /
`DrawSubPixel` exactly like every other tilemap, and into the standard color-
math chain.

Mode 7 priority is always `normalPriority` for BG1 (3 in mode 7 mapping —
`SnesPpu.cpp:1218-1225`):

```c++
if constexpr(layerIndex == 1) {
    uint8_t color = _vram[((tileIndex << 6) + ((yOffset & 0x07) << 3) + (xOffset & 0x07))] >> 8;
    priority = (color & 0x80) ? highPriority : normalPriority;
    colorIndex = (color & 0x7F);
} else {
    priority = normalPriority;
    colorIndex = _vram[((tileIndex << 6) + ((yOffset & 0x07) << 3) + (xOffset & 0x07))] >> 8;
}
```

### 9.2 EXTBG (BG2 overlay)

`RenderMode7` (`SnesPpu.cpp:850-859`):

```c++
constexpr uint8_t spritePriorities[4] = { 2, 4, 6, 7 };
RenderSprites(spritePriorities);
RenderTilemapMode7<0, 3, 3>();
if(_state.ExtBgEnabled) {
    RenderTilemapMode7<1, 1, 5>();
}
```

EXTBG (`$2133.6`) enables BG2 in mode 7 as a second logical layer reading the
same tilemap but using bit 7 of the pixel byte as a per-pixel priority bit
(normal=1, high=5). The "color" then masks off bit 7 — `colorIndex = (color & 0x7F)`.

### 9.3 Mode 7 matrix and scroll latching

`_state.Mode7.HScrollLatch` and `VScrollLatch` are taken once per scanline at
`_drawStartX == 0` (`SnesPpu.cpp:1137-1141`), so writes to `$210D/$210E` during
hblank can update them for the next scanline.

---

## 10. SMW Yoshi's-House dialog box & sprite-build observations

### 10.1 Why the dialog interior should be translucent blue (Mesen2 logic)

Translucent blue in SMW's dialog box on Yoshi's House intro is a per-pixel
color-math add: BG2 (or BG3) fills the dialog interior with a base color
(typically near-blue), then enables CGADSUB to add the sub-screen below it.
The sub-screen contains the level scene (BG1 + sprites). The "translucent
blue" effect is `dialog_interior_pixel + scene_below_pixel`, halved.

In Mesen2 terms this requires:

1. **Sub-screen rendering of BG1/sprites must produce real pixels** —
   `RenderSprites` / `RenderTilemap` write to `_subScreenBuffer` and
   `_subScreenPriority` with `drawSub == true`, gated by `$212D` (TS). luna
   needs to perform the same per-layer `drawSub` writes.
2. **`$2131` (CGADSUB) must enable color math for the layer drawing the
   dialog box interior** — Mesen2 writes `_state.ColorMathEnabled = value &
   0x3F` (`SnesPpu.cpp:2209`) and stamps `AllowColorMath` on the winning
   pixel for that layer (`SnesPpu.cpp:993`).
3. **`$2130.1` (`ColorMathAddSubscreen`) must be set** so the math second
   operand is the actual sub-screen pixel (`SnesPpu.cpp:1354-1356`), not the
   fixed color `$2132`.
4. **CGWSEL[7:6] must be 0 or 2** (`Never` or `InsideWindow`) so the dialog
   region isn't forced to black — this is the polarity to double-check in luna.
5. **CGWSEL[5:4] must allow math in the dialog region** — likely `Never` (0)
   or `OutsideWindow` (1) depending on the dialog's window setup.

If luna's sub-screen rendering is incomplete (only fills backdrop, never
draws BG1/sprites to `_subScreenBuffer`), the `ColorMathAddSubscreen` branch
at `SnesPpu.cpp:1354-1361` would see `_subScreenPriority[x] == 0` and
**fall back to fixed color with halve disabled**:

```c++
if(_subScreenPriority[x] > 0) {
    otherPixel = pixelB;
} else {
    //there's nothing in the subscreen at this pixel, use the fixed color and disable halve operation
    otherPixel = _state.FixedColor;
    halfShift = 0;
}
```

If `$2132` was last written to a black-ish fixed color, the dialog interior
would render as `dialog_pixel + 0 = dialog_pixel` (no apparent blend), or if
the dialog's base color is dark and CGADSUB is in subtract mode, the result
collapses to **pure black**. This matches the reported symptom exactly.

The Mesen2 `_subScreenPriority[x] > 0` polarity is load-bearing: any BG/OBJ
write to the sub-screen sets priority ≥ 1, so the moment luna correctly
renders any sub-screen layer at that pixel, the math operand switches from
fixed-color to real sub-pixel. If luna's `_subScreenPriority` uses a
different "no winner" sentinel (e.g. 0xFF), the comparison polarity will be
wrong.

### 10.2 Why Mario sprite might be missing

In Mesen2, sprites are evaluated in `EvaluateNextLineSprites` (`SnesPpu.cpp:595-623`)
which requires either `!ForcedBlank` to actually evaluate OAM, AND the OAM
shadow data must be written to `_oamRam` before the line is rendered. SMW
typically streams a fresh shadow OAM to `$2104` during NMI via DMA from
`$0200-$03FF` in WRAM. If:

- NMI isn't firing (luna's `$4200.7` not handled), the per-frame OAM build
  never happens; OR
- The OAM DMA (typically channel 0 via `$420B = 0x01`) doesn't push the data
  to `$2104`; OR
- The OAM auto-address-reset at vblank (`SnesPpu.cpp:464-472`) isn't
  happening, so writes go to the wrong sprite slots,

...sprite-zero will be at OAM index 0 from a previous frame or zeros, and
no Mario sprite is visible. Compare luna's vblank handler to Mesen2's
`_state.InternalOamAddress = (_state.OamRamAddress << 1)` block at
`SnesPpu.cpp:471`.

### 10.3 Artifacts during level load

Likely root cause: hidden writes happening to PPU registers during the
non-blanking part of the level-load sequence. Mesen2 reflects every register
write into the per-line state, and `Read`/`Write` of `$21xx`/`$42xx` triggers
a `RenderScanline()` first (`SnesPpu.cpp:1712-1714, 1884-1886`) so the line
up to that point uses the *old* state, and the rest of the line uses the
*new* state. luna needs the same mid-scanline state latching for transitions
like "force blank off + new BG1 tilemap addr written during line 224" to
match real hardware.

---

## Quick polarity / bit-cheat-sheet for cross-checking luna

| Register | Bit(s) | Mesen2 field | Polarity |
|---|---|---|---|
| `$2100.7` | force blank | `_state.ForcedBlank` | 1 = force blank on |
| `$2100.0-3` | brightness | `_state.ScreenBrightness` | 0..15 |
| `$212C` | TM | `_state.MainScreenLayers` | bit 0=BG1, 1=BG2, 2=BG3, 3=BG4, 4=OBJ |
| `$212D` | TS | `_state.SubScreenLayers` | same |
| `$212E` | TMW | `_state.WindowMaskMain[0..4]` | bit i = mask layer i |
| `$212F` | TSW | `_state.WindowMaskSub[0..4]` | bit i = mask layer i |
| `$2130.7-6` | CGWSEL | `_state.ColorMathClipMode` | 0=Never, 1=Outside, 2=Inside, 3=Always — force MAIN BLACK |
| `$2130.5-4` | CGWSEL | `_state.ColorMathPreventMode` | same enum — gate MATH off |
| `$2130.1` | CGWSEL | `_state.ColorMathAddSubscreen` | 1 = use sub-pixel else fixed |
| `$2130.0` | CGWSEL | `_state.DirectColorMode` | direct color (mode 3/4/7) |
| `$2131.7` | CGADSUB | `_state.ColorMathSubtractMode` | 1 = subtract, 0 = add |
| `$2131.6` | CGADSUB | `_state.ColorMathHalveResult` | 1 = halve after add/sub |
| `$2131.5` | CGADSUB | `_state.ColorMathEnabled & 0x20` | backdrop math enable |
| `$2131.4` | CGADSUB | `_state.ColorMathEnabled & 0x10` | OBJ math enable (gated by palette ≥ 4) |
| `$2131.0..3` | CGADSUB | `_state.ColorMathEnabled & 0x01..0x08` | BG1..BG4 math enable |
| `$2132` | COLDATA | `_state.FixedColor` | bits 7/6/5 of write byte = B/G/R selectors, low 5 = value |
| `$2133.7` | SETINI | external sync | NOT USED |
| `$2133.6` | SETINI | `_state.ExtBgEnabled` | mode 7 EXTBG |
| `$2133.3` | SETINI | `_state.HiResMode` | pseudo-hi-res |
| `$2133.2` | SETINI | `_state.OverscanMode` | 239-line |
| `$2133.1` | SETINI | `_state.ObjInterlace` | sprite interlace |
| `$2133.0` | SETINI | `_state.ScreenInterlace` | screen interlace |
| `$4200.7` | NMITIMEN | `_state.EnableNmi` | enable NMI |
| `$4200.5` | NMITIMEN | `_state.EnableVerticalIrq` | |
| `$4200.4` | NMITIMEN | `_state.EnableHorizontalIrq` | |
| `$4200.0` | NMITIMEN | `_state.EnableAutoJoypadRead` | |
| `$4210.7` | RDNMI | `_nmiFlag` | set at NMI scanline, cleared on read |
| `$4211.7` | TIMEUP | `_irqFlag` | set at IRQ match, cleared on read |
| `$4212.7` | HVBJOY | scanline ≥ NMI scanline | live |
| `$4212.6` | HVBJOY | hclock outside `[4, 1096]` | live HBlank |
| `$4212.0` | HVBJOY | `_autoReadActive` | auto-joypad in progress |

---

## Appendix: complete code-quote map

For every claim above, the cited file:line gives the source. Quoting policy:
all code snippets are verbatim from the Mesen2 master branch as fetched into
`/tmp/mesen2/` — line numbers will drift slightly with future commits but
the semantic anchors (function names, register addresses, enum members)
are stable.
