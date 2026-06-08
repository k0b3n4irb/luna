# Faithful Port + Step-by-Step Dichotomy (auto-loaded)

IMPORTANT: This is **THE** method for luna. It supersedes ad-hoc debugging,
heuristics, and trial-and-error. When it conflicts with a faster-looking
shortcut, this rule wins. "C'est notre salut."

## 1. Faithful translation, not invention

luna exists to be a faithful, accurate port of **ares** (gold standard) +
**Mesen2** (second source). When a subsystem is wrong, the answer is **NOT**
to patch luna's existing (possibly divergent) code with hacks, env-gated
experiments, magic constants, or trial-and-error timing tweaks. The answer is
to **translate the reference faithfully — including its ARCHITECTURE**, not
just its per-operation logic.

Translating French → German means translating the **grammar** too, not only
the words. In emulator terms:

- **Words / sentences** = per-opcode logic, register decode, bit layouts.
- **Grammar** = the scheduling / timing / interleave model: how the
  subsystem is clocked, how it synchronises with the CPU/bus/DMA, bus
  arbitration, the cooperative-thread structure.

Port **both**. If ares uses cooperative cycle-interleaved threads
(`Thread::synchronize`, exact per-step `_clock`, blocking bus arbitration in
`read()`), luna must **replicate that model** for the affected subsystem —
not approximate it with a batched per-access budget.

### The cautionary tale (Super FX, 2026-06)

Four days were lost here. The GSU **engine** was ported faithfully (proven
byte-exact vs Mesen via a differential harness). But luna kept its **own**
integration/scheduling (batched, CPU-driven `step_coproc`) instead of porting
ares' cooperative-thread model. Days were then burned on hacks (clsr, RAN
arbitration, drain, DMASYNC) patching symptoms of that architectural
divergence — none fixed it. **The grammar was never translated.** The agreed
fix (2026-06-08) is to port ares' cooperative scheduling model (option 1: the
real two-thread cycle-interleaved model), not to keep approximating it.

## 2. Proceed by steps and by dichotomy

Never make a big change in one leap; never debug by guessing.

- **Step by step.** Smallest landable increment. Each step: build (the rebuild
  hook), test (`cargo test --workspace --lib`), and — if it changes anything
  a human can perceive — GUI-validate before the next step. No multi-day
  blind rewrites; stage them.

- **By dichotomy (bisection).** To find WHERE luna diverges from the
  reference, **binary-search the divergence** — do not theorise about causes.
  Capture a reference trace (ares/Mesen, headless), inject the reference's
  state into luna, run, compare, and halve the search space until the
  **FIRST diverging operation / cycle / value** is pinned. Then read the
  reference at exactly that point and translate it.

## 3. The differential harness IS the method

The Super FX harnesses are the template — replicate the pattern for any
timing/accuracy-sensitive subsystem:

- **Reference trace:** drive ares or Mesen2 headless (Mesen: `--testRunner
  script.lua rom -novideo -noaudio`; register the exec/mem callbacks only in
  the target window so `getState` isn't called on millions of pre-window ops;
  `emu.read` returns SIGNED bytes — mask `& 0xFF`).
- **Single-step harness:** inject each reference pre-state into luna's unit,
  run ONE op, compare to the reference's next row. Proves per-op logic.
- **Trajectory harness:** inject full state + memory ONCE, run luna FREELY,
  compare the whole trajectory + final memory. Proves loads / accumulation /
  control-flow / memory writes.
- When a layer is proven byte-exact yet the system still diverges, the bug is
  in the **layer above** (integration / scheduling / inputs) — port THAT
  faithfully too, and bisect there.

Reusable luna diagnostics already exist: `--superfx-trace`, `--dma-trace`,
`--mem-trace`, `--dump-vram`, `--dump-coproc-ram`, `luna frames`, and the
`gsu_differential` / `gsu_trajectory` test harnesses. Prefer extending these
over inventing one-off prints. luna's CLI/API-introspectable design exists
precisely to make this differential method tractable — use it.

## 4. What is forbidden

- Env-gated "fix" hacks committed as the solution. (Env gates are fine for a
  LOCALISATION experiment; the committed FIX must be a faithful port.)
- Magic timing constants / multipliers chosen to make one scene look right.
- Concluding "the reference model won't help" from approximations of it —
  only a faithful port refutes a faithful port.
- Big-bang rewrites with no intermediate measurement. Stage + bisect.
