# Cycle-Accuracy Milestone — APU↔CPU↔PPU Synchronization Plan

**Status:** in progress — **Phases 1, 2, 3 landed; Phase 4 core landed
(delivery edge cases deferred); Phase 5 increments 0+1 + Phase 5b landed;
Phase 5 inc 2 deferred** (mid-frame
DMA↔HDMA preemption at scanline boundaries; `f3bd002` resumable segment API +
`d2a17fc` segmented driver; SA-1 real per-access cycle accounting replacing
the flat 6 mclk/insn budget). Phase 5 inc 2 (true dot-276 `hdmaPosition`
sub-line timing) is **deferred** — it cannot be represented on luna's
whole-line renderer without corrupting output (proven 2026-06-17: firing
HDMA at dot 276 regressed the golden corpus — hicolor64 black lines,
redspace black top line, mode7 garbage). It needs a per-dot/sub-line
renderer. See `docs/hdma_ares_audit.md` (§"Phase 5 inc 2 … DEFERRED").
- Phase 1 (io_cycle-driven per-access catch-up + DMA coproc double-charge
  fix): done — APU `db19ca8`/snes.rs, PPU sched, coproc `535d2e7`.
- Phase 2 (SPC700 per-opcode cycles + branch-taken penalty + master-clock
  cadence): done `db19ca8`; cycle backstop + 9 off-by-one fixes `3ab5e21`.
- Phase 3 (65c816 internal/idle cycles): done `2da74fc`; Tom Harte cycle
  backstop `990c255` (0 mismatches, 100% state). It made a fixed-instr
  SMRPG smoke land on a black post-intro frame, which *looked* like a
  regression but was not (HEAD A/B confirmed) — and which a later (2026-06)
  investigation showed was **never an SA-1 deadlock at all** (see §7
  "SA-1 game status").
- Phase 4 (per-access IRQ/NMI/HDMA): **core done; delivery edge cases
  deferred.** Dot-precise H/V IRQ `d4b0bb6` (fixes modes 01/11; unblocks
  DKC's H-IRQ raster) + HDMA time cost (18 mclk/line + 8/byte, charged via
  the `sched_advance` stall loop). The DMA↔HDMA preemption part landed in
  Phase 5. The remaining items are all **NMI/IRQ-*delivery-timing* edge
  cases** ($4211 TIMEUP hold, ares "last dot of field" guard, htime 10-clock
  detection delay, and the `nmitimenUpdate` late-NMI-enable) — **deferred**
  (2026-06-18): they are high-regression-risk on luna's level-driven IRQ
  model and CLI-unvalidatable (GUI-only). The late-NMI-enable in particular
  needs luna's `nmi_flag` modeled as a faithful ares `nmiLine` (cleared at
  VBlank end) FIRST — a naive port black-screened SMRPG's intro. See §7 and
  the `project_nmitimen_late_enable_breaks_smrpg` finding.
- Phase 5: see the table in §5.
**Author:** synthesized from the ares + Mesen2 timing correlation
(`docs/accuracy_scorecard.md` §"DMA / HDMA / timing") and the Chrono
Trigger APU-deadlock investigation.
**Goal:** replace luna's instruction-atomic, lump-charge scheduler with
fine-grained (per-bus-access, ~2–12 mclk) synchronization between the
65c816, the SPC700/DSP, the PPU, DMA/HDMA, and coprocessors — so
timing-sensitive software (Square's Akao sound driver in CT/FF, raster
IRQ effects, the Tom Harte `cycles[]` traces) runs correctly.

---

## 1. Why (the problem this fixes)

Concrete failures traced to the coarse scheduler:

- **Chrono Trigger audio deadlock.** CT's Akao driver uses a tight,
  timing-coupled CPU↔SPC handshake. Under luna it deadlocks (near-silent,
  black screen). The failure mode is **path-dependent** — the SPC stalls
  at the IPL `cmp $F4,#$CC` loop under one stepping cadence and at the
  driver (~`$0BA1`) under another. That sensitivity to *how* we step is
  the signature of a scheduler too coarse to maintain CPU↔SPC phase.
  SMW's looser N-SPC driver tolerates the approximation; CT's does not.
- **Tom Harte `cycles[]` unvalidated.** Tier-1 proves *what* each opcode
  computes, not *when* each bus cycle happens (the core is
  instruction-atomic). MVN/MVP can't be gated at all (their cycle-budget
  partial model needs cycle stepping). See `accuracy_scorecard.md` §6.2.
- **SPC700 timing C-grade.** Branch-taken `+2` penalty unapplied; the APU
  runs at a flat ~84 mclk/instruction regardless of opcode; timers B−.
- **DMA double-charges the coprocessor** and models no mid-line HDMA
  preemption (scorecard DMA section, grade C+).

## 2. Current architecture (precise)

CPU-driven, **instruction-atomic, lump-charge** (`luna-core/src/snes.rs`):

```
Snes::step():
    cpu.step(&mut bus)            // runs ONE full CPU instruction;
                                  // bus.io_cycle() only ACCUMULATES total_mclk
    consumed = total_mclk - before
    advance_scheduler(consumed)   // PPU scanline cursor — caught up in one lump
    apu_real.step(consumed)       // APU — one lump, internally ~84 mclk/instr
    mapper.step_coproc(consumed)  // coproc — one lump (and DMA double-charges it)
```

Key facts:
- **`SnesBus::io_cycle(m)` is `total_mclk += m`** — nothing advances
  mid-instruction. The per-access cost is *known* but not *applied* until
  the instruction finishes. (The CPU-core doc comment claiming io_cycle
  "catches up the PPU immediately" is aspirational, not implemented.)
- The **audio queue drops samples when full** (no backpressure) — so it
  does *not* stall the SPC; it is already decoupled from timing. (This
  refutes the earlier "queue backpressure" hypothesis — the fragility is
  the scheduler, not the queue.)
- SPC700 advances at a fixed rate; 65c816 timing is final-state-only.

## 3. Reference models (already studied — see scorecard)

- **ares** — each component (CPU, SMP, DSP, PPU, coprocessors) is a
  **libco cothread**; a scheduler resumes whichever is furthest behind.
  Each `step(clocks)` yields control at ~2-mclk granularity; memory
  accesses call `step()` inline, so a read mid-instruction advances every
  other component before returning. Per-access `dmaEdge()`/`irqPoll()`.
- **Mesen2** — **event-driven master loop** on `_hClock`: `IncMasterClock`
  in 2-mclk steps, `ProcessPendingTransfers()` (HDMA preempts DMA at
  H=276 dots), per-dot IRQ/H-counter matching, `SyncCoprocessors`.

Both **agree**: components advance in lockstep at ≤2-mclk granularity,
with interrupts/HDMA polled *during* CPU instructions. luna's lump model
is the architectural outlier.

## 4. Target architecture for luna

Keep the **CPU-driven** structure (no need to port libco cothreads) but
make **`io_cycle` the synchronization point**: every bus access (and every
internal CPU idle cycle) advances the PPU, APU, and coprocessor by that
access's mclk *immediately*, mid-instruction. This is the
"catch-up-on-every-access" model (bsnes-accuracy-lite style):

```
SnesBus::io_cycle(m):
    total_mclk += m
    ppu.advance(m)            // scanline/dot cursor + line events
    apu.advance(m)            // SPC700 driven by master clock, not a flat rate
    coproc.advance(m)         // single sync path → no double-charge
    poll_irq_nmi_hdma()       // per-access, like ares dmaEdge/irqPoll
```

The SPC700 is advanced by converting accumulated master cycles into SPC
cycles (21.477 MHz ÷ 1.024 MHz ≈ 20.97), executing SPC instructions until
its cycle debt is paid — using **real per-opcode cycle counts** (incl. the
branch-taken penalty), not a flat 84.

## 5. Phased migration (each phase independently shippable + validated)

| Phase | Change | Unblocks | Risk |
|---|---|---|---|
| **1. io_cycle-driven catch-up** ✅ done | Move PPU/APU/coproc advancement out of the end-of-`step()` lump and into `io_cycle`, advancing per access. Collapses the three lump calls into one per-access sync. **Naturally fixes the DMA coproc double-charge.** | Mid-instruction PPU/APU/coproc accuracy; foundation for all later phases | Med — hottest path; perf-sensitive |
| **2. SPC700 cycle accuracy** ✅ done | Real per-opcode cycles + branch-taken penalty; drive the APU from the master clock (mclk→SPC-cycle ratio) instead of a flat rate. | **CT/Akao handshake**, SPC700 B→A−, Tom Harte SPC `cycles[]` | Med |
| **3. 65c816 cycle accuracy** ✅ done | Have the CPU core call `io_cycle` at the correct *intra-instruction* points with correct per-cycle costs (read/write/idle ordering). The core already emits `io_cycle` per **bus** access (Phase 1 relies on it); what was missing was the **internal/idle** cycles — RMW dead cycles, branch-taken / page-cross penalties, etc. — plus the Tom Harte `cycles[]` backstop to drive them out (cf. the SPC700 Phase-2 method). Landed `2da74fc`. | Tom Harte 65c816 `cycles[]`, A−→A | High — touched every opcode/addressing path |
| **4. Per-access IRQ/NMI/HDMA** ✅ core done, ⚠️ delivery edge cases deferred | Poll interrupts + HDMA in `io_cycle`: dot-precise H/V-IRQ (`d4b0bb6`, HTIME respected) ✅; HDMA-vs-DMA preemption landed in Phase 5 ✅. **Deferred** (high-risk on luna's level IRQ model, GUI-only validation): `$4211` TIMEUP hold, ares "last dot of field" guard, htime 10-clock detection delay, and the `nmitimenUpdate` late-NMI-enable — the last needs a faithful `nmiLine` (cleared at VBlank end) first; a naive port black-screened SMRPG (see §7). | Raster-IRQ games, DMA/timing C+→B+ | Med |
| **5. DMA/HDMA cycle-stepping** ✅ inc 0+1, ✅ 5b, ⚠️ inc 2 deferred | Segmented sync DMA (`f3bd002`); HDMA preempts a mid-frame DMA at scanline boundaries (`d2a17fc`, line-granular). Phase 5b ✅ (`097ffe7`): SA-1 now charges **real per-access cycles** (ares `coprocessor/sa1/memory.cpp`: IO/ROM/IRAM/open-bus = 1 step, BWRAM = 2 steps, idle = 1 step; `conflict()` contention deferred to Increment B) via a signed mclk deficit, replacing the flat 6 mclk/insn lump. **inc 2 (dot-276 sub-line `hdmaPosition`) deferred** — faithful dot-276 corrupts rendering on luna's whole-line renderer (needs a per-dot renderer); the boundary model is hardware-correct. See `docs/hdma_ares_audit.md`. | DMA/timing → A−; SA-1 contention | Med |

> **Note (2026-06-11):** the marquee raster-IRQ bug — Doom's letterbox-border
> flicker — turned out **not** to be a Phase 4/5 item. It was a PPU register-latch
> bug (reading `$213F` did not reset the OPHCT/OPVCT byte flip-flop; ares
> `io.cpp:167-169`), fixed surgically. Phases 4–5 remain genuine HDMA/DMA-timing
> accuracy work (mid-line HDMA preemption, per-byte DMA grid, `$4211` hold), but
> they are not what fixed Doom. See `accuracy_scorecard.md`.

**Validation per phase:** the Tom Harte harness, the CT/SMW/SMRPG
audio+visual smoke, `cargo test --workspace --lib`, and the coproc/DMA/PPU
sweep mandated by `.claude/rules/coproc-testing.md`. Per `audible-fixes-
test-first.md`, Phase 2 (APU) needs an ear-check before commit. The Tom
Harte cycle backstop pattern is now established: a test case's `cycles[]`
trace **length** equals the instruction's cycle count, which is asserted
against the core's per-instruction cycle total (SPC700 lands this in
`tests/tom_harte.rs`; Phase 3 mirrors it for the 65c816).

## 6. Risks & guardrails

- **Performance.** `io_cycle` is the single hottest call. Per-access
  advancement must stay branch-light; benchmark each phase (the dev
  profile is `opt-level=1` for playable speed). If per-access proves too
  costly, fall back to per-*group*-of-accesses with the same semantics.
- **Regression surface.** Each phase is large; ship and validate
  independently. Phase 1 is the riskiest single change (it rewires the
  master loop) but is the prerequisite for everything else.
- **Scope.** This is a multi-PR, multi-session milestone — not one change.

## 7. Phase 4 (per-access IRQ/NMI/HDMA) — core done, edge cases deferred

Phases 1–3 are done (see the status header). Phase 4's **core landed**: the
H/V-IRQ is now polled dot-precisely inside `io_cycle` (`d4b0bb6`, HTIME
respected) and the HDMA-vs-DMA preemption landed in Phase 5. Now that the
master clock advances per cycle (Phases 1–3), interrupt *latching* is no
longer pinned to instruction boundaries.

### Deferred Phase-4 delivery edge cases (2026-06-18)

The remaining items are NMI/IRQ **delivery-timing** refinements:
`$4211` TIMEUP hold, the ares "last dot of field" guard
(`vcounter(6) || hcounter(6)`), the htime/vtime 10-clock detection delay
(`vcounter(10)`/`hcounter(10)`), and the `nmitimenUpdate` semantics
(late-NMI-enable + IRQ-disable-clear). They are **deferred**: high
regression risk on luna's deliberate *level-driven* IRQ model (the Doom
level-IRQ fix), and not CLI-validatable — only the GUI exposes the failure.

The **`nmitimenUpdate` late-NMI-enable** is the cautionary tale: a faithful
port (enable NMITIMEN.7 mid-frame → fire NMI now) **black-screened SMRPG's
intro**. `--mem-trace` pinned it: SMRPG runs its intro with NMI *off*,
polling, then enables NMI once (`$81` at `$C3:B9D4`) with `nmi_flag`
stale-set — because luna's `nmi_flag` is **not** a faithful ares `nmiLine`
(ares clears `nmiLine` at VBlank *end*; luna clears `nmi_flag` only on a
`$4210` read). ares wouldn't fire there; luna did → spurious NMI →
corruption. An `in_vblank` gate didn't fix it (the write's scanline is
non-deterministic CPU-vs-GUI). **Prerequisite for re-attempting any of
these: model `nmi_flag` as a real `nmiLine` (cleared at VBlank end + hold
window)** — a sensitive change to the `$4210`/RDNMI path. The IRQ-disable-
clear half is faithful and safe but marginal (a no-op for SMRPG). Recorded
in the `project_nmitimen_late_enable_breaks_smrpg` memory.

Phase 4's remaining value (raster-IRQ effects, scorecard DMA C+→B+) is
**not** gated on any SA-1 game (see below).

### SA-1 game status (investigated 2026-06-02) — NOT cycle-accuracy bugs

The "SA-1 deadlock" that earlier drafts of this plan named as the Phase 4
payoff **does not exist**. Re-investigation with the SA-1 tracers
(`--sa1-log`/`--sa1-side-log`/`--sa1-trace`, the I-RAM-write trace added in
`cc00aaa`) plus `--cpu-trace`/`--mem-trace`/`--peek` showed:

- **Super Mario RPG — works.** The "intro hang at ~frame 2150 / NMIs
  frozen at 1598" is just the **title/demo screen waiting for a Start
  press** the CLI never sends. `luna state --input "1600:0x1000,1610:0,…"`
  reaches New Game → the name-entry screen. The SA-1↔S-CPU mailbox
  round-trips correctly every frame. No scheduler change needed.
- **Kirby Super Star — FIXED (`b4a4525`), was a DMA bus-decode bug, not
  timing.** The S-CPU boots, then DMAs a small stub into WRAM through the
  `$2180` (WMDATA) port and `JMP $000E` into it. But `DmaBusView` (the
  DMA-side bus view in `snes.rs`) only decoded `b_offset <= $3F` (PPU
  regs) and **silently dropped `$2180-$2183`** — the CPU-side `SnesBus`
  handled the WRAM port, DMA did not. So WRAM `$00:000E` stayed `$00`, the
  `JMP $000E` ran a `BRK` → crash-trap vector `$00:FFE6 = $5FFF` → runaway
  → INIDISP stuck forced-blank (black). The SA-1 idling at `$C0:8CB8` on
  I-RAM `$300E` was a **downstream effect** of the crashed S-CPU. Fix:
  added a `wm_addr` field to `DmaBusView` and wired `$80-$83` into its
  `read_b`/`write_b`, mirroring the CPU-side port. Kirby now boots to its
  title and is playable; `inidisp_write_count` 1 → 1911, NMI service ~93%.
  (The earlier "boot diverges / skips the WRAM-stub setup" hypothesis was
  wrong — luna *did* DMA the stub; the writes were dropped on the floor.)

So: **do not validate Phase 4 against SMRPG/Kirby as "deadlock fixes."**
Use the SMRPG name-entry smoke (with `--input`, per
`.claude/rules/coproc-testing.md`) only as a *no-regression* check; SMRPG's
remaining "hang" is the title screen waiting for Start, not a luna bug.
The Kirby crash chain (now resolved) is recorded in the
`project_smrpg_sa1_deadlock` memory.

### History (completed)

**Phase 1 — io_cycle-driven catch-up** (`535d2e7` + earlier). Moved
PPU/APU/coproc advancement out of the end-of-`step()` lump into `io_cycle`
(per access), collapsing the three lump calls into one per-access sync and
removing the DMA coproc double-charge.

**Phase 2 — SPC700 cycle accuracy** (`db19ca8`, `3ab5e21`). Real per-opcode
cycle table + branch-taken penalty, APU driven from the master clock. The
Tom Harte cycle backstop then caught 9 opcodes charging one cycle too many.

**Phase 3 — 65c816 internal/idle cycles** (`2da74fc`, backstop `990c255`).
Added `Cpu::io`/`idle2`/`idle4`/`idle6` and the RMW/branch/stack/jump
idles per ares `wdc65816/memory.cpp`; the Tom Harte cycle backstop (count
`io_cycle` invocations == `cycles[].len()`, gated by `LUNA_TOM_HARTE_CYCLES`,
sampled via `LUNA_TOM_HARTE_SAMPLE=N`) drove it to 0 mismatches at 100%
state. WAI/STP excluded as halt artifacts.

**Phase 3b — 65c816 entry-for-entry `cycles[]` bus-trace oracle.** The Phase-3
backstop only checked the cycle *count*; this upgrade checks the per-cycle
**bus grammar** (kind + addr + value, entry for entry) against the Tom Harte
`cycles[]`. `RamBus` (`luna-bus/src/testing.rs`) gained an opt-in per-cycle
trace (`enable_trace`/`take_trace`, `TraceKind::{Read,Write,Internal}`); the
harness `classify`/`compare_trace` (`tests/tom_harte.rs`) diff luna's
read/write/idle sequence to the reference. Result: **4,740,000 / 5,040,000
entries match (94.05%)** at 0 state-fail / 0 count-mismatch. The oracle found
and drove out **three real ares bus-order divergences** (all state-invariant,
fixed in `opcodes.rs`): **JSL `$22`** and **JSR `($abs,X)` `$fc`** pushed the
return address *after* fetching the whole operand instead of interleaving the
push between operand fetches (ares `instructionCallLong` /
`instructionCallIndexedIndirect`); and **all 16-bit RMW** (INC/DEC via
`modify_memory`, ASL/LSR/ROL/ROR/TSB/TRB via `modify_memory_with`) wrote the
**low byte first** instead of ares' **high-byte-first** write-back
(`instructions-modify.cpp` `instruction*Modify16`). The residual 30 diverging
opcodes are **ares-faithful abstractions, not luna bugs**: the 28 emulation-mode
RMW (`*.e`) dummy-write-back that ares models as `idle()`, and **WDM `$42`**
whose 2nd-byte `fetch()` (a read in ares) Tom Harte traces as internal. The
oracle is informational (not gated) precisely because matching it for those 30
would mean *diverging from ares* — the opposite of the faithful-port pillar.

**Phase 5b — SA-1 real per-access cycles.** ✅ done (`097ffe7`). Replaced the flat
`MCLK_PER_SA1_INSN = 6` lump in `Sa1Chip::step_coproc` with a signed
mclk *deficit*: each main-CPU advance adds to it, each SA-1 instruction
subtracts its real cost. The cost is accumulated per bus access in
`Sa1Bus` (`read`/`write` add `Sa1Mapper::sa1_region_steps`, `io_cycle`
adds 1) and charged as `steps × 2 mclk/step`, faithful to ares
`coprocessor/sa1/memory.cpp` (IO/ROM/IRAM/open-bus = 1 step, BWRAM = 2
steps, `idle()` = 1 step). The `conflict()` BWRAM/IRAM contention steps
(Increment B) are not yet modelled. The deficit is capped
(`DEFICIT_CAP`) to bound any catch-up burst, floored at 1 step/insn so a
zero-cost path can't stall the loop, and left unserialized (a
≤1-instruction transient, reset to 0 on load). Validated: SMRPG intro +
post-Start name-entry render cleanly, `nmis_serviced` ≈ 5588 at
`-n 55000000` (well past the title-wait plateau); and the full
`tools/validate-hdma-corpus.sh` sweep (Contra III / Tales / F-Zero /
FF6 / Gradius III / SCV4 / Super Metroid / Yoshi's Island / Axelay)
renders with no banding, missing layer, or garbled split.
