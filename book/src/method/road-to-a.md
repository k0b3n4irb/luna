# The road to "A"

If Luna is already [observably accurate where it has been measured](accuracy.md),
what is left? This chapter is the honest answer — the shape of the remaining
frontier, and why it is *not* a pair of multi-month rewrites.

## The frontier is small and surgical

It is tempting to assume that the last stretch of cycle-accuracy needs two scary
architectural rewrites: a fully cooperative, cycle-interleaved scheduler, and a
per-dot renderer. The useful finding is that **both are, in substance, already
in place.**

- The CPU, audio and coprocessor cores already synchronise at **bus-access
  granularity** — one access at a time, never running ahead of each other.
- The renderer already commits the visible part of a scanline **lazily**, with
  the register state that was live at each point, so mid-line changes are latched
  where they happen.

So the work that remains is not structural. It is a handful of **small, surgical,
bisectable** residuals — a delivery-timing edge here, a sub-cycle refinement
there — each one isolatable with the differential harness and landable on its
own.

## Two kinds of "remaining"

1. **Breadth of validation.** The strongest lever left is simply *measuring
   more*: pointing the differential at more titles and more code paths to turn
   "observably correct where measured" into "observably correct, broadly." This
   is confidence work, and it is cheap now that the harness is self-contained.

2. **Sub-cycle refinements below the floor.** A few timing details — the exact
   poll point of an interrupt, a one-byte edge in an obscure transfer mode — are
   theoretically more faithful than Luna's current model, yet sit *below* the
   threshold any measurement (or any game) can detect. They are worth doing for
   completeness, not because anything is wrong.

## Why no big-bang rewrite

The faithful-port method is explicit about this: never make a large change in
one leap, and never rewrite a layer that measurement shows is already correct.
The architecture earned its place by passing the tests; the path to "A
everywhere" runs through a sequence of small, proven steps on top of it — not
through tearing it down.
