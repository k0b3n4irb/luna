# The differential harness

The faithful-port method says: *don't theorise about a timing bug — bisect it
against the reference.* That only works if getting a reference trace is cheap.
For Luna it is — a reference emulator can be driven **headless**, and Luna itself
is built to be introspectable from the command line.

## The pattern

A differential compares Luna against a reference on the *same* run, looking for
the first point they disagree:

1. **Reference trace.** Drive a reference emulator headless and log the events of
   interest — every memory access, an interrupt delivery, a register read — with
   their master-clock timestamp. The reference runs fully headless from a
   scripted test runner, so a trace is one command.

2. **Luna trace.** Run the same ROM through Luna's CLI with the matching trace
   filter (`luna state <rom> --mem-trace … --mem-trace-addr …`), frame-aligned
   with `--until-frame`.

3. **Diff.** The two emulators' clocks have different origins, so compare the
   *origin-independent* signal — the inter-event deltas and the event sequence —
   and find the first divergence.

Luna's CLI / API-first design exists precisely to make this tractable: what the
CLI measures is exactly what the GUI shows, so a difference found headless is a
difference a player would see.

## A worked example: interrupt delivery

Luna used to sample interrupts at the instruction boundary rather than one
cycle earlier, where the references sample them — a documented simplification.
Is it observable?

The harness answered directly. A reference trace of the NMI vector fetches on
*Doom* over 300 frames, compared against Luna's, showed the **same ~47
deliveries** and the **same ~357,366-master-clock inter-NMI cadence**, jitter
distribution included. So the simplification was below the measurement floor on
that title, and the rewrite was deprioritised rather than skipped.

It was done in the end (2026-09, gaps #1 and #2), and the sequel is the more
useful half of the lesson. Reading both references in full showed the rewrite
was not a rewrite at all: **neither emulator interrupts mid-instruction**, so
only the sampling point had to move — 101 markers and one bus hook. *Doom*
still renders byte-identically, exactly as the harness predicted. What the
harness could not have told us is that the change would expose a real bug
elsewhere: Luna raised the `I` mask at the end of the interrupt frame instead
of before the vector fetch, which was harmless until the poll moved onto that
fetch and the handler began re-entering itself forever.

Two things to take from that. A measurement that says "unobservable here" is
evidence about the titles measured, not a proof of equivalence. And a faithful
port pays off even when the thing it fixes was invisible, because the next
change is written against a model that matches the reference.

That is the method working as designed — sometimes it *refutes* Luna and points
at the fix; here it *confirmed* Luna and saved a risky rewrite. Either way the
answer comes from a measurement, captured autonomously, against a known-good
reference.

> The same harnesses underpin Luna's CPU cores (the per-cycle bus-trace oracle),
> its coprocessors (byte-exact trajectory replays), and its renderer (the golden
> ROM suite). See [Testing & determinism](testing.md).
