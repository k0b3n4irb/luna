# The CPUs — 65C816 & SPC700

The SNES has two processors, and Luna implements each as a **standalone core**
with no console-specific glue — usable from any consumer, and tested in
isolation against an exhaustive per-instruction test suite.

| Core | Crate | Role |
|---|---|---|
| **65C816** | `luna-cpu-65c816` | The main CPU — runs the game |
| **SPC700** | `luna-cpu-spc700` | The audio CPU — drives the S-DSP |

## 65C816 — the main CPU

| | |
|---|---|
| **On the console** | the S-CPU: 16-bit, 24-bit addresses, an 8-bit emulation mode |
| **In luna** | `crates/luna-cpu-65c816/src/` (`cpu.rs`, `opcodes.rs`, `addressing.rs`) |
| **Proven by** | the per-instruction suite, 5.08M cases at 100% (`crates/luna-cpu-65c816/tests/tom_harte.rs`), and its `cycles[]` bus-order oracle |
| **Grade** | **A−** (scorecard: *CPU 65c816*) |
| **Open gaps** | [`docs/luna_65c816_gaps.md`](https://github.com/k0b3n4irb/luna/blob/main/docs/luna_65c816_gaps.md): `WAI` wakes on an 8-master-clock grid (#4); the interrupt delay after a DMA burst, where the two references disagree (#5) |

> The table above opens every subsystem page: what the hardware is, where
> luna's port lives, what proves it, its grade in the
> [accuracy scorecard](../method/accuracy.md#where-luna-stands), and what is
> still open.

A 16-bit processor with an 8-bit emulation mode, three index/accumulator width
modes (the `M` and `X` flag bits), 24-bit addressing, and a rich set of
addressing modes. Luna's core passes **100% of the processor test suite for the
`65816`** (5.08M cases), including the per-cycle bus-access order — validated by a
`cycles[]` oracle that checks every memory access of every instruction, not just
the final register state.

A few deliberate notes:

- For the `(dp,X)` emulation-mode pointer wrap, Luna follows the per-instruction
  test suite's verified hardware behaviour.
- Interrupts are **sampled one cycle before an instruction's last bus access**
  and taken at the boundary that follows — which is what both reference
  emulators do. Neither interrupts mid-instruction; the sampling point is what
  decides whether an interrupt arriving late in an instruction is taken now or
  one instruction later, and it is also what gives `CLI` / `SEI` their
  one-instruction recognition delay (see
  [The differential harness](../method/differential.md)).

## SPC700 — the audio CPU

| | |
|---|---|
| **On the console** | the S-SMP: 8-bit, its own 64 KB of RAM, its own clock |
| **In luna** | `crates/luna-cpu-spc700/src/` (`step.rs` is the cycle-stepped core that runs; `opcodes.rs` is an atomic interpreter kept as a test oracle) |
| **Proven by** | the per-instruction suite, 256K cases at 100% (`crates/luna-cpu-spc700/tests/tom_harte.rs`); the cycle-stepped core compared byte for byte with the atomic one |
| **Grade** | **A−** (scorecard: *SPC700*) |
| **Open gaps** | [`docs/luna_spc700_gaps.md`](https://github.com/k0b3n4irb/luna/blob/main/docs/luna_spc700_gaps.md): the reset values of `S` and `PSW` (#1); after `STOP` the whole APU stops and a stub answers the ports (#3) |

An 8-bit processor running in its own clock domain, in lockstep with the main
CPU at bus-access granularity. Luna's core passes **100% of the processor test
suite for the `spc700`** (256K cases), with per-opcode cycle counts (2–12 cycles,
not a flat model) modelled on the hardware reference.

The CPU↔CPU interleave is cycle-exact: the SPC700 advances one bus access at a
time, so the two processors never run ahead of each other — the prerequisite
for games that synchronise tightly through the APU I/O ports.

> For the audio side this CPU drives — the S-DSP — see [The APU](apu.md).
