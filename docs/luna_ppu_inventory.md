# luna inventory — PPU compositor / DMA / OAM auto-write

This is a read-only inventory of luna's current implementation, structured to be diffable against the ares + Mesen2 reference reports.

Scope: `luna-ppu`, `luna-dma`, `luna-core` (`snes.rs` + `cpu_regs.rs`).
Out of scope: SA-1 / coprocs / APU / mappers (unless they materially touch PPU MMIO).

The production renderer entry point is `luna_ppu::render_frame_with` (`crates/luna-ppu/src/renderer.rs:459`). Both the GUI (`crates/luna-gui/src/app.rs:282`) and the CLI screenshot path (`crates/luna-cli/src/main.rs:769`) call it. Everything below is sourced from that path.

---

## 1. Compositor pipeline (`renderer.rs`)

### 1.1 Top-level walk through `render_frame_with`

`renderer.rs:459-631`. Pseudocode order of operations per pixel:

1. **Forced blank** check: if `INIDISP.7` set, return an all-zero frame (`renderer.rs:463-465`).
2. **Brightness** taken from `INIDISP & 0x0F` (`renderer.rs:466-470`).
3. **Priority table** chosen by `priority_table(bgmode)` (`renderer.rs:474`).
4. **Backdrop colour** = `cgram_to_bgr5(ppu, 0)` (`renderer.rs:475`).
5. **Sub-screen** colour = `(coldata_r, coldata_g, coldata_b)` only (no BG/OBJ on sub-screen) — `renderer.rs:480`.
6. **Window masks** precomputed once per frame via `compute_window_masks(ppu)` (`renderer.rs:485`).
7. **Mode 7 branch**: if `bgmode & 7 == 7`, BG1 routed through `render_mode7_scanline_indexed`, BG2/3/4 are forced empty (`renderer.rs:489-508`).
8. **Per-line loop** (`renderer.rs:491-629`):
   - Compute the four BG indexed scanlines + the sprite indexed scanline (`renderer.rs:494-509`).
   - Per pixel, build closure `main_layer_enabled(layer_idx)` (`renderer.rs:521-530`) — TM + TMW + per-layer combined mask.
   - Walk `priority_table` slots top-to-bottom; first opaque layer pixel matching its slot-priority wins (`renderer.rs:541-573`).
   - Track winner_layer / winner_cgram for color-math gating (`renderer.rs:539-540`).
   - Apply direct-color OR cgram lookup → `main_bgr5` (`renderer.rs:561-568`).
   - Compute `in_math_window`, `math_globally_enabled` from `CGWSEL[5:4]` (`renderer.rs:583-589`).
   - Gate by `cgadsub` layer-enable bits (`renderer.rs:590-604`).
   - Apply `color_math(main, sub, cgadsub)` if enabled (`renderer.rs:605-609`).
   - Apply force-main-black from `CGWSEL[7:6]` (`renderer.rs:614-620`).
   - Final brightness scale (`renderer.rs:622-627`).

### 1.2 `cgadsub` layer-enable bits (`renderer.rs:574-604`)

Quote (`renderer.rs:590-604`):

```rust
let layer_enabled_for_math = if math_globally_enabled {
    match winner_layer {
        0..=3 => ppu.cgadsub & (1 << winner_layer) != 0,
        4 => {
            // Only OBJ palettes 4-7 participate. Sprite
            // CGRAM indices start at 128; palette `p`
            // begins at 128 + p*16. So palette ≥ 4 ⇒
            // CGRAM index ≥ 192.
            (ppu.cgadsub & 0x10) != 0 && winner_cgram >= 192
        }
        _ => ppu.cgadsub & 0x20 != 0, // backdrop
    }
} else {
    false
};
```

The comment header (`renderer.rs:574-582`) documents:

```
//   bit 0..=3 = BG1..BG4
//   bit 4 = OBJ (palettes 4-7 only)
//   bit 5 = backdrop
```

The OBJ ≥ 192 gate is implemented exactly as commented: `winner_cgram >= 192`.

### 1.3 `cgwsel` bits 5:4 — math-enable region (`renderer.rs:583-589`)

Quote:

```rust
let in_math_window = masks.combined[5][x];
let math_globally_enabled = match (ppu.cgwsel >> 4) & 0x03 {
    0 => true,
    1 => in_math_window,
    2 => !in_math_window,
    _ => false,
};
```

Comment (`renderer.rs:580-582`):

```
//   00 = always; 01 = inside math window;
//   10 = outside math window; 11 = never.
```

`masks.combined[5]` is the dedicated **colour-math window** (layer index 5 in the masks array — see §3).

### 1.4 `cgwsel` bits 7:6 — force-main-black region (`renderer.rs:610-620`)

Quote:

```rust
// CGWSEL bits 7:6 — "force main screen black" region.
// Same 4-value semantics as the math-enable region,
// referencing the same math-window mask:
//   00 = never; 01 = inside; 10 = outside; 11 = always.
let force_black = match (ppu.cgwsel >> 6) & 0x03 {
    0 => false,
    1 => in_math_window,
    2 => !in_math_window,
    _ => true,
};
let final_bgr5 = if force_black { (0, 0, 0) } else { rgb5 };
```

Polarity here is documented as: `00 = never, 01 = inside, 10 = outside, 11 = always`. **The math-enable region (`>>4`) and the force-black region (`>>6`) both reference `in_math_window` (i.e. the layer-5 math-window mask).** This is luna's design choice.

The corresponding `PPU` struct comment (`ppu.rs:194-196`):

```
/// `$2130` CGWSEL — bit 7:6 force-main-black region, bit 5:4
/// math-enable region (both reference the colour-math window),
/// bit 1 sub-BG/OBJ enable, bit 0 direct-colour mode for 8bpp.
```

### 1.5 `cgwsel` bit 1 — sub-BG/OBJ enable

Not referenced anywhere in the renderer. Search confirms:

```
$ grep -n 'cgwsel.*0x02\|cgwsel & 2\|cgwsel >> 1' renderer.rs
(no matches)
```

The PPU comment at `ppu.rs:196` acknowledges the bit exists but the renderer ignores it. Sub-screen is unconditionally the COLDATA fixed colour (§2).

### 1.6 `cgwsel` bit 0 — direct colour (`renderer.rs:561-568`)

Quote:

```rust
let direct = ppu.cgwsel & 0x01 != 0
    && matches!(slot.kind, LayerKind::Bg)
    && bg_bpp(ppu.bgmode, slot.idx as usize) == 8;
main_bgr5 = if direct {
    direct_color_to_bgr5(cgram_idx)
} else {
    cgram_to_bgr5(ppu, cgram_idx)
};
```

`direct_color_to_bgr5` at `renderer.rs:853-858`:

```rust
fn direct_color_to_bgr5(palette_index: u8) -> (u8, u8, u8) {
    let r3 = palette_index & 0x07;
    let g3 = (palette_index >> 3) & 0x07;
    let b2 = (palette_index >> 6) & 0x03;
    (r3 << 2, g3 << 2, b2 << 3)
}
```

This is the 3-3-2 bit decomposition.  ares/Mesen2 also fold in palette-offset bits (the tile's palette bits add to the 5-bit channels) — luna's `direct_color_to_bgr5` does **not** add the palette-offset contribution. Effect on SMW Yoshi's House cutscene is probably nil (cutscene isn't 8bpp direct-colour), but flagged for the diff.

### 1.7 OBJ palette ≥ 192 gate

Implemented at `renderer.rs:598`: `(ppu.cgadsub & 0x10) != 0 && winner_cgram >= 192`. The cgram-index threshold is correct (sprite palettes 4..7 start at 128 + 4*16 = 192).

### 1.8 Mode 7 EXTBG handling

**Absent.** `grep extbg` in renderer.rs returns nothing. `setini` is stored on the PPU struct (`ppu.rs:271`) but the renderer never inspects it. In Mode 7 the renderer renders BG1 only (`renderer.rs:489-508`):

```rust
let is_mode7 = ppu.bgmode & 0x07 == 0x07;
// ...
let bgs: [IndexedScanline; 4] = if is_mode7 {
    [
        render_mode7_scanline_indexed(ppu, y, opts),
        [None; 256],
        [None; 256],
        [None; 256],
    ]
} else {
    ...
};
```

So Mode 7 EXTBG (where BG2 reuses the Mode-7 framebuffer with priority bits in the high tile-byte) is not modelled.

### 1.9 Hi-res / pseudo-hires handling

**Absent.** No code path examines `setini` bits 3 (pseudo-512) or 5 (hi-res). Modes 5/6 (which select 512px hires natively) fall through to the Mode-1 priority table per `renderer.rs:1009-1011`:

```rust
// Modes 5/6 not yet wired into the priority engine; fall
// back to the Mode-1 layout.
_ => MODE1_BG3LO_TABLE,
```

The frame buffer is hard-coded to 256 px (`renderer.rs:19-22`):

```rust
pub const FRAME_W: usize = 256;
pub const FRAME_H: usize = 224;
```

---

## 2. Sub-screen rendering

Quote `renderer.rs:476-480`:

```rust
// Sub-screen colour: in this phase we don't render an actual
// sub-screen, just the fixed COLDATA backdrop. CGWSEL bit 1
// would enable BG/OBJ on the sub-screen; that's a stretch goal.
let sub_bgr5 = (ppu.coldata_r, ppu.coldata_g, ppu.coldata_b);
```

**There is no sub-screen compositor.** The "sub" passed into `color_math` is always the fixed COLDATA triple. TS (`$212D`) and TSW (`$212F`) are stored on `Ppu` (`ppu.rs:241, 246`) but **the renderer never reads `ppu.ts` or `ppu.tsw`**. `grep 'ppu.ts\|ppu.tsw'` in renderer.rs returns nothing.

Consequence for the SMW Yoshi's House dialog box: a translucent dialog needs main = box fill, sub = BG behind it, and the math op blends them. luna gives main = box fill, sub = COLDATA. If COLDATA = (0, 0, 0) (the post-reset default — `ppu.rs:341-343` initialises `coldata_*` to 0), then "half-add" or "add" of a black sub-screen against an opaque box produces the box colour at half intensity → looks **black**, exactly matching the reported symptom.

---

## 3. Window mask computation

`compute_window_masks` at `renderer.rs:654-704`. Layout (`renderer.rs:649-652`):

```
Layers are indexed `0..=3` for BG1..BG4, `4` for OBJ, `5` for
the dedicated colour-math window.
```

Quote of the per-layer cfg table (`renderer.rs:663-670`):

```rust
let layer_cfg = [
    (ppu.w12sel & 0x0F, ppu.wbglog & 0x03),
    (ppu.w12sel >> 4, (ppu.wbglog >> 2) & 0x03),
    (ppu.w34sel & 0x0F, (ppu.wbglog >> 4) & 0x03),
    (ppu.w34sel >> 4, (ppu.wbglog >> 6) & 0x03),
    (ppu.wobjsel & 0x0F, ppu.wobjlog & 0x03),
    (ppu.wobjsel >> 4, (ppu.wobjlog >> 2) & 0x03),
];
```

Per-window resolution (`renderer.rs:671-703`):

```rust
for (layer, &(sel, logic_bits)) in layer_cfg.iter().enumerate() {
    let w1_invert = sel & 0x01 != 0;
    let w1_enable = sel & 0x02 != 0;
    let w2_invert = sel & 0x04 != 0;
    let w2_enable = sel & 0x08 != 0;
    if !w1_enable && !w2_enable {
        continue; // no window for this layer → mask stays all-false
    }
    for x in 0..FRAME_W {
        let r1 = if w1_enable { in_w1[x] ^ w1_invert } else { false };
        let r2 = if w2_enable { in_w2[x] ^ w2_invert } else { false };
        out.combined[layer][x] = match (w1_enable, w2_enable) {
            (true, false) => r1,
            (false, true) => r2,
            (true, true) => match logic_bits {
                0 => r1 || r2,  // OR
                1 => r1 && r2,  // AND
                2 => r1 ^ r2,   // XOR
                _ => !(r1 ^ r2),// XNOR
            },
            _ => unreachable!(),
        };
    }
}
```

Notable: when **neither** window is enabled for a layer the mask is left all-false (`continue` at `renderer.rs:676-678`). Combined with the `main_layer_enabled` closure at `renderer.rs:521-530`:

```rust
let main_layer_enabled = |layer_idx: usize| -> bool {
    let layer_bit = 1u8 << layer_idx;
    if ppu.tm & layer_bit == 0 {
        return false;
    }
    if ppu.tmw & layer_bit != 0 && masks.combined[layer_idx][x] {
        return false;
    }
    true
};
```

→ `TMW` only blanks a layer where the combined mask is true. A layer with neither window enabled has combined-mask = all-false → `TMW` does nothing → layer always shows. (Standard SNES.)

`make_window` at `renderer.rs:823-831` returns true when `left <= right` for the inclusive range; if `left > right` the window is empty. This matches the documented hardware semantic.

---

## 4. OBJ / sprite path

### 4.1 Decode (`renderer.rs:299-353`)

`decode_all_sprites` walks 128 OAM slots. For each slot:
- `x_low` = `oam.peek(idx*4 + 0)`
- `y_live` = `oam.peek(idx*4 + 1)`
- **OAM shadow Y fallback** (`renderer.rs:316-325`):

```rust
// When the live Y is the off-screen hide signal $F0, fall
// back to the shadow — the last visible Y the game ever
// wrote into this slot.
let y_pos = if y_live == 0xF0 {
    ppu.oam.shadow_y[idx]
} else {
    y_live
};
```

- tile = (tile_low) | ((attrs & 1) << 8) → 9-bit
- palette = (attrs >> 1) & 0x07
- priority = (attrs >> 4) & 0x03
- h_flip = attrs & 0x40, v_flip = attrs & 0x80
- high table bits: high_byte_idx = 0x200 + idx/4, bit_off = (idx % 4) * 2 → 2 bits: x.high + size flag

X sign extension (`renderer.rs:334-338`):

```rust
let mut x = (x_high_bit << 8) | (x_low as i16);
if x >= 256 {
    x -= 512;
}
```

### 4.2 Scanline render (`render_sprites_scanline_indexed_with`, `renderer.rs:1140-1186`)

Quote relevant block (`renderer.rs:1150-1184`):

```rust
let sprites = decode_all_sprites(ppu);
// Iterate highest-OAM-index first so lower indices visually win
// (sprite 0 = front-most for the same screen pixel).
for sp in sprites.iter().rev() {
    let row_in_sprite = y.wrapping_sub(sp.y as u16);
    if usize::from(row_in_sprite) >= sp.h as usize {
        continue;
    }
    for col in 0..sp.w {
        let screen_x = sp.x + col as i16;
        if !(0..256).contains(&screen_x) {
            continue;
        }
        let mut sc = col as usize;
        let mut sr = row_in_sprite as usize;
        if sp.h_flip {
            sc = (sp.w - 1) as usize - sc;
        }
        if sp.v_flip {
            sr = (sp.h - 1) as usize - sr;
        }
        let tile_x = sc / 8;
        let tile_y = sr / 8;
        let pix_x = sc % 8;
        let pix_y = sr % 8;
        let tile_id = (sp.tile.wrapping_add(((tile_y * 16) + tile_x) as u16)) & 0x01FF;
        let tile_off = sprite_tile_byte_offset(ppu.obsel, tile_id);
        let idx = decode_tile_pixel(ppu, tile_off, pix_y, pix_x, 4);
        if idx == 0 {
            continue;
        }
        let cgram_idx = 128u16 + (sp.palette as u16) * 16 + (idx as u16);
        out[screen_x as usize] = Some((cgram_idx as u8, sp.priority));
    }
}
```

Priority is the 2-bit (0..=3) value, passed through to the priority engine. The priority tables (`MODE0_TABLE` … `MODE7_TABLE`, `renderer.rs:931-992`) interleave `obj(0..=3)` slots between BG slots. All four priorities (0, 1, 2, 3) are emitted.

### 4.3 Per-line cap

**Absent.** Real PPU caps at 32 sprites / 34 tiles per line and sets STAT77 bits 6/7. luna iterates all 128 sprites unconditionally per scanline (`renderer.rs:1153`). `STAT77` always reads back as `0x01` (`ppu.rs:374`), the chip-ID value.

### 4.4 Sprite-zero handling

Not modelled. No code reference to "sprite zero", time-over, or range-over. The renderer treats all OAM slots uniformly.

### 4.5 Big-sprite tile addressing (`renderer.rs:1171-1176`)

Sprite tiles for big sprites use the 16-tile-wide "sprite plane" — `tile_id = base.wrapping_add(row*16 + col)`. Documented as "SNES sprites use a 16-tile-wide sprite plane" in the non-indexed twin (`renderer.rs:401-403`).

`sprite_tile_byte_offset` (`renderer.rs:262-273`) handles the two-page OBSEL nameselect quirk:

```rust
let page0 = (usize::from(obsel) & 0x07) << 13;
let nameselect = usize::from(obsel >> 3) & 0x03;
let page1 = page0.wrapping_add((nameselect + 1) << 12);
let tile_in_page = usize::from(tile & 0xFF) * 32;
if (tile & 0x100) == 0 {
    page0 + tile_in_page
} else {
    (page1 + tile_in_page) & 0xFFFF
}
```

### 4.6 OAM shadow Y

`Oam.shadow_y: [u8; 128]` (`memory.rs:322`) — per-slot fallback for the case where the live Y is `0xF0`. Updated on `poke` (`memory.rs:370-374`) and on `write` (`memory.rs:412-413`). Initial value is `0xF0` for all slots (`memory.rs:342`), so a fresh boot with empty OAM still produces no sprites until the game writes a non-`$F0` Y somewhere.

**Relevant to bug (a):** if the cutscene phase NEVER writes a non-`0xF0` Y for the Mario sprite slot, the shadow stays `0xF0` and the renderer still draws nothing. This makes the shadow useless when the SMW shadow OAM at `$7E:0200` is also empty (as reported).

---

## 5. OAM auto-write / OAMADDR reload at VBlank

### 5.1 `$2102` / `$2103` write handlers (`memory.rs:376-392`)

```rust
fn reset_byte_address(&mut self) {
    self.address = (self.word_address & 0x01FF) << 1;
}

/// `$2102` write — low 8 bits of the word address.
pub fn set_address_low(&mut self, value: u8) {
    self.word_address = (self.word_address & 0xFF00) | u16::from(value);
    self.reset_byte_address();
}

/// `$2103` write — bit 8 of the word address (lives in bit 0 of the
/// written byte). Bit 7 of the value is the priority-rotation flag
/// (not yet modeled).
pub fn set_address_high(&mut self, value: u8) {
    self.word_address = (self.word_address & 0x00FF) | (u16::from(value & 0x01) << 8);
    self.reset_byte_address();
}
```

Bit 7 of `$2103` (priority rotation flag) is acknowledged but `not yet modeled`. The "latched" `oamaddr` (set by the most recent `$2102/$2103` write, the field ares calls `latch.oamAddress`) is not stored as a separate field — `word_address` is overwritten on every write.

### 5.2 OAMADDR reload at VBlank end / forced-blank end

**Absent.** Searching `grep -rn 'oamaddr\|oam_addr' luna-ppu luna-core` produces only the `$2102/$2103` write paths above. There is no scheduler hook at VBlank entry that reloads `Oam.address` from a latched value.

In ares this is `PPU::OAM::write/main()` doing `address = (latch.oamAddress << 1)` at the start of every visible scanline when `INIDISP.7` is clear. In luna this never happens.

**Relevant to bug (a):** real games typically:
1. Update shadow OAM at `$7E:0200..` during gameplay.
2. At NMI, DMA shadow → OAM with `OAMADDR=0` set just before the DMA.

If `$2103` got bit-7 priority rotation set (or some sequence left `OAMADDR` non-zero), the next OAM DMA writes start mid-OAM, scrambling sprites. luna can't reproduce this exact failure mode but also can't reproduce the **correct** behaviour (`OAMADDR` reload to the latched value when forced blank exits) — meaning if a game sets `OAMADDR=0` via `$2102/$2103` BEFORE the DMA but during forced blank, luna's `Oam.address` is already 0, so the DMA does land in the right place.

The bug-reported symptom is "OAM empty" — which suggests not a misaligned write but a missing write. See §6 / §7.

### 5.3 OAM `$2104` write (`memory.rs:396-416`)

```rust
pub fn write(&mut self, value: u8) {
    let addr = self.address;
    if addr & 0x200 != 0 {
        // High table: direct byte write at the wrapped offset.
        let off = usize::from(addr & 0x21F);
        self.data[off] = value;
    } else if addr & 1 == 0 {
        // Low table even byte — latch until the odd-byte commit.
        self.latch = value;
    } else {
        // Low table odd byte — commit the latched pair atomically.
        let even_off = usize::from(addr.wrapping_sub(1) & 0x1FF);
        self.data[even_off] = self.latch;
        self.data[even_off + 1] = value;
        self.update_shadow(even_off + 1, value);
    }
    self.advance();
}
```

The even/odd "atomic pair" write to the low table is correct. The high table uses a direct byte write per access. `advance()` wraps at `0x220`.

### 5.4 OAM read (`memory.rs:420-424`)

```rust
pub fn read(&mut self) -> u8 {
    let value = self.data[usize::from(self.address) & 0x21F];
    self.advance();
    value
}
```

No internal latching on reads (matches hardware).

---

## 6. DMA / HDMA controller

### 6.1 General DMA trigger — `$420B` (`snes.rs:889-913`)

```rust
if Self::is_mdmaen(addr) {
    let mut view = DmaBusView {
        wram: self.wram,
        mapper: self.mapper,
        ppu: self.ppu,
    };
    let bytes = self.dma.run_mdma(&mut view, value);
    // DMA stalls the CPU during the transfer. Cost model
    // (per fullsnes §"SNES DMA Timing"):
    //   * 8 mclk one-shot overhead at the start of the burst
    //   * 8 mclk per channel that runs (already implicit in
    //     the per-byte cost since we lump the bus overhead
    //     into each byte)
    //   * 8 mclk per byte transferred
    // We charge `8 + 8 × bytes` — close enough for game
    // compatibility without modelling the per-channel
    // header explicitly.
    self.io_cycle(u64::from(8 + bytes.saturating_mul(8)));
    return;
}
```

`Dma::run_mdma` (`controller.rs:61-70`) loops channels 0..7 in ascending order and fires each whose bit is set in `mask`. Each channel's `run` (`channel.rs:266-304`) transfers `das` bytes (or 64 KB if `das == 0`), using the channel's `mode.pattern()` to choose B-bus offsets. The channel zeros `das` on completion (hardware behaviour).

### 6.2 HDMA enable — `$420C` (`snes.rs:914-917`)

```rust
if Self::is_hdmaen(addr) {
    self.dma.hdmaen = value;
    return;
}
```

Note: the write is **immediate**. There is no "edge-of-VBlank" gating — a write to `$420C` mid-frame takes effect on the next per-scanline tick. ares latches `$420C` at H = 6 of the next scanline; luna applies it at the next scanline boundary.

### 6.3 HDMA scanline service (`snes.rs:432-456`)

The scheduler calls `hdma_run_line()` after every scanline tick where `ppu_line < vblank_start`:

```rust
if self.ppu_line < vblank_start {
    self.hdma_run_line();
}
```

`hdma_run_line` (`snes.rs:449-456`) constructs a `DmaBusView` and dispatches to `Dma::hdma_run_line` (`controller.rs:95-103`):

```rust
pub fn hdma_run_line<B: DmaBus>(&mut self, bus: &mut B) -> u32 {
    let mut total = 0u32;
    for ch in 0..8 {
        if self.hdmaen & (1 << ch) != 0 {
            total += self.channels[ch].hdma_step_line(bus);
        }
    }
    total
}
```

The per-channel step (`channel.rs:348-400`) handles:
- one mode "unit" (1/2/4 bytes) when `hdma_do_transfer` is set
- decrement of `ntlr & 0x7F`
- preservation of `ntlr & 0x80` (repeat) bit
- termination on a `0` header byte after exhaustion
- indirect mode re-fetch of the data pointer on each entry transition

`hdma_init_frame` (`snes.rs:440-447`) is called on the frame wrap (`snes.rs:425-427`):

```rust
if self.ppu_line >= scanlines {
    // Frame wrap: back to line 0, clear VBlank bit, and re-
    // initialise HDMA tables for the new frame.
    self.ppu_line = 0;
    self.cpu_regs.hvbjoy &= !0x80;
    self.frame_count = self.frame_count.saturating_add(1);
    self.hdma_init_frame();
}
```

Note: HDMA init is called on the **frame wrap** (line 261 → 0), not on the more canonical "line 0 of new frame" point. ares does it at H=6 of line 0. In luna terms these are nearly identical, but a transition stuck right at the line boundary can subtly miss.

### 6.4 Mode 0..7 byte patterns (`channel.rs:62-90`)

```rust
pub fn pattern(self) -> &'static [u8] {
    match self {
        TransferMode::OneByteOneReg => &[0],
        TransferMode::TwoBytesTwoRegs => &[0, 1],
        TransferMode::TwoBytesOneReg | TransferMode::TwoBytesOneRegAlt => &[0, 0],
        TransferMode::FourBytesTwoPairs | TransferMode::FourBytesTwoPairsAlt => &[0, 0, 1, 1],
        TransferMode::FourBytesFourRegs => &[0, 1, 2, 3],
        TransferMode::FourBytesTwoRegsAlt => &[0, 1, 0, 1],
    }
}
```

All 8 mode values are decoded; modes 6/7 are aliases of 2/3 (matches fullsnes).

### 6.5 Per-channel registers (`channel.rs:163-198`)

`DmaChannel` exposes all $43xN regs (params, BBAD, A1Tx, A1B, DASx, DASB, A2Ax, NTLR, unused). Reads/writes at `channel.rs:222-257` are byte-granular and round-trip cleanly.

### 6.6 DMA-during-DMA / HDMA-during-DMA timing

Not modelled. luna's `run_mdma` runs to completion in a single bus tick; the only cost is the lump-sum `io_cycle(8 + 8 * bytes)` charge after the burst. Real hardware interleaves HDMA channels into MDMA's HBlank slots — luna does not.

---

## 7. NMI / VBlank / frame scheduling

### 7.1 Scheduler core (`snes.rs:345-435`)

`advance_scheduler` (`snes.rs:345-351`):

```rust
fn advance_scheduler(&mut self, mcycles: u32) {
    self.mcycles_in_line += mcycles;
    while self.mcycles_in_line >= MCYCLES_PER_SCANLINE {
        self.mcycles_in_line -= MCYCLES_PER_SCANLINE;
        self.advance_one_scanline();
    }
}
```

`MCYCLES_PER_SCANLINE = 1364` (`snes.rs:105`). Per-line constants:
- `NTSC_SCANLINES_PER_FRAME = 262`, `NTSC_VBLANK_START_LINE = 225` (`snes.rs:143-149`)
- `PAL_SCANLINES_PER_FRAME = 312`, `PAL_VBLANK_START_LINE = 240` (`snes.rs:145-153`)

`advance_one_scanline` (`snes.rs:355-435`) — the per-line event dispatch.

### 7.2 NMI / VBlank entry (`snes.rs:359-387`)

```rust
self.ppu_line += 1;
if self.ppu_line == vblank_start {
    // Entering VBlank. Latch the NMI flag visible at $4210
    // and set the VBlank bit of HVBJOY.
    self.cpu_regs.nmi_flag = true;
    self.cpu_regs.hvbjoy |= 0x80;
    if self.cpu_regs.nmitimen & 0x80 != 0 {
        self.cpu.trigger_nmi();
        self.nmis_serviced = self.nmis_serviced.saturating_add(1);
    }
    // Joypad auto-read: hardware copies the live pad state
    // into $4218-$421F at the start of every VBlank when
    // NMITIMEN.0 is set. Busy bit clears a few lines later.
    self.cpu_regs.latch_joypad_auto_read();
    if self.cpu_regs.nmitimen & 0x01 != 0 {
        self.joypad1_shift = self.cpu_regs.joypad1_latched;
        self.joypad2_shift = self.cpu_regs.joypad2_latched;
    }
} else if self.ppu_line == vblank_start + 3 {
    // ~3 scanlines after VBlank entry the auto-read sequence
    // is done. Drop HVBJOY.0 so polling games see ready.
    self.cpu_regs.clear_joypad_busy();
}
```

NMI fires on entry to line 225 (NTSC) / 240 (PAL).

**Crucially absent:** there is NO OAMADDR reload at VBlank entry, NO `Ppu` callback at VBlank-end, and NO sprite-overflow flag tracking. The PPU is just sampled by the renderer when the frontend asks for a frame.

### 7.3 Frame wrap (`snes.rs:420-427`)

```rust
if self.ppu_line >= scanlines {
    // Frame wrap: back to line 0, clear VBlank bit, and re-
    // initialise HDMA tables for the new frame.
    self.ppu_line = 0;
    self.cpu_regs.hvbjoy &= !0x80;
    self.frame_count = self.frame_count.saturating_add(1);
    self.hdma_init_frame();
}
```

VBlank bit cleared at frame wrap (line 0). HDMA reinitialised. No OAM reload, no PPU state callback.

### 7.4 HVBJOY (`$4212`) read path (`snes.rs:796-801`)

```rust
if reg_off == 0x4212 {
    let (h, _) = current_hv(*self.mclk_total, self.scanlines_per_frame);
    let in_hblank = h == 0 || h >= 274;
    let hblank_bit = if in_hblank { 0x40 } else { 0x00 };
    return (self.cpu_regs.hvbjoy & !0x40) | hblank_bit;
}
```

Comment (`snes.rs:784-795`):

```
// $4212 HVBJOY: bit 7 = vblank (latched in `cpu_regs.hvbjoy`),
// bit 6 = hblank (live H-counter), bit 0 = auto-read busy
// (latched in `cpu_regs.hvbjoy`).
//
// Per ares' `cpu/io.cpp`:
//   data.bit(6) = hcounter() <= 2 || hcounter() >= 1096;
// The `hcounter()` is in master cycles (0..1364); our
// `current_hv` returns H in *dots* (`mclk / 4`, 0..341),
// so the equivalent threshold is `h == 0 || h >= 274`.
```

This is the recent commit (af748c9 in CLAUDE.md history) implementing live HBlank. **Bit 6 is computed live from H, bits 7 and 0 come from the latched `cpu_regs.hvbjoy`.**

### 7.5 NMITIMEN (`$4200`) handling

`cpu_regs.rs:178-179`:

```rust
0x4200 => self.nmitimen = value,
```

Stored verbatim. Decoded in two places:
- `snes.rs:364` — bit 7 (NMI enable) gates whether VBlank entry triggers `cpu.trigger_nmi()`.
- `snes.rs:379` & `cpu_regs.rs:132` — bit 0 (auto-joypad enable) gates joypad latch.
- `snes.rs:408-415` — bits 5:4 select V/H IRQ source:

```rust
let irq_mode = (self.cpu_regs.nmitimen >> 4) & 0x03;
let fire_irq = match irq_mode {
    0b00 => false,
    0b01 => true,
    0b10 => self.ppu_line == self.cpu_regs.vtime,
    0b11 => self.ppu_line == self.cpu_regs.vtime && self.cpu_regs.htime == 0,
    _ => false,
};
```

Comment (`snes.rs:404-407`) acknowledges: "Most games use this with HTIME = 0 which lands on the canonical scanline-start raster point" — i.e. dot-accurate H-IRQ timing is not modelled.

### 7.6 RDNMI (`$4210`) read clears NMI flag (`cpu_regs.rs:87-92`)

```rust
0x4210 => {
    // RDNMI: bit 7 = nmi flag (cleared by this read).
    let v = if self.nmi_flag { 0x80 } else { 0x00 } | 0x02; // CPU rev 2
    self.nmi_flag = false;
    v
}
```

The NMI flag is cleared on read. Note: reading `$4210` does NOT deassert the NMI line on the CPU — that happens via the `nmi_pending` field on `Snes`, which is never cleared by the read of `$4210`. (Real hardware: reading `$4210` clears `nmi_flag` and also lowers the NMI line on the CPU until next VBlank.)

Checking `Snes::nmi_pending` — it's set via `cpu.trigger_nmi()` (which is a CPU-side latch) and never cleared by RDNMI in luna. The CPU consumes the NMI when it services it, so this is mostly OK; but the edge case where a game reads `$4210` to acknowledge the NMI mid-handler isn't perfectly modelled.

---

## 8. Joypad auto-read

### 8.1 Live state ingress (`cpu_regs.rs:168-174`)

```rust
pub fn set_joypad(&mut self, idx: usize, mask: u16) {
    match idx {
        0 => self.joypad1 = mask,
        1 => self.joypad2 = mask,
        _ => {}
    }
}
```

`Snes::set_joypad` (`snes.rs:464-466`) just forwards. The frontend (GUI / CLI / MCP) calls `Snes::set_joypad(0, mask)`. The bit layout is documented in CLAUDE.md and verified by `joypad_bit_layout_byss_udlr_axlr` (`cpu_regs.rs:438`).

### 8.2 VBlank latch (`cpu_regs.rs:131-138`)

```rust
pub fn latch_joypad_auto_read(&mut self) {
    if self.nmitimen & 0x01 == 0 {
        return;
    }
    self.joypad1_latched = Self::clean_dpad(self.joypad1);
    self.joypad2_latched = Self::clean_dpad(self.joypad2);
    self.hvbjoy |= 0x01;
}
```

Called from `snes.rs:378`. Sets HVBJOY bit 0 (busy) which is cleared 3 lines later (`snes.rs:383-387`).

### 8.3 D-pad lockout (`cpu_regs.rs:144-153`)

```rust
fn clean_dpad(mask: u16) -> u16 {
    let mut m = mask;
    if m & 0x0C00 == 0x0C00 {
        m &= !0x0C00; // up + down → drop both
    }
    if m & 0x0300 == 0x0300 {
        m &= !0x0300; // left + right → drop both
    }
    m
}
```

Bits 11/10 = Up/Down; bits 9/8 = Left/Right. Matches CLAUDE.md.

### 8.4 Manual-shift refresh on auto-read (`snes.rs:379-382`)

```rust
if self.cpu_regs.nmitimen & 0x01 != 0 {
    self.joypad1_shift = self.cpu_regs.joypad1_latched;
    self.joypad2_shift = self.cpu_regs.joypad2_latched;
}
```

Re-loads the manual-mode (`$4016/$4017`) shift registers from the just-latched value, per the comment in `snes.rs:372-377`:

```
// Per ares' `controllerPort.latch()` chained off the
// auto-poll counter rollover, the same auto-read pulse
// also re-arms the manual-mode shift register. Games
// that read $4016/$4017 right after the auto-read
// window expect the shift register to reflect the
// just-latched controller state.
```

### 8.5 `$4218..$421F` reads (`cpu_regs.rs:107-111`)

```rust
0x4218 => self.joypad1_latched as u8,
0x4219 => (self.joypad1_latched >> 8) as u8,
0x421A => self.joypad2_latched as u8,
0x421B => (self.joypad2_latched >> 8) as u8,
0x421C..=0x421F => 0x00,
```

`$421C..$421F` (joypads 3 & 4 via multitap) return 0.

### 8.6 `$4016` strobe (`snes.rs:861-883`)

```rust
if offset == 0x4016 {
    let next_strobe = (value & 0x01) != 0;
    if !*self.joypad_strobe && next_strobe {
        // Rising edge or held-high: reload.
        *self.joypad1_shift = self.cpu_regs.joypad1;
        *self.joypad2_shift = self.cpu_regs.joypad2;
    }
    *self.joypad_strobe = next_strobe;
    if next_strobe {
        // Keep the shift register sync'd with the live
        // state while strobe is held high.
        *self.joypad1_shift = self.cpu_regs.joypad1;
        *self.joypad2_shift = self.cpu_regs.joypad2;
    }
}
```

Manual-mode strobe / shift behaviour, MSB-first, returning 1 once 16 bits are exhausted (`snes.rs:767-774`).

---

## 9. Open issues / stubs / TODOs

### 9.1 In luna-ppu

- `renderer.rs:1009-1011` — "Modes 5/6 not yet wired into the priority engine; fall back to the Mode-1 layout."
- `renderer.rs:362-365` — *historical* "Per-sprite priority bits are decoded but not yet used to slot sprites between BG layers — that lands once the per-pixel priority engine is in place" comment, on the older `render_sprites_scanline` (non-indexed) function. The indexed twin DOES honour priorities, so this comment is stale wrt the indexed compositor but still describes the simpler `render_sprites_scanline` path.
- `renderer.rs:476-480` — "in this phase we don't render an actual sub-screen, just the fixed COLDATA backdrop. CGWSEL bit 1 would enable BG/OBJ on the sub-screen; that's a stretch goal."
- `ppu.rs:185` — "Visual registers (stored but not yet rendered)"
- `ppu.rs:388` — "Bit 7 of the value is the priority-rotation flag (not yet modeled)" (OAMADDR `$2103`).
- `ppu.rs:614-618` — write-side fallthrough: "Drop silently; we'll wire each register as the renderer needs it."

### 9.2 In luna-dma

- `controller.rs:19-21` — `$420C HDMAEN` field comment says "Stored but not yet acted upon (HDMA is in a later phase)." This is now **stale**: HDMA is wired in `controller.rs:78-103` and run from `snes.rs:433`. The doc comment was not updated when HDMA landed.

### 9.3 In luna-core

- `snes.rs:4-6` — "The PPU / APU / DMA are still TODOs" — also stale: PPU, DMA, and APU are now wired (real SPC700 included).
- `snes.rs:939` — "Mapper claims SRAM writes; anything not yet routed drops." (generic open-bus fall-through)
- `snes.rs:174-179` — coproc cartridges other than SA-1 panic at construction.
- `cpu_regs.rs:85` — RDNMI / TIMEUP read implicitly clear flags but do not lower the CPU's NMI/IRQ lines.

### 9.4 Notable absences (not marked TODO but real gaps)

- **Sub-screen compositing**: §2.
- **OAMADDR reload at end-of-VBlank / forced-blank-end**: §5.2.
- **Sprite per-line cap + range-over/time-over flags in STAT77**: §4.3.
- **Sprite-zero collision tracking**: §4.4.
- **Mode 7 EXTBG (BG2 overlay)**: §1.8.
- **Hi-res / pseudo-hires (modes 5/6, SETINI bits 3/5)**: §1.9.
- **Interlace**: SETINI bit 1 → unused. STAT78 field-toggle bit not flipped per field.
- **Mosaic V disable** (SETINI bit 2): unused. Mosaic itself is implemented for indexed BGs at `renderer.rs:1056-1061`.
- **PPU H/V counter dot-accuracy**: `current_hv` returns 0..340 dots, but inside-instruction PPU state is sampled only at instruction boundaries (`advance_scheduler` runs after each `cpu.step`).
- **CGRAM direct-color palette offset**: §1.6. luna's `direct_color_to_bgr5` does the 3-3-2 decompose but does NOT add the palette-offset contribution that real hardware applies.

---

## 10. Things that look right

A non-exhaustive list of things that match ares/Mesen2 closely so the gap analysis doesn't re-litigate them:

- Tile decode (2bpp / 4bpp / 8bpp planar) — `tile.rs:18-46`. BGR555 → RGB888 with the canonical "replicate top-3-bits" 5-to-8 scaling at `tile.rs:65-69`.
- VRAM word-address remapping (`memory.rs:172-180`), the 4 modes 0/1/2/3.
- VRAM prefetch quirk on `$2139/$213A` (`memory.rs:107-130`).
- CGRAM low/high latched write protocol on `$2122` (`memory.rs:241-252`).
- OAM low-table even/odd "atomic pair" commit (`memory.rs:396-416`).
- BG `$2107..$210A` SC bits 0-1 selecting 32×32 / 64×32 / 32×64 / 64×64 tilemap layouts (`renderer.rs:1044-1050`, `1260-1266`), with the sub-screen offset `+N*0x800` bytes.
- BG `BGMODE` bits 4-7 selecting 16×16 tile size per BG (`renderer.rs:1066-1117`).
- Per-pixel priority engine with mode-specific tables (`renderer.rs:931-1013`), including the BG3-priority bit-3 of BGMODE that promotes BG3.hi above OBJ.3 in Mode 1 (`MODE1_BG3HI_TABLE`, `renderer.rs:963-974`).
- Mode 7 affine: matrix * (sx, sy) + (cx<<8, cy<<8), with M7SEL screen-over modes and H/V flips (`renderer.rs:739-818`).
- Mode 7 hardware multiplier on M7A / M7B-high writes, exposed via MPYL/M/H (`ppu.rs:504-510`).
- COLDATA per-channel accumulation: 3 enable bits + 5-bit value (`ppu.rs:595-611`).
- Window inclusive range, empty-when-left>right (`renderer.rs:823-831`).
- Two-window logic ops OR/AND/XOR/XNOR (`renderer.rs:690-700`).
- W1/W2 invert handling per layer (`renderer.rs:680-689`).
- Color math: add / subtract / half / per-channel 5-bit saturated arithmetic (`renderer.rs:863-880`).
- Sprite `OBSEL` size pairs all 8 codes including the code-7 32×32 large quirk (`renderer.rs:282-295`).
- Sprite two-page nameselect addressing (`renderer.rs:262-273`).
- OAM 9-bit X with sign extension (`renderer.rs:334-338`).
- DMA all 8 modes including aliases (`channel.rs:79-90`).
- DMA `das == 0` → 64 KB (`channel.rs:274-278`).
- DMA increment Up / Down / Fixed (`channel.rs:118-122`).
- DMA B→A direction (`channel.rs:287-289`).
- HDMA non-repeat (transfer once per entry, gap on subsequent lines) vs repeat (transfer every line) — `channel.rs:393-398`.
- HDMA indirect mode with `dasb`-bank data pointer (`channel.rs:328-334`, `355-369`).
- HDMA terminator (header byte = 0) ends the channel (`channel.rs:323-326`, `381-383`).
- HDMA chain of multiple entries.
- NMITIMEN bit 7 → NMI on VBlank entry.
- NMITIMEN bit 0 → auto-joypad-read latch.
- WRIO (`$4201`) bit 7 0→1 transition latches PPU H/V counters via `current_hv` (`snes.rs:919-928`).
- SLHV (`$2137`) read latches H/V (`snes.rs:733-737`).
- OPHCT / OPVCT low-then-high read protocol (`ppu.rs:416-435`).
- STAT78 read clears the BG-scroll write-twice latch and the latch-hit bit (`ppu.rs:407-415`).
- BG scroll write-twice protocol with shared latch (`ppu.rs:454-474`).
- Mode 7 write-twice with separate (sticky) latch (`ppu.rs:481-485`).
- Multiplier on `$4203` write, divider on `$4206` write (`cpu_regs.rs:182-206`), including div-by-zero quotient `$FFFF` + remainder = original dividend.
- D-pad opposing-direction lockout (CLAUDE.md mandated; verified in `cpu_regs.rs:144-153`).
- Manual-shift refresh on auto-read (CLAUDE.md mandated; verified in `snes.rs:379-382`).
- HVBJOY bit 6 (HBlank) live from H-counter (`snes.rs:796-801`, recent commit).
- HVBJOY bit 7 latch (set at VBlank entry, cleared at frame wrap).
- HVBJOY bit 0 (auto-read busy) set at VBlank latch, cleared at VBlank + 3 lines.

---

## Appendix A — `Ppu` register fields touched / untouched by the renderer

| Reg | Field | Stored | Read by renderer |
|-----|-------|--------|------------------|
| $2100 INIDISP | `inidisp` | Y | Y (forced blank + brightness) |
| $2101 OBSEL | `obsel` | Y | Y (`sprite_tile_byte_offset`, `sprite_size_pair`) |
| $2105 BGMODE | `bgmode` | Y | Y (mode select, priority table, big-tile bits, BG3 priority) |
| $2106 MOSAIC | `mosaic` | Y | Y (per-BG mosaic snap) |
| $2107-$210A BGxSC | `bg[i].tilemap_*` | Y | Y |
| $210B-$210C BGxNBA | `bg[i].char_addr_words` | Y | Y |
| $210D-$2114 BGxxOFS | `bg[i].h_scroll`, `v_scroll` | Y | Y |
| $211A M7SEL | `m7sel` | Y | Y (Mode 7 only) |
| $211B-$2120 M7x | `m7a/b/c/d/x/y` | Y | Y (Mode 7 only) |
| $2121 CGADD | `cgram.address` | Y | (via cgram.color) |
| $2122 CGDATA | `cgram.data` | Y | Y |
| $2123-$2125 W*SEL | `w12sel`, `w34sel`, `wobjsel` | Y | Y |
| $2126-$2129 WH0..3 | `wh0..wh3` | Y | Y |
| $212A WBGLOG | `wbglog` | Y | Y |
| $212B WOBJLOG | `wobjlog` | Y | Y |
| $212C TM | `tm` | Y | Y |
| $212D TS | `ts` | Y | **N** |
| $212E TMW | `tmw` | Y | Y |
| $212F TSW | `tsw` | Y | **N** |
| $2130 CGWSEL | `cgwsel` | Y | Y (bits 0, 4-5, 6-7) — **bit 1 not consulted** |
| $2131 CGADSUB | `cgadsub` | Y | Y |
| $2132 COLDATA | `coldata_r/g/b` | Y | Y (as the fixed sub-screen) |
| $2133 SETINI | `setini` | Y | **N** |
| $213E STAT77 | `stat77` | Y (init = 0x01) | N (read returned to CPU) |
| $213F STAT78 | `stat78` | Y (init = 0x02) | N (read returned to CPU) |

Eight registers stored-but-not-rendered (TS, TSW, SETINI bits 1/2/3/5/6, CGWSEL bit 1) — all of which gate features the SMW dialog box / cutscene relies on.

---

## Appendix B — Render dispatch (which path runs)

- `Snes::render_frame_png` → `Snes::render_frame` → `render_frame_with(&ppu, opts)` in `renderer.rs:459`.
- GUI: `crates/luna-gui/src/app.rs:282` calls `luna_ppu::render_frame_with(&snes.ppu, opts)`.
- CLI screenshot: `crates/luna-cli/src/main.rs:769` calls `render_frame_with`. The `--bg N` CLI option (`main.rs:767`) instead calls `render_frame_bg_with(&snes.ppu, idx, opts)` (single-BG debug path).

So the production pixel path is `render_frame_with` for both the GUI and the CLI screenshot, and that's what this report inventories.
