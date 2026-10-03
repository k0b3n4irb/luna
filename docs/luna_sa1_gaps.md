# luna SA-1 coprocessor — correctness gaps vs ares

Reference-first audit of luna's SA-1 against ares
(`ares/sfc/coprocessor/sa1/io.cpp`, `memory.cpp`, `dma.cpp`). Companion
to the BG / OBJ / APU gap docs and the DMA audit
([`hdma_ares_audit.md`](hdma_ares_audit.md)). The May-2026 fix snapshot is
frozen in [`archive/sa1_status.md`](archive/sa1_status.md); the rationale for
the deliberate CIWP/SIWP `0xFF` deviation it used to hold now lives **here**
(row #3 below).

> **Status 2026-10-04:** rows #1 and #5-#19 are done. **Open:** #2, #3, #4
> and #20 — see "⚠️ Open divergences" below.

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

The original divergences (1a-1d describe the code **before** the fix):

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
luna cleared it only when the byte equalled **exactly** `0x02`
(`value & 0x02 != 0 && value == 0x02`), so `$2250 = 0x03` (acm + md)
failed to clear MR. (Pre-fix description; the fixed code is `update_arith`
and the `$2250` arm in `crates/luna-bus/src/sa1.rs`.)

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
stepping cadence (master-clock-driven; exact dot precision is bounded by
the batched scheduler grain, the residual named at the end of this
document) — and the timer fires and games using HV-mode raster timing are no
longer dead. **Open:** the V wrap is fixed at the NTSC line count — row #20.

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

Now, at every SA-1 instruction's **last cycle** — the poll one cycle
before its final bus access, which is where ares puts `SA1::lastCycle`
(2026-09-20; it was the instruction boundary before, see
[`luna_65c816_gaps.md`](luna_65c816_gaps.md) #1):

- **IRQ** is a level through CIV: S-CPU request (CFR flag **and** the
  live CCNT bit 7 — both refs drop the request when CCNT is rewritten
  with bit 7 clear), timer and DMA, each gated by CIE.
- **NMI** is a one-shot through CNV, armed by a CCNT bit-4 write with
  the NMI enabled, or by CIE enabling the NMI while its flag is pending.

Tests `scpu_irq_request_vectors_the_sa1_cpu_through_civ`,
`scpu_nmi_request_vectors_the_sa1_cpu_through_cnv_once`. SMRPG (intro +
name entry, `nmis_serviced` 3335 @ frame 3988 unchanged), Kirby Super
Star and Kirby's Dream Land 3 checked after the change.

## ✅ Found by the 2026-09-11 audit — all ported (2026-09-12 → 2026-09-14)

| # | Gap | ares / Mesen2 | luna |
|---|---|---|---|
| ~~7~~ | ~~**CC1 character conversion**~~ — ✅ **DONE 2026-09-12**: `dmaCC1` / `dmaCC1Read` ported line for line. CDMA decodes colour depth from bits 0-1 and width from bits 2-4 (they were swapped), pixels come out LSB-first, and the conversion happens ONE CHARACTER AT A TIME on the S-CPU's own BW-RAM reads through `bwram.dma`, answering from I-RAM at DDA — not in one bulk pass at the trigger | ares `dma.cpp:48-107`, `io.cpp:452-461`, `bwram.cpp:29` | `sa1.rs` `dma_cc1`, `dma_cc1_read` |
| ~~8~~ | ~~**CC2**~~ — ✅ **DONE 2026-09-12**: `dmaCC2` ported. The `$2240-$224F` BRF register file is stored, a write to BRF[7] or BRF[15] converts one tile row into I-RAM at DDA using ares' planar byte map, and the 4-bit line counter advances (reset when DCNT clears DMA enable) | ares `dma.cpp:108-128`, `io.cpp:348-368,327` | `sa1.rs` `dma_cc2` |
| ~~9~~ | ~~**BW-RAM bitmap view**~~ — ✅ **DONE 2026-09-14**: the SA-1 sees BW-RAM three ways (`bwram_target_sa1`): linear `$40-$5F` (mirrored), the bitmap projection `$60-$6F` (one pixel per address, 4 bpp two-a-byte low nibble first or 2 bpp four-a-byte per BBF `$223F` bit 7, writes are read-modify-write), and the `$6000-$7FFF` window as linear page `CBM & $1F` or, with CBM bit 7 (`sw46`), bitmap page `CBM & $7F`. Bitmap writes bypass BWPA as in ares (Mesen2 protects them; the two differ only with both enables clear). Tests `sa1_reads_bwram_as_pixels_through_banks_60_to_6f`, `cbm_bit_7_turns_the_sa1_window_into_a_bitmap_page`, `sa1_linear_bwram_spans_banks_40_to_5f` | ares `bwram.cpp:45-130`, `memory.cpp:39-49` | `sa1.rs` `bwram_target_sa1`, `bitmap_read/write` |
| ~~10~~ | ~~**CCNT bit 6 (RDYB) wait**~~ — ✅ **DONE 2026-09-12**: the chip is parked while the S-CPU holds RDYB; its timer keeps ticking, as in ares. Test `ccnt_bit_6_parks_the_sa1_but_keeps_its_timer_running` | ares `sa1.cpp:46-50`; Mesen2 `Run` | `coproc/sa1.rs` |
| ~~11~~ | ~~**Normal DMA**~~ — ✅ **DONE 2026-09-14**: `dmaNormal` ported. DCNT names the devices (sd ROM/BW-RAM/I-RAM, dd I-RAM/BW-RAM); only the four hardware pairs move bytes, any other pair just runs DTC down. ROM is read through the SA-1's map (`rom_read_sa1`, ares `rom.cpp:61-66`), BW-RAM and I-RAM by raw offset. Each byte charges the SA-1 its steps (2 / 1 / 2 / 2 plus `conflict()` steps against the S-CPU's address) through `take_dma_steps`, which the chip driver subtracts from its budget — the SA-1 stalls for the transfer. DMA enable is no longer cleared at completion (neither reference does). Tests `normal_dma_*` (5) | ares `dma.cpp:2-46`; Mesen2 `RunDma` | `sa1.rs` `run_normal_dma`; `coproc/sa1.rs` `step_coproc` |
| ~~12~~ | ~~**Register dispatch not split by CPU side**~~ — ✅ **DONE 2026-09-14**: `readIOCPU` / `readIOSA1` / `writeIOCPU` / `writeIOSA1` ported. The S-CPU reads only SFR (`$2300`), everything else in `$2200-$23FF` is open bus; the SA-1 reads CFR, HCR/VCR (latched together by the `$2302` read), MR, OF and the VLBP ports. Writes are owned per side (`cpu_side_register` / `sa1_side_register`; `$2231-$2237` shared) — a write to the other side's register is dropped. No register is memory-backed any more. Tests `unowned_register_slots_read_open_bus_on_both_sides`, `register_writes_are_owned_by_one_side`, `hcr_vcr_latch_on_the_2302_read` | ares `io.cpp`; Mesen2 `Sa1.cpp:81-428` | `sa1.rs` `read`, `read_io_sa1`, `write_with_side` |
| ~~13~~ | ~~**ROM not mirrored**~~ — ✅ **DONE 2026-09-12**: SA-1 ROM addresses run through the shared `rom_mirror` (ares' `bus.mirror`), so a cart smaller than the 4 MB the super-MMC addresses repeats instead of reading open bus | ares `rom.cpp:7-10` | `sa1.rs` `rom_offset` |
| ~~14~~ | ~~**VLBP**~~ — ✅ **DONE 2026-09-14**: ares' `va` / `vbit` model. Fixed mode (VBD bit 7 clear) advances on the `$2258` write itself, auto-increment mode on the `$230D` read; `$230C` never advances; the window is unmasked; `$225B` zeroes `vbit`; reads go through `readVBR` (ROM by the SA-1's map, BW-RAM / I-RAM raw, never a register). Tests `vlbp_*` (4) | ares `io.cpp:427-444, 62-89`; `memory.cpp:113-133`; Mesen2 `:220-236, 387-401` | `sa1.rs` `vlbp_*`, `read_vbr` |
| ~~15~~ | ~~**BW-RAM protection power-on**~~ — ✅ **DONE 2026-09-14**: SBWE / CBWE come up clear and BWPA `$0F`, so every BW-RAM byte refuses writes from either side until a game enables one (SMRPG, Kirby Super Star and Kirby's Dream Land 3 all do; CLI fbhash identical to v1.22.0 at 21 checkpoints). Test `bwram_is_write_protected_at_power_on` | ares `sa1.cpp:231-237`; Mesen2 `Reset` | `sa1.rs` `new` |

---

## ✅ SA-1 speed vs a console — ported 2026-09-27

Found by running the **SNES-SA1 Speed Test v5.1** (VitorVilela7,
`speed_test_v51.sfc`) against the photos of a real 1L8B-10 console in the
same repository (`img/hardware/v51-1L8B-10/`), with Mesen2 as second
source. Three ares mechanisms, plus one invented guard, were missing:

| # | Gap | ares | luna | Speed Test (console / Mesen2 / before → after) |
|---|---|---|---|---|
| ~~16~~ | ~~ROM penalty on jumps~~ — ✅ `idleJump` / `idleBranch` hooks on `Bus`, called at ares' 15 sites; SA-1 charges one step (+ ROM conflict) when the new PC is in ROM, branches only when it is odd | `wdc65816.hpp:11-12`, `instructions-pc.cpp`, `sa1/memory.cpp:6-19` | `Bus::idle_jump/idle_branch`, `Sa1Bus` | WRAM\|ROM 10.068 / 10.068 / 10.738 → **10.068**; `RTI` ROM 15.586 / 15.588 / 16.106 → **15.586** |
| ~~17~~ | ~~DMA address in the conflict check~~ — ✅ `DmaBusView` sets the S-CPU `mar` on every A-bus access | `cpu/dma.cpp:96,153,163,167` | `DmaBusView::read_a/write_a/tick` | DMA ROM\|ROM 5.075 / 5.093 / 10.46 → **5.059** |
| ~~18~~ | ~~120-mclk budget clamp~~ — ✅ removed (no counterpart in ares; it dropped a scanline's HDMA time) | SA-1 thread always catches up | `Sa1Chip::step_coproc` | HDMA WRAM\|ROM 10.054 / 10.068 / 8.40 → **10.067** |
| ~~19~~ | ~~I-RAM conflict during refresh~~ — ✅ skipped while `dramRefresh == 1` (the refresh replayed as five 6+2 pairs) | `sa1/iram.cpp:2`, `cpu/timing.cpp:24-28` | `Mapper::set_scpu_refresh`, `advance_time` | I-RAM\|I-RAM 3.722 / 3.591 / 3.590 → **3.679** |

**Residuals shared with the references** (not luna gaps): `JMP` in ROM
reads 8.477 in luna **and** Mesen2 against 7.670 on the console; the SA-1
core during an I-RAM↔BW-RAM SA-1 DMA reads 0.14 in both against ~10.3.
**Residual vs Mesen2:** S-CPU DMA from I-RAM reads 3.71 (console 5.51,
Mesen2 5.43) — ares' address-based `conflict()` gives the same as luna.

## ⚠️ Open divergences

Rows #2 and #20 and the rationale under #3 were recorded by the 2026-10-04
repository audit: **found by the 2026-10-04 audit, not yet checked against
ares / Mesen2** beyond the citations already in the table. No fix is
proposed here; each is to be resolved by a faithful port
(`.claude/rules/faithful-port-and-dichotomy.md`).

| # | Issue | ares ref | luna | Status |
|---|---|---|---|---|
| 2 | `$2202` (SIC) models a bit-6 "S-CPU NMI clear" that hardware doesn't have (SIC only has chdma=bit5, cpu=bit7). The latch it clears, `s_nmi_to_main`, is a **placeholder**: it is initialised `false`, written `false` by that arm and by nothing else, and read nowhere — and its doc comment ("raised on `$2209` bit-6 0→1 edge") contradicts the `$2209` decode beside it | `io.cpp:155-163` | `crates/luna-bus/src/sa1.rs`: field `Sa1Mapper::s_nmi_to_main`, the `0x2202` arm of the register write | ⚠️ open — inert today (no observable effect); the field is part of the save-state layout |
| 3 | CIWP/SIWP reset default is `0xFF` (allow-all) where ares **and** Mesen2 reset both to `0x00` (block-all) — a **deliberate** deviation chosen to keep one homebrew demo working; rationale below | `sa1.cpp:239` (`io.siwp = 0`), `io.cpp:112-113` (`io.ciwp = 0`); Mesen2 `Sa1Types.h` value-init + `Sa1::CpuRegisterWrite` `$2200` | `crates/luna-bus/src/sa1.rs`: `Sa1Mapper::new` (`siwp: 0xFF, ciwp: 0xFF`) | ⚠️ open — intentional, against both references |
| 4 | CCNT reset edge sets `CIWP = 0` (`io.cpp:113`) | `io.cpp:103-114` | `crates/luna-core/src/coproc/sa1.rs`: the `is_ccnt` branch of the chip-side register write (comment "ares io.cpp:113 also clears CIWP=0 here. luna does NOT") | ⚠️ open — **deferred**: verified absent, but it lives in the same deliberately-deviated I-RAM protection model as #3. Adding it broke an SA-1 I-RAM test (the synthetic handler doesn't pre-arm CIWP) and it is the GUI-blackout-prone area described below. Revisit with the protection model as a whole + GUI validation. |
| 20 | **HV-timer V wrap is fixed at 262 lines** — ares' `SA1::status.scanlines` follows the console's region, so on a PAL console the SA-1 HV timer wraps V at 312 | `sa1.cpp:63-94` (`SA1::step`; region-dependent `scanlines` — not re-read for this row) | `crates/luna-bus/src/sa1.rs`: field `Sa1Mapper::scanlines`, initialised `262` in `Sa1Mapper::new` (which takes no region) and never written again; read by `timer_step2` (`vcounter >= self.scanlines`) | ⚠️ open — PAL SA-1 carts only: an HV-mode timer IRQ on `vcnt` ≥ 262 never fires and the V counter runs 50 lines short per frame. NTSC is unaffected. |

### Row #3 — why the CIWP/SIWP default is `0xFF` (moved here from `archive/sa1_status.md`, 2026-10-04)

`$2229` SIWP and `$222A` CIWP are the per-page I-RAM write-protect masks
for the S-CPU and the SA-1 side. Both references reset them to `0x00`
(every page refuses writes until the game opens it). luna starts from
`0xFF` (every page writable).

The reference default was tried on 2026-05-27 and the opensnes
`sa1_starfield` demo went **black in luna-gui**: its `sa1_boot.asm` writes
`CIWP = $FF` (`$222A`) and never touches `$2229`, so it depends on an open
SIWP default — with `0x00` the main CPU's I-RAM seed is silently dropped.
The `0xFF` default was kept "until we hit a real cart that probes the reset
state". The same reasoning is written at the site, in the comment above
`siwp` / `ciwp` in `Sa1Mapper::new`.

What that leaves **open** — none of it resolved:

- The value was chosen to make one homebrew program work, against both
  references. Whether that program runs on a console, in ares or in Mesen2
  with the `0x00` default has **not** been established; if it does, the
  real divergence is elsewhere in luna's I-RAM write path (which side's
  mask gates which writer), not in the reset value.
- The failure was visible **only in the GUI**: the CLI smoke screenshot
  passed. Any change here needs GUI validation, not a CLI screenshot.
- Row #4 (CCNT reset edge) cannot be ported while this default stands.
- The masks themselves are tested (`siwp_page_mask_protects_iram_from_main`,
  `ciwp_protection_only_applies_to_sa1_writes`); only the reset state
  deviates.

---

## ✅ Verified correct (do not regress)

- **CC1 / CC2 `cdsel` logic** (the old "cdsel inversion" regression is
  fixed; the conversion formats themselves were ported later — rows #7 and
  #8, `dma_cc1` / `dma_cc2`): `cden=1,cdsel=1` → CC1 on the `$2236` DDA byte; `cden=1,
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
2. ~~#5 timer HV mode~~ — **done**.
3. ~~#6-#15 (the 2026-09-11 audit)~~ — **done** 2026-09-12 → 2026-09-14.
4. ⚠️ #2-#4 and #20 — open, see "⚠️ Open divergences". #3/#4 are the
   deliberate I-RAM protection deviation (rationale under row #3).
5. The scheduler grain (batched `step_coproc` vs ares' cothreads) is the
   remaining accuracy residual — a timing model, not a register.
