# luna SPC700 core — correctness audit vs ares

Reference-first audit of the SPC700 CPU core (`crates/luna-cpu-spc700`)
against ares (`ares/component/processor/spc700/*`). Companion to the
other `luna_*_gaps.md` docs.

Authored 2026-05-30; refreshed 2026-05-30 after the Tom Harte harness
landed (see history below).

## Methodology note — what is already gated

As of the SPC700 Tom Harte harness (`crates/luna-cpu-spc700/tests/
tom_harte.rs`, commit `b70bf07`), the **instruction semantics are
machine-proven**: the `SingleStepTests/spc700` suite passes
**256,000 / 256,000** (all 256 opcodes × 1000 cases). That covers, per
opcode, the full register + PSW + RAM state transition — including the
8-bit ALU half-carry/overflow, `DAA`/`DAS`, `DIV`/`MUL`, the 16-bit
`ADDW`/`SUBW`/`CMPW` half-carry (the suspect the first draft of this doc
flagged — now verified), `BRK`, and every addressing mode with the
P-flag direct-page select.

So, like the 65C816 audit (`luna_65c816_gaps.md`), this pass does **not**
re-litigate per-opcode semantics. It targets what Tom Harte cannot
reach — `BRK`/vector, `SLEEP`/`STOP`, reset/power, and the timing model
— cross-checked against ares.

## Severity legend

- 🔴 real bug
- 🟠 accuracy gap
- 🟡 precision / cosmetic
- 🟢 verified correct (do not regress) / resolved

---

## 🟢 Verified correct against ares (do not regress)

### BRK (`$0F`) and the software-interrupt vector

ares `instructionBreak` (`instructions.cpp:148-159`):

```cpp
read(PC); push(PC >> 8); push(PC >> 0); push(P); idle();
n16 address = read(0xffde) | (read(0xffde + 1) << 8);   // $FFDE/$FFDF
PC = address; IF = 0; BF = 1;
```

luna (`opcodes.rs:1716-1728`) matches exactly: push PC.h, PC.l, P; jump
through the vector at **`$FFDE`/`$FFDF`**; clear I, set B. (Also gated by
Tom Harte opcode `$0F`.)

### PSW bit layout

ares (`spc700.hpp:135`): `c<<0 | z<<1 | i<<2 | h<<3 | b<<4 | p<<5 |
v<<6 | n<<7`. luna (`flags.rs`): `C=0, Z=1, I=2, H=3, B=4, P=5, V=6,
N=7` — **identical**. The `P` flag's direct-page select (`$00xx` vs
`$01xx`, `cpu.rs:direct_addr`) is the standard SPC700 behavior.

### SLEEP (`$EF`) / STOP (`$FF`)

ares `instructionWait`/`instructionStop` (`instructions.cpp:558-590`)
set `r.wait`/`r.stop` and spin (`read(PC); idle();`) until the flag is
cleared. luna models them as `sleeping`/`stopped` fields (`cpu.rs:24-26`)
with `step()` early-returning a small tick (`opcodes.rs:28-39`) so the
scheduler keeps advancing — functionally equivalent. The SNES SMP wires
no interrupt input, so both halts persist until reset (correct; see
below).

### No external interrupt lines

The SPC700 in the SNES has no NMI/IRQ pins, so `step()` polls no
interrupt before fetch (`opcodes.rs:28-50`) — matching ares, where the
only control-transfer-on-event is the software `BRK`. Correct.

### Dispatch completeness

`execute()` is an exhaustive `match` over all 256 opcodes with **no
catch-all panic / `todo!` / `unreachable!`**. Every opcode is
implemented (the `TCALL` family is handled via grouped arms).

---

## 🟡 Minor / cosmetic

| # | Sev | Item | ares | luna |
|---|-----|------|------|------|
| 1 | 🟡 | **Reset register values.** ares `power()` cold-boots `S=0xEF`, `P=0x02` (`spc700.cpp:32-41`); luna `reset()` uses `SP=0xFF`, `PSW=0` (`cpu.rs:50-62`). The IPL ROM's opening `MOV X,#$EF; MOV SP,X` overwrites SP within ~2 instructions and the difference never reaches game code; Tom Harte supplies explicit state, so it isn't gated either. luna's `PC = [$FFFE/$FFFF]` reset-vector load is correct. | `S=0xEF, P=0x02` | `SP=0xFF, PSW=0` |
| 2 | 🟡 | **Halt/branch timing granularity.** `SLEEP`/`STOP` return a fixed conservative tick per `step()` rather than ares' per-cycle `read+idle` spin; the taken-branch `+2` penalty is added in `step()` rather than threaded per access. Cycle-exactness only; no state effect. | per-cycle spin | `opcodes.rs:28-50` |

---

## History

The original (2026-05-30 morning) draft listed two open items, both now
resolved:

- ~~🟠 **#1 No Tom Harte test**~~ — **DONE** (`b70bf07`): the harness was
  added mirroring the 65C816 one (`#[ignore]`, fetch via
  `tools/fetch-tom-harte-spc700.sh`, `LUNA_TOM_HARTE_REQUIRE=1` strict
  gate). Passes 256,000/256,000. This retroactively verified the
  `ADDW`/`SUBW` (`$7A`/`$9A`) half-carry the draft suspected.
- ~~🟡 **#2 Stale comments**~~ — **DONE**: the `cycles.rs` / `opcodes.rs`
  "Phase 2 / future" branch-penalty comments were refreshed (the penalty
  was already applied in `step()`); `docs/luna_apu_gaps.md`'s false
  "SPC700 is Tom-Harte-validated" claim was corrected.

## Verdict

No correctness (🔴) defects. The instruction core is machine-proven
(Tom Harte 100%) and the `BRK`/`SLEEP`/`STOP`/PSW/reset paths are a
faithful match to ares. The only residue is cosmetic reset values and
the cycle-granularity inherent to the atomic / per-opcode-tick timing
model (the same trade-off documented for the 65C816, APU, and PPU
cores) — not worth point-fixing outside a deliberate cycle-timing
rewrite.
