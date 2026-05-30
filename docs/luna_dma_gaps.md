# luna DMA / HDMA subsystem — correctness gaps vs ares

Reference-first audit of luna's DMA/HDMA against ares
(`ares/sfc/cpu/dma.cpp`). Companion to the BG / OBJ / APU gap docs.

Scope: `crates/luna-core/src/dma/` (`channel.rs`, `controller.rs`) and
the scheduler wiring in `snes.rs` (`$420B` MDMAEN, `$420C` HDMAEN, the
frame/scanline HDMA hooks).

Authored 2026-05-30.

**Headline:** unlike the BG/OBJ/APU audits, there's no clear visible
bug here — the DMA/HDMA core is faithful and well-covered by tests. The
findings are edge-case hardware *restrictions* and timing
*approximations*.

## Severity legend

- 🔴 real bug, correct ROMs misbehave
- 🟠 feature / restriction missing
- 🟡 precision / timing approximation

---

## 🟠 1. A-bus access restrictions (`validA`) not enforced

ares `dma.cpp:54-83`: the DMA A-bus **cannot** reach the B-bus or CPU
I/O — reads there return open bus (`0x00`/MDR) and writes are dropped.
The blocked ranges (banks `00-3f`/`80-bf`) are:

| range | what |
|---|---|
| `2100-21ff` | B-bus (PPU regs) |
| `4000-41ff` | CPU I/O (joypad serial) |
| `4200-421f` | CPU I/O (NMITIMEN…) |
| `4300-437f` | DMA registers |

luna's `DmaChannel::run` (`channel.rs:286-309`) and the HDMA paths read
/ write the A-bus with no `validA` gate, so a DMA whose A-address lands
in these ranges sees real register data instead of open bus (and a
write hits the register instead of being dropped). Rare, but a
documented restriction (and used by a few protection tricks).

## 🟠 2. WRAM→WRAM transfer not blocked

ares `dma.cpp:94`: a transfer to B-bus `$2180` (WMDATA) from a WRAM
A-address is **invalid** — the byte is dropped:

```cpp
bool valid = addressB != 0x80
  || ((addressA & 0xfe0000) != 0x7e0000 && (addressA & 0x40e000) != 0x0000);
```

luna performs the write unconditionally, so a WRAM→WRAM DMA via `$2180`
would corrupt WRAM where hardware no-ops it. Rare.

---

## 🟡 Precision / timing

| # | Issue | ares ref | luna |
|---|---|---|---|
| 3 | MDMA cost charged as flat `8 + bytes·8`; ares adds a per-channel `+8` (and aligns the burst start to a whole CPU cycle) | `dma.cpp:16-22,108-122` | `snes.rs:1444` lumps per-channel into per-byte |
| 4 | Sync DMA is **atomic** (runs all bytes in one `run_mdma` call) so it never yields to HDMA mid-transfer; ares lets HDMA stop an active DMA at a scanline boundary (`dmaEnable = false`) | `dma.cpp:146,175` | OK in practice — sync DMA almost always runs in V-blank with no active HDMA |
| 5 | Enabling an HDMA channel mid-frame via `$420C` doesn't set it up until the next frame's `hdma_init` | `dma.cpp:28-33` | `controller.rs:78` only sets up at frame start |
| 6 | Indirect-HDMA `hdmaCompleted && hdmaFinished()` early-out after reading the first pointer byte not modelled | `dma.cpp:165` | `channel.rs:337-343` reads both pointer bytes regardless |

---

## ✅ Verified correct (do not regress)

- **All 8 transfer modes** + their B-bus offset patterns, incl. the
  aliases (mode 5 `[0,1,0,1]`, 6=`2`, 7=`3`) — matches ares
  `transfer()` `index.bit(...)` logic and the HDMA `lengths[8]` table.
- **Direction** (A→B / B→A), **A-increment** (+1 / −1 / fixed),
  `das == 0` → 64 KB.
- **HDMA**: header decode, repeat (bit 7) vs non-repeat first-line-only,
  7-bit line counter, indirect-mode pointer load + walk, multi-entry
  chaining, terminator (`00`) handling. luna's "preserve header bit 7
  for continuation `do_transfer`" is equivalent to ares' "current
  counter `.bit(7)`" for all valid line counts (1-127).
- **`$43x5/6` shared** between the DMA byte count and the HDMA indirect
  address — correct (hardware shares the register pair).
- Channel register read/write (`$43x0-$43xF`); `$420B` ascending
  channel order; per-scanline HDMA hooked at every visible line.
- Per-byte cooperative `bus.tick(8)` so coprocessors (SA-1) interleave
  with the DMA instead of freezing.

## Suggested order

1. **#1 validA** — the cleanest real restriction; one gate in the
   A-bus read/write helpers.
2. **#2 WRAM→WRAM block** — small, well-defined.
3. 🟡 #3-#6 — timing approximations; low real-world return (the current
   model is game-compatible).
