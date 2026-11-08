---
paths:
  - "crates/**"
  - "docs/**"
---
# Reference-first implementation

Before writing or rewriting any SNES subsystem feature (bus, opcode timing,
PPU register, DMA mode, DSP envelope, SA-1, joypad…), read the matching
implementation in both references and understand it fully.

| Reference | Role | Base URL |
|---|---|---|
| **ares** | gold standard | `https://raw.githubusercontent.com/ares-emulator/ares/master/ares/sfc/` |
| **Mesen2** | independent second source | `https://raw.githubusercontent.com/SourMesen/Mesen2/master/Core/SNES/` |

1. **Ask the local SNES documentation index first** (its search tool): the corpus already held the
   answer twice. Ares core and Mesen2 are indexed as code.
2. **Fetch** both sources into `/tmp/luna-ref/` (`/reference-fetch <subsys>`;
   listing: `gh api repos/ares-emulator/ares/contents/ares/sfc/<subsys>`).
3. **Read** the register decoders, state machines, bit layouts; quote line
   numbers.
4. **Both must agree.** When they diverge, document it and pick the one
   with more clarity (usually ares); the 65c816 Tom Harte suites can
   overrule both.
5. **Write a short spec** to `/tmp/luna-ref/<feature>_reference.md`
   (registers, transitions, edge cases); promote it to `docs/` if it lasts.
6. **Inventory** what luna does, then **implement against the spec**, never
   from memory or `fullsnes.htm` paraphrases.
7. **Update the scorecard in the same commit**: `docs/accuracy_scorecard.md`
   and the row of the `docs/luna_*_gaps.md` audit.

Skipping this caused the SA-1 CCNT and cdsel inversions, the echo FIR
half-scale bug, the CGWSEL polarity and the M7SEL bit-swap. Worked example:
`docs/ppu_compositor_reference.md` with `docs/ares_ppu_notes.md` and
`docs/mesen2_ppu_notes.md`.
