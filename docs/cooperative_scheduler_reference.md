# Cooperative Scheduler — ares reference + luna port plan

Governed by `.claude/rules/faithful-port-and-dichotomy.md`. This is the
blueprint for porting ares' cooperative GSU↔CPU timing into luna, **faithfully,
step by step, by dichotomy**. Written reference-first from the actual ares
source (`ares/ares/scheduler/{scheduler,thread}.{hpp,cpp}`,
`ares/sfc/coprocessor/superfx/{superfx,timing,bus}.cpp`).

## 1. ares' model (the target grammar)

**Thread** (`thread.hpp/cpp`): each emulated component is a cothread with:
- `_frequency` (Hz), `_scalar = Second / frequency` (`Second = (u64-1)>>1`),
  `_clock` (u64) — an **absolute time** value on a timebase shared by all
  threads. A fast thread has a *small* scalar, so each of its cycles advances
  `_clock` by less time → more cycles fit in the same time window.
- `step(clocks)`: `_clock += _scalar * clocks`.
- `synchronize(other)`: `while(other.clock() < this.clock()) co_switch(other.handle())`
  — runs `other`'s cothread until it catches up to `this`'s absolute time.

**Scheduler** (`scheduler.cpp`): owns the threads, `enter()`/`exit()` via
`co_switch`, keeps `_clock`s bounded (subtracts the minimum on `exit`), assigns
`_uniqueID` to break clock ties. The CPU is the **primary** thread.

**SuperFX integration** (`superfx.cpp`/`timing.cpp`/`bus.cpp`):
- `SuperFX::main()`: if `!sfr.g` → `step(6)` (idle); else run one GSU op.
- `SuperFX::step(clocks)`: services the romcl/ramcl buffer delays, then
  `Thread::step(clocks)` + **`Thread::synchronize(cpu)`** — so after EVERY
  internal step (each memory access) control can return to the CPU.
- `readIO/writeIO` ($3000-$32FF MMIO): **`cpu.synchronize(*this)` FIRST** — the
  GSU is caught up to the CPU's exact time before the register is read/written.
- The GSU's own bus accesses (`read()` in memory.cpp) **block**:
  `while(!regs.scmr.ron){ step(...); }` until it owns ROM/RAM (arbitration).
- `clsr` selects the GSU Frequency (21.48 MHz fast / 10.74 MHz slow) → it lives
  in the **scalar**; the per-op `step(clsr?5:6)` counts are GSU *clocks*.

**Net:** all components advance on one absolute time axis; before the CPU
observes the GSU it runs the GSU to the CPU's exact time; the GSU yields to the
CPU after each memory access; GSU bus accesses arbitrate.

## 2. luna's model (current grammar)

CPU-driven, single-threaded. `total_mclk` is the master clock (master-cycle
units, the 21.48 MHz domain — the CPU IS the timebase). Per CPU bus access,
`SnesBus::read_inner`/`io_cycle` → `advance_time` → `mapper.step_coproc(mcycles)`;
per DMA byte, `DmaBusView::tick` → `step_coproc`. The GSU runs whole
instructions bounded by `clock_deficit`:
`clock_deficit += main_mclk; while(g && deficit>0){ run_one(); deficit -= cycles }`.

**`clock_deficit` IS `synchronize`-to-clock in disguise:** it equals
`cpu_clock_advanced − gsu_clock_advanced`; running until `deficit ≤ 0` ==
running the GSU until `gsu_clock ≥ cpu_clock`. So luna already does
synchronize-before-access (via io_cycle) and per-DMA-byte interleave.

## 3. The precise deviations (what to translate)

1. **Rate / scalar missing.** luna deducts `cycles` (1 master clock per GSU
   clock) regardless of clsr. Faithful: a GSU clock costs `clsr?1:2` master
   clocks (slow = master/2). Equivalent to ares' scalar. *(Tested standalone in
   the batched model → authentic but laggy; land it only as part of the whole
   faithful model, GUI-validated.)*
2. **Granularity: instruction vs step.** luna's `run_one` is atomic
   (overshoot ≤ one instruction ≈ ≤40 cycles). ares yields/synchronises after
   each `step()` (each memory access ≈ ≤6 cycles). The GSU must become
   **sub-instruction steppable** with an exact clock.
3. **No bus arbitration.** luna's `gsu_read`/`gsu_write` are instantaneous and
   never block; the SNES/DMA side returns open-bus on contention but the GSU
   never stalls waiting to own ROM/RAM.

Engine logic itself is **proven byte-exact** (gsu_differential / gsu_trajectory
harnesses, 0 divergence) — do NOT touch it; only the grammar above.

## 4. Incremental plan (dichotomy into landable steps)

Each step: build (hook) + `cargo test --workspace --lib` + GUI-validate the GSU
titles (Star Fox / Yoshi / Doom / Stunt Race / SF2) + differential-measure vs a
Mesen reference trace BEFORE the next step. Revert any step that regresses.

- **Step 1 (this doc).** Establish the target architecture + plan. ✅
- **Step 2.** Introduce an explicit GSU `Thread`-like clock (absolute,
  master-cycle units) + `scalar` (clsr), replacing the bare `clock_deficit`
  counter with a named, documented abstraction. Behaviour-preserving (keep the
  1:1 rate active by default; rate change lands in a later, validated step) so
  it cannot regress on its own.
- **Step 3.** Make the GSU sub-instruction steppable: `step_coproc` advances the
  GSU to the exact target clock, the GSU pausing at `step()` (memory-access)
  boundaries — the ares granularity. Requires resumable instruction execution
  (explicit cycle-state, no co_switch needed since luna stays CPU-driven).
- **Step 4.** Bus arbitration: the GSU stalls (advances its own clock) while it
  doesn't own ROM/RAM, mirroring ares' `while(!ron){step}`; the SNES/DMA side
  reads the GSU RAM at the exact synchronised cycle.
- **Step 5.** Land the faithful scalar (clsr rate) as the final piece, now that
  the interleave is exact, and GUI-validate the whole.

Stop and measure at each step. Never land more than one deviation-fix at a time.

## 4c. RESOLVED — the garble was a level-vs-edge coprocessor IRQ bug (2026-06-08)

The dichotomy below (§4b) correctly concluded the garble is NOT GSU cycle-timing.
The actual root cause, pinned confound-free by the NMI-aligned WRAM differential
(`luna wram-trace` vs Mesen2), was in the **CPU IRQ model**, not the GSU:

luna bridged the coprocessor's **level** `/IRQ` line into the 65C816's **sticky
edge** `pending_irq`, re-arming it every step. During Star Fox's IRQ handler, the
latch was re-armed (while `I` masked it) and survived the `RTI`, so one GSU IRQ
was serviced **twice** (frame 43: handler ran 2× vs Mesen's 1×). The spurious
2nd pass set three object-table flags the hardware keeps clear → garble by frame
200 (832 divergent WRAM bytes). **Fix (commit 86e9702):** a dedicated level
`Cpu::irq_line` sampled fresh each instruction; the H/V-timer keeps its edge
latch. GUI-validated "le jour et la nuit" across all GSU 3D titles. 832→21
divergent bytes @ frame 200.

**The cooperative cycle-scheduler (steps 3-5) is now confirmed unnecessary for
the garble.** A tiny residual remains: a one-time ~1-frame phase slip around
frame 142 (luna frame 142 == Mesen frame 141, byte-exact), re-syncing by frame
200 and leaving ~16 off-by-one counter bytes. THAT residual *is* GSU completion-
timing (cycle-rate) — cosmetically invisible. If ever pursued, the harness
(`luna wram-trace`) makes the cycle-rate port measurable; but it is low-priority.

## 4b. STEP 3 DICHOTOMY RESULT — cycle-timing is NOT the cause (2026-06-08)

Before the risky resumable-engine rewrite, the dichotomy measurement
**redirected** us (this is the method working):

- GSU overshoot distribution: ~89% of `step_coproc` calls already within ares'
  ~6-cycle granularity; tail ≤96 cycles. A ≤96-cycle refinement cannot fix a
  **frame-level** (357,368-cycle) phenomenon.
- luna's GSU runs **12-30× more instructions per STOP** than Mesen (~19-46k vs
  ~1.5k). Opcode mix: luna is branch/loop+NOP heavy (a long path); Mesen is
  store-heavy (real work).
- A plot loop at `$01:CFxx-$D017` is **38%** of luna's GSU work in its window.
  An initial finding "Mesen never runs `$1D004`" was a **FRAME-MISALIGNMENT
  ARTIFACT** — CORRECTED: Mesen runs `$1D004` heavily at frames **200-206**
  (its hottest PC, 1136×); luna runs it at frames **202-217**. So it is a
  REAL, shared routine, NOT phantom. r14/r8 differing at the `$8295` GO is
  likewise the two emulators at different scene moments, NOT a CPU-fed bug
  (verified: the CPU writes ZERO to R14/R8 via MMIO — they're GSU-internal).

**Honest conclusion:** dichotomy confirmed (a) the residual is NOT GSU
cycle-timing (cooperative cycle-port steps 3-5 SHELVED; steps 1-2 kept as clean
foundation), and (b) it is FRAME-LEVEL — luna spends ~2× more PPU frames in the
`$D0xx` phase (202-217 vs 200-206) ⟹ luna's scene/reveal progresses at a
different rate. BUT the specific cause is **NOT cleanly isolable by scene-level
differential** because luna and Mesen have an irreducible boot-frame offset:
every "luna vs Mesen at frame N" comparison is confounded. The ONLY clean
differential is **state injection** (the gsu_trajectory harness injects Mesen's
GSU state and gets BYTE-EXACT output — engine + first GO-run proven). To find
the cross-GO / CPU-side divergence cleanly, the differential must be extended to
**full-system state injection** (CPU + WRAM + PPU + GSU from a Mesen savestate,
run both forward, bisect the first divergence) — a large undertaking. Without
it, scene-level comparisons keep producing misalignment artifacts (as the
"phantom routine" did). **Next, if pursued:** full-system differential via
savestate injection; OR accept the residual as a scene-rate cycle-accuracy gap.

## 5. Open architectural question (decide at Step 3)

luna stays **CPU-driven** (no global cothread rewrite) — the GSU becomes a
finely-steppable state machine the CPU drives to an exact clock, which
reproduces ares' synchronize semantics for the GSU↔CPU pair WITHOUT converting
PPU/APU to cothreads. If Steps 2-5 don't fix it, the fallback is the full
cothread scheduler (convert the whole loop) — but try the scoped model first
(smaller blast radius, same grammar for the GSU↔CPU pair that matters).
