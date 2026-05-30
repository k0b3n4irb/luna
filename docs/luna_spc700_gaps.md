# luna SPC700 core — correctness audit vs ares

Reference-first audit of the SPC700 CPU core (`crates/luna-cpu-spc700`)
against ares (`ares/component/processor/spc700/algorithms.cpp`,
`instructions.cpp`). Companion to the other `luna_*_gaps.md` docs.

Authored 2026-05-30.

**Headline:** the core is in good shape — every semantically tricky
instruction the SA-1/APU regression history flagged is now an exact
match to ares. The one real gap is **test coverage**: unlike the
65c816, the SPC700 has **no Tom Harte processor-test backstop**, so
edge-case opcode bugs could lurk undetected.

## Severity legend

- 🔴 real bug
- 🟠 coverage / infrastructure gap
- 🟡 minor / stale docs

---

## 🟠 1. No Tom Harte test (the exhaustive backstop is missing)

The 65c816 core has `crates/luna-cpu-65c816/tests/tom_harte.rs` running
the SingleStepTests vectors (5M cases). The SPC700 has **none** — only
78 hand-written inline unit tests. SingleStepTests publishes an
[`spc700`](https://github.com/SingleStepTests/spc700) dataset, so the
gap is purely that nobody wired it up.

Consequence: instructions the inline tests don't cover aren't validated
against hardware. Concrete suspect found in this audit: the **`SUBW`
(`$9A`) and `ADDW` (`$7A`) half-carry** is computed directly
(`(ya & 0xFFF) </>= (mem & 0xFFF)`) rather than via ares' two-`SBC`/`ADC`
byte chain — it looks correct but isn't Tom-Harte-verified.

**Fix:** add an SPC700 Tom Harte harness mirroring the 65c816 one
(`#[ignore]`, reads `LUNA_TOM_HARTE_SPC700_DIR` / a default
`tests/tom-harte-spc700/v1`), plus a fetch script. The dataset is a
large external download, so the test stays opt-in like the 65c816 one.

---

## 🟡 2. Stale doc claims / comments

- `docs/luna_apu_gaps.md` (now corrected) wrongly listed the SPC700 as
  "validated against the Tom Harte SPC700 test suite
  (`tests/tom_harte.rs`)" — that file does not exist. **Fixed** in this
  pass.
- `cycles.rs` header and `opcodes.rs:25-27` say the +2 taken-branch
  penalty is "a future" / "Phase 2" item, but `step()` (opcodes.rs:44-48)
  already adds `SPC700_BRANCH_TAKEN_PENALTY` when `branch_taken` is set.
  The comments are stale.

---

## ✅ Verified correct (do not regress)

- **8-bit ALU** (`adc_u8`/`sbc_u8`/`cmp_u8`): the half-carry
  (`(a&0xF)+(b&0xF)+c > 0xF`) and overflow (`(a^r)&(b^r)&0x80`) formulas
  are algebraically identical to ares' `(x^y^z)&0x10` /
  `~(x^y)&(x^z)&0x80`; `SBC = ADC(a, ~b)` matches `algorithmSBC`.
- **DAA / DAS** (`$DF`/`$BE`): exact match to ares
  `instructionDecimalAdjust*` including the `CF=1`/`CF=0` set and the
  `> 0x99` / `(A&15) > 0x09` conditions.
- **DIV** (`$9E`): a verbatim port of ares `instructionDivide`,
  including the `Y < X<<1` vs `256-X` odd-quotient branch, the H/V flags
  from the original Y/X, and the X==0 (no div-by-zero) path. Three
  dedicated tests.
- **MUL** (`$CF`): `YA = Y*A`, N/Z from the high byte only.
- **16-bit ADDW/SUBW/CMPW** CF/VF/NF/ZF (the HF caveat is in #1).
- **Per-opcode cycle table** + the **taken-branch +2 penalty** (applied
  in `step()`).

## Suggested order

1. **#1 Tom Harte harness** — the high-value action: gives the SPC700
   the same exhaustive validation the 65c816 has and would catch any
   lurking edge-case (e.g. the SUBW HF).
2. **#2 stale comments** — trivial cleanup.
