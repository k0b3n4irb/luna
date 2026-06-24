# The differential harness

The faithful-port method says: *don't theorise about a timing bug — bisect it
against the reference.* That only works if getting a reference trace is cheap.
For Luna it is — both reference emulators can be driven **headless**, and Luna
itself is built to be introspectable from the command line.

## The pattern

A differential compares Luna against a reference on the *same* run, looking for
the first point they disagree:

1. **Reference trace.** Drive ares or Mesen2 headless and log the events of
   interest — every memory access, an interrupt delivery, a register read — with
   their master-clock timestamp. Mesen2 runs fully headless via
   `--testRunner <script.lua> <rom>`, so a trace is one command.

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

Luna takes interrupts at the instruction boundary rather than at a per-cycle
poll point — a documented simplification. Is it observable?

The harness answers directly. A Mesen2 trace of the NMI vector fetches on *Doom*
over 300 frames, compared against Luna's, showed the **same ~47 deliveries** and
the **same ~357,366-master-clock inter-NMI cadence**, jitter distribution
included. The conclusion is evidence, not hope: Luna's interrupt model is
cycle-correct at the observable level, and a per-cycle-poll rewrite would be a
theoretical refinement below the measurement floor.

That is the method working as designed — sometimes it *refutes* Luna and points
at the fix; here it *confirmed* Luna and saved a risky rewrite. Either way the
answer comes from a measurement, captured autonomously, against the gold
standard.

> The same harnesses underpin Luna's CPU cores (the SingleStepTests `cycles[]`
> oracle), its coprocessors (byte-exact trajectory replays), and its renderer
> (the golden ROM suite). See [Testing & determinism](testing.md).
