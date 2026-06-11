# GSU timing residuals — ares scheduler model + DRAM refresh (reference)

> **RETRACTED 2026-06-11.** This doc was originally written as a port plan
> to fix the Doom letterbox-border flicker by faithfully porting ares'
> cooperative GSU↔CPU scheduler. **That central thesis is retracted.** The
> flicker was root-caused as a **PPU register bug**: reading `$213F`
> (STAT78) did not reset the OPHCT/OPVCT byte-read flip-flop (ares
> `io.cpp:167-169`), so V-counter reads were 50% wrong. That sent Doom's
> raster IRQ handler down a no-ack branch which re-fired the H/V IRQ
> ~200×/frame and pinned the S-CPU at `I=1`. It was **NOT** a GSU
> scheduler / cooperative-thread / "~3.3× slow loop" / state-injection-
> oracle problem: the GSU engine is byte-exact and its per-task timing
> matches Mesen within 1%. See `docs/accuracy_scorecard.md` and the
> `project_doom_flicker_opvct_latch` memory.
>
> What survives, and why this doc is kept: the ares Thread/Scheduler
> cothread model below is an accurate reference for genuine remaining
> timing residuals, and two real items are recorded here — the missing
> **DRAM refresh** feature, and the **shipped SCMR GSU-side bus
> arbitration** stall. Everything that framed the flicker as a scheduling
> problem has been cut.

Governed by `.claude/rules/faithful-port-and-dichotomy.md`. Written
reference-first from the actual ares source
(`ares/ares/scheduler/{scheduler,thread}.{hpp,cpp}`,
`ares/sfc/coprocessor/superfx/{superfx,timing,bus}.cpp`).

## 1. ares' model (the target grammar) — REFERENCE

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

## 2. luna's model and the mapping to ares

CPU-driven, single-threaded. `total_mclk` is the master clock (master-cycle
units, the 21.48 MHz domain — the CPU IS the timebase). Per CPU bus access,
`SnesBus::read_inner`/`io_cycle` → `advance_time` → `mapper.step_coproc(mcycles)`;
per DMA byte, `DmaBusView::tick` → `step_coproc`. The GSU runs whole
instructions bounded by `clock_deficit`:
`clock_deficit += main_mclk; while(g && deficit>0){ run_one(); deficit -= cycles }`.

**`clock_deficit` IS ares' `synchronize`-to-clock at instruction
granularity.** It equals `cpu_clock_advanced − gsu_clock_advanced`; running
until `deficit ≤ 0` == running the GSU until `gsu_clock ≥ cpu_clock`. Called
per CPU bus access (≈ synchronize-before-access) and per DMA byte, luna already
reproduces ares' synchronize semantics for the GSU↔CPU pair without converting
the whole emulator loop to cothreads. Mesen's `Gsu::Run()` (run whole
instructions until `_state.CycleCount < masterClock * _clockMultiplier`) is the
same whole-instruction model — luna matches the gold standard here, and no
sub-instruction "resumable engine" is needed.

The GSU engine logic itself is **proven byte-exact** (`gsu_differential` /
`gsu_trajectory` harnesses, 0 divergence vs Mesen) — opcode logic, `romcl`/
`ramcl` ROM/RAM-buffer latency, and the internal `step(clocks)` that services
buffers + accumulates `self.cycles` all mirror ares. Do NOT touch it.

## 3. Real residual — DRAM refresh (missing feature)

A genuine missing ares feature, independent of the (retracted) flicker thesis.

ares (`cpu/timing.cpp:21-29,70-72`) halts the S-CPU **40 master cycles every
scanline** (5×`step(6)+step(2)`, hcounter ≈ 538) to refresh work RAM. luna
omits it, so the CPU runs ~40 mclk/line (≈2.9 %/frame) too fast and multi-frame
tasks finish slightly early.

A faithful implementation (per-scanline 40-mclk CPU stall, charged like the HDMA
stall in `sched_one_line`) was prototyped and **helped non-GSU timing**: DKC's
first WRAM divergence moved from frame 89 → gone (0 diffs at f89, was 8);
residual 17 → 13 bytes; the CPU per-frame rate correctly dropped.

It is **not landed.** When refresh re-advances the master clock it also steps
the coprocessor (correct — the GSU runs on its own clock during the S-CPU
work-RAM refresh pause), so composing it with luna's GSU integration shifts the
GSU launch phase and regressed the GSU titles (Star Fox blacked out, WRAM diff
@ frame 200: 21 → 2087). The regression was bisected to a **sub-frame phase
residual** in luna's pre-GSU-launch CPU/VRAM-upload timing that refresh tips
across a vblank boundary, not to the GSU engine or the upload loop (both match
Mesen at frame and instruction granularity). Closing that residual requires
comprehensive sub-cycle CPU-position fidelity for an **invisible** payoff (all
games already play fine).

**Status:** DRAM refresh is a real, faithful missing piece. Keep the patch in
git history; revisit only as part of a deliberate full-cycle-accuracy effort.
Until then the residual stays — invisible, games play fine.

## 4. Shipped — SCMR GSU-side bus arbitration (faithful correctness fix)

The one structural piece luna was genuinely missing relative to both
references, ported and **shipped**. Faithful, byte-exact-preserving, and a real
correctness improvement (though, per the retraction, neutral on the Doom
flicker — that was the PPU OPVCT bug).

**The reference (Mesen `Core/SNES/Coprocessors/GSU/`):**
```cpp
void Gsu::WaitForRomAccess(){ if(!_state.GsuRomAccess){ _waitForRomAccess=true; _stopped=true; } }
void Gsu::WaitForRamAccess(){ if(!_state.GsuRamAccess){ _waitForRamAccess=true; _stopped=true; } }
void Gsu::UpdateRunningState(){ _stopped = !SFR.Running || _waitForRamAccess || _waitForRomAccess; }
// SCMR write: GsuRamAccess=(v&8); GsuRomAccess=(v&0x10); if granted, clear _waitFor*; UpdateRunningState.
```
When the GSU accesses ROM/RAM it does **not own** (SCMR ron/ran=0), it stops —
`Run()`'s loop exits, `Step()` advances the clock with no work — and resumes
when the CPU grants access via SCMR. The stall is at **instruction
granularity** (the current `Exec()` finishes, then no more); no mid-instruction
resumability is needed. ares uses the same idea via the blocking
`while(!regs.scmr.ron){ step(...); }` in its GSU bus path.

**luna's gap (was):** luna already had the `scmr_ron`/`scmr_ran` bits, the
CPU-side `busy_rom_vector` returned on CPU ROM reads during GSU run
(`superfx.rs:1453`), and RAM-busy gating (`1461`). It **lacked only the
GSU-SIDE stall**: `gsu_read`/`gsu_write` read/write ROM/RAM directly and never
stalled on `!scmr_ron`/`!scmr_ran`.

**The fix (shipped):** `superfx.rs` gained `wait_for_rom_access` /
`wait_for_ram_access`, with `check_rom/ram_access` at every `gsu_read`/
`gsu_write`; `step_coproc` gates its loop on `!stalled()` and drains the deficit
while parked; `set_scmr` releases the wait flags on grant and resumes. Verified:
`gsu_trajectory` / `gsu_differential` still byte-exact; Star Fox renders
(non-regression); the stall engages as expected (~23 RAM stalls/frame on Doom,
where the GSU renders the next frame while the CPU reads the framebuffer under
`ran=0`). A faithful standalone correctness fix, neutral on visible output.

## 5. Architecture note

luna stays **CPU-driven** (no global cothread rewrite). The `clock_deficit`
mechanism already reproduces ares' `synchronize` for the GSU↔CPU pair, and both
references run the GSU at whole-instruction granularity, so a full ares cothread
scheduler is unnecessary and Rust-hostile (no native `co_switch`, huge blast
radius). The remaining real residual is the sub-frame CPU-timing precision that
gates DRAM refresh (§3); it is a frontier full-cycle-accuracy item with an
invisible payoff, not a scheduling-architecture problem.
