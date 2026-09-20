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
cases pass — 100%, 0 failures** (since commit `2424f89`; full suite
re-verified green 2026-05-30). That covers, per-opcode, the full
register + flag + RAM state transition for all 256 opcodes — including
the BCD `ADC`/`SBC` pipeline, `REP`/`SEP`/`XCE`, every addressing mode
(including the direct-page / stack bank-0 wrap and the `(dp,X)` base-wrap
fixed in `2424f89`), stack push/pull, and the `BRK`/`COP`/`RTI`
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
- 🔧 was wrong, now fixed (kept for the record)

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
cost through `Bus::io_cycle` (`lib.rs:8-11`). That design choice is the
source of the items below — none changes architectural register/RAM state
(so Tom Harte stays green), each is a cycle-accuracy question.

Interrupts are no longer among them: since 2026-09-20 both 65C816s
sample the lines at the instruction's last cycle, as ares' `lastCycle()`
and Mesen2's per-cycle `DetectNmiSignalEdge` do (#1, #2 below).

| # | Sev | Gap | ares ref | luna |
|---|-----|-----|----------|------|
| 1 | 🔧 | **Interrupt poll granularity** — **fixed 2026-09-20**. NMI/IRQ were recognised only at the *start of the next* `step()`, so an interrupt arriving during an instruction's final access got in up to a whole instruction early. Both CPUs now sample at the instruction's last cycle, where the references do: `Bus::last_cycle(i_flag)` is ares' `nmiTest()`/`irqTest()`, called from the `last_*` helpers that mark every instruction's final access. Neither reference interrupts mid-instruction — both still *service* at the boundary; only the sampling point moved. `idleIRQ()` came with it: the dead cycle of a two-cycle implied opcode is now a dummy read of `PB:PC` when an interrupt is pending. | `wdc65816/registers.hpp:30` (`#define L lastCycle();`), `sfc/cpu/irq.cpp:83-93`, `wdc65816/memory.cpp:1-16`; Mesen2 `SnesCpu.Shared.h:315-347,387-394` | `cpu.rs` `last_cycle`, `snes.rs` / `coproc/sa1.rs` `last_cycle`; tests `last_cycle_invariant.rs` (exactly one poll per instruction, all 256 opcodes × 5 modes) |
| 2 | 🔧 | **Interrupt-enable delay quirk** — **fixed 2026-09-20**, with no code of its own. Neither reference has a recognition-delay counter: the quirk falls out of polling before the instruction's final action (`L idleIRQ(); flag = 0;`). So `CLI` cannot unmask an IRQ for its own instruction, `SEI` cannot mask one its own poll already saw, and `PLP` / `REP` / `SEP` behave the same way. | `instructions-other.cpp:97-100,113-120`; Mesen2 reads `I` in `DetectNmiSignalEdge` | `opcodes.rs` (implied ops poll before their register effect; REP/SEP after the operand fetch); tests `cli_does_not_unmask_an_irq_for_its_own_instruction`, `sei_does_not_mask_an_irq_its_own_poll_already_saw` |
| 3 | 🔧 | **Dummy bus cycles in the interrupt sequence** — **fixed 2026-09-19** (`d117412`, shipped in v1.25.0). ares `interrupt()` opens with `read(PC.d); idle();` before the pushes; luna omitted both, so a hardware NMI/IRQ took 6 bus cycles in native mode where hardware takes 8 — every handler started 14 mclk early. `hardware_interrupt_entry` now performs them, called from `service_nmi`/`service_irq` only (BRK/COP correctly keep the short sequence). The trailing `idleJump()` is owed nothing: it is an empty virtual in `wdc65816.hpp:12` that the SNES CPU never overrides. This was the root cause of PPU gap #7b (HiColor128, now pixel-exact). | `instruction.cpp:1-14`, `wdc65816.hpp:12` | `opcodes.rs` `hardware_interrupt_entry`; test `hardware_interrupts_spend_two_cycles_brk_does_not` |
| 4 | 🟡 | **WAI resume granularity.** luna advances the bus in fixed `WAI_TICK_MCYCLES` (8 mclk) chunks while waiting, so wake latency is quantized rather than single-cycle. Harmless for the `WAI; BRA -3` VBlank idiom. | single `idle()` loop | `opcodes.rs:58` |
| 5 | 🟡 | **Post-DMA interrupt delay — the references disagree.** Mesen2 delays NMI/IRQ recognition by one cycle after a DMA/HDMA burst (`SnesCpu.cpp:124-128`, `SnesCpu.Shared.h:323-346`). ares has the same field, `status.irqLock`, set by `dmaRun` / `hdmaSetup` / `hdmaRun` and on a `$4200` write — but `CPU::step()` clears it at its top and every access and DMA step calls `step()`, so it is **always 0** by the time any `lastCycle()` runs. It is dead code as ares is written. luna follows ares and does not implement it; adopting Mesen2's live rule would be a separate, separately-measured change. | ares `timing.cpp:12`, `dma.cpp:21,32,40`, `irq.cpp:49,89` vs Mesen2 as above | not implemented (deliberate) |

---

## Verdict

No correctness (🔴) defects found, and as of 2026-09-20 no 🟠 ones
either. The instruction core is machine-proven (Tom Harte 100%, and the
`cycles[]` oracle reports zero cycle mismatches), and the asynchronous
interrupt path is now a faithful port end to end: the lines, the sampling
point, the `I` mask's position, and the entry sequence.

What remains is one quantisation (#4, `WAI` on an 8-mclk grid) and one
place where the two references genuinely disagree (#5), where we follow
ares. Both are timing-only, with no known game impact.

## History

The 2026-06 audit called #1 "the highest-value item, but it implies
threading a cycle position through the instruction core (a structural
change, not a patch)". That turned out to overstate it. Reading both
references in full showed **neither interrupts mid-instruction** — they
service at the boundary exactly as luna did, and differ only in *when the
decision is sampled*. ares marks the cycle before each instruction's
final access with `L`; Mesen2 recomputes every cycle and reads the value
back at the boundary. So the work was 101 markers and one bus hook, not a
per-cycle CPU rewrite — and #2 then needed no code at all.

The part that did bite was a latent ordering bug: luna raised `I` at the
*end* of the interrupt frame, where both references raise it *before* the
vector fetch. Since the vector's high byte carries the poll, luna polled
its own entry sequence unmasked, and a still-asserted level re-latched
inside it — an infinite handler re-entry. Harmless while the poll sat at
the boundary; fatal the moment it moved.
