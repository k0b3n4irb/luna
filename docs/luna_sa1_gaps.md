# luna SA-1 coprocessor — correctness gaps vs ares

Reference-first audit of luna's SA-1 against ares
(`ares/sfc/coprocessor/sa1/io.cpp`, `memory.cpp`, `dma.cpp`). Companion
to the BG / OBJ / APU / DMA gap docs; complements `archive/sa1_status.md` (the
fix-snapshot + the deliberate CIWP/SIWP `0xFF` deviation).

Scope: the register I/O + math unit + DMA/MMC in
`crates/luna-bus/src/sa1.rs` and the chip-side state in
`crates/luna-core/src/coproc/sa1.rs`.

Authored 2026-05-30.

## Severity legend

- 🔴 real bug — heavily-used unit produces wrong results
- 🟠 feature / behaviour missing
- 🟡 precision / minor deviation

---

## ✅ 1. Math unit (`$2250-$2254`) — DONE

ares `io.cpp:398-423`. `update_arith` was rewritten as a verbatim port;
the four divergences below are fixed. New tests
`divider_negative_dividend_is_floored`,
`divider_treats_divisor_as_unsigned`, `multiply_resets_mb_after_op`,
`sigma_accumulates_into_40_bit_result`. SMRPG (an SA-1 game) is a
0-pixel before/after diff — the fix makes the math hardware-correct so
it can only help.

The original divergences:

### 1a. Division is signed/signed truncated, not signed-÷-unsigned floored

ares: the **dividend is signed** (i16), the **divisor is unsigned**
(n16), and the remainder is **Euclidean (always ≥ 0)**:

```cpp
n16 remainder = dividend >= 0 ? dividend % divisor
              : (dividend % divisor + divisor) % divisor;
n16 quotient  = (dividend - remainder) / divisor;
io.mr = remainder << 16 | quotient;
```

luna does `ma / mb` and `ma % mb` with both operands **signed i16**
(truncated). Diverges whenever the dividend is negative or the divisor
has bit 15 set. E.g. **MA=−100, MB=7**: ares → q=−15, r=5; luna →
q=−14, r=−2. The existing test only covers 100/7 (positive, where the
two agree).

### 1b. Sigma (cumulative MAC) — no 40-bit mask, no overflow flag

ares `acm` mode: `io.mr += (i16)ma*(i16)mb; io.overflow = io.mr >> 40;
io.mr = (n40)io.mr;`. luna `mr.saturating_add(product)` keeps a full
i64 — it never masks MR to 40 bits and never computes the **overflow
flag** read at `$230B` (OF), which luna doesn't expose at all.
`saturating_add` also differs from the hardware wrap.

### 1c. MA/MB not reset after an operation

ares zeroes **MB** after a multiply/sigma and **both MA and MB** after a
divide (`io.cpp:402,414-415,422`). luna leaves them intact, so a game
reading MA/MB back after an op sees stale operands instead of 0.

### 1d. MCNT (`$2250`) MR-clear condition too narrow

ares clears MR whenever `acm` (bit 1) is set: `if(io.acm) io.mr = 0;`.
luna (`sa1.rs:1254`) clears only when the byte equals **exactly** `0x02`
(`value & 0x02 != 0 && value == 0x02`), so `$2250 = 0x03` (acm + md)
fails to clear MR.

**Why it matters:** SA-1 titles (Super Mario RPG, Kirby Super Star,
Kirby's Dream Land 3, PGA Tour, etc.) lean on the math unit for
scaling / perspective / physics. A wrong division or un-masked
accumulator produces visibly wrong geometry.

---

## ✅ 5. Timer HV mode (`$2210` hvselb=0) — IMPLEMENTED 2026-06-23

ares `sa1.cpp:63-94` runs the SA-1 timer in two modes selected by TMC
(`$2210`) bit 7 (`hvselb`). **Both are now a faithful port** of ares'
`SA1::step`, sharing one `hcounter`/`vcounter` model:

- **HV** (hvselb=0): `hcounter += 2` per 2 clocks, wraps at 1364;
  `vcounter++` wraps at `scanlines`; IRQ when `hcounter == hcnt<<2`
  (hen) and/or `vcounter == vcnt` (ven).
- **Linear** (hvselb=1): an 11-bit H feeding a 9-bit V free-runner; same
  compare switch. (This replaced luna's earlier non-faithful 18-bit
  single-counter model.)

**Key correction:** the timer is SELF-CONTAINED — it keeps its own H/V
counters, it does NOT read the PPU beam. The old "needs the PPU dot
view / wait for Phase 4" note was wrong; HV mode just wraps at
1364/`scanlines` to mimic beam timing. HCR/VCR (`$2302-$2305`) now read
back the live counters in dots. Unit-tested (`sa1.rs` `timer_*`: H match
linear + HV, V match HV, CTR restart, level-flag re-fire).

The IRQ-vs-PPU-scanline *alignment* is as accurate as luna's SA-1
stepping cadence (master-clock-driven; exact dot precision is the
general SA-1 cycle-accuracy refinement, Phase 4/5) — but the timer fires
correctly and games using HV-mode raster timing are no longer dead.

**No regression risk for SMRPG:** it writes TMC once (`= $00`, timer
off) and never touches `$2211-$2215`; smoke is byte-identical to the
pre-change baseline.

---

## ✅ 6. SA-1 CPU interrupt delivery — FIXED 2026-09-11

The timer, DMA and S-CPU → SA-1 interrupt sources were modelled up to
`sa1_irq_line()` / `sa1_nmi_line()`, but **nothing ever handed them to
the SA-1's 65c816**: `step_coproc` never set its IRQ line nor latched an
NMI, and the core does not poll `Bus::irq_pending()`. The SA-1 CPU never
vectored through CIV/CNV and a SA-1 `WAI` could only end by polling.
(#5's ✅ above was only proven up to the line — its tests asserted
`sa1_irq_line()`, not delivery.)

Now, at every SA-1 instruction boundary (ares `SA1::lastCycle`, Mesen2
`Sa1::ProcessInterrupts`):

- **IRQ** is a level through CIV: S-CPU request (CFR flag **and** the
  live CCNT bit 7 — both refs drop the request when CCNT is rewritten
  with bit 7 clear), timer and DMA, each gated by CIE.
- **NMI** is a one-shot through CNV, armed by a CCNT bit-4 write with
  the NMI enabled, or by CIE enabling the NMI while its flag is pending.

Tests `scpu_irq_request_vectors_the_sa1_cpu_through_civ`,
`scpu_nmi_request_vectors_the_sa1_cpu_through_cnv_once`. SMRPG (intro +
name entry, `nmis_serviced` 3335 @ frame 3988 unchanged), Kirby Super
Star and Kirby's Dream Land 3 checked after the change.

## 🔴 Open — found by the 2026-09-11 audit (faithful port pending)

| # | Gap | ares / Mesen2 | luna |
|---|---|---|---|
| 7 | **CC1 character conversion**: CDMA fields swapped (colour depth = bits 0-1, width = bits 2-4), 4bpp pixel order MSB-first instead of LSB-first, whole transfer converted at once instead of per-tile on S-CPU BW-RAM reads | ares `dma.cpp:64-107`, `io.cpp:453-459`; Mesen2 `Sa1.cpp:319-325,718-730`; fullsnes | `sa1.rs` `cc1_*` |
| 8 | **CC2** ignores the BRF register file `$2240-$224F` (one byte per pixel) and the 4-bit line counter | ares `dma.cpp:110-128`; Mesen2 `:744-767` | `sa1.rs` `cc2_consume_byte` |
| 9 | **BW-RAM bitmap view** (`$60-$6F`, BBF `$223F`, CBM bit 7 / `sw46`) missing | ares `bwram.cpp:45-130`, `memory.cpp:39-49` | `sa1.rs` SA-1-side decode |
| 10 | **CCNT bit 6 (RDYB) wait** ignored — the SA-1 keeps running | ares `sa1.cpp:46-50`; Mesen2 `Run` | `coproc/sa1.rs` |
| 11 | **Normal DMA** ignores the DCNT source device and decodes through the S-CPU map; costs the SA-1 no time | ares `dma.cpp:2-46`; Mesen2 `RunDma` | `sa1.rs` DMA |
| 12 | **Register dispatch not split by CPU side** (S-CPU reads of `$2301` return CFR instead of open bus — Kirby does 2.9 M of them) | ares `io.cpp`; Mesen2 `Sa1.cpp:81-428` | `sa1.rs` `read`/`write` |
| 13 | **ROM not mirrored** for carts under 4 MB | ares `rom.cpp:7-10` `bus.mirror` | `sa1.rs` ROM offset |
| 14 | **VLBP** advances on the `$230C` read instead of the `$2258` write; data masked | ares `io.cpp:427-439`; Mesen2 `:220-228` | `sa1.rs` VBD |
| 15 | **BW-RAM protection power-on** `sbwe/cbwe = $80`, `bwpa = $00` (refs: write-protected until enabled) | ares `sa1.cpp:231-237`; Mesen2 `Reset` | `sa1.rs` `new` |

---

## 🟡 Minor deviations / notes

| # | Issue | ares ref | luna |
|---|---|---|---|
| 2 | `$2202` (SIC) models a bit-6 "S-CPU NMI clear" that hardware doesn't have (SIC only has chdma=bit5, cpu=bit7) | `io.cpp:155-163` | `sa1.rs:1097-1099` |
| 3 | CIWP/SIWP reset default is `0xFF` (allow-all) not `0x00` — a **deliberate** deviation (an opensnes demo depends on it); see `archive/sa1_status.md` | `io.cpp` reset | intentional |
| 4 | CCNT reset edge sets `CIWP = 0` (`io.cpp:113`) | `io.cpp:103-114` | **deferred** — verified absent, but it lives in the deliberately-deviated I-RAM protection model (CIWP/SIWP default `0xFF`, `archive/sa1_status.md`). Adding it broke an SA-1 I-RAM test (synthetic handler doesn't pre-arm CIWP) and is the GUI-blackout-prone area the status doc warns about. Revisit with the protection model holistically + GUI validation. |

---

## ✅ Verified correct (do not regress)

- **CC1 / CC2 `cdsel` logic** (the old "cdsel inversion" regression is
  fixed — only the *selection*; the conversion formats themselves are
  open items #7/#8): `cden=1,cdsel=1` → CC1 on the `$2236` DDA byte; `cden=1,
  cdsel=0` → CC2 on the BRF[7]/BRF[15] (`$2247/$224F`) writes; normal
  DMA on the final DDA byte gated by `dd` (IRAM `$2236` / BWRAM
  `$2237`). Matches ares `io.cpp:449-488`.
- **Signed 16×16 multiply** (non-acm) → 32-bit MR (`multiplier_signed_
  negative` test).
- **MMC banking** (CXB/DXB/EXB/FXB + the `$2220-$2223` mode bits), the
  IRAM mirror, BW-RAM windows.
- **IRQ mailbox**: CCNT/SCNT latch the IRQ/NMI flag on *every* write
  with the bit set (not edge-detect) — the fix for the SMRPG handshake
  deadlock; acks are explicit via SIC/CIC.
- DMA per-byte coprocessor catch-up (the starfield fix).

## Suggested order

1. ~~#1 math unit (a/b/c/d)~~ — **done**.
2. 🟠 #5 timer HV mode — needs the PPU H/V dot view; pair with cycle-accuracy Phase 4.
3. 🟡 #2-#4 — minor; left as notes.
