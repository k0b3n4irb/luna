# Reference-First Implementation (auto-loaded)

IMPORTANT: Before writing or rewriting **any** SNES subsystem feature —
bus dispatch, CPU opcode timing, PPU register, DMA mode, DSP envelope,
SA-1 coprocessor logic, joypad scan, etc. — consult the corresponding
implementation in **both** reference emulators **and understand it
fully** before touching luna code.

## The two canonical references

| Reference | Role | Base URL |
|---|---|---|
| **ares** | Gold standard for hardware accuracy | `https://raw.githubusercontent.com/ares-emulator/ares/master/ares/sfc/` |
| **Mesen2** | Independent second source; cross-check when ares is unclear | `https://raw.githubusercontent.com/SourMesen/Mesen2/master/Core/SNES/` |

## Workflow per feature

1. **Fetch** the relevant files from both repos (`curl -s`) into
   `/tmp/`. For a directory listing use the GH API:
   `gh api repos/ares-emulator/ares/contents/ares/sfc/<subsys> --jq '.[].name'`.
2. **Read** the actual source — register decoders, state machines,
   bit layouts. Quote line numbers when summarising.
3. **Both references must agree** on the semantic before luna
   adopts it. When they diverge, document the discrepancy and pick
   the one with more clarity (usually ares' verified behaviour).
4. **Write up a short spec** to `/tmp/<feature>_reference.md`:
   register table, state-transition diagram, edge cases. This is
   the diff target. Promote it to `docs/` if it has lasting value.
5. **Inventory** what luna currently does (the `Explore` agent works
   well for this) into `/tmp/luna_<feature>_inventory.md`.
6. **Then** implement against the spec — never from memory or from
   `fullsnes.htm` paraphrases alone. The bit layouts and timing
   quirks differ between secondary docs and what real hardware
   does; ares + Mesen2 are the empirical truth.

## Why this matters

Patches that skip this step have caused real regressions in the
luna history:

- SA-1 CCNT bit-5 vs bit-7 inversion
- CC1/CC2 cdsel inversion
- Echo FIR half-scale precision bug
- CGWSEL bits 7:6 force-main-black polarity (fixed 2026-05)
- M7SEL H-flip/V-flip/screen-over bit-swap — implemented as bits
  6/7/1:0 instead of the real bits 0/1/7:6 (ares io.cpp:411-414);
  Chrono Trigger's intro pendulum pivoted from the bottom (fixed 2026-05)

Always read the source first.

## Worked examples in the tree

For a recent end-to-end application of this workflow, see:

- `docs/ppu_compositor_reference.md` — the synthesised ares + Mesen2 spec
- `docs/accuracy_scorecard.md` + the per-subsystem `docs/luna_*_gaps.md` —
  the current correctness grades + open-gap lists against that spec
- `docs/ares_ppu_notes.md`, `docs/mesen2_ppu_notes.md` — raw per-reference notes
