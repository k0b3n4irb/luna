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

## 6. SCOPE — faithful GSU cooperative scheduler (2026-06-09, evidence-backed)

Prerequisite for landing DRAM refresh (§4d) and closing the cycle residual.
This scope is grounded in two measured experiments, not theory.

### 6.1 What is already faithful (do NOT touch)
- **Engine: byte-exact** vs Mesen (`gsu_trajectory_vs_mesen`, `gsu_differential_
  vs_mesen`). Opcode logic, `romcl`/`ramcl` ROM/RAM-buffer latency, the internal
  `step(clocks)` that services buffers + accumulates `self.cycles` — all mirror
  ares. **Any scheduler change must keep these green.**
- **`clock_deficit` IS ares `synchronize`** at instruction granularity:
  `step_coproc(mclk)` adds `mclk` to the deficit and runs the GSU until
  `gsu_clock ≥ cpu_clock`. Called per CPU bus access (≈ synchronize-before-access)
  and per DMA byte.

### 6.2 The deviations, RE-RANKED by evidence (not the old §3 order)
1. **Granularity / stall-interleave (PRIMARY — the Star Fox blocker).**
   `run_one()` is atomic and `step_coproc` runs *whole instructions* until the
   deficit clears. Worse, CPU **stalls** (refresh/DMA/HDMA) are charged as a
   single lump at the scanline boundary, so the GSU is advanced in a **40-mclk
   burst** there instead of smoothly. ares yields after *every* `step()` (each
   memory access, ≈ ≤6 GSU clocks). **Measured:** adding DRAM refresh (§4d)
   blacked out Star Fox (WRAM @f200 21→2087) — the lump-burst GSU stepping can't
   interleave at the fidelity refresh demands.
2. **Rate / scalar (SECONDARY — NOT the Star Fox blocker).**
   `mclk_per_gsu_clock = 1`; faithful is `clsr ? 1 : 2`. **Measured:** setting it
   to `clsr?1:2` did NOT fix the refresh regression — Star Fox's *intro runs
   clsr=fast* (rate already 1). Matters for clsr=slow scenes (some Stunt Race FX
   / Doom), but is not the architectural blocker. Cheap to land once granularity
   is fixed.
3. **Bus arbitration (TERTIARY).** `gsu_read`/`gsu_write` skip the `ron`/`ran`
   blocking (`while(!ron) step`). The `scmr_ron`/`scmr_ran` bits exist but Phase 2
   skipped enforcement. Add only if a divergence pins to it.

### 6.3 Architecture decision: scoped CPU-driven, NOT a cothread rewrite
luna's `clock_deficit` already reproduces `synchronize` for the GSU↔CPU pair, so
a full ares cothread scheduler (converting the whole emulator loop to
`co_switch`) is unnecessary and Rust-hostile (no native `co_switch`; huge blast
radius). Keep the CPU as driver. The one real change: make the GSU **resumable at
`step()` (memory-access) granularity** so `step_coproc` advances it to an EXACT
master-clock target, pausing mid-instruction — ares' per-step yield WITHOUT
cothreads.

### 6.4 The hard part — resumable engine (three strategies)
`run_one()` today runs a whole instruction atomically (each internal `step()`
just accrues cycles). To suspend mid-instruction at a `step()` boundary:
- **(a) Explicit cycle-state machine** — thread "clocks remaining to target"
  through `execute()`; on hitting 0 mid-op, save a resume point. Most faithful,
  but every opcode becomes resumable. HIGH effort/risk (~1-2 wk).
- **(b) Smooth-stall distribution (the SPIKE)** — keep atomic `run_one`, but
  STOP lump-stepping the GSU at stall boundaries: distribute the refresh/DMA
  stall into the GSU's normal per-access deficit so it advances smoothly, and/or
  cap `step_coproc` to ≤6-clock sub-steps. If the Star Fox break is the *burst*
  (not the per-instruction overshoot), this alone fixes it. LOW effort, decides
  everything. **DO THIS FIRST.**
- **(c) Stackful coroutine** (`corosensei`/`generator` crate) — wrap the engine
  in a coroutine that yields at each `step()`. Closest to ares, minimal engine-
  code change, adds a dep + per-yield cost. MEDIUM effort (~3-5 d).

### 6.4b SPIKE RESULT (2026-06-09) — cheap path REFUTED, resumable engine required
Ran strategy (b): re-applied refresh, then stopped lump-stepping the GSU during
the stall (`is_stall` guard so `step_coproc` only runs on real CPU-instruction
time). **No change** — Star Fox still black, WRAM @f200 still **2087** (identical
to the lump version). So the burst-stepping is NOT the cause. Two decisive facts:
- **Refresh itself is correct** for non-GSU: DKC frame_count 1593→1647 (CPU
  correctly slowed), frame-89 divergence → 0. The implementation is sound.
- **luna+refresh matches Mesen *worse* (2087) than luna-without-refresh (21)** —
  even though Mesen HAS refresh. The only explanation: luna's GSU integration
  carries an error that the *missing* refresh was COMPENSATING for. Adding the
  correct CPU slowdown unmasks it; Star Fox crashes into a black spin-loop
  (frame_count went the wrong way: 3026→2655 = CPU *faster*, i.e. stuck looping).

**Verdict:** the blocker is NOT stall-stepping and NOT the clsr rate — it is the
GSU integration's coarse interleave masking a latent timing error. The cheap fix
(b) is ruled out. **The faithful sub-instruction resumable engine (strategy a or
c) is required** — there is no shortcut. Next step before committing the 1-2 week
build: bisect WHERE luna+refresh first diverges from Mesen+refresh (the
`wram-trace` first-divergence frame) to pin the exact integration error the
resumable engine must fix — that turns the rewrite from speculative to targeted.

### 6.4c TARGETED BISECTION (2026-06-09) — it's CPU/upload timing, NOT the GSU engine
Bisected the FIRST divergence of luna+refresh vs Mesen+refresh (both have refresh
now). Result CONVERGES and REFINES the scope:
- First divergence: **frame 142** — the *exact same bytes* (`$7E0045/0047/004D`,
  `$7E188B/188D`, luna `00` vs Mesen `2C/30/68/10`) and the *exact same event* as
  the original Star Fox slip from the I/O-timing work: the **GSU-launch /
  VRAM-upload phase**.
- luna+refresh launches the GSU (`$301F` GO) at **frame 143, scanline 55**;
  Mesen+refresh at **frame 142, scanline 55**. **Same scanline, ONE FRAME LATE.**
- So it is NOT a sub-frame interleave error — it's a clean 1-frame lateness:
  luna's CPU/VRAM-upload runs slightly slower than Mesen's (both refreshed), so
  the multi-frame upload finishes one frame later and the GSU launch tips over
  the vblank boundary, then cascades to the black crash. The MISSING refresh
  masked it (no-refresh = faster CPU = upload on time = launch f142, re-synced).

**Revised conclusion:** the resumable GSU engine (§6.4) is NOT the frame-level
blocker — the GSU isn't even running yet at the divergence (it's the CPU's
pre-launch upload). The blocker is a **residual CPU instruction/access timing
inaccuracy** (luna ~1 upload-frame slower than Mesen WITH the same refresh),
amplified to a whole frame by vblank quantization. Candidates: a remaining
per-instruction or per-access cycle cost (the upload loop is `STA $2118`/`$2119`
@6 + loop control), or a small over/under in the refresh amount/position vs ares
(40 @ hcounter 538). NEXT: differential the VRAM-upload *duration* (mclk for the
upload) luna vs Mesen frame-by-frame to find the per-iteration cycle gap — that
pins the exact cost, likely a much smaller fix than a GSU-engine rewrite. The
resumable engine is still wanted for full sub-frame fidelity, but it is NOT what
unblocks refresh; exact CPU timing is.

### 6.5 Staged plan (each oracle-gated)
- **Spike (DONE, see §6.4b):** strategy (b) refuted as the *stall* fix.
- **Bisection (DONE, see §6.4c):** blocker is CPU/upload timing (1-frame-late GSU
  launch), not the GSU engine. Pursue exact CPU timing first.
- **Stage 1:** land the chosen granularity fix. Oracle: trajectory byte-exact;
  Star Fox `wram-trace` @f200 with refresh DROPS toward 0 (not 2087); GUI clean.
- **Stage 2:** land DRAM refresh (§4d patch) + `clsr` scalar — now they compose.
  Oracle: DKC frame-89/`$0028` aligned; all GSU titles render+play; SA-1 (SMRPG)
  + non-GSU no regression.
- **Stage 3:** `ron`/`ran` arbitration, only if a residual pins to it.

### 6.6 Oracles (must hold throughout)
`gsu_trajectory`/`gsu_differential` byte-exact · Star Fox `wram-trace` vs Mesen
@f200 → toward 0 (≤21 floor, never up) · GUI: Star Fox / Doom / Stunt Race FX /
SF2 render+play clean (user-validated, non-negotiable) · DKC frame-89 aligned ·
full `--lib` + smoke (SMRPG SA-1, RPM interlace).

### 6.7 Effort / risk / recommendation
Spike = ½-1 day, low risk, decides the strategy. If (b) works: total ≈ 2-3 days
incl. refresh. If (a)/(c) needed: ≈ 1-2 weeks. **Recommendation: run the spike
first** — it's cheap and converts this scope from plan to validated path. Don't
commit to the resumable-engine rewrite until the spike rules out the cheap fix.

---

## 4d. Cycle-timing residual — DRAM refresh found, but blocked on GSU timing (2026-06-09)

Chasing the residual cycle-timing drift (DKC intro fires ~2 frames early; Star
Fox 1-frame slip): the dichotomy pinned a **real missing feature — DRAM
refresh**. ares (`cpu/timing.cpp:21-29,70-72`) halts the S-CPU **40 master
cycles every scanline** (5×`step(6)+step(2)`, hcounter ≈ 538) to refresh work
RAM. luna omitted it, so the CPU ran ~40 mclk/line (≈2.9 %/frame) too fast and
multi-frame tasks finished early. luna ≡ Mesen byte-exact through DKC frame 88,
then the intro-setup phase fired early — a non-WRAM cycle-budget difference.

Implemented faithfully (per-scanline 40-mclk CPU stall, charged like the HDMA
stall in `sched_one_line`). Result:
- **Helped non-GSU timing:** DKC first divergence moved frame 89 → gone (0 diffs
  at f89, was 8); residual 17 → 13 bytes; CPU per-frame rate correctly dropped.
- **REGRESSED the GSU titles — reverted.** Star Fox went BLACK (title + level);
  WRAM diff @ frame 200 blew up 21 → 2087. Cause: the refresh re-advance also
  steps the coprocessor (correct — the GSU runs on its own clock during the
  S-CPU work-RAM refresh pause), so the GSU gains ~40 mclk/line *relative to the
  CPU*. luna's GSU integration timing (batched `step_coproc` / `clock_deficit`)
  is approximate and was implicitly tuned WITHOUT refresh; composing the two
  re-misaligns the GSU launch and breaks rendering.

**Conclusion:** DRAM refresh is a genuine missing piece and the residual's main
cause, but it **cannot land until the GSU timing is itself faithful** (the
cooperative cycle-interleave model, §1-5 above) so the two compose. This is the
exact "translate the whole grammar" point: a faithful CPU-timing fix exposes the
un-translated GSU scheduling. Landing order must be: faithful GSU cooperative
scheduler FIRST, then DRAM refresh. Until then the residual stays (invisible;
games play fine). Patch kept in git history / reflog for when the GSU port lands.

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
