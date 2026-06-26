# Why "faithful port"

Luna exists to be a **faithful, accurate reconstruction** of real Super Nintendo
hardware. When a subsystem is wrong, the answer is **not** to patch Luna's code
with hacks, magic constants, or trial-and-error timing tweaks. It is to
**translate the reference faithfully — including its architecture.**

"The reference" here means the hardware itself, as captured by the most accurate
documentation, disassembly and reference implementations available (the projects
Luna learns from are credited in the [Acknowledgements](../acknowledgements.md)).
The point is the same either way: Luna's behaviour is *derived*, not invented.

## Translate the grammar, not just the words

Translating French to German means translating the *grammar* too, not only the
words. In emulator terms:

- **Words / sentences** = per-opcode logic, register decode, bit layouts.
- **Grammar** = the scheduling and timing model: how a subsystem is clocked, how
  it synchronises with the CPU, bus and DMA, how bus arbitration works.

Port **both**. If the reference behaviour comes from cooperative,
cycle-interleaved execution with exact per-step clocking and blocking bus
arbitration, the reconstruction must replicate *that model* — not approximate it
with a batched per-access budget.

## The cautionary tale

Luna's Super FX core was once reconstructed faithfully at the *engine* level —
proven byte-exact by a differential harness. But it kept Luna's *own* batched,
CPU-driven scheduling instead of the reference's cooperative-thread model. Days
were then lost on hacks that patched the symptoms of that architectural
divergence; none of them fixed it, because **the grammar was never translated.**
The lesson is permanent: when the engine is proven correct yet the system still
diverges, the bug is in the layer *above* — port that faithfully too.

## Proceed by steps, and by dichotomy

Two working rules follow from this:

1. **Step by step.** Never make a big change in one leap. The smallest landable
   increment, each one built, tested, and — if it changes anything perceivable —
   validated in the GUI before the next.

2. **By dichotomy.** To find *where* Luna diverges from the reference, don't
   theorise about causes — **binary-search the divergence.** Capture a reference
   trace, inject its state into Luna, run, compare, and halve the search space
   until the *first* diverging operation is pinned. Then read the reference at
   exactly that point and translate it.

That second rule is only tractable if capturing a reference trace is cheap. The
[next chapter](differential.md) is about exactly that.
