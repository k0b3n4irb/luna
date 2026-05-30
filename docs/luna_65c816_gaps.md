# luna 65C816 CPU core — correctness gaps vs ares

Reference-first audit of `crates/luna-cpu-65c816` against ares
(`ares/component/processor/wdc65816/*`, with the SNES interrupt glue in
`ares/sfc/cpu/{irq,timing}.cpp` for cross-reference). Companion to
`luna_bg_gaps.md` / `luna_obj_gaps.md` / `luna_apu_gaps.md` /
`luna_dma_gaps.md` / `luna_sa1_gaps.md` / `luna_spc700_gaps.md`.

## Methodology note — what is already gated

Unlike the other subsystems, the 65C816 **instruction semantics** are
exhaustively validated by the Tom Harte `SingleStepTests/65816` suite
(`crates/luna-cpu-65c816/tests/tom_harte.rs`): **5,080,000 / 5,080,000
cases pass** (commit `f23d5cd`). That covers, per-opcode, the full
register + flag + RAM state transition for all 256 opcodes — including
the BCD `ADC`/`SBC` pipeline, `REP`/`SEP`/`XCE`, every addressing mode
(including the direct-page / stack bank-0 wrap and the `(dp,X)` base-wrap
fixed in `f23d5cd`), stack push/pull, and the `BRK`/`COP`/`RTI`
single-step transitions. The only opcodes the suite skips are `MVN`
(`$54`) / `MVP` (`$44`), because their re-entrant model isn't gateable
on a 100-cycle atomic budget.

So this audit deliberately does **not** re-litigate per-opcode
semantics — those are machine-proven. It targets what Tom Harte cannot
reach: **asynchronous interrupt delivery, the cycle/timing model,
`WAI`/`STP`, `MVN`/`MVP` atomicity, and reset/power**, cross-checked
byte-for-byte against ares.

## Severity legend

- 🔴 real bug, wrong architectural state
- 🟠 accuracy gap that can affect timing-sensitive software
- 🟡 precision / cycle-exactness, low real-world impact
- 🟢 verified correct (do not regress) / intentional non-gap

---

## 🟢 Verified correct against ares (do not regress)

### Hardware NMI / IRQ push sequence

ares `interrupt()` (`instruction.cpp:1-14`):

```cpp
N push(PC.b);                 // native only: push PB
push(PC.h); push(PC.l);
push(EF ? P & ~0x10 : P);     // emulation: clear B (bit 4); native: P as-is
IF = 1; DF = 0;
PC.l = read(r.vector + 0); PC.h = read(r.vector + 1); PC.b = 0x00;
```

luna `service_software_interrupt` (`opcodes.rs:~1054`, via
`service_nmi`/`service_irq` at `opcodes.rs:107-123`) matches exactly:
PB pushed in native only; the pushed P has bit 4 **cleared in emulation,
left as-is in native**; `I` set, `D` cleared; `PB=0`; jump through the
16-bit vector. Vectors: NMI `$FFEA`/`$FFFA`, IRQ `$FFEE`/`$FFFE`
(native/emulation) — the standard 65C816 table.

### BRK / COP

ares `instructionInterrupt` (`instructions-other.cpp:54-65`) pushes `P`
**unmodified**; the emulation B=1 falls out of `XF` being forced to 1 in
emulation. luna sets bit 4 explicitly (`set_b_bit_in_emulation = true`)
in the emulation branch — identical result, since emulation ⇒ `XF=1`.
Vectors `BRK $FFE6`/`$FFFE`, `COP $FFE4`/`$FFF4` match. (Also gated by
Tom Harte opcodes `$00`/`$02`.)

### MVN / MVP block move

ares `instructionBlockMove{8,16}` (`instructions-other.cpp:28-52`):
`B = dest_bank; read(src:X); write(dest:Y); X±=adj; Y±=adj;
if(A.w--) PC.w -= 3;`. luna `block_move` (`opcodes.rs:~1146`) is the same
re-entrant one-byte-per-step machine: sets `DB = dest`, moves one byte,
adjusts X/Y, and rewinds `PC -= 3` while the 16-bit counter is nonzero
(post-decrement semantics match `A.w--`). The only divergence is
cosmetic: ares' 8-bit form adjusts `X.l`/`Y.l` (preserving `X.h`),
luna adds to the full 16-bit register then masks to 8-bit when `XF=1` —
equivalent whenever `X.h==0`, which is invariant in 8-bit index mode.
**Re-entrancy is the correct model** (interruptible mid-block, matching
hardware and ares); do not "optimize" it into an atomic loop.

### WAI / STP

ares `instructionWait`/`instructionStop` (`instructions-other.cpp:67-80`)
spin on `r.wai`/`r.stp`. luna models them as `waiting`/`stopped` flags
consumed in `step()` (`opcodes.rs:37-61`). `WAI` wakes on **either** NMI
or IRQ *regardless of the `I` flag* (the I flag only gates whether the
handler is entered) — luna matches and documents the `SEI; WAI` idiom.
`STP` halts until `reset()`.

### Emulation-mode stack confinement & reset

Push/pull (`opcodes.rs:~2608`) keep `S` within page 1 (`$0100-$01FF`) in
emulation; `step()` re-pins `S.h=$01` at the start (and defensively at
the end) of every instruction. Reset (`cpu.rs:109`) loads the `$FFFC`
vector with `E=1`, `P=M|X|I` (`D=0`), `S.h=$01`, `PB=DB=DP=0` — matching
ares `power()` (`p=0x34`, `s=0x01ff`, `e=1`).

### Intentional non-gaps

- **ABORT vector / RDY line** — absent in luna *and* in ares' SNES core;
  the SNES wires no abort or ready pin to the CPU. Not a gap.

---

## 🟠 / 🟡 Timing-model gaps (inherent to the atomic / bus-as-clock core)

luna's CPU does not track master cycles itself; each bus access pays its
cost through `Bus::io_cycle` (`lib.rs:8-11`), and interrupts are polled
at instruction boundaries. That design choice is the source of every
item below — none changes architectural register/RAM state (so Tom Harte
stays green), but each is a cycle-accuracy deviation from ares' per-cycle
`lastCycle()` / `idleIRQ()` model.

| # | Sev | Gap | ares ref | luna |
|---|-----|-----|----------|------|
| 1 | 🟠 | **Interrupt poll granularity.** NMI/IRQ are recognized only at the *start of the next* `step()`, never mid-instruction. ares polls at the precise last cycle of the instruction in progress. | `instruction.cpp` `L`/`idleIRQ`, `sfc/cpu/irq.cpp` | `opcodes.rs:74-91` (boundary only) |
| 2 | 🟡 | **Interrupt-enable delay quirk.** Real 65xx delays interrupt *recognition* one instruction after `CLI`/`SEI`/`PLP` change `I`. luna uses the post-instruction `I` at the next boundary, so a pending IRQ after `CLI` is taken one instruction early. | poll point vs flag write order | `opcodes.rs:84` reads current `I` |
| 3 | 🟡 | **Dummy bus cycles in the interrupt sequence.** ares `interrupt()` does `read(PC.d); idle();` before the pushes and `idleJump()` after; luna omits these (timing delegated to the bus). Affects open-bus/MDR and exact cycle counts, not state. | `instruction.cpp:2-3,13` | `service_software_interrupt` |
| 4 | 🟡 | **WAI resume granularity.** luna advances the bus in fixed `WAI_TICK_MCYCLES` (8 mclk) chunks while waiting, so wake latency is quantized rather than single-cycle. Harmless for the `WAI; BRA -3` VBlank idiom. | single `idle()` loop | `opcodes.rs:58` |

---

## Verdict

No correctness (🔴) defects found. The instruction core is
machine-proven (Tom Harte 100%) and the asynchronous-interrupt path is a
faithful port of ares `interrupt()`. The open items are all
cycle-granularity deviations that follow directly from the atomic /
bus-as-clock architecture — the same trade-off documented for the APU
and PPU cores. They would only be worth closing as part of a deliberate
per-cycle CPU-timing rewrite (cf. the cycle-accuracy phase plan), not as
point fixes.

## Suggested order (if/when pursued)

1. 🟠 #1 interrupt poll granularity — the highest-value item, but it
   implies threading a cycle position through the instruction core (a
   structural change, not a patch).
2. 🟡 #2–#4 — only meaningful once #1 exists; low real-world return.
