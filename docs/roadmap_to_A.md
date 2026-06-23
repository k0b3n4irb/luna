# Roadmap to "A everywhere" — the cycle-accuracy frontier

Status: **plan** (2026-06-23). Goal: flip the remaining **A−** subsystems
(65c816, SPC700, PPU, SA-1) and the **B−/B** ones (DMA/HDMA, Bus) up to **A**,
faithfully, without big-bang rewrites.

## The headline finding — re-scope before building

The two scary "architectural rewrites" everyone assumes this needs are
**mostly already done** in luna. Building either as a literal ares port would
re-do finished work and risk regressions against a faithful baseline. Per the
repo's own code + ground-truth docs:

1. **Cooperative cothread scheduler — the *grammar* is already ported (no libco).**
   `cycle_accuracy_plan.md` §4 deliberately chose a **CPU-driven** model with
   `io_cycle`/`advance_time` as the per-access synchronization point (Phases 1–5
   landed). The CPU↔SPC interleave is **cycle-exact at bus-access granularity**
   (`Apu::run_to_target`, a Mesen2 `Spc::Run` port — resumable, one access at a
   time). `cooperative_scheduler_reference.md` concludes a global libco rewrite
   is "unnecessary and Rust-hostile." **→ Do NOT port libco.**

2. **Per-dot PPU renderer — already a lazy sub-range renderer; dot-276 is resolved.**
   luna renders lazily per dot-range (`ppu.rs` `flush_partial_scanline`,
   `last_flushed_dot`): a mid-line `$2100-$2133` write commits `[last_flushed,
   dot)` with the *old* register state first, so the visible region already gets
   dot-precise mid-line latching. `hdma_ares_audit.md` (2026-06-20) **closes**
   dot-276 HDMA: dot 276 is in HBlank past the 256 visible dots, so a line's
   HDMA only affects the *next* line — which `sched_one_line` already does. All
   58 goldens render correctly. **→ Do NOT rewrite the renderer.**

**So the real frontier is the *delivery-timing* + *contention* residuals** the
"−" grades actually hang on — each small, surgical, and bisectable. The reason
they were deferred is **validation**, not difficulty: they're interrupt/HDMA
*delivery* timing, currently "GUI-only, CLI-unvalidatable." Fix the validation
gap first and they become tractable.

## What each "−" actually hangs on

| Subsystem | Grade | The "−" (precise) | Class |
|---|:--:|---|:--:|
| 65c816 | A− | Interrupt taken at instruction *boundary*, not the per-access poll point; 2/5.08M Tom Harte `cycles[]` edge fails | delivery-timing |
| SPC700 | A− | Cycle model complete; "−" is residual *confidence* (breadth), not a known defect | validation |
| PPU | A− | Feature edges (EXTBG / offset-per-tile / mosaic / interlace vertical-doubling) + hi-res sub-subpixel color-math (documented approximation) | feature |
| SA-1 | A− | `conflict()` contention landed 2026-06-20; "−" is corpus-validation breadth + small `luna_sa1_gaps.md` rows | feature + validation |
| DMA/HDMA | B− | Deferred Phase-4 delivery edge cases (`$4211` hold, field guard, htime delay); per-byte cycle grid; indirect 1-byte quirk | delivery-timing + feature |
| Bus | B | `$2000-5FFF` register speed table (6 vs 8 mclk); mapper-detection scoring | feature (standalone) |

The deferred Phase-4 **NMI/IRQ delivery-timing** cases touch CPU + PPU + DMA at
once and are the real high-value frontier. They are gated on one thing: a
faithful `nmiLine`.

## Phased plan

```
P0  Delivery-timing differential harness      (KEYSTONE; no behavior change; de-risks all of P1–P4)
P1  Faithful nmiLine model ($4210/RDNMI)       (PREREQUISITE — the SMRPG black-screen gate)
     ├─ P2  nmitimenUpdate late-NMI-enable      (unlocked by P1)
     └─ P3  Per-access interrupt polling (CPU edge→level)
P4  Remaining HDMA/timer delivery edges ($4211 hold, field guard, htime delay)
P5  SA-1 conflict() contention breadth + gap rows     (independent; parallel after P0)
P6  PPU feature completeness (EXTBG / OPT / mosaic / interlace doubling)   (independent)
Tier-0/1 quick wins (parallel, no architecture): Bus speed table; DMA indirect 1-byte quirk
```

**Why this order:** `cycle_accuracy_plan.md` §7 names the faithful `nmiLine`
(P1) as the explicit prerequisite — a naive late-NMI port black-screened
SMRPG's intro precisely because `nmi_flag` isn't cleared at VBlank end. Every
delivery-timing item (P2/P3/P4) is rooted there. P5/P6 and the Tier-0/1 fixes
are independent and parallelizable once P0 exists.

### P0 — Delivery-timing differential harness (FIRST step)
- **What:** a Mesen `--testRunner` Lua trace of per-frame `$4210`/`$4211` read
  values + timing, NMI/IRQ vector-fetch dot positions, HVBJOY transitions; a
  matching luna CLI trace (extend `MemTraceLog`/`CpuTraceLog` with an
  interrupt-delivery event, reuse `--mem-trace` `$4200-$421F` filtering); a diff
  test modeled on `tests/spc_trajectory.rs`.
- **Risk:** none — read-only, zero emulation change. Converts the four
  "GUI-only" deferrals into bisectable diffs. **Highest leverage in the plan.**
- **Files:** `tools/mesen-wram-hash.lua` (template), `crates/luna-cli/src/main.rs`,
  `crates/luna-core/src/snes.rs`, `crates/luna-core/tests/`.

### P1 — Faithful `nmiLine` (HIGH risk, the tripwire)
- Replace `cpu_regs.nmi_flag` (cleared only on `$4210` read) with an ares
  `nmiLine`: set at VBlank entry, **cleared at VBlank end**, with the RDNMI hold
  window. Stage as a shadow field asserted-equal first, then flip the clear
  point behind the SMRPG-intro NMI-count assertion, then remove the old field.
- **Validate:** P0 diff vs Mesen over SMRPG intro + SMW; SMRPG name-entry smoke;
  58 goldens unchanged. **Files:** `cpu_regs.rs`, `snes.rs` (`sched_one_line`,
  `$4210` path).

### P2 — `nmitimenUpdate` late-NMI-enable (MEDIUM, was HIGH pre-P1)
- Enabling `NMITIMEN.7` mid-VBlank with the faithful `nmiLine` asserted fires
  the NMI; + the IRQ-disable-clear half (land that first, it's a SMRPG no-op).
- **Validate:** SMRPG intro (`$81` write at `$C3:B9D4`) must produce zero
  spurious NMIs. **Files:** `cpu_regs.rs`, `snes.rs`.

### P3 — Per-access interrupt polling (HIGH — every game's IRQ timing)
- Move NMI/IRQ *sampling* from the instruction boundary into
  `io_cycle`/`advance_time` (the level inputs already flow through
  `set_irq_line`). NMI first, then verify IRQ unchanged.
- **Validate:** Tom Harte `cycles[]` oracle (the 2/5.08M fails are candidates to
  close); Doom letterbox raster (the level-IRQ fix must NOT regress).
  **Files:** `snes.rs` (`step`, `advance_time`, `poll_hv_irq`),
  `cpu-65c816/src/opcodes.rs`, `cpu-65c816/tests/tom_harte.rs`.

### P4 — HDMA/timer delivery edges (MEDIUM, 3 independent edits)
- `$4211` TIMEUP hold; ares "last dot of field" guard; htime/vtime 10-clock
  detection delay — all in `poll_hv_irq`. Land + validate each separately.

### P5 — SA-1 contention breadth (LOW-MED, parallel)
- Validate `conflict()` BWRAM/IRAM/ROM contention across the SA-1 corpus + close
  remaining `luna_sa1_gaps.md` rows.

### P6 — PPU feature completeness (LOW, parallel, per-feature goldens)
- EXTBG / offset-per-tile / mosaic / interlace vertical-doubling, each a
  self-contained renderer addition behind a golden. (NOT a renderer rewrite.)

### Tier-0/1 quick wins (no architecture, parallelizable)
- **Bus speed table** (`$2000-3FFF/$4200-5FFF`→6 mclk, `$4000-41FF`→12) — ~1 h.
- **DMA indirect-HDMA last-channel 1-byte quirk** (ares `dma.cpp`) — ~2 h.

## Explicitly NOT recommended
- **Literal libco cothread port** — re-does the grammar luna already speaks
  (`run_to_target`/`advance_time`); blast-radius = the whole core; payoff =
  behavioral parity with today. The Super FX cautionary tale in reverse.
- **Literal per-dot renderer** — nothing visible to split at dot 276; risks the
  58 goldens for zero visual gain. Only revisit if a concrete game renders wrong
  under the boundary+lazy-flush model — then bisect *that game* (the method).

## Bottom line
The path to "A everywhere" is **a sequence of surgical, bisectable delivery-
timing + feature fixes**, NOT two multi-week rewrites — because the cooperative
grammar and the lazy renderer are already in place. The keystone is **P0** (the
delivery-timing harness): it costs nothing in regression risk and unblocks the
deferred Phase-4 work that the CPU/PPU/DMA "−" grades hang on.
