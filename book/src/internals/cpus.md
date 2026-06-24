# The CPUs — 65C816 & SPC700

The SNES has two processors, and Luna implements each as a **standalone core**
with no console-specific glue — usable from any consumer, and tested in
isolation against the [SingleStepTests](https://github.com/SingleStepTests)
suites.

| Core | Crate | Role |
|---|---|---|
| **65C816** | `luna-cpu-65c816` | The main CPU — runs the game |
| **SPC700** | `luna-cpu-spc700` | The audio CPU — drives the S-DSP |

## 65C816 — the main CPU

A 16-bit processor with an 8-bit emulation mode, three index/accumulator width
modes (the `M` and `X` flag bits), 24-bit addressing, and a rich set of
addressing modes. Luna's core passes **100% of the SingleStepTests `65816`
suite** (5.08M cases), including the per-cycle bus-access order — validated by a
`cycles[]` oracle that checks every memory access of every instruction, not just
the final register state.

A few deliberate notes:

- For the `(dp,X)` emulation-mode pointer wrap, the SingleStepTests suite
  overrides ares' page-wrap behaviour — Luna follows the test suite.
- Interrupts are taken at the **instruction boundary**. A headless differential
  against Mesen2 confirms this matches the observable NMI/IRQ delivery cadence
  on real games (see [The differential harness](../method/differential.md)).

## SPC700 — the audio CPU

An 8-bit processor running in its own clock domain, in lockstep with the main
CPU at bus-access granularity. Luna's core passes **100% of the SingleStepTests
`spc700` suite** (256K cases), with per-opcode cycle counts (2–12 cycles, not a
flat model) ported from ares.

The CPU↔CPU interleave is cycle-exact: the SPC700 advances one bus access at a
time (`Apu::run_to_target`, a port of Mesen2's `Spc::Run`), so the two
processors never run ahead of each other — the prerequisite for games that
synchronise tightly through the APU I/O ports.

> For the audio side this CPU drives — the S-DSP — see [The APU](apu.md).
