# luna SA-1 coprocessor — correctness gaps vs ares

Reference-first audit of luna's SA-1 against ares
(`ares/sfc/coprocessor/sa1/io.cpp`, `memory.cpp`, `dma.cpp`). Companion
to the BG / OBJ / APU / DMA gap docs; complements `sa1_status.md` (the
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

## 🔴 1. Math unit (`$2250-$2254`) — division & sigma wrong

ares `io.cpp:398-423`. luna's `update_arith` (`sa1.rs:905-928`) diverges
in three ways:

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

## 🟡 Minor deviations / notes

| # | Issue | ares ref | luna |
|---|---|---|---|
| 2 | `$2202` (SIC) models a bit-6 "S-CPU NMI clear" that hardware doesn't have (SIC only has chdma=bit5, cpu=bit7) | `io.cpp:155-163` | `sa1.rs:1097-1099` |
| 3 | CIWP/SIWP reset default is `0xFF` (allow-all) not `0x00` — a **deliberate** deviation (an opensnes demo depends on it); see `sa1_status.md` | `io.cpp` reset | intentional |
| 4 | CCNT reset path sets `CIWP = 0` on the SA-1-reset edge — verify luna does this (it's chip-side) | `io.cpp:103-114` | check `coproc/sa1.rs` |

---

## ✅ Verified correct (do not regress)

- **CC1 / CC2 `cdsel` logic** (the old "cdsel inversion" regression is
  fixed): `cden=1,cdsel=1` → CC1 on the `$2236` DDA byte; `cden=1,
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

1. **#1a division** — the highest-impact correctness bug; port ares'
   signed-÷-unsigned Euclidean algorithm verbatim + add negative /
   bit-15-divisor tests.
2. **#1b/#1c/#1d** — sigma 40-bit mask + overflow flag, MA/MB reset,
   MCNT clear — small, in the same function.
3. 🟡 #2-#4 — minor.
