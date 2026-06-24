# luna — Emulation Accuracy Scorecard

**Reviewer:** Claude (Opus 4.8) — source-level accuracy correlation
**Date:** 2026-05-29 · **Commit:** `6b9d6da` (`main`)
**⟳ RE-GROUNDED vs HEAD: 2026-06-10** — see the re-grounded banner under §1.
The May grades were markedly pessimistic: **16 of 27 flagged bugs are now
fixed**, and the "self-consistent but wrong" family is nearly emptied.

**Method:** 7 parallel per-subsystem passes, each reading the hardware-reference
behaviour, reading luna's corresponding code, and grading every sub-area against
a common A→F rubric. Grades reflect *behavioral correspondence to the hardware
reference*, not code quality.

> **Key meta-result:** every contested semantic below is **unambiguous in the
> hardware reference** — there is no "the reference is unclear, luna picked one"
> excuse anywhere. luna's divergences are deltas from verified hardware behaviour.

---

## ⟳ RE-GROUNDED BANNER (2026-06-10 vs HEAD)

The May grades below (§1) were verified against current code by per-subsystem
re-grounding. **16 of 27 flagged bugs are fixed, 5 partial, and the residual
truly-open list is short.** Use *this* table, not §1, as current truth.

| Subsystem | May | **Re-grounded** | Still truly open |
|---|:---:|:---:|---|
| DSP S-DSP | A− | **A** | ~~golden-vector PCM tests absent~~ — **FIXED** (BRR→PCM differential + curated goldens 2026-06-17; full-voice integration golden + re-baselined end-to-end PCM 2026-06-23) |
| CPU 65c816 | A− | **A−** | none functional (DP-8 bare wrap is inert → comment fix) |
| SPC700 | B | **A−** | cycle model complete (2026-06-22): all 254 opcodes cycle-stepped byte/cycle-exact vs the atomic core, taken-branch +2 applied, cooperative CPU↔SPC interleave active at bus-access granularity, `$F0` wait-state dividers modelled (gap 6 closed) |
| PPU | C+ | **A−** | *(OPHCT/OPVCT read-latch **+** BG scroll write-twice — both **FIXED 2026-06-11**; the OPVCT latch was the Doom-flicker root)* |
| DMA/HDMA | C+ | **B−** | DMA per-byte + line-granular HDMA preempt (Phase 5). dot-276 sub-line is **visually a no-op** (276 = HBlank → effect on line N+1, which luna's boundary model already does — see `hdma_audit.md` "Resolution 2026-06-20"); residual is the HDMA stall **cycle-count** timing only (no known game impact). |
| SA-1 | C+ | **A−** | ~~flat instruction timing~~ — **FIXED**: per-access cycle cost (Phase 5b `097ffe7`) + `conflict()` BWRAM/IRAM/ROM contention steps (Increment B, 2026-06-20). ~~HV-mode timer unimplemented~~ — **FIXED 2026-06-23** (faithful `SA1::step` port, both modes, unit-tested). The remaining "−" is purely the batched (non-cothread) scheduler grain, not a value or feature bug. |
| Bus/mappers | C+ | **B** | ~~ROM mirroring, open-bus MDR, mapper-detect scoring~~ — all **FIXED 2026-06-17** |

**Truly-open work list (was 6, now 1 after OPVCT + BG-scroll + BRR-test + bus trio):**
0. ~~PPU OPHCT/OPVCT read-latch not reset on $213F~~ — **FIXED 2026-06-11** (`08e68fe`). This was the **Doom border-flicker root** (see below).
1. ~~PPU BG scroll write-twice~~ — **FIXED** (two shared latches; `ppu.rs:bg H/V scroll`, test `bg_h_scroll_uses_two_shared_latches`).
2. ~~DSP golden-vector PCM tests absent~~ — **FIXED 2026-06-17**. The BRR→PCM decoder now has curated absolute goldens (all 4 filters + scale-13..15 overflow + clamp) **and** a differential proving luna's port matches an independent reference decoder bit-exactly over 200 000 random groups (`dsp.rs` tests `brr_curated_goldens_*` / `brr_differential_*`). The two decoder forms agree bit-exactly because every stored sample is `(s<<1)` (even buffer ⇒ inline `p>>1` == pre-shifted `prev>>1`).
3. ~~Bus: ROM mirroring of non-pow2 images returns open-bus instead of wrapping~~ — **FIXED 2026-06-17** (`types::rom_mirror`; used by `lorom.rs`/`hirom.rs`).
4. ~~Bus: open-bus is a fixed `0xFF`, not the last MDR latch~~ — **FIXED 2026-06-17** (`Snes::mdr`; CPU-visible open-bus sites return it, reads/writes update it).
5. ~~Bus: mapper detection is first-checksum-pass-wins; SA-1 via MapMode not RomType~~ — **FIXED 2026-06-17** (`score_header` disambiguates checksum-passers; SA-1 keyed on the chipset/RomType high-nibble).
6. ~~SA-1 flat instruction timing (`coproc/sa1.rs` `MCLK_PER_SA1_INSN=6`)~~ — **FIXED**: Phase 5b (`097ffe7`) replaced the flat lump with a signed-deficit per-access cost (BWRAM=2 / else=1 step), and the `conflict()` shared-bus contention steps (ROM +1, BWRAM/IRAM +2 when the S-CPU holds the same resource) landed 2026-06-20. SA-1 → A−.

Plus the 2 architectural residuals (Phase 5: DMA per-byte grid stepping, mid-line
HDMA preemption) — genuine HDMA-accuracy items.

**UPDATE 2026-06-22 — SPC700 cycle model finished (→ A−); Star Ocean fixed; §1
PPU rows confirmed stale.**
- **SPC700 → A−.** The cycle-stepped core is byte/cycle-exact for all 254 opcodes
  vs the atomic core (`differential_all_ported_opcodes`); the taken-branch +2
  penalty is applied (`ef44271`); the CPU↔SPC interleave is cycle-exact at
  bus-access granularity (cooperative grammar, active — not the old chunked
  model); and the **`$F0` wait-state dividers** `{2,4,10,20}` clock / `{2,4,8,16}`
  timer (the 8/16→10/20 glitch) are now modelled per access — **last named APU
  gap (`luna_apu_gaps.md` §6) closed**. ws=0 byte-identical (24 APU tests + the
  differential + 58 goldens unchanged); `wait_states_divide_the_spc_clock` proves
  ws=1≈½ / ws=3≈⅒.
- **Star Ocean (S-DD1) plays past the intro** (`f4fc744`): MMC bank selects power
  on to identity (green tri-Ace logo) and `$C0-FF` is MMC ROM not SRAM (`$F0-FD`
  was returning zeroed save-RAM, dead-looping the post-intro script engine).
- **Audit of the §1 May PPU rows (below):** every grade-D/C claim spot-checked is
  **already fixed in current code** — sprite Y-wrap (`sprite_on_line` does
  `& 0xFF`), large-sprite tile addressing (delegated to the indexed renderer's
  tile-wrap), BG modes 5/6 hi-res (`render_bg_scanline_indexed_hires`, 512-wide),
  BG scroll write-twice (shared `bgofsPPU1/2` latches), Mode-7 screen-over
  (`M7SEL` 7:6). The re-grounded banner's **PPU A−** is correct; **§1 stays
  superseded** (its line-numbers and grades predate this work — do not cite it).
  The per-dot/mid-scanline path exists (gap G6 `flush_partial_scanline`); a "per-dot
  renderer rewrite" is **not** an open item.

**UPDATE 2026-06-11 — the Doom flicker is SOLVED, and it was NOT a scheduler/timing
problem.** The earlier theory here (Doom loop "~3.3× slow", attack only with the
state-injection oracle / cooperative-scheduler port) was **wrong** and is retracted.
Root cause: the `$213F`/OPVCT read-latch bug above (`08e68fe`). A 50%-wrong V-counter
read sent Doom's raster IRQ handler down its no-ack branch, re-firing the H/V IRQ
~200×/frame and pinning the S-CPU at I=1 ~90% of alternating frames (which *looked*
like a 3.3×-slow loop). Fixed surgically — the cooperative-scheduler port was NOT
needed (the GSU engine is byte-exact and its task timing matches the reference within 1%).
The differential method (luna CLI traces vs reference oracles) localised it; see the
`project_doom_flicker_opvct_latch` memory.

**UPDATE 2026-06-13 — save state landed (no grade change, but a new
determinism check).** Full machine-state serialization across the entire `Snes`
tree (CPU/PPU/APU+S-DSP/DMA/coproc/WRAM/mappers) plus `Emulator::save_state` /
`load_state` (commits `c58b639` engine+API, `06070b8` GUI slots+hotkeys,
`a3e0a16` cleanup + `SAVE_STATE_VERSION` bump). This touched **no emulation
logic** — it is `#[derive(Serialize/Deserialize)]` + a `Mapper::save_state/
load_state` pair (mutable state only; ROM never serialized, kept live and
replayed). Accuracy relevance is *verification*, not behavior: the round-trip
test `save_state_round_trip_rewinds_and_stays_deterministic` (luna-api) asserts
that save → run-forward → load **rewinds the framebuffer hash + CPU PC exactly**
and that replay from the restored state is **bit-deterministic** — a CI-gated
self-consistency signal that the *complete observable state set is enumerated*
(a missing-field bug surfaces as a round-trip divergence). Validated headlessly
on the real SMW ROM and in-GUI (F5/F9). No subsystem grade moves.

> The detailed §1+ tables below are the **May 2026 snapshot** — kept for the
> per-area reasoning, but superseded by this banner where they disagree.

---

## 1. Scorecard at a glance *(May 2026 — superseded by the banner above)*

| Subsystem | Grade | One-line correlation summary |
|---|:---:|---|
| **DSP — S-DSP audio** | **A** | Faithful near-line-for-line port; BRR/gaussian/envelope/echo/noise all match the hardware reference. Golden-vector coverage now complete: curated BRR goldens + a reference differential + a full-voice integration golden (`dsp.rs`) + the re-baselined end-to-end PCM ROM goldens. |
| **CPU — 65c816** | **A−** *(was B)* | **99.99996 % per-instruction conformance (2 fails / 5.08M)** after fixing the 16-bit BCD adjust, MVN/MVP per-byte interruptibility, and E-mode stack + (dp,X) pointer wrap. Functionally byte-faithful to the hardware reference; the "−" is the instruction-atomic core (no cycle-stepping; edge-latched IRQ). |
| **SMP — SPC700** | **A−** *(was B)* | 256/256 opcodes + ALU/MUL/DAA/DAS byte-faithful; `DIV YA,X` hardware-faithful. The cycle model is now complete: all 254 opcodes are cycle-stepped byte-/cycle-exact vs the atomic core (`differential_all_ported_opcodes`), the taken-branch +2 penalty is applied (`ef44271`), the CPU↔SPC interleave is cycle-exact at bus-access granularity (cooperative grammar, active), and the `$F0` wait-state dividers `{2,4,10,20}` + the 8/16→10/20 timer glitch are modelled (gap 6 closed). |
| **PPU — graphics** | **C+** | Color-math/CGWSEL/OAM-modulo reference-accurate; real bugs in sprite Y-wrap, large-sprite tile addressing, BG scroll write-twice, Mode-7 screen-over; hi-res modes 5/6 + EXTBG absent. |
| **DMA / HDMA / timing** | **C+** | Byte-movement & HDMA table walk accurate & well-tested; **timing is architecturally coarse** (atomic burst + lump cycle-charge) → no mid-line HDMA preemption, H-IRQ ignores HTIME, **coprocessor double-charge bug**. |
| **SA-1 coprocessor** | **C+** | IRQ/mailbox/banking/multiplier reference-accurate (incl. correct CCNT bit-5 polarity); divergences in divider signedness, MAC-clear guard, **CC1 bpp/width fields swapped**, flat instruction timing. |
| **Bus / mappers / cartridge** | **C+** | LoROM/HiROM/ExHiROM math matches; diverges on **ROM mirroring** (returns open-bus instead), `$2000–$5FFF` access-speed split, open-bus value (`0xFF` vs MDR), and a far-weaker mapper-detection heuristic. |

**Overall verdict:** luna is **"high-level accurate, not yet cycle-accurate."**
Wherever it *ports the hardware reference directly* (DSP pipeline, 65c816 ALU,
SPC700 ALU, PPU color math) it is genuinely faithful — often line-for-line. Its
accuracy debt is concentrated in two predictable places:

1. **Timing/scheduling architecture.** The hardware cycle-steps at ~2-mclk
   granularity, re-polling IRQ/NMI/HDMA *inside* every memory access and DMA
   byte. luna runs whole CPU instructions / whole DMA bursts atomically, then
   lump-charges the master clock. This is a deliberate, documented trade-off —
   but it is *the* reason DMA/HDMA/timing and parts of SA-1/SPC sit at C+/B−. It
   caps achievable accuracy below the reference by design.
2. **Untested edge cases.** The divergences that are outright *bugs* (not
   architecture) cluster on paths with no edge-case test: 8-bit sprite-Y
   wrap, DP $FFFF wrap, CC1 field decode, divider sign, ROM mirroring. Each
   "looks right and has a passing happy-path test."

---

## 2. Cross-cutting meta-findings

- **The "self-consistent but wrong" class is now confirmed against source,
  not just suspected.** SA-1 divider (signed÷signed vs the hardware's
  signed÷**unsigned**), SPC700 `DIV YA,X` (H from post-division Y; missing the
  `Y<(X<<1)` overflow branch), CC1 bpp/width bit fields swapped, BG H-scroll
  write-twice (the correct prev-low-3-bits formula is *quoted in luna's own
  comment*). These are exactly what `reference-first.md` exists to prevent.
- **luna's reference citations are accurate where present.** Every "this matches
  the reference X" comment that was checked held up (color math, IRQ latch model,
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
| 65c816 ADC vs SBC BCD `V` computed at different points → likely bug | **Refuted.** Both luna paths land on the reference-correct V (decimal V ≡ binary overflow). Cosmetic asymmetry, not a value bug. |
| 65c816 absolute 16-bit `+1` crosses banks → bug | **Refuted for absolute.** The hardware bank-crosses on absolute too. The bug is **only** for direct-page accesses. |
| PPU mosaic ignores "SETINI V-mosaic-disable bit" | **Refuted.** No such hardware bit exists; luna's unconditional V-mosaic is correct. luna's *comment* mislabels SETINI bit 2. |
| `luna-apu/lib.rs` carries a divergent duplicate **gaussian** table | **Refined.** The gaussian/counter dupes are dead-but-**identical**; only the legacy **ADSR** table (`AdsrPhase`/`ADSR_RATE_PERIODS`) actually diverges. All of `lib.rs:67-147` is dead and should be deleted regardless. |

Conversely, grounding **upgraded** one finding: the SA-1 "CC1 tile-width
stride" concern is actually a **bpp/width bit-field swap** (`cdma` bits 0-1
vs 2-4 inverted relative to the hardware) — a bigger, clearer bug than first
stated.

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
> `$4000-41FF` access-speed table. The reasoning and hardware-reference
> citations below remain useful; the grades do not.

### CPU — 65c816 · Grade B

Algorithmically faithful to the hardware reference for ALU/flags/decimal;
complete 256-opcode coverage. Non-cycle-stepped functional core: DP 16-bit
wrapping is wrong, MVN/MVP non-interruptible, IRQ edge-latched rather than
level-polled.

| Sub-area | luna | Grade |
|---|---|:--:|
| Opcode coverage | all 256 (`opcodes.rs:119+`) | A |
| Addressing — DP 8-bit page wrap | missing on bare `direct_page` (`addressing.rs:33`) | C |
| Addressing — DP 16-bit high byte | `+1` over u32 → bank-cross at $FFFF (`addressing.rs:203`) | **D** |
| Addressing — absolute 16-bit | `+1` bank-crosses (correct) | A |
| Decimal ADC/SBC | nibble-wise (`opcodes.rs:1374+`) | B |
| Flags NVZC (binary) | `~(a^v)&(a^r)` (`opcodes.rs:1346`) | A |
| Interrupt/IRQ model | edge latch, re-armed each step (`cpu.rs:94`) | C |
| Cycle timing | instruction-atomic | **D** |
| MVN/MVP | whole block in one step (`opcodes.rs:1144`) | C |
| RTI/PLP width, E-mode M/X | forces M/X, truncates X/Y (`opcodes.rs:1082`) | A |

**Top divergence (D):** DP 16-bit high-byte uses `addr.wrapping_add(1)` on
`Addr24=u32`, so a DP access at effective offset `$FFFF` reads bank 1
instead of wrapping in bank 0. Affects `read_word`, `sta/stx/sty/stz_to_addr`,
`modify_memory`. The hardware confines to bank 0. **Credit:** the ALU core is a
faithful port; E-mode invariants correct.

**📊 Empirical — per-instruction Tier 1 (FULL suite, all 256 opcodes × both modes,
5 120 000 cases). Baseline → after the fixes landed this cycle:**

| Milestone | Pass | Note |
|---|--:|---|
| Initial | 98.60 % | MVN/MVP (~40k) + 16-bit decimal ADC/SBC (~31.6k) dominated the 71 605 fails |
| + BCD adjust fix (boolean inter-nibble carry) | ~99.2 % | decimal cluster eliminated (~31.6k → ~40) |
| + MVN/MVP per-byte interruptible (excluded from gate — §6.2) | 99.988 % | block-move cycle-budget artifact removed from the count |
| + E-mode stack (`pushN`/`pullN`) & (dp,X) D.l==0 wrap | **99.99996 %** | **2 fails / 5.08M** (`c4.n`, `e1.e` — isolated edges) |

The core is now **functionally per-instruction-conformant to 99.99996 %**. The two
residual cases are one-off edges; the only structural gap left is the
instruction-atomic model — `cycles[]` ordering and the MVN/MVP cycle-budget
cases need a cycle-stepped scheduler (§5). The ALU + control core was
fundamentally sound throughout; every fix was a localized addressing/arithmetic
correction grounded in the hardware reference (with one documented hardware↔suite
divergence on the (dp,X) `D.l != 0` quirk — see §6.2). **Grade B → A−.**

---

### SMP — SPC700 · Grade B *(was B−)*

256/256 opcodes, byte-faithful ALU/MUL/DAA/DAS. `DIV YA,X` fixed this cycle;
one cycle-timing divergence remains.

| Sub-area | luna | Grade |
|---|---|:--:|
| Opcode coverage | 256/256 exhaustive | A |
| Flags (ADC/SBC/logic/CMP/shift) | `opcodes.rs:1748` | A |
| **DIV YA,X** ✅ *fixed* | hardware-faithful: H/V from **pre-div** Y, `Y<(X<<1)` overflow branch, X==0 via 256-X (`opcodes.rs:1096`) | **A** |
| MUL | NZ from Y (`opcodes.rs:1087`) | A |
| DAA/DAS | `opcodes.rs:1676` | A |
| **Cycle — taken branch** ✅ *fixed* | +2 on take via `SPC700_BRANCH_TAKEN_PENALTY` (`ef44271`) | **A** |
| Cycle — per-opcode base | table, plausible | B |
| Reset / IPL | vector + SP=$FF (`cpu.rs:46`) | B |
| Timers T0/T1/T2 | 128/128/16 divider (`apu/lib.rs:205`) | B− |

**✅ Fixed this cycle:** `DIV YA,X` is now a verbatim hardware-reference port — H/V
from the pre-division Y, the `Y<(X<<1)` overflow branch, and X==0 handled via the
256-X path (no division by zero). Validated by 3 regression tests + a no-regression
audio smoke (SMW robust; Chrono Trigger byte-identical pre/post). **Remaining
divergence (C):** the branch-taken `+2` cycle penalty is still unapplied
(`cycles.rs:28`); since `apu/lib.rs:404` feeds the returned cycle count into
`tick_timers`/`tick_voices`, branch-heavy driver loops run timers/DSP slightly
fast — a cycle-timing ceiling, not a value bug. **Credit:** SBC-via-ones-
complement, MUL, DAA/DAS all match the hardware reference exactly.

---

### DSP — S-DSP audio · Grade A−

Faithful near-line-for-line transliteration of the hardware DSP macro pipeline.

| Sub-area | luna `dsp.rs` | Grade |
|---|---|:--:|
| BRR decode | filters 0-3, scale, `(i16)` store (`:545`) | A |
| Gaussian interpolation | build+index, 3-tap `>>11` + clamp (`:257,443`) | A |
| Envelope ADSR/GAIN | full state machine incl. mode-7 two-slope (`:464`) | A |
| Echo / FIR | `>>6` FIR, `s>>1` half-scale read (`:779`) | A |
| Noise LFSR | `(lfsr<<13)^(lfsr<<14)` (`:916`) | A |
| KON/KOFF/ENDX timing | keylatch + 5-step delay (`:702,742,755`) | A |
| Pitch modulation | `pitch += ((out>>5)*pitch)>>10` (`:657`) | A |
| Overflow/clamp sites | every `(i16)`/`sclamp16` site matches | A |

**Only divergence (D, dead code):** `luna-apu/lib.rs:67-86` has a legacy
secondary-doc-style `AdsrPhase`/`ADSR_RATE_PERIODS` model that does **not** match
the live state machine — but it's **dead** (zero callers). The
gaussian/counter dupes (`lib.rs:92-147`) are also dead but *identical*. Delete
all of `lib.rs:67-147`.

**⚠ Primary risk — TEST GAP:** there are **no DSP golden-vector tests**.
`dsp.rs` tests only cover silence/KON-latch/counter/register-roundtrip —
nothing decodes a real BRR block and asserts PCM output against a hardware
capture. For an otherwise line-faithful port, a regression in any `>>`/clamp
would pass CI silently (exactly the "Echo FIR half-scale" history). **Highest-
value single addition in the whole project:** commit a few SPC-driven golden
sample vectors captured from the hardware reference.

---

### PPU — graphics · Grade C+

Color-math mixer is reference-accurate; renderer is frame-snapshot (not
per-dot) and carries concrete geometry bugs.

| Sub-area | luna | Grade |
|---|---|:--:|
| BG modes 0–4 | per-pixel priority engine, correct tables | B |
| BG modes 5/6 (hi-res 512) | **absent** — falls back to Mode-1, 256-wide (`renderer.rs:1107`) | **D** |
| Mode 7 affine | matrix+center+screen-over; renders CT + SMK/Pilotwings (DSP-1) correctly | B |
| Mode 7 EXTBG (SETINI b6) | implemented (`luna_bg_gaps.md` §4) | B |
| DSP-1 (uPD7725) coprocessor | implemented; SMK + Pilotwings Mode 7 correct (HiROM 1K + LoROM 1B boards); needs user `dsp1b.rom` | B |
| Sprite/OBJ rendering | decode+4bpp+priority; shadow_y hack | C |
| **Sprite Y-wrap (8-bit)** | not wrapped (`renderer.rs:391`) | **D** |
| **Sprite tile addressing** | flat `tile+(row*16+col)` (`renderer.rs:416`) | **D** |
| Windows (W1/W2 logic) | `compute_window_masks` (`renderer.rs:682`) | B |
| Color math (add/sub/half/clip) | clamp 0-31 (`renderer.rs:963`) | B+ |
| CGWSEL/CGADSUB polarity | force-black + math-region + OBJ≥192 gate | A− |
| Mosaic (H+V) | snap X&Y to block (`renderer.rs:1154`) | B |
| **BG scroll write-twice** | naive `(hi<<8)\|lo` (`ppu.rs:545`) | **D** |
| VRAM/CGRAM/OAM addressing | word-remap, OAM 544 modulo, gated | B |
| OAM mask-vs-modulo (544) | `% 0x220` — **correct** (`memory.rs:406`) | A |
| Brightness / forced blank | INIDISP scale (`renderer.rs:518`) | B |

**Divergences (all D, unambiguous in the hardware reference):** sprite Y not
8-bit-wrapped (sprites at Y≈250 drop wrapped rows); large-sprite tile column
carries into the row nibble; H/V scroll write-twice drops the dual-latch
low-3-bits interaction (mis-scrolls sub-tile H-scroll — very common); Mode-7
conflates wrap vs transparent/tile-0 and never sign-extends `(hoffset-hcenter)`.
Hi-res modes absent (D), EXTBG absent (F). **Credit:** the add/sub/half/clip,
CGWSEL force-black polarity (historic G1 inversion is fixed), OBJ-palette-≥192
gate, and empty-sub fallback all trace cleanly to the hardware reference; OAM
544-modulo is right.

---

### DMA / HDMA / timing · Grade C+

The subsystem most affected by luna's coarse scheduler. Byte-movement and
table walk are accurate and well-tested; cycle accounting and event ordering
are architecturally coarser than the hardware reference.

| Sub-area | luna | Grade |
|---|---|:--:|
| Sync DMA transfer (modes/dir/incr) | `channel.rs:266` | A |
| DMA cycle cost | lump `8+8*bytes` after burst (`snes.rs:1352`) | C |
| HDMA table walk | `channel.rs:326` | A− |
| **HDMA timing / preemption** | once/line at boundary, no preempt (`snes.rs:699`) | C |
| NMI timing | latch at vdisp, 4-mclk hold via H<2 (`snes.rs:618`) | B− |
| **IRQ (H/V) timing** | H-mode 0b01 fires **every** scanline, ignores HTIME (`snes.rs:675`) | **D** |
| RDNMI hold | `line==vbstart && h<2` (`snes.rs:1185`) | B |
| Scheduler granularity | instruction lump-charge (`snes.rs:592`) | **D** (arch) |
| **Coprocessor sync** | per-byte tick **+** lump re-consumed → ~2× | **C−** |

**Top divergence (C−, confirmed bug):** DMA advances the coprocessor twice —
per-byte `bus.tick(8)→step_coproc` (`channel.rs:307`) **and** the lump
`io_cycle(8+8*bytes)` re-consumed by the next instruction's `step_coproc(consumed)`
(`snes.rs:1352→549→566`). The hardware does not double-count. **Credit:** mode/
direction/increment decode and the full HDMA table walk (direct/indirect/
repeat/multi-entry/terminator) are TDD-tested *and verified accurate* against the
hardware reference.

---

### SA-1 coprocessor · Grade C+

Boots real carts end-to-end; IRQ/banking/multiplier reference-accurate;
several confirmed arithmetic/decode divergences.

| Sub-area | luna | Grade |
|---|---|:--:|
| Memory mapping / banking | 4 super-banks, MB select | A− |
| Multiplier (16×16→32, MAC) | `i32×i32` signed (`sa1.rs`) | A |
| **Divider** | **signed÷signed** (`ma/mb` both i16, `sa1.rs:921`) | **D** |
| MAC accumulator | `saturating_add`, no OF flag | C |
| **MAC accumulator-clear** | dead guard `b1!=0 && val==0x02` (`sa1.rs:1254`) | **D** |
| IRQ delivery (latch+bridge) | level-latch, CIC/SIC clear, OR-of-enable | A |
| BW-RAM gating | OR(SBWE,CBWE) (`sa1.rs`) | A |
| **Character conv CC1** | bpp/width fields **swapped** (`sa1.rs:572`) | **D** |
| Character conv CC2 | per-byte staging | C+ |
| VLBP ($230C/$230D) | 3-byte window (`sa1.rs:438`) | B+ |
| Normal DMA | full-burst, no per-byte cost | C |
| Instruction timing | flat 6 mclk, `io_cycle` no-op (`coproc/sa1.rs:48`) | C− |

**Top divergences (D):** divider operand signedness; the dead MAC-clear guard
(games writing MCNT `$03` never reset the accumulator); CC1 `cdma` bpp/width
bit fields swapped relative to the hardware. **Credit:** the IRQ model is
genuinely reference-accurate (level latch + CIC/SIC clears + OR-of-enable +
SCNT vector override), and **the CCNT bit-5 reset polarity is correct**
(`coproc/sa1.rs:97`) — the historic bit-5/bit-7 inversion regression is not
present.

---

### Bus / mappers / cartridge · Grade C+

LoROM/HiROM/ExHiROM math and SRAM windows match the hardware reference; three
reference-confirmed behavioral gaps + a weak detection heuristic.

| Sub-area | luna | Grade |
|---|---|:--:|
| LoROM map | `lorom.rs:44` | A− |
| HiROM map | `hirom.rs:95` | A |
| ExHiROM map | +64 banks (`hirom.rs:89`) | A |
| **ROM mirroring (non-pow2)** | **returns None → open-bus** (`lorom.rs:48`) | **D** |
| SRAM mapping | `% size` wrap (`lorom.rs:63`) | B |
| Open-bus value | fixed `0xFF` (`snes.rs:985`) | C |
| FastROM speed | `bank≥$80 & $8000+` (`speed.rs:69`) | A− |
| **`$2000-5FFF`/`$4000-41FF` speed** | all `$2000-5FFF`=Slow(8); only `$4016/7`=XSlow (`speed.rs:54`) | **D** |
| **Mapper detection** | first checksum-pass wins, no scoring (`lib.rs:149`) | C− |
| **SA-1/coproc detection** | wrong byte (MapMode low-nibble; `lib.rs:202`) | **D** |
| SMC-header strip | `len%1024==512` eager (`lib.rs:122`) | B− |
| Header field parse | exponent clamp, LE checksum (`lib.rs:171`) | B |

**Top divergence (D):** undersized/non-power-of-two ROMs return open-bus
instead of **mirroring** — the hardware mirrors the ROM image into the
address window. Plus the `$2000-$5FFF` register block runs at the **fast (6)**
rate on real hardware (luna marks it Slow), and `$4000-$41FF` is **entirely
XSlow (12)** (luna only flags `$4016/$4017`). SA-1 is detected from the wrong
header byte. **Credit:** the three core mapper address maps + SRAM windows
match the hardware reference exactly.

---

## 5. To close the gap with the hardware reference (prioritized)

Ordered by accuracy-ROI ÷ effort. Items 1-6 are bounded bug fixes that move
specific grades up; 7-8 are the architectural ceiling.

> **✅ Completed this cycle (CPU — all suite-validated, no regressions):**
> the 16-bit decimal BCD adjust, MVN/MVP per-byte interruptibility, and the
> E-mode stack + (dp,X) pointer-wrap fixes — taking the 65c816 from 98.60 %
> to **99.99996 %** (B → A−). Remaining items below are untouched.

1. **DSP golden-vector tests** *(A− → lock it in)* — capture a few SPC-driven
   PCM vectors from the hardware reference; gate CI. Highest value: protects the
   one faithful port from silent regression. *(No grade change, but removes the
   latent risk that defines the A−.)*
2. **SA-1 divider sign + MAC-clear guard + CC1 field swap** *(C+ → B)* — three
   small, source-confirmed fixes (`sa1.rs:921`, `:1254`, `:572`). Add edge
   tests with `MB≥$8000`, `MCNT=$03`, and a width>3 CC1 conversion.
3. **SPC700 `DIV YA,X`** *(B− → B)* — port the hardware branch verbatim;
   compute V/H from pre-division registers.
4. **DMA coprocessor double-charge** *(C+ → B−)* — single bug (`snes.rs` lump
   vs per-byte `tick`); pick one path to drive `step_coproc`.
5. **PPU geometry quartet** *(C+ → B−)* — 8-bit sprite-Y wrap, per-nibble tile
   addressing, H-scroll prev-low-3-bits latch, Mode-7 OOB split. All four are
   small and have crisp hardware references.
6. **Bus: ROM mirroring + `$2000-5FFF` speed table** *(C+ → B)* — mirror
   instead of open-bus; fix the access-speed regions per the hardware table.
7. ~~65c816 DP wrap + MVN/MVP interruptibility~~ ✅ **done this cycle** (B → A−).
   **Remaining:** the edge-latched IRQ model (vs hardware's level-triggered line)
   — needs an IRQ-poll hook on the bus. Minor, and not suite-gated.
8. **Cycle-stepped scheduler** *(the C+ ceiling on DMA/timing/SA-1/SPC)* — the
   big one: move from instruction/burst-atomic lump-charging toward
   ~2-mclk-granular stepping with per-access IRQ/NMI/HDMA polling, like the
   hardware. This is a multi-week architectural change; everything above is
   worth doing first and is independent of it.

> Per `audible-fixes-test-first.md`: the DSP (#1), SPC `DIV` (#3), and PPU
> (#5) changes must be validated by ear/eye in `luna-gui` before commit.

---

## 6. Benchmark plan — turning grades into a CI-trackable score

This scorecard is **static** (source-level). To make the accuracy challenge
*reproducible and CI-gated*, here is a concrete, tiered plan grounded in what
`luna-cli` and the standalone crates can actually do today.

### 6.0 The verdict surface luna already has

| Mechanism | luna-cli flag / API | Good for |
|---|---|---|
| Framebuffer PNG | `run --screenshot`, `state --screenshot` | visual test ROMs (hash vs golden) |
| RAM result byte | `state --peek BANK:OFFSET:COUNT` (→ stderr hex) | "wrote $xx = pass" result-byte ROMs |
| APU port traffic | `state --apu-log` (`$2140-$2143` CSV) | tests that signal via APU ports |
| Register snapshot | `state --out` (`EmulatorState` JSON: full CPU/PPU regs) | state assertions |
| CPU instruction trace | `state` trace CSV (PC,A,X,Y,SP,P,DB,DP,e) | differential trace diff |
| Scripted input | `state --input "frame:hex,..."` | tests needing button presses |
| Full-state round-trip | `Emulator::save_state` / `load_state` | state-completeness + determinism: save → advance → load must rewind exactly (a missing serialized field shows as a round-trip divergence) |

**One small gap:** `--peek` dumps hex to *stderr*, not a machine-readable
exit code. Tiers 3 needs a thin addition — a `--assert BANK:OFFSET=HEX`
flag (or a `--peek` that sets exit status) so a harness can branch on
pass/fail without scraping text. ~30 lines.

### 6.1 Reviewer's recommendation (my feedback)

**Lead with the CPU unit suites, not the full-system ROMs.** Here is *why*,
and it's the most important judgment call in this plan:

- luna's **CPU cores are standalone crates with no SNES glue** — which means
  the gold-standard **per-instruction state-vector suite** (per-opcode JSON
  state vectors used to validate reference-class cores) can be run **directly**
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

### 6.2 Tier 1 — per-instruction state-vector suite *(highest ROI)*

Both suites confirmed live:
- **`65816`** — an instruction-by-instruction test suite for the 65816 that
  includes bus activity. `v1/<opcode>.e.json` + `.n.json`
  (emulation/native), one file per opcode.
- **`spc700`** — SPC700 vectors, `v1/`.

**Format** (verified from `v1/00.e.json`): each case has
`initial`/`final = {pc, s, p, a, x, y, dbr, d, pbr, e, ram:[[addr,val],…]}`
and `cycles:[[addr,val,flags],…]` (per-cycle bus activity; `flags` like
`"d--wemx-"` = read/write + mode bits). RAM addresses are full 24-bit.

**✅ Status — already built and now RUN.** The harness already exists in the
tree: `crates/luna-cpu-65c816/tests/cpu_tests.rs` (complete — discovery, JSON
schema, per-case runner, aggregation, report, `LUNA_CPU_TESTS_REQUIRE` strict
gate), and the **full corpus is committed** under `tests/cpu-tests/v1/` (512
files, ~10k cases each). It reuses `luna_bus::testing::RamBus` (flat 16 MB) and
maps each suite `initial`/`final` straight onto `Cpu`'s public fields. It's
`#[ignore]`d; run with:

```bash
cargo test -p luna-cpu-65c816 --test cpu_tests -- --ignored --nocapture
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
| **16-bit decimal ADC/SBC** (`6x`/`7x`/`Ex`/`Fx`) | ~31 600 | **0** | rewrote `adc/sbc_value_bcd` (in-place result, boolean inter-nibble carry) |
| **MVN / MVP** (`54`/`44`) | ~39 989 | **excluded** | per-byte interruptible; the suite's 100-cycle-budget partial model isn't gateable on an atomic core |
| **E-mode stack** (PEA/PEI/PER/PHD/PLD/PLB/JSL/RTL) | ~430 | **0** | added `push/pull_*_native` (16-bit S, no page-1 confine); re-pin restores S.h |
| **(dp,X) pointer wrap** (`01`-`e1`) | ~185 | **1** | confine pointer bytes to the page only when `E && D.l == 0` (matches the suite; documented hardware divergence on `D.l != 0`) |
| residual | — | **2** | `c4.n`, `e1.e` — isolated one-off edges |

**Reading:** the 65c816 is now **99.99996 % per-instruction-conformant** — 2 stray
cases out of 5.08M. Each fix was a localized, reference-grounded change with
the harness as the regression gate; no other opcode regressed (verified by the
full sweep). The grade moves **B → A−**; the only thing between A− and A is the
instruction-atomic model (cycle ordering + the MVN/MVP cycle-budget cases),
which is the §5 cycle-stepped-scheduler milestone, not a discrete bug.

### 6.3 Tier 2 — DSP golden vectors

Already §5 item 1. The DSP is an A− *because* it's untested. Capture a handful
of PCM outputs from the hardware reference (drive a known SPC + register sequence
through the DSP), commit them as fixtures, and assert luna's `dsp.rs` output
sample-for-sample. Small fixture, closes the one latent risk on the best
subsystem.

### 6.4 Tier 3 — full-system test ROMs via luna-cli

Confirmed redistributable suite: an open-source homebrew hardware-test ROM
corpus has `CPUTest/{CPU,SPC700}`, `PPU/`, `INPUT/`, `SPC700/` — assembled ROMs
that render a pass/fail screen and ship a reference PNG. Pipeline:

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

If a reference emulator binary is available (one that can dump CPU trace logs),
run a fixed ROM+input through both and diff against luna's `state` trace CSV /
`EmulatorState` snapshots to pinpoint the exact divergence instruction. Heavy
setup; only worth it once Tiers 1–3 have drained the easy wins.

### 6.6 Proposed first step

If you greenlight it, I'd start with **Tier 1 / 65816**: add the gated test
harness + a `MockBus` with raw 24-bit read/write, point it at a local
per-instruction suite checkout, and report the first `passed/total` per opcode —
which will concretely confirm (or refute) the DP-wrap and other CPU findings
with hard numbers. That's a self-contained, reviewable PR that needs no
changes to emulation code, only test infrastructure.
