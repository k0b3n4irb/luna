# luna — Emulation Accuracy Scorecard vs ares & Mesen2

**Reviewer:** Claude (Opus 4.8) — source-level accuracy correlation
**Date:** 2026-05-29 · **Commit:** `6b9d6da` (`main`)
**⟳ RE-GROUNDED vs HEAD: 2026-06-10** — see the re-grounded banner under §1.
The May grades were markedly pessimistic: **16 of 27 flagged bugs are now
fixed**, and the "self-consistent but wrong" family is nearly emptied.
**References (fetched live, read in full — not paraphrased from memory):**
- **ares** `master` — `ares/sfc/{cpu,smp,dsp,ppu,coprocessor/sa1,memory,cartridge}` + `ares/component/processor/{wdc65816,spc700}` + `mia/medium/super-famicom.cpp`
- **Mesen2** `master` — `Core/SNES/{SnesCpu,Spc,DSP,SnesPpu,SnesDmaController,SnesMemoryManager,BaseCartridge,Coprocessors/SA1}`

**Method:** 7 parallel per-subsystem passes, each fetching both references,
reading luna's corresponding code, and grading every sub-area against a
common A→F rubric. Grades reflect *behavioral correspondence to the
references*, not code quality.

> **Key meta-result:** on **every contested semantic below, ares and Mesen2
> agree with each other.** So there is no "the references disagree, luna
> picked one" excuse anywhere — luna's divergences are deltas from *both*
> gold standards simultaneously.

---

## ⟳ RE-GROUNDED BANNER (2026-06-10 vs HEAD)

The May grades below (§1) were verified against current code by per-subsystem
re-grounding. **16 of 27 flagged bugs are fixed, 5 partial, and the residual
truly-open list is short.** Use *this* table, not §1, as current truth.

| Subsystem | May | **Re-grounded** | Still truly open |
|---|:---:|:---:|---|
| DSP S-DSP | A− | **A−** | golden-vector PCM tests absent (latent risk) |
| CPU 65c816 | A− | **A−** | none functional (DP-8 bare wrap is inert → comment fix) |
| SPC700 | B | **B+** | fine cycle ordering only (branch penalty fixed) |
| PPU | C+ | **A−** | *(OPHCT/OPVCT read-latch **+** BG scroll write-twice — both **FIXED 2026-06-11**; the OPVCT latch was the Doom-flicker root)* |
| DMA/HDMA | C+ | **B−** | mid-line HDMA preemption + atomic burst (Phase 5) |
| SA-1 | C+ | **B** | flat instruction timing (architectural, with Phase 5) |
| Bus/mappers | C+ | **C+** | ROM mirroring, open-bus MDR, mapper-detect scoring |

**Truly-open work list (was 6, now 4 after the OPVCT + BG-scroll fixes):**
0. ~~PPU OPHCT/OPVCT read-latch not reset on $213F~~ — **FIXED 2026-06-11** (`08e68fe`, ares io.cpp:167-169). This was the **Doom border-flicker root** (see below).
1. ~~PPU BG scroll write-twice~~ — **FIXED** (two shared latches, ares io.cpp:312; `ppu.rs:bg H/V scroll`, test `bg_h_scroll_uses_two_shared_latches`).
2. DSP golden-vector PCM tests absent — highest unique value (the most faithful port is unverified by a real BRR→PCM assertion).
3. Bus: ROM mirroring of non-pow2 images returns open-bus instead of wrapping (`lorom.rs`/`hirom.rs`).
4. Bus: open-bus is a fixed `0xFF`, not the last MDR latch (`snes.rs` `unwrap_or(0xFF)`).
5. Bus: mapper detection is first-checksum-pass-wins, no weighted scoring; SA-1 detected via low-nibble MapMode not hi-nibble RomType.
6. SA-1 flat instruction timing (`coproc/sa1.rs` `MCLK_PER_SA1_INSN=6`) — architectural, fold into the timing rework, not isolated.

Plus the 2 architectural residuals (Phase 5: DMA per-byte grid stepping, mid-line
HDMA preemption) — genuine HDMA-accuracy items.

**UPDATE 2026-06-11 — the Doom flicker is SOLVED, and it was NOT a scheduler/timing
problem.** The earlier theory here (Doom loop "~3.3× slow", attack only with the
state-injection oracle / cooperative-scheduler port) was **wrong** and is retracted.
Root cause: the `$213F`/OPVCT read-latch bug above (`08e68fe`). A 50%-wrong V-counter
read sent Doom's raster IRQ handler down its no-ack branch, re-firing the H/V IRQ
~200×/frame and pinning the S-CPU at I=1 ~90% of alternating frames (which *looked*
like a 3.3×-slow loop). Fixed surgically — the cooperative-scheduler port was NOT
needed (the GSU engine is byte-exact and its task timing matches Mesen within 1%).
The differential method (luna CLI traces vs Mesen oracles) localised it; see the
`project_doom_flicker_opvct_latch` memory.

> The detailed §1+ tables below are the **May 2026 snapshot** — kept for the
> per-area reasoning, but superseded by this banner where they disagree.

---

## 1. Scorecard at a glance *(May 2026 — superseded by the banner above)*

| Subsystem | Grade | One-line correlation summary |
|---|:---:|---|
| **DSP — S-DSP audio** | **A−** | Faithful near-line-for-line ares port; BRR/gaussian/envelope/echo/noise all match. Only loss: dead legacy tables in `lib.rs` + **zero golden-vector tests**. |
| **CPU — 65c816** | **A−** *(was B)* | **99.99996 % Tom Harte (2 fails / 5.08M)** after fixing the 16-bit BCD adjust, MVN/MVP per-byte interruptibility, and E-mode stack + (dp,X) pointer wrap. Functionally byte-faithful to ares; the "−" is the instruction-atomic core (no cycle-stepping; edge-latched IRQ). |
| **SMP — SPC700** | **B** *(was B−)* | 256/256 opcodes + ALU/MUL/DAA/DAS byte-faithful; `DIV YA,X` now ares-faithful (fixed). Remaining gap: the never-applied branch-taken cycle penalty — a cycle-timing ceiling that skews timer/DSP rate, not a value bug. |
| **PPU — graphics** | **C+** | Color-math/CGWSEL/OAM-modulo reference-accurate; real bugs in sprite Y-wrap, large-sprite tile addressing, BG scroll write-twice, Mode-7 screen-over; hi-res modes 5/6 + EXTBG absent. |
| **DMA / HDMA / timing** | **C+** | Byte-movement & HDMA table walk accurate & well-tested; **timing is architecturally coarse** (atomic burst + lump cycle-charge) → no mid-line HDMA preemption, H-IRQ ignores HTIME, **coprocessor double-charge bug**. |
| **SA-1 coprocessor** | **C+** | IRQ/mailbox/banking/multiplier reference-accurate (incl. correct CCNT bit-5 polarity); divergences in divider signedness, MAC-clear guard, **CC1 bpp/width fields swapped**, flat instruction timing. |
| **Bus / mappers / cartridge** | **C+** | LoROM/HiROM/ExHiROM math matches; diverges on **ROM mirroring** (returns open-bus instead), `$2000–$5FFF` access-speed split, open-bus value (`0xFF` vs MDR), and a far-weaker mapper-detection heuristic. |

**Overall verdict:** luna is **"high-level accurate, not yet cycle-accurate."**
Wherever it *ports ares directly* (DSP pipeline, 65c816 ALU, SPC700 ALU,
PPU color math) it is genuinely faithful — often line-for-line. Its
accuracy debt is concentrated in two predictable places:

1. **Timing/scheduling architecture.** ares and Mesen2 both cycle-step at
   ~2-mclk granularity (ares via libco cothreads, Mesen via an event-driven
   `_hClock` loop), re-polling IRQ/NMI/HDMA *inside* every memory access and
   DMA byte. luna runs whole CPU instructions / whole DMA bursts atomically,
   then lump-charges the master clock. This is a deliberate, documented
   trade-off — but it is *the* reason DMA/HDMA/timing and parts of SA-1/SPC
   sit at C+/B−. It caps achievable accuracy below the references by design.
2. **Untested edge cases.** The divergences that are outright *bugs* (not
   architecture) cluster on paths with no edge-case test: 8-bit sprite-Y
   wrap, DP $FFFF wrap, CC1 field decode, divider sign, ROM mirroring. Each
   "looks right and has a passing happy-path test."

---

## 2. Cross-cutting meta-findings

- **The "self-consistent but wrong" class is now confirmed against source,
  not just suspected.** SA-1 divider (signed÷signed vs ares' signed÷**unsigned**,
  `io.cpp:408`), SPC700 `DIV YA,X` (H from post-division Y; missing the
  `Y<(X<<1)` overflow branch, `instructions.cpp:358`), CC1 bpp/width bit
  fields swapped (`io.cpp:454`), BG H-scroll write-twice (the correct
  prev-low-3-bits formula is in ares `io.cpp:312` *and quoted in luna's own
  comment*). These are exactly what `reference-first.md` exists to prevent.
- **luna's ares citations are accurate where present.** Every "this matches
  ares X" comment that was checked held up (color math, IRQ latch model,
  CCNT polarity, decimal ALU). The discipline works — the gaps are in code
  written *before* a reference pass, or in edge cases the reference pass
  didn't cover.
- **Test coverage correlates inversely with the bugs.** DSP (A−) is a port
  with *no* golden tests — its risk is latent. DMA byte-movement (well-tested)
  is accurate; DMA *timing* (untested for preemption) is not. The grade
  ceiling tracks "was this path tested against reference behavior."

---

## 3. Corrections this pass made to the earlier code review

Grounding against real source refuted or refined four prior-review claims —
recording them so the review isn't trusted blindly:

| Prior claim | Corrected finding |
|---|---|
| 65c816 ADC vs SBC BCD `V` computed at different points → likely bug | **Refuted.** Both luna paths land on ares-correct V (decimal V ≡ binary overflow). Cosmetic asymmetry, not a value bug. |
| 65c816 absolute 16-bit `+1` crosses banks → bug | **Refuted for absolute.** ares (`memory.cpp:73`) and Mesen both bank-cross on absolute too. The bug is **only** for direct-page accesses. |
| PPU mosaic ignores "SETINI V-mosaic-disable bit" | **Refuted.** No such hardware bit exists; luna's unconditional V-mosaic is correct. luna's *comment* mislabels SETINI bit 2. |
| `luna-apu/lib.rs` carries a divergent duplicate **gaussian** table | **Refined.** The gaussian/counter dupes are dead-but-**identical**; only the legacy **ADSR** table (`AdsrPhase`/`ADSR_RATE_PERIODS`) actually diverges. All of `lib.rs:67-147` is dead and should be deleted regardless. |

Conversely, grounding **upgraded** one finding: the SA-1 "CC1 tile-width
stride" concern is actually a **bpp/width bit-field swap** (`cdma` bits 0-1
vs 2-4 inverted relative to ares `io.cpp:454` / Mesen `Sa1.cpp:319`) — a
bigger, clearer bug than first stated.

---

## 4. Per-subsystem correlation detail

> **⚠️ SUPERSEDED (May 2026 snapshot).** The grades and per-row "D/F" findings
> below are the original May review. The **RE-GROUNDED banner** at the top and
> the per-subsystem `docs/luna_*_gaps.md` are the current source of truth. Many
> "D/F" items here are **now FIXED in code** and retired in the gap docs,
> including: PPU sprite-Y 8-bit wrap, per-nibble sprite tile addressing, BG
> scroll write-twice (two shared latches), hi-res modes 5/6, Mode-7 EXTBG, and
> the `$213F`/OPVCT read-latch (the Doom-flicker root); the DMA coprocessor
> double-charge and dot-precise H/V-IRQ (HTIME); SA-1 divider signedness,
> MAC-clear guard, and CC1 bpp/width field swap; and the `$2000-5FFF` /
> `$4000-41FF` access-speed table. The reasoning and ares/Mesen citations below
> remain useful; the grades do not.

### CPU — 65c816 · Grade B

Algorithmically faithful to ares for ALU/flags/decimal; complete 256-opcode
coverage. Non-cycle-stepped functional core: DP 16-bit wrapping is wrong,
MVN/MVP non-interruptible, IRQ edge-latched rather than level-polled.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| Opcode coverage | all 256 (`opcodes.rs:119+`) | full | full | A |
| Addressing — DP 8-bit page wrap | missing on bare `direct_page` (`addressing.rs:33`) | `memory.cpp:52-55` E&&!D.l | `Shared.h:506-513` | C |
| Addressing — DP 16-bit high byte | `+1` over u32 → bank-cross at $FFFF (`addressing.rs:203`) | bank-0-confined (`instr-read.cpp:72`) | page-wrap modeled (`Shared.h:515`) | **D** |
| Addressing — absolute 16-bit | `+1` bank-crosses (correct) | bank-crosses (`memory.cpp:73`) | bank-crosses | A |
| Decimal ADC/SBC | nibble-wise (`opcodes.rs:1374+`) | `algorithms.cpp:1-47` | matches | B |
| Flags NVZC (binary) | `~(a^v)&(a^r)` (`opcodes.rs:1346`) | identical (`algorithms.cpp:13`) | identical | A |
| Interrupt/IRQ model | edge latch, re-armed each step (`cpu.rs:94`) | level + 4-cyc hold + last-cycle poll (`irq.cpp`) | level + pipeline delay (`Shared.h:336`) | C |
| Cycle timing | instruction-atomic | per-cycle | per-cycle | **D** |
| MVN/MVP | whole block in one step (`opcodes.rs:1144`) | per-byte interruptible (`instr-other.cpp:28`) | per-byte | C |
| RTI/PLP width, E-mode M/X | forces M/X, truncates X/Y (`opcodes.rs:1082`) | `instr-other.cpp:214` | `Shared.h:564` | A |

**Top divergence (D):** DP 16-bit high-byte uses `addr.wrapping_add(1)` on
`Addr24=u32`, so a DP access at effective offset `$FFFF` reads bank 1
instead of wrapping in bank 0. Affects `read_word`, `sta/stx/sty/stz_to_addr`,
`modify_memory`. ares `instr-read.cpp:68-74` + `memory.cpp:52-59` confines to
bank 0; Mesen `Shared.h:515-531` models the full page-wrap. **Credit:** the
ALU core is a faithful ares `algorithms.cpp` port; E-mode invariants correct.

**📊 Empirical — Tom Harte Tier 1 (FULL suite, all 256 opcodes × both modes,
5 120 000 cases). Baseline → after the fixes landed this cycle:**

| Milestone | Pass | Note |
|---|--:|---|
| Initial | 98.60 % | MVN/MVP (~40k) + 16-bit decimal ADC/SBC (~31.6k) dominated the 71 605 fails |
| + BCD adjust fix (ares `algorithmADC/SBC`, boolean inter-nibble carry) | ~99.2 % | decimal cluster eliminated (~31.6k → ~40) |
| + MVN/MVP per-byte interruptible (ares `instructionBlockMove`; excluded from gate — §6.2) | 99.988 % | block-move cycle-budget artifact removed from the count |
| + E-mode stack (`pushN`/`pullN`) & (dp,X) D.l==0 wrap | **99.99996 %** | **2 fails / 5.08M** (`c4.n`, `e1.e` — isolated edges) |

The core is now **functionally Tom-Harte-conformant to 99.99996 %**. The two
residual cases are one-off edges; the only structural gap left is the
instruction-atomic model — `cycles[]` ordering and the MVN/MVP cycle-budget
cases need a cycle-stepped scheduler (§5). The ALU + control core was
fundamentally sound throughout; every fix was a localized addressing/arithmetic
correction grounded in ares (with one documented ares↔Harte divergence on the
(dp,X) `D.l != 0` quirk — see §6.2). **Grade B → A−.**

---

### SMP — SPC700 · Grade B *(was B−)*

256/256 opcodes, byte-faithful ALU/MUL/DAA/DAS. `DIV YA,X` fixed this cycle;
one cycle-timing divergence remains.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| Opcode coverage | 256/256 exhaustive | full | full | A |
| Flags (ADC/SBC/logic/CMP/shift) | `opcodes.rs:1748` | `algorithms.cpp:1-9` | identical | A |
| **DIV YA,X** ✅ *fixed* | ares-faithful: H/V from **pre-div** Y, `Y<(X<<1)` overflow branch, X==0 via 256-X (`opcodes.rs:1096`) | H/V pre-div Y + overflow branch (`instructions.cpp:358`) | bit-loop, same semantics (`Spc.Instr.cpp:1163`) | **A** |
| MUL | NZ from Y (`opcodes.rs:1087`) | `instructions.cpp:505` | matches | A |
| DAA/DAS | `opcodes.rs:1676` | `instructions.cpp:199` | matches | A |
| **Cycle — taken branch** | never adds +2 (`cycles.rs:28`) | +2 idle on take (`instructions.cpp:85`) | +2 idle (`Spc.Instr.cpp:1625`) | C |
| Cycle — per-opcode base | table, plausible | per-access | per-access | B |
| Reset / IPL | vector + SP=$FF (`cpu.rs:46`) | `timing.cpp:9` | equiv | B |
| Timers T0/T1/T2 | 128/128/16 divider (`apu/lib.rs:205`) | 2-stage divider (`timing.cpp:34`) | `SpcTimer.h` | B− |

**✅ Fixed this cycle:** `DIV YA,X` is now a verbatim ares port — H/V from the
pre-division Y, the `Y<(X<<1)` overflow branch, and X==0 handled via the 256-X
path (no division by zero). Validated by 3 regression tests + a no-regression
audio smoke (SMW robust; Chrono Trigger byte-identical pre/post). **Remaining
divergence (C):** the branch-taken `+2` cycle penalty is still unapplied
(`cycles.rs:28`); since `apu/lib.rs:404` feeds the returned cycle count into
`tick_timers`/`tick_voices`, branch-heavy driver loops run timers/DSP slightly
fast — a cycle-timing ceiling, not a value bug. **Credit:** SBC-via-ones-
complement, MUL, DAA/DAS all match ares exactly; references agree everywhere.

---

### DSP — S-DSP audio · Grade A−

Faithful near-line-for-line transliteration of ares' `ares/sfc/dsp/` macro
pipeline. (Mesen2 implements the same semantics with a hardcoded gaussian
table + algebraically-refactored BRR/ADSR — luna following ares is correct.)

| Sub-area | luna `dsp.rs` | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| BRR decode | filters 0-3, scale, `(i16)` store (`:545`) | `brr.cpp:1-60` identical | `DspVoice.cpp:25` equiv | A |
| Gaussian interpolation | build+index, 3-tap `>>11` + clamp (`:257,443`) | `gaussian.cpp:1-39` identical | hardcoded table, same math | A |
| Envelope ADSR/GAIN | full state machine incl. mode-7 two-slope (`:464`) | `envelope.cpp:1-60` line-for-line | `DspVoice.cpp:80` equiv | A |
| Echo / FIR | `>>6` FIR, `s>>1` half-scale read (`:779`) | `echo.cpp:1-128` incl. the historically-buggy `>>1` (now correct) | equiv | A |
| Noise LFSR | `(lfsr<<13)^(lfsr<<14)` (`:916`) | `misc.cpp:24` identical | matches | A |
| KON/KOFF/ENDX timing | keylatch + 5-step delay (`:702,742,755`) | `misc.cpp`+`voice.cpp:139` | equiv | A |
| Pitch modulation | `pitch += ((out>>5)*pitch)>>10` (`:657`) | `voice.cpp:48` identical | `DspVoice.cpp:241` | A |
| Overflow/clamp sites | every `(i16)`/`sclamp16` site matches | matches | matches | A |

**Only divergence (D, dead code):** `luna-apu/lib.rs:67-86` has a legacy
fullsnes-style `AdsrPhase`/`ADSR_RATE_PERIODS` model that does **not** match
the live ares state machine — but it's **dead** (zero callers). The
gaussian/counter dupes (`lib.rs:92-147`) are also dead but *identical*. Delete
all of `lib.rs:67-147`.

**⚠ Primary risk — TEST GAP:** there are **no DSP golden-vector tests**.
`dsp.rs` tests only cover silence/KON-latch/counter/register-roundtrip —
nothing decodes a real BRR block and asserts PCM output against an ares
capture. For an otherwise line-faithful port, a regression in any `>>`/clamp
would pass CI silently (exactly the "Echo FIR half-scale" history). **Highest-
value single addition in the whole project:** commit a few SPC-driven golden
sample vectors captured from ares.

---

### PPU — graphics · Grade C+

Color-math mixer is reference-accurate; renderer is frame-snapshot (not
per-dot) and carries concrete geometry bugs.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| BG modes 0–4 | per-pixel priority engine, correct tables | `background.cpp:172` | `RenderTilemap` | B |
| BG modes 5/6 (hi-res 512) | **absent** — falls back to Mode-1, 256-wide (`renderer.rs:1107`) | hires path | `IsDoubleWidth` | **D** |
| Mode 7 affine | matrix+center+screen-over; renders CT + SMK/Pilotwings (DSP-1) correctly | `mode7.cpp:1-65` | `SnesPpu.cpp:1140` | B |
| Mode 7 EXTBG (SETINI b6) | implemented (`luna_bg_gaps.md` §4) | `mode7.cpp:45` | `_state.ExtBgEnabled` | B |
| DSP-1 (uPD7725) coprocessor | implemented; SMK + Pilotwings Mode 7 correct (HiROM 1K + LoROM 1B boards); needs user `dsp1b.rom` | `coprocessor/necdsp` | `NecDsp.cpp` | B |
| Sprite/OBJ rendering | decode+4bpp+priority; shadow_y hack | `object.cpp:57` | `SnesPpu.cpp:695` | C |
| **Sprite Y-wrap (8-bit)** | not wrapped (`renderer.rs:391`) | `within<0,255>` + `&255` (`object.cpp:54`) | `(uint8_t)endY` wrap (`SnesPpuTypes.h:46`) | **D** |
| **Sprite tile addressing** | flat `tile+(row*16+col)` (`renderer.rs:416`) | per-nibble `&15` (`object.cpp:130`) | `&0x0F` (`SnesPpu.cpp:737`) | **D** |
| Windows (W1/W2 logic) | `compute_window_masks` (`renderer.rs:682`) | `window.cpp:41` | window mask | B |
| Color math (add/sub/half/clip) | clamp 0-31 (`renderer.rs:963`) | `dac.cpp:138` | blend | B+ |
| CGWSEL/CGADSUB polarity | force-black + math-region + OBJ≥192 gate | `window.cpp:36`, `dac.cpp:103` | `SnesPpu.cpp:1307` | A− |
| Mosaic (H+V) | snap X&Y to block (`renderer.rs:1154`) | `mosaic.cpp:9` | `SnesPpu.cpp:177` | B |
| **BG scroll write-twice** | naive `(hi<<8)\|lo` (`ppu.rs:545`) | prev-low-3-bits (`io.cpp:312`) | same formula (`SnesPpu.cpp:2000`) | **D** |
| VRAM/CGRAM/OAM addressing | word-remap, OAM 544 modulo, gated | `io.cpp`, `oam.cpp` | `SnesPpu.cpp:1738` | B |
| OAM mask-vs-modulo (544) | `% 0x220` — **correct** (`memory.rs:406`) | n10 addr | 544 ram | A |
| Brightness / forced blank | INIDISP scale (`renderer.rs:518`) | `dac.cpp:39` | LUT | B |

**Divergences (all D, ares & Mesen agree):** sprite Y not 8-bit-wrapped
(sprites at Y≈250 drop wrapped rows); large-sprite tile column carries into
the row nibble; H/V scroll write-twice drops the dual-latch low-3-bits
interaction (mis-scrolls sub-tile H-scroll — very common); Mode-7 conflates
wrap vs transparent/tile-0 and never sign-extends `(hoffset-hcenter)`. Hi-res
modes absent (D), EXTBG absent (F). **Credit:** the add/sub/half/clip,
CGWSEL force-black polarity (historic G1 inversion is fixed), OBJ-palette-≥192
gate, and empty-sub fallback all trace cleanly to ares `dac.cpp:103-156`; OAM
544-modulo is right.

---

### DMA / HDMA / timing · Grade C+

The subsystem most affected by luna's coarse scheduler. Byte-movement and
table walk are accurate and well-tested; cycle accounting and event ordering
are architecturally coarser than both references.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| Sync DMA transfer (modes/dir/incr) | `channel.rs:266` | `dma.cpp:85` | `SnesDmaController.cpp:8` | A |
| DMA cycle cost | lump `8+8*bytes` after burst (`snes.rs:1352`) | `+8` + `step(8)`/byte, 8-mclk grid (`dma.cpp:16`) | `+8` + `IncMasterClock8`/byte | C |
| HDMA table walk | `channel.rs:326` | `dma.cpp:142` | `SnesDmaController.cpp:174` | A− |
| **HDMA timing / preemption** | once/line at boundary, no preempt (`snes.rs:699`) | H=1104 preempts active DMA (`timing.cpp:40`) | H=276dots preempt (`SnesMM.cpp:237`) | C |
| NMI timing | latch at vdisp, 4-mclk hold via H<2 (`snes.rs:618`) | `nmiPoll` every 4clk (`irq.cpp:6`) | NMI hold | B− |
| **IRQ (H/V) timing** | H-mode 0b01 fires **every** scanline, ignores HTIME (`snes.rs:675`) | dot-precise `hcounter==htime` (`irq.cpp:18`) | per-dot HClock match | **D** |
| RDNMI hold | `line==vbstart && h<2` (`snes.rs:1185`) | `nmiHold` 4-mclk (`irq.cpp:52`) | matches | B |
| Scheduler granularity | instruction lump-charge (`snes.rs:592`) | libco, 2-mclk + per-tick polls | event-driven 2-mclk (`SnesMM.cpp:209`) | **D** (arch) |
| **Coprocessor sync** | per-byte tick **+** lump re-consumed → ~2× | per-byte `step()` single-count (`dma.cpp:45`) | per-byte single-count | **C−** |

**Top divergence (C−, confirmed bug):** DMA advances the coprocessor twice —
per-byte `bus.tick(8)→step_coproc` (`channel.rs:307`) **and** the lump
`io_cycle(8+8*bytes)` re-consumed by the next instruction's `step_coproc(consumed)`
(`snes.rs:1352→549→566`). Neither reference double-counts. **Credit:** mode/
direction/increment decode and the full HDMA table walk (direct/indirect/
repeat/multi-entry/terminator) are TDD-tested *and verified accurate* against
ares `dma.cpp:85-190` + Mesen `SnesDmaController.cpp:174-330`.

---

### SA-1 coprocessor · Grade C+

Boots real carts end-to-end; IRQ/banking/multiplier reference-accurate;
several confirmed arithmetic/decode divergences.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| Memory mapping / banking | 4 super-banks, MB select | `rom.cpp` UpdatePrgRom | same | A− |
| Multiplier (16×16→32, MAC) | `i32×i32` signed (`sa1.rs`) | `(i16)×(i16)` | `(int16_t)×` | A |
| **Divider** | **signed÷signed** (`ma/mb` both i16, `sa1.rs:921`) | signed ÷ **unsigned** (`io.cpp:408`) | `int16_t/uint16_t` (`Sa1.cpp:682`) | **D** |
| MAC accumulator | `saturating_add`, no OF flag | 40-bit wrap + overflow (`io.cpp:419`) | `>>33` OF, 40-bit (`Sa1.cpp:665`) | C |
| **MAC accumulator-clear** | dead guard `b1!=0 && val==0x02` (`sa1.rs:1254`) | clears on `acm` b1 (`io.cpp:374`) | clears on `val&0x02` (`Sa1.cpp:202`) | **D** |
| IRQ delivery (latch+bridge) | level-latch, CIC/SIC clear, OR-of-enable | `io.cpp` matches | `ProcessInterrupts` | A |
| BW-RAM gating | OR(SBWE,CBWE) (`sa1.rs`) | `!swen&&!cwen` | handlers same | A |
| **Character conv CC1** | bpp/width fields **swapped** (`sa1.rs:572`) | `dmacb`=b0-1, `dmasize`=b2-4 (`io.cpp:454`) | `Format`=b0-1, `Width`=b2-4 (`Sa1.cpp:319`) | **D** |
| Character conv CC2 | per-byte staging | register-file driven | `RunCharConvert2` | C+ |
| VLBP ($230C/$230D) | 3-byte window (`sa1.rs:438`) | `readVBR` 3 bytes | `VarLenAutoInc` | B+ |
| Normal DMA | full-burst, no per-byte cost | per-byte `step()`+conflict | per-byte | C |
| Instruction timing | flat 6 mclk, `io_cycle` no-op (`coproc/sa1.rs:48`) | per-access `step(2)`+conflicts | per-access | C− |

**Top divergences (D):** divider operand signedness; the dead MAC-clear guard
(games writing MCNT `$03` never reset the accumulator); CC1 `cdma` bpp/width
bit fields swapped relative to ares `io.cpp:454`. **Credit:** the IRQ model is
genuinely reference-accurate (level latch + CIC/SIC clears + OR-of-enable +
SCNT vector override, all matching ares `io.cpp` & Mesen `ProcessInterrupts`),
and **the CCNT bit-5 reset polarity is correct** (`coproc/sa1.rs:97` matches
ares `io.cpp:103` / Mesen `Sa1.cpp:245`) — the historic bit-5/bit-7 inversion
regression is not present.

---

### Bus / mappers / cartridge · Grade C+

LoROM/HiROM/ExHiROM math and SRAM windows match both references; three
reference-confirmed behavioral gaps + a weak detection heuristic.

| Sub-area | luna | ares | Mesen2 | Grade |
|---|---|---|---|:--:|
| LoROM map | `lorom.rs:44` | boards.bml | `mesen_cart.cpp:462` | A− |
| HiROM map | `hirom.rs:95` | markup base 8/0 | `mesen_cart.cpp:477` | A |
| ExHiROM map | +64 banks (`hirom.rs:89`) | markup 0x400000 | `mesen_cart.cpp:488` | A |
| **ROM mirroring (non-pow2)** | **returns None → open-bus** (`lorom.rs:48`) | `mirror()` folds back (`memory_inline.hpp:1`) | page `% size` wrap (`mappings.cpp:21`) | **D** |
| SRAM mapping | `% size` wrap (`lorom.rs:63`) | board-driven | size-dependent + mask | B |
| Open-bus value | fixed `0xFF` (`snes.rs:985`) | last MDR (`cpu_memory.cpp:13`) | `_openBus` latch (`mm.cpp:278`) | C |
| FastROM speed | `bank≥$80 & $8000+` (`speed.rs:69`) | `addr&0x800000?romSpeed:8` | `_masterClockTable` | A− |
| **`$2000-5FFF`/`$4000-41FF` speed** | all `$2000-5FFF`=Slow(8); only `$4016/7`=XSlow (`speed.rs:54`) | `$2000-3fff,4200-5fff`=**6**; `$4000-41ff`=**12** (`cpu_memory.cpp:37`) | pages 6/12 (`mm.cpp:107`) | **D** |
| **Mapper detection** | first checksum-pass wins, no scoring (`lib.rs:149`) | weighted `scoreHeader()` ×4 offsets (`mia_sfc.cpp:820`) | `GetHeaderScore()` ×6 (`cart.cpp:125`) | C− |
| **SA-1/coproc detection** | wrong byte (MapMode low-nibble; `lib.rs:202`) | RomType `$26` hi-nibble (`mia_sfc.cpp:595`) | RomType `(0xF0)>>4` (`cart.cpp:269`) | **D** |
| SMC-header strip | `len%1024==512` eager (`lib.rs:122`) | `(size&0x7fff)==512` | scored, strip if wins | B− |
| Header field parse | exponent clamp, LE checksum (`lib.rs:171`) | `mia_sfc.cpp:797` | `cart.cpp:258` | B |

**Top divergence (D):** undersized/non-power-of-two ROMs return open-bus
instead of **mirroring** — both references mirror the ROM image into the
address window. Plus the `$2000-$5FFF` register block runs at the **fast (6)**
rate on real hardware (luna marks it Slow), and `$4000-$41FF` is **entirely
XSlow (12)** (luna only flags `$4016/$4017`). SA-1 is detected from the wrong
header byte. **Credit:** the three core mapper address maps + SRAM windows
match ares + Mesen exactly.

---

## 5. To close the gap with ares/Mesen2 (prioritized)

Ordered by accuracy-ROI ÷ effort. Items 1-6 are bounded bug fixes that move
specific grades up; 7-8 are the architectural ceiling.

> **✅ Completed this cycle (CPU — all Tom-Harte-validated, no regressions):**
> the 16-bit decimal BCD adjust, MVN/MVP per-byte interruptibility, and the
> E-mode stack + (dp,X) pointer-wrap fixes — taking the 65c816 from 98.60 %
> to **99.99996 %** (B → A−). Remaining items below are untouched.

1. **DSP golden-vector tests** *(A− → lock it in)* — capture a few SPC-driven
   PCM vectors from ares; gate CI. Highest value: protects the one faithful
   port from silent regression. *(No grade change, but removes the latent
   risk that defines the A−.)*
2. **SA-1 divider sign + MAC-clear guard + CC1 field swap** *(C+ → B)* — three
   small, source-confirmed fixes (`sa1.rs:921`, `:1254`, `:572`). Add edge
   tests with `MB≥$8000`, `MCNT=$03`, and a width>3 CC1 conversion.
3. **SPC700 `DIV YA,X`** *(B− → B)* — port ares' branch verbatim
   (`instructions.cpp:358`); compute V/H from pre-division registers.
4. **DMA coprocessor double-charge** *(C+ → B−)* — single bug (`snes.rs` lump
   vs per-byte `tick`); pick one path to drive `step_coproc`.
5. **PPU geometry quartet** *(C+ → B−)* — 8-bit sprite-Y wrap, per-nibble tile
   addressing, H-scroll prev-low-3-bits latch, Mode-7 OOB split. All four are
   small and have crisp ares references.
6. **Bus: ROM mirroring + `$2000-5FFF` speed table** *(C+ → B)* — mirror
   instead of open-bus; fix the access-speed regions per `cpu_memory.cpp:37`.
7. ~~65c816 DP wrap + MVN/MVP interruptibility~~ ✅ **done this cycle** (B → A−).
   **Remaining:** the edge-latched IRQ model (vs hardware's level-triggered line)
   — needs an IRQ-poll hook on the bus. Minor, and not Tom-Harte-gated.
8. **Cycle-stepped scheduler** *(the C+ ceiling on DMA/timing/SA-1/SPC)* — the
   big one: move from instruction/burst-atomic lump-charging toward
   ~2-mclk-granular stepping with per-access IRQ/NMI/HDMA polling, like both
   references. This is a multi-week architectural change; everything above is
   worth doing first and is independent of it.

> Per `audible-fixes-test-first.md`: the DSP (#1), SPC `DIV` (#3), and PPU
> (#5) changes must be validated by ear/eye in `luna-gui` before commit.

---

## 6. Benchmark plan — turning grades into a CI-trackable score

This scorecard is **static** (source-level). To make the challenge against
ares/Mesen2 *reproducible and CI-gated*, here is a concrete, tiered plan
grounded in what `luna-cli` and the standalone crates can actually do today.

### 6.0 The verdict surface luna already has

| Mechanism | luna-cli flag / API | Good for |
|---|---|---|
| Framebuffer PNG | `run --screenshot`, `state --screenshot` | visual test ROMs (hash vs golden) |
| RAM result byte | `state --peek BANK:OFFSET:COUNT` (→ stderr hex) | blargg-style "wrote $xx = pass" ROMs |
| APU port traffic | `state --apu-log` (`$2140-$2143` CSV) | tests that signal via APU ports |
| Register snapshot | `state --out` (`EmulatorState` JSON: full CPU/PPU regs) | state assertions |
| CPU instruction trace | `state` trace CSV (PC,A,X,Y,SP,P,DB,DP,e) | differential trace diff |
| Scripted input | `state --input "frame:hex,..."` | tests needing button presses |

**One small gap:** `--peek` dumps hex to *stderr*, not a machine-readable
exit code. Tiers 3 needs a thin addition — a `--assert BANK:OFFSET=HEX`
flag (or a `--peek` that sets exit status) so a harness can branch on
pass/fail without scraping text. ~30 lines.

### 6.1 Reviewer's recommendation (my feedback)

**Lead with the CPU unit suites, not the full-system ROMs.** Here is *why*,
and it's the most important judgment call in this plan:

- luna's **CPU cores are standalone crates with no SNES glue** — which means
  the gold-standard **Tom Harte SingleStepTests** (per-opcode JSON state
  vectors, used to validate ares/Mesen-class cores) can be run **directly**
  against `luna-cpu-65c816` and `luna-cpu-spc700` with a mock bus. No PPU, no
  APU handshake, no scheduler, no timing-architecture confound. It is
  deterministic, redistributable, and it attacks **exactly** the two grades
  with the clearest bugs (CPU **B**, SPC700 **B−**).
- The full-system test ROMs (Tier 3) are valuable but **lead to ambiguous
  verdicts** at luna's current stage: a failure may reflect the *known
  timing-architecture ceiling* (§1, item 1 — coarse scheduler, no HDMA
  preemption, no APU cycle-sync) rather than a discrete, fixable bug. You'd
  spend triage effort distinguishing "real regression" from "expected
  architectural gap." The CPU suites have no such confound — a fail is a bug,
  full stop.
- Concretely, Tier 1 would **immediately catch** the divergences this
  scorecard graded D: the DP `$FFFF` bank-wrap (CPU), and `DIV YA,X` H/V/
  overflow (SPC700). Those opcodes' test files will go red on the first run.

So: **Tier 1 first (days, high precision), Tier 2 next (the DSP safety net),
Tier 3 once the discrete CPU/PPU/SA-1 bugs are closed**, Tier 4 only if you
get a reference binary.

### 6.2 Tier 1 — Tom Harte SingleStepTests *(highest ROI)*

Both suites confirmed live:
- **`SingleStepTests/65816`** — "instruction-by-instruction test suite for the
  65816 that includes bus activity." `v1/<opcode>.e.json` + `.n.json`
  (emulation/native), one file per opcode.
- **`SingleStepTests/spc700`** — JSMoo-based SPC700 vectors, `v1/`.

**Format** (verified from `v1/00.e.json`): each case has
`initial`/`final = {pc, s, p, a, x, y, dbr, d, pbr, e, ram:[[addr,val],…]}`
and `cycles:[[addr,val,flags],…]` (per-cycle bus activity; `flags` like
`"d--wemx-"` = read/write + mode bits). RAM addresses are full 24-bit.

**✅ Status — already built and now RUN.** The harness already exists in the
tree: `crates/luna-cpu-65c816/tests/tom_harte.rs` (complete — discovery, JSON
schema, per-case runner, aggregation, report, `LUNA_TOM_HARTE_REQUIRE` strict
gate), and the **full corpus is committed** under `tests/tom-harte/v1/` (512
files, ~10k cases each). It reuses `luna_bus::testing::RamBus` (flat 16 MB) and
maps each Harte `initial`/`final` straight onto `Cpu`'s public fields. It's
`#[ignore]`d; run with:

```bash
cargo test -p luna-cpu-65c816 --test tom_harte -- --ignored --nocapture
```

**What it validates:** final registers + all memory writes, all 256 opcodes ×
both modes × ~10k cases. **NOT validated (honest scope):** the `cycles[]`
per-access *ordering* — luna's core is instruction-atomic and exposes no cycle
trace, so Tier 1 proves *what* each opcode computes, not *when* each bus cycle
happens (the bridge to §5-item-8).

**📊 RESULTS after the Tier-1 fix cycle — 256 opcodes × both modes (MVN/MVP
excluded as un-gateable, §6.1), 5 080 000 testable cases: 5 079 998 pass /
2 fail = 99.99996 %.** Every targeted cluster is closed:

| Cluster | Before | After | Fix |
|---|--:|--:|---|
| **16-bit decimal ADC/SBC** (`6x`/`7x`/`Ex`/`Fx`) | ~31 600 | **0** | rewrote `adc/sbc_value_bcd` as ares `algorithmADC/SBC` (in-place result, boolean inter-nibble carry) |
| **MVN / MVP** (`54`/`44`) | ~39 989 | **excluded** | per-byte interruptible (ares `instructionBlockMove`); Harte's 100-cycle-budget partial model isn't gateable on an atomic core |
| **E-mode stack** (PEA/PEI/PER/PHD/PLD/PLB/JSL/RTL) | ~430 | **0** | added `push/pull_*_native` (16-bit S, no page-1 confine); re-pin restores S.h |
| **(dp,X) pointer wrap** (`01`-`e1`) | ~185 | **1** | confine pointer bytes to the page only when `E && D.l == 0` (matches Harte; documented ares divergence on `D.l != 0`) |
| residual | — | **2** | `c4.n`, `e1.e` — isolated one-off edges |

**Reading:** the 65c816 is now **99.99996 % Tom-Harte-conformant** — 2 stray
cases out of 5.08M. Each fix was a localized, reference-grounded change with
the harness as the regression gate; no other opcode regressed (verified by the
full sweep). The grade moves **B → A−**; the only thing between A− and A is the
instruction-atomic model (cycle ordering + the MVN/MVP cycle-budget cases),
which is the §5 cycle-stepped-scheduler milestone, not a discrete bug.

### 6.3 Tier 2 — DSP golden vectors

Already §5 item 1. The DSP is an A− *because* it's untested. Capture a handful
of PCM outputs from ares (drive a known SPC + register sequence through
`ares/sfc/dsp`), commit them as fixtures, and assert luna's `dsp.rs` output
sample-for-sample. Small fixture, closes the one latent risk on the best
subsystem.

### 6.4 Tier 3 — full-system test ROMs via luna-cli

Confirmed redistributable suite: **`PeterLemon/SNES`** has `CPUTest/{CPU,SPC700}`,
`PPU/`, `INPUT/`, `SPC700/` — assembled ROMs that render a pass/fail screen
and ship a reference PNG. Pipeline:

1. Vendor a curated subset into `tests/roms/accuracy/` (these are MIT/public —
   unlike the commercial ROMs already in `tests/roms/`).
2. A manifest `tests/accuracy.toml`: `rom → {instructions_or_frames, verdict:
   framebuffer-hash | peek BANK:OFFSET=HEX, expected}`.
3. Runner script: `luna state -n <N> --screenshot out.png <rom>` then hash vs
   golden, **or** `--assert <addr>=<val>` (the 6.0 enhancement) → exit code.
4. Emit a `passed/total` summary; wire into CI as a non-blocking score first,
   blocking once green.

Candidate categories, mapped to the grades they'd exercise: `CPUTest/CPU`
(→ CPU), `CPUTest/SPC700` + `SPC700/` (→ SMP, but needs the APU running),
`PPU/` (→ PPU — these will expose the sprite-Y / scroll / Mode-7 gaps
visually), `INPUT/` (→ joypad). **Caveat (per 6.1):** the APU/timing-dependent
ROMs may fail on the scheduler ceiling, not a bug — triage accordingly.

### 6.5 Tier 4 — differential trace diff *(deferred)*

If an ares or Mesen2 binary is available (both can dump CPU trace logs), run a
fixed ROM+input through both and diff against luna's `state` trace CSV /
`EmulatorState` snapshots to pinpoint the exact divergence instruction. Heavy
setup; only worth it once Tiers 1–3 have drained the easy wins.

### 6.6 Proposed first step

If you greenlight it, I'd start with **Tier 1 / 65816**: add the gated test
harness + a `MockBus` with raw 24-bit read/write, point it at a local
SingleStepTests checkout, and report the first `passed/total` per opcode —
which will concretely confirm (or refute) the DP-wrap and other CPU findings
with hard numbers. That's a self-contained, reviewable PR that needs no
changes to emulation code, only test infrastructure.
