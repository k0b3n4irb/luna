# Cycle-Accuracy Milestone — APU↔CPU↔PPU Synchronization Plan

**Status:** in progress — **Phases 1, 2, 3 landed; Phase 4 in progress;
Phase 5 increments 0+1 landed** (mid-frame DMA↔HDMA preemption at scanline
boundaries; `f3bd002` resumable segment API + `d2a17fc` segmented driver).
Remaining Phase 5: inc 2 (true dot-276 `hdmaPosition` sub-line timing) and
Phase 5b (SA-1 real per-opcode cycles) — both deferred as isolated
follow-ons.
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
- Phase 4 (per-access IRQ/NMI/HDMA): **partially done.** Dot-precise H/V
  IRQ `d4b0bb6` (fixes modes 01/11; unblocks DKC's H-IRQ raster) + HDMA
  time cost (18 mclk/line + 8/byte, charged via the `sched_advance` stall
  loop). Remaining: $4211 TIMEUP hold, the ares "last dot" guard, htime==0
  delay. The DMA↔HDMA preemption part needs steppable DMA → moved to
  Phase 5.
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
| **4. Per-access IRQ/NMI/HDMA** | Poll interrupts + HDMA in `io_cycle`: dot-precise H/V-IRQ (HTIME respected), H≈278 HDMA-vs-DMA preemption, RDNMI as a true 4-cycle hold. | Raster-IRQ games, DMA/timing C+→B+ | Med |
| **5. DMA/HDMA cycle-stepping** ⏳ inc 0+1 done | Segmented sync DMA (`f3bd002`); HDMA preempts a mid-frame DMA at scanline boundaries (`d2a17fc`, line-granular). Open: inc 2 dot-276 sub-line `hdmaPosition`; Phase 5b SA-1 real cycles. | DMA/timing → A−; SA-1 contention | Med |

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

## 7. Next step — Phase 4 (per-access IRQ/NMI/HDMA)

Phases 1–3 are done (see the status header). The active work is Phase 4:
poll interrupts + HDMA **inside `io_cycle`** instead of only at
instruction boundaries — dot-precise H/V-IRQ (HTIME respected), H≈278
HDMA-vs-DMA preemption, RDNMI as a true 4-cycle hold. Now that the master
clock advances per cycle (Phases 1–3), interrupt *latching* is the last
piece still pinned to instruction boundaries. Phase 4 is justified on its
own merits — raster-IRQ effects and HDMA-vs-DMA timing (scorecard DMA
section C+→B+) — **not** by any SA-1 game (see below).

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
