# luna PPU/DMA/OAM — prioritized correctness gap list

Cross-referenced against `/tmp/ppu_compositor_reference.md`. Each gap cites both luna's current state (`luna:file:line`) and the references where the correct behavior lives (`ares:file:line`, `mesen2:file:line`).

Priority key: **P0** = directly causes a user-visible bug we've reproduced; **P1** = causes a class of visible bugs; **P2** = silent corruption / timing-sensitive; **P3** = nice-to-have feature parity.

---

## P0 — Confirmed root causes of the SMW Yoshi's House bugs

### G1. Force-main-black polarity (CGWSEL bits 7:6) — INVERTED

**Symptom**: dialog interior renders pure black instead of translucent blue.

**luna** (`luna-ppu/src/renderer.rs:610-620`):
```rust
let force_black = match (ppu.cgwsel >> 6) & 0x03 {
    0 => false,
    1 => in_math_window,    // ✗
    2 => !in_math_window,   // ✗
    _ => true,
};
```

**ares** (`window.cpp:36-38` + `dac.cpp:120-122`): `array[] = {true, value, !value, false}; output.above.colorEnable = array[aboveMask]`. So `aboveMask=1` → `above.colorEnable = value` → main visible INSIDE window → force-black OUTSIDE.

**Mesen2** (`SnesPpu.cpp:1307-1326`, `SnesPpuTypes.h:13-19`): enum `ColorWindowMode { Never=0, OutsideWindow=1, InsideWindow=2, Always=3 }` — value 1 ("OutsideWindow") forces black OUTSIDE the window.

**Both references agree**: luna's match arms 1 and 2 are swapped. Correct mapping:
```rust
let force_black = match (ppu.cgwsel >> 6) & 0x03 {
    0 => false,
    1 => !in_math_window,   // 1 = OutsideWindow → black outside
    2 => in_math_window,    // 2 = InsideWindow  → black inside
    _ => true,
};
```

Note: bits 5:4 (math-region, `renderer.rs:583-589`) appear correct as-written. Verify by direct cross-check against the same source lines.

**Impact**: SMW's dialog box is the canonical "main visible inside, math against sub" recipe — this is exactly the case where the polarity error makes the symptom appear. Any game that uses force-main-black-outside-window will collapse to black with the bug.

---

### G2. Sub-screen is not a real compositor

**Symptom**: dialog interior + every other translucent overlay in every SNES game.

**luna** (`luna-ppu/src/renderer.rs:476-480`):
```rust
// Sub-screen colour: in this phase we don't render an actual
// sub-screen, just the fixed COLDATA backdrop. CGWSEL bit 1
// would enable BG/OBJ on the sub-screen; that's a stretch goal.
let sub_bgr5 = (ppu.coldata_r, ppu.coldata_g, ppu.coldata_b);
```
`ppu.ts` and `ppu.tsw` are stored but unread by the renderer (grep-verified in inventory). `cgwsel` bit 1 is unread.

**ares** (`dac.cpp:43-80`): `DAC::below()` runs the same full priority walk as `above()` against `output.below` Pixel records populated by `Background::run` / `Object::run` per the TS bits.

**Mesen2** (`SnesPpu.cpp:1340-1376`): two parallel framebuffers `_mainScreenBuffer` / `_subScreenBuffer` with parallel priorities; `ColorMathAddSubscreen` selects which goes into the math operand.

**Required work**:
1. Render BG/OBJ to a parallel sub-screen buffer, gated by TS/TSW (mirroring the existing TM/TMW path).
2. Honor `cgwsel & 0x02` (bit 1): when set, math operand is the sub-screen winner; when clear, math operand is COLDATA.
3. Implement the empty-sub fallback (G3 below — couples to this).

**Impact**: gigantic. Every game with color math (Zelda 3, Yoshi's Island, FF6, SMRPG, every Mode-7 menu, transparent menus...) is affected.

---

### G3. OAM auto-address-reset at VBlank missing

**Symptom**: SMW shadow OAM is empty during Yoshi's House cutscene → no Mario.

**luna** (`luna-ppu/src/memory.rs:376-392` + inventory §5.2): `$2102/$2103` write handlers update `word_address` immediately, but there is **no scheduler hook** that re-applies the latched word_address at start-of-VBlank.

**ares** (`object.cpp:31-32`):
```
if(t.y == self.vdisp() && !self.io.displayDisable) addressReset();
```
where `addressReset()` (`object.cpp:1-4`) does `oamAddress = oamBaseAddress; setFirstSprite();`.

**Mesen2** (`SnesPpu.cpp:464-472`):
```
if(_scanline == _nmiScanline) {
    if(!_state.ForcedBlank) {
        _state.InternalOamAddress = (_state.OamRamAddress << 1);
    }
}
```

**Plus**: write to $2100 that exits force-blank at vcounter==vdisp triggers the same reset (ares `ppu_io.cpp:194`, Mesen2 `SnesPpu.cpp:1889-1896`).

**Required work**:
1. Add a `latched_oamaddr: u16` field to `Oam` that's only updated by direct `$2102/$2103` writes (not by OAM streaming).
2. In the scheduler at start-of-VBlank entry, when force-blank is off, set `Oam.address = latched_oamaddr << 1`.
3. In the $2100 write handler, when the current write would exit force-blank AND scanline == vdisp, do the same reset.

**Impact**: confirmed root cause hypothesis for missing Mario sprite. Likely also affects other SMW scenes, every game that relies on the per-frame auto-reset (which is "the standard idiom" per Mesen2 comment).

---

## P1 — Real bugs, will bite some games

### G4. `math.transparent` empty-sub fallback missing

When `cgwsel & 0x02` is set (sub as math operand) AND the sub-screen has no real winner at that x, both refs SUBSTITUTE the fixed color AND disable halve for that dot.

**ares** (`dac.cpp:124-130`):
```
if(io.blendMode && math.transparent) {
  math.blendMode  = false;
  math.colorHalve = false;
}
```

**Mesen2** (`SnesPpu.cpp:1354-1364`):
```
if(_subScreenPriority[x] > 0) {
    otherPixel = pixelB;
} else {
    otherPixel = _state.FixedColor;
    halfShift = 0;
}
```

**luna**: N/A (no sub-screen exists yet — but when G2 lands, this MUST come with it).

**Impact**: dialog/menu overlays that animate the sub-screen (sub mostly empty for parts of the frame) will halve a non-zero operand against zero, darkening incorrectly.

---

### G5. `$2104` does not call `setFirstSprite()`

**luna** (`luna-ppu/src/memory.rs:396-416` `Oam::write`): no equivalent.

**ares** (`ppu_io.cpp:236`): every $2104 write calls `obj.setFirstSprite()` (`object.cpp:6-9`), which refreshes the OAM-priority rotation starting index when the priority-rotation flag is set.

**Mesen2** (`SnesPpu.cpp:1672-1675`): handled inside `UpdateOamAddress` which is called from both $2102/$2103 AND from the per-line auto-reset.

**Impact**: games that use OAM priority rotation (very few, but Star Ocean does) will get wrong sprite priority. Lower priority than G1-G3 because rare.

---

### G6. Mid-scanline register write latching missing

**Symptom**: level-load artifacts. Games change BG tilemap/char base, TM/TS, scroll, or palette mid-frame; luna applies them only at frame boundaries.

**luna** (`luna-ppu/src/renderer.rs:455-470` etc.): `render_frame_with` is called once per frame and uses the PPU state as-of the end of the frame for every scanline.

**ares**: per-cycle dispatch — writes naturally interleave.

**Mesen2** (`SnesPpu.cpp:1712-1714, 1884-1886`): calls `RenderScanline()` before applying a register write that affects rendering, so the in-progress line is split at the write x position.

**Required work**: significant. Two reasonable approaches:
- (a) Render scanline-by-scanline, called from the scheduler each line end; PPU writes during that line trigger a "render up to x" partial flush.
- (b) Cycle-accurate per-dot rendering (ares-style). Bigger lift, more correct.

**Impact**: HUD/level-separator effects (status bar HDMA, Mode-7 horizon HDMA, mid-frame palette swaps for sky gradients, etc.). The "artifacts during level load" symptom likely overlaps with this AND with G7 below.

---

### G7. VRAM/CGRAM/OAM access gating during active display

Both refs (ares `ppu_io.cpp:19-61`, Mesen2 equivalent):
- VRAM read/write during active display → returns 0 / discards.
- CGRAM write during active display → routed through `latch.cgramAddress` (mux updated per dot by `DAC::paletteColor()`).
- OAM read/write during active display → routed through `latch.oamAddress`.

**luna**: none of these gates exist.

**Required work**: track current scanline and forced-blank flag in the bus access path; reject/redirect writes per region.

**Impact**: games with slightly-misbehaved DMA timing (writes that land 1-2 cycles into active display) will silently corrupt VRAM/CGRAM on luna where real hardware would silently reject. Hard to attribute to specific symptoms without case-by-case testing.

---

## P2 — Correctness gaps unlikely to cause this bug but worth fixing

### G8. Sprite double-buffering

ares' `t.tile[!t.active]` consumption pattern (`object.cpp:16-33, 61, 93`) means a scanline renders tiles fetched on the PREVIOUS scanline. luna evaluates sprites synchronously per scanline. Most games don't depend on this timing.

### G9. Direct-color palette-group bits

`renderer.rs:853-858` only uses the 8-bit palette index. ares/Mesen2 also fold in 3 bits from the tilemap's palette-group field. luna's `direct_color_to_bgr5` is missing the palette-group contribution. Affects 8bpp modes (3/4/7) with direct color — rare in commercial games.

### G10. PPU register read open-bus

`$2134..$213F` reads should return open-bus for unimplemented register positions, and the read-mux for $2138 (OAMDATAREAD) / $2139-$213A (VMDATALREAD/HREAD) / $213B (CGDATAREAD) needs the same address-latch quirks as the writes.

---

## P3 — Feature parity, low priority for current bug

### G11. Mode 7 EXTBG (SETINI bit 6)
F-Zero, Pilotwings — when EXTBG is set, BG2 reuses the Mode-7 framebuffer with priority bits from the tilemap high byte. luna ignores SETINI.

### G12. Hi-res / pseudo-hi-res (Mode 5/6, SETINI bit 3)
Frame buffer is hard-coded 256 wide in `renderer.rs:19-22`. Modes 5/6 currently fall through to Mode-1 priority table at `renderer.rs:1009-1011`. Affects: Kirby Super Star menus, some FX-chip carts, RPG menu high-res text.

### G13. Mode 7 matrix sign-extension edge cases
Existing Mode-7 path may not handle all M7X/M7Y center sign extension cases per ares' `mode7.cpp` — verify if Mode-7 games look right currently.

---

## Recommended landing order

The SMW Yoshi's House bug needs **G1 + G2 + G3 + G4 together**. Doing G1 alone fixes nothing visible (cgwsel bits 7:6 are 00 for this scene). Doing G2 alone gives the wrong color (sub is rendered but math-region polarity still wrong elsewhere). Doing G3 alone fixes the missing Mario but the dialog box is still wrong.

Suggested sequence:

1. **G1** first (1-line swap, low risk, validated by both refs). Land standalone, add a regression test.
2. **G3** next (medium lift; needs scheduler hook + state on `Oam`; low risk of breaking other tests). Validate by checking Mario reappears in the cutscene.
3. **G2 + G4 together** (large lift; refactors the renderer significantly). Add tests with deliberate sub-screen color math scenarios.
4. **G5, G6, G7** as separate follow-up commits once the cutscene renders correctly, motivated by other failing test ROMs.
5. **G8-G13** prioritized by which test ROMs they unlock.

Estimated effort:
- G1: 15 min including test.
- G3: 1-2 hours including test.
- G2 + G4: half-day to a day. Significant rewrite of `renderer.rs` mixer.
- G6 (mid-scanline latching): 1-3 days depending on chosen approach.
- Others: small to medium each.
