# Accuracy

Luna's goal is **cycle-accuracy across every subsystem** — not "close enough to
run games," but behaviour that matches the real console down to the timing of
individual bus accesses. This chapter is about how Luna knows it is getting
there.

## How accuracy is measured, not asserted

A grade only means something if it is backed by a measurement. Luna leans on
three independent kinds of evidence, each catching what the others miss:

1. **Per-instruction CPU suites** — exhaustive state-transition vectors prove
   each opcode is correct in isolation. Both cores pass 100%
   ([Testing & validation](testing.md)).
2. **Golden full-system ROM tests** — homebrew hardware-test ROMs exercise the
   CPU, PPU and bus together, catching interaction bugs a single instruction
   never would.
3. **The differential harness** — when a *timing* question can't be settled by
   a pass/fail test, Luna bisects it against a reference emulator and reads off
   the first point of divergence ([The differential harness](differential.md)).

The rule throughout: a behaviour is "correct" when a measurement says so — never
because it looks right on one screen.

## Where Luna stands

Subsystem by subsystem, the implementation gaps that once separated Luna from
the hardware are closed: the CPUs are exhaustively verified, the PPU renders the
full feature set (background modes, Mode 7, hi-res, windows, mosaic, interlace),
the audio path is reconstructed to per-access timing, and the coprocessors
(SA-1, Super FX, DSP-1, S-DD1) are faithful ports.

Where Luna's timing model is a deliberate simplification, the differential
harness is used to measure whether the simplification is observable, rather
than to argue that it isn't. That measurement is evidence about the present,
not a licence to stop: interrupt sampling was one such simplification, measured
unobservable on *Doom* — and porting it faithfully anyway is what later closed
two named CPU gaps. So the honest statement is that Luna is **observably
accurate everywhere it has been measured**, and that being unobservable is a
reason to deprioritise a port, never a reason to skip it.

## What "A everywhere" still asks for

"Observably accurate where measured" is not yet a formal *A everywhere*, and
Luna does not claim it is. What remains is **breadth of validation** — running
the differential across more titles and more code paths — rather than missing
features or known defects. A handful of sub-cycle refinements remain below the
current measurement floor; they are theoretical-accuracy work, not bugs any game
exposes.

That honesty is the point. Luna would rather under-claim a grade it can prove
than over-claim one it cannot.
