# S-DD1 coprocessor — synthesised ares + Mesen2 reference

**Purpose:** the faithful-port spec for luna's S-DD1 (graphics decompression
chip — Star Ocean, Street Fighter Alpha 2). Built per `.claude/rules/
reference-first.md` from **both** references read in full:

- **ares** `ares/sfc/coprocessor/sdd1/{sdd1.cpp,sdd1.hpp,decompressor.cpp,decompressor.hpp}`
- **Mesen2** `Core/SNES/Coprocessors/SDD1/{Sdd1.cpp,Sdd1Mmc.cpp,Sdd1Decomp.cpp,…}`

> **Key finding:** ares and Mesen2 are **byte-identical** on the two critical
> tables (`run_count` 256 entries, `evolution_table` 33 states) and structurally
> identical on the whole pipeline (IM→GCD→8×BG→PEM→CM→OL). There is **no
> divergence** to arbitrate — both implement the canonical "Andreas Naive"
> reference. luna copies the tables verbatim.

The S-DD1 is two independent subsystems: (1) an **MMC** (memory mapper, banks the
up-to-8 MB ROM into the bus), and (2) a **streaming decompressor** that runs
**during a DMA** reading from the chip.

---

## 1. Registers + memory map

S-DD1 control registers (`$4800-$480F`, banks `$00-3F`/`$80-BF`; address masked to
`$4800 | addr & 0x0F`):

| Reg | Name | Meaning |
|---|---|---|
| `$4800` | r4800 — **hard enable** | bit-per-channel: channel n is S-DD1-decompressible |
| `$4801` | r4801 — **soft enable** | bit-per-channel arm; **cleared by the chip** when a stream ends |
| `$4804` | r4804 — MMC bank A | write masked `& 0x8F`; low 4 bits = 1 MB ROM bank for `$C0-CF` |
| `$4805` | r4805 — MMC bank B | `& 0x8F`; bank for `$D0-DF`; **bit 7** = `$20-3F` mirror flag |
| `$4806` | r4806 — MMC bank C | `& 0x8F`; bank for `$E0-EF` |
| `$4807` | r4807 — MMC bank D | `& 0x8F`; bank for `$F0-FF`; **bit 7** = `$A0-BF` mirror flag |

`$4802-3` / `$4808-F` read through to ROM.

**ROM decode (ares `mcuRead`/`mmcRead`):**

- **`$00-3F,$80-BF : $8000-FFFF`** (banked LoROM-style region):
  ```
  if (!addr.bit23 && addr.bit21 && r4805.bit7) addr.bit21 = 0   // $20-3F mirror
  if ( addr.bit23 && addr.bit21 && r4807.bit7) addr.bit21 = 0   // $A0-BF mirror
  rom_addr = (addr.bits[16:21] << 15) | addr.bits[0:14]
  ```
- **`$C0-FF : $0000-FFFF`** (MMC-banked region; `mmcRead`): `addr.bits[20:21]`
  selects r4804/5/6/7; `rom_addr = (reg & 0x0F) << 20 | addr.bits[0:19]`.

---

## 2. DMA-triggered decompression (the integration crux)

The chip decompresses **on the fly while a DMA reads from it**. ares `mcuRead`
`$C0-FF` path:

```
if (r4800 & r4801) {                       // some channel armed
  for n in 0..8 {
    if r4800.bit(n) && r4801.bit(n) && addr == dma[n].address {
      if !dmaReady { decompressor.init(addr); dmaReady = true; }   // first read
      data = decompressor.read();           // one decompressed byte
      if (--dma[n].size == 0) { dmaReady = false; r4801.bit(n) = 0; }  // stream end
      return data;
    }
  }
}
return mmcRead(addr);                        // not decompressing → normal ROM
```

The chip captures the per-channel DMA **address** (`$43x2-4`) and **size**
(`$43x5-6`) by intercepting writes to `$00-3F,$80-BF:$4300-437F` (ares `dmaWrite`,
which then forwards to the CPU's real DMA). **S-DD1 always uses fixed-address DMA**,
so `addr == dma[n].address` holds for every byte of the transfer.

Protocol the game uses: write `$43xx` (addr+size, fixed mode) → write r4800+r4801
to arm the channel → write `$420B` (MDMAEN) → the DMA streams decompressed bytes;
the chip clears r4801.bit(n) and re-arms `dmaReady` when size hits 0.

### luna integration note (this is where luna differs from the refs' structure)

In luna, `$4300-437F` writes go to the **DMA controller**, not the mapper, and
`mapper.read()` serves both CPU reads and DMA A-bus reads (via
`DmaBusView::read_a`). So to port ares faithfully, luna must **forward `$43xx`
writes to the S-DD1 mapper** (in addition to the DMA controller) so it can capture
`dma[n].address/size`. `$4800-480F` already reaches `mapper.read/write` (CPU
fall-through). Then the `mcuRead` logic ports verbatim: `mapper.read(addr)` matches
`addr == dma[n].address` with `r4800 & r4801` armed → decompress.

Detection: chipset byte `$FFD6` — `(chipset & 0x0F) >= 0x03 && (chipset & 0xF0)
== 0x40` (S-DD1 = coprocessor high-nibble **4**; cf. SuperFX=1, DSP=0, SA-1=3).
S-DD1 carts are LoROM-based; header at `$7FC0`.

---

## 3. The decompressor (IM → GCD → 8×BG → PEM → CM → OL)

Six submodules. `init(offset)` resets all; `read()` returns one output byte
(delegates to OL). The header byte at `offset` configures CM + OL (bits 7:6 =
bitplane mode, 5:4 = context-bits mode).

### 3.1 Input Manager (IM) — bit reader
State: `offset` (ROM byte pos), `bitCount` (**init = 4**, skips the header's low
4 bits). `getCodeWord(codeLen)`:
```
cw = mmcRead(offset) << bitCount;  bitCount++
if (cw & 0x80) { cw |= mmcRead(offset+1) >> (9 - bitCount); bitCount += codeLen }
if (bitCount & 0x08) { offset++; bitCount &= 0x07 }
return cw
```

### 3.2 Golomb-Code Decoder (GCD) — `run_count[256]` (VERBATIM — both refs identical)
```
0x00,0x00,0x01,0x00,0x03,0x01,0x02,0x00, 0x07,0x03,0x05,0x01,0x06,0x02,0x04,0x00,
0x0f,0x07,0x0b,0x03,0x0d,0x05,0x09,0x01, 0x0e,0x06,0x0a,0x02,0x0c,0x04,0x08,0x00,
0x1f,0x0f,0x17,0x07,0x1b,0x0b,0x13,0x03, 0x1d,0x0d,0x15,0x05,0x19,0x09,0x11,0x01,
0x1e,0x0e,0x16,0x06,0x1a,0x0a,0x12,0x02, 0x1c,0x0c,0x14,0x04,0x18,0x08,0x10,0x00,
0x3f,0x1f,0x2f,0x0f,0x37,0x17,0x27,0x07, 0x3b,0x1b,0x2b,0x0b,0x33,0x13,0x23,0x03,
0x3d,0x1d,0x2d,0x0d,0x35,0x15,0x25,0x05, 0x39,0x19,0x29,0x09,0x31,0x11,0x21,0x01,
0x3e,0x1e,0x2e,0x0e,0x36,0x16,0x26,0x06, 0x3a,0x1a,0x2a,0x0a,0x32,0x12,0x22,0x02,
0x3c,0x1c,0x2c,0x0c,0x34,0x14,0x24,0x04, 0x38,0x18,0x28,0x08,0x30,0x10,0x20,0x00,
0x7f,0x3f,0x5f,0x1f,0x6f,0x2f,0x4f,0x0f, 0x77,0x37,0x57,0x17,0x67,0x27,0x47,0x07,
0x7b,0x3b,0x5b,0x1b,0x6b,0x2b,0x4b,0x0b, 0x73,0x33,0x53,0x13,0x63,0x23,0x43,0x03,
0x7d,0x3d,0x5d,0x1d,0x6d,0x2d,0x4d,0x0d, 0x75,0x35,0x55,0x15,0x65,0x25,0x45,0x05,
0x79,0x39,0x59,0x19,0x69,0x29,0x49,0x09, 0x71,0x31,0x51,0x11,0x61,0x21,0x41,0x01,
0x7e,0x3e,0x5e,0x1e,0x6e,0x2e,0x4e,0x0e, 0x76,0x36,0x56,0x16,0x66,0x26,0x46,0x06,
0x7a,0x3a,0x5a,0x1a,0x6a,0x2a,0x4a,0x0a, 0x72,0x32,0x52,0x12,0x62,0x22,0x42,0x02,
0x7c,0x3c,0x5c,0x1c,0x6c,0x2c,0x4c,0x0c, 0x74,0x34,0x54,0x14,0x64,0x24,0x44,0x04,
0x78,0x38,0x58,0x18,0x68,0x28,0x48,0x08, 0x70,0x30,0x50,0x10,0x60,0x20,0x40,0x00,
```
`getRunCount(codeNum) -> (mpsCount, lpsIndex)`:
```
cw = IM.getCodeWord(codeNum)
if (cw & 0x80) { lpsIndex = 1; mpsCount = run_count[cw >> (codeNum ^ 7)] }
else           { mpsCount = 1 << codeNum }   // lpsIndex stays 0
```

### 3.3 Bits Generator (BG) — 8 instances `bg0..bg7`, each `codeNum = 0..7`
State: `mpsCount`, `lpsIndex` (init 0,0). `getBit() -> (bit, endOfRun)`:
```
if (!mpsCount && !lpsIndex) GCD.getRunCount(codeNum, &mpsCount, &lpsIndex)
if (mpsCount) { bit = 0; mpsCount-- } else { bit = 1; lpsIndex = 0 }
endOfRun = !(mpsCount || lpsIndex)
```

### 3.4 Probability Estimation Module (PEM) — `evolution_table[33]` {codeNum, nextMPS, nextLPS} (VERBATIM)
```
{0,25,25},{0,2,1},{0,3,1},{0,4,2},{0,5,3},{1,6,4},{1,7,5},{1,8,6},{1,9,7},
{2,10,8},{2,11,9},{2,12,10},{2,13,11},{3,14,12},{3,15,13},{3,16,14},{3,17,15},
{4,18,16},{4,19,17},{5,20,18},{5,21,19},{6,22,20},{6,23,21},{7,24,22},{7,24,23},
{0,26,1},{1,27,2},{2,28,4},{3,29,8},{4,30,12},{5,31,16},{6,32,18},{7,24,22},
```
`contextInfo[32]` = {status, mps} (init 0,0). `getBit(context)`:
```
s = evolution_table[contextInfo[ctx].status]
bit = bg[s.codeNum].getBit(&endOfRun)
if (endOfRun) {
  if (bit) { if (!(status & 0xFE)) contextInfo[ctx].mps ^= 1;  status = s.nextLPS }
  else     { status = s.nextMPS }
}
return bit ^ contextInfo[ctx].mps
```

### 3.5 Context Model (CM) — bitplane interleave + context bits
State from header: `bitplanesInfo = firstByte & 0xC0`, `contextBitsInfo =
firstByte & 0x30`; `bitNumber`, `currentBitplane`, `prevBitplaneBits[8]` (u16).
Init `currentBitplane`: mode 0x00→1, 0x40→7, 0x80→3 (0xC0 set per bit).
`getBit()`:
```
switch bitplanesInfo {
  0x00: currentBitplane ^= 1
  0x40: currentBitplane ^= 1; if (!(bitNumber & 0x7F)) currentBitplane = (currentBitplane+2)&7
  0x80: currentBitplane ^= 1; if (!(bitNumber & 0x7F)) currentBitplane ^= 2
  0xC0: currentBitplane = bitNumber & 7
}
ctxBits = &prevBitplaneBits[currentBitplane]
ctx = (currentBitplane & 1) << 4
switch contextBitsInfo {
  0x00: ctx |= ((*ctxBits & 0x01C0) >> 5) | (*ctxBits & 1)
  0x10: ctx |= ((*ctxBits & 0x0180) >> 5) | (*ctxBits & 1)
  0x20: ctx |= ((*ctxBits & 0x00C0) >> 5) | (*ctxBits & 1)
  0x30: ctx |= ((*ctxBits & 0x0180) >> 5) | (*ctxBits & 3)
}
bit = PEM.getBit(ctx);  *ctxBits = (*ctxBits << 1) | bit;  bitNumber++;  return bit
```

### 3.6 Output Logic (OL) — bitplanes → bytes
State: `bitplanesInfo` (= firstByte & 0xC0), regs `r0,r1,r2` (init r0=1).
`decompress() -> byte`:
```
0x00 | 0x40 | 0x80:   // 2 planes → 2 bytes, returned across two calls
  if (r0 == 0) { r0 = ~r0; return r2 }              // even call: buffered high byte
  for (r0=0x80, r1=0, r2=0; r0; r0>>=1) {           // odd call: read 16 bits
    if (CM.getBit()) r1 |= r0
    if (CM.getBit()) r2 |= r0
  }
  return r1
0xC0:                 // 8bpp: 1 plane → 1 byte
  for (r0=0x01, r1=0; r0; r0<<=1) { if (CM.getBit()) r1 |= r0 }
  return r1
```

Mode summary (header bits 7:6): `0x00/0x40/0x80` = 2-plane (2/4 bpp gfx), `0xC0` =
1-plane (8 bpp). Bits 5:4 = which previous bits form the context window.

---

## 4. luna porting plan (stages)

1. **Pure `Sdd1Decompressor`** (luna-bus, new file): IM/GCD/BG/PEM/CM/OL + the two
   verbatim tables, reading input via a `&[u8] + offset` (or a closure). Unit-test
   the building blocks + table integrity in isolation — **zero SNES coupling**.
2. **`Sdd1Mapper` MMC** (luna-bus): the `$4800-4807` regs + the banked-ROM read
   paths (`$00-3F/$80-BF:8000-FFFF` + `$C0-FF`), detection (`0x40`) + factory.
   No decompression yet — game boots + runs uncompressed code.
3. **Wire decompression**: forward `$43xx` writes to the mapper (capture addr/size),
   the `mcuRead` armed-DMA path in `mapper.read`. End-to-end: Star Ocean / SF Alpha
   2 graphics (user GUI eyeball — `audible-fixes`/visible-rendering rule).
4. **Save-state + edge cases** (serialize decompressor + MMC state).

Validation: the two tables get an integrity test; each submodule a behavioural
test; end-to-end is the real game in the GUI (no public S-DD1 golden corpus).
