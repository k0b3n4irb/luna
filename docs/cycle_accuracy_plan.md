# Cycle-Accuracy Milestone — APU↔CPU↔PPU Synchronization Plan

**Status:** in progress — **Phases 1 & 2 landed**; Phase 3 active.
- Phase 1 (io_cycle-driven per-access catch-up + DMA coproc double-charge
  fix): done — APU `cfef84a`/snes.rs, PPU sched, coproc `7c5bef0`.
- Phase 2 (SPC700 per-opcode cycles + branch-taken penalty + master-clock
  cadence): done `cfef84a`; cycle backstop + 9 off-by-one fixes `081e78d`.
- Phases 3–5: see the table in §5.
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
| **3. 65c816 cycle accuracy** 🔨 active | Have the CPU core call `io_cycle` at the correct *intra-instruction* points with correct per-cycle costs (read/write/idle ordering). The core already emits `io_cycle` per **bus** access (Phase 1 relies on it); what's missing is the **internal/idle** cycles — RMW dead cycles, branch-taken / page-cross penalties, MVN/MVP register-adjust cycles — plus the Tom Harte `cycles[]` backstop to drive them out (cf. the SPC700 Phase-2 method). | Tom Harte 65c816 `cycles[]`, MVN/MVP gateable, A−→A | High — touches every opcode/addressing path |
| **4. Per-access IRQ/NMI/HDMA** | Poll interrupts + HDMA in `io_cycle`: dot-precise H/V-IRQ (HTIME respected), H≈278 HDMA-vs-DMA preemption, RDNMI as a true 4-cycle hold. | Raster-IRQ games, DMA/timing C+→B+ | Med |
| **5. DMA/HDMA cycle-stepping** | Per-byte DMA interleaved with the master clock; mid-DMA HDMA preemption; single coproc sync. | DMA/timing → A−; SA-1 contention | Med |

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

## 7. Next step — Phase 3 (65c816 internal/idle cycles)

Phases 1 & 2 are done (see the status header). The active work is Phase 3.
The 65c816 core is **already per-access** (every `read`/`write` calls
`io_cycle`), so this is not a rewrite — it is filling in the **internal
(non-bus) cycles** the core currently skips. Concretely:
1. Add an idle-cycle primitive to the core (an `io(bus)` that charges one
   internal cycle, ~6 mclk FAST, per ares `memory.cpp` `io()`), and a way
   to total cycles per instruction for validation.
2. Insert idle cycles at the ares-accurate points: RMW dead cycles,
   branch-taken (+1) and page-cross (+1) penalties, indexed page-cross
   reads, MVN/MVP register-adjust cycles, RTI/RTS/JSR internal cycles, etc.
3. Wire the Tom Harte `cycles[]` backstop (count `io_cycle` invocations in
   the harness bus, assert `== cycles.len()`; strict under
   `LUNA_TOM_HARTE_REQUIRE=1`) — let it drive out every missing idle cycle,
   exactly as the SPC700 Phase-2 backstop surfaced its 9 off-by-ones.
4. Validate: full Tom Harte (state unchanged, cycles now gated), CT/SMW
   audio smoke, coproc/DMA/PPU sweep, SMRPG screenshot.

### History (completed)

**Phase 1 — io_cycle-driven catch-up** (done). Moved PPU/APU/coproc
advancement out of the end-of-`step()` lump into `io_cycle` (per access),
collapsing the three lump calls into one per-access sync and removing the
DMA coproc double-charge.

**Phase 2 — SPC700 cycle accuracy** (done). Real per-opcode cycle table +
branch-taken penalty, APU driven from the master clock. The Tom Harte
cycle backstop then caught 9 opcodes charging one cycle too many.
