# Mode 6 offset-per-tile repro ROM

A minimal hand-built SNES ROM that exercises **Mode 6 (hi-res) offset-per-
tile (OPT)** — there is no Mode 6 OPT ROM in any public test corpus, so
this was built to settle bg-gap #15 (`docs/luna_bg_gaps.md`).

## What it shows

BG1 renders vertical 4-colour bars (red / green / blue / white, one colour
per tile column via the palette group). BG3's OPT data gives every screen
column **≥ 16** a horizontal offset of **+16** (= one 16-px hi-res tile),
with the BG1 enable bit set. So:

- left half (columns 0–15, no OPT): `R G B W R G B W …`
- right half (columns 16–31): the colour sequence shifts.

A **correct** emulator shifts by **one** tile (ares `background.cpp:66` —
in hi-res the base scroll doubles but the OPT override does not):
`… G B W R …` (column 16 → green). The bug luna had **doubled** the OPT
offset and shifted by **two** tiles (column 16 → blue).

This was a real bug, fixed in `luna-ppu` (`render_bg_scanline_indexed_hires`
+ `opt_scroll`), guarded by the unit test
`mode6_opt_offset_is_not_doubled_in_hires`.

## Rebuilding

Needs [bass](https://github.com/ARM9/bass) (byuu's assembler — patch
`nall/arithmetic/natural.hpp` with `#include <stdexcept>` to build on
modern GCC, and copy `data/architectures/` next to the binary):

```bash
python3 gen_data.py     # writes tiles.bin / bg1map.bin / bg3map.bin / pal.bin
bass main.asm           # writes mode6opt.sfc
```

The ROM is headerless (no valid checksum); load it with luna's
`Cartridge::from_bytes_forced(.., MapperKind::LoRom)`.
