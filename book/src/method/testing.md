# Testing & validation

Luna's accuracy is held in place by **two complementary tiers** of automated
tests, plus the differential harness from the [previous chapter](differential.md).
The exact commands and file locations live with the code (see the repository's
`tests/` directories and contributing notes); this chapter describes *what* is
validated and *why* it is trustworthy.

## Tier 1 — per-instruction CPU suites (hardware truth)

Both CPU cores are checked against **exhaustive single-instruction
state-transition vectors** — roughly ten thousand cases per opcode, covering
every addressing mode and flag combination. Each case sets the processor to a
known state, runs one instruction, and asserts the resulting registers, flags
and memory.

These vectors are an **independent correctness oracle**: they encode real
hardware behaviour, not Luna's. Current state — **both cores pass 100%**:

| Core | Cases | Result |
|---|---|---|
| 65C816 | 5,080,000 | ✅ all pass |
| SPC700 | 256,000 | ✅ all pass |

The datasets are large, so they are fetched on demand and gated in their own CI
job rather than run on every `cargo test`.

## Tier 2 — full-system golden ROM tests (regression baselines)

End-to-end homebrew hardware-test ROMs exercise the *whole* machine — CPU, PPU
and bus together. Each test boots a ROM, runs it until the framebuffer settles
(or to a fixed, deterministic instruction count for animated scenes), and
asserts a hash of the result:

- **Display ROMs** hash the 256×224 framebuffer — opcode result screens (every
  group renders its all-PASS table), background modes, hi-colour blending,
  windows, Mode 7, and mosaic.
- **Audio ROMs** play music or sound effects rather than draw, so they hash the
  APU's 32 kHz PCM output instead of the framebuffer.

Unlike the Tier-1 vectors, these hashes are captured from **Luna's own output**,
so they are **regression baselines**, not an independent oracle. Each ROM ships
a reference image from real hardware; a Luna render is eyeballed against it
before a baseline is blessed, and re-blessed after any intended render change.

The ROM corpus is **not vendored** — it is fetched on demand and the tests
skip cleanly if it is absent, so a plain build never depends on it.

### A nuance worth knowing: PAL timing

The display suite is loaded as **PAL**. Several ROMs do a single wait-for-vblank
and then write their entire result table in one burst that only fits inside
PAL's longer vblank (~72 lines vs NTSC's 37). Run as NTSC, Luna *correctly*
drops the writes that overflow into active display — which is hardware-accurate,
but leaves those particular screens blank. Loading the suite as PAL reproduces
the output the ROMs were authored for; Luna's NTSC timing is itself correct.

## Why two tiers

The per-instruction suites prove each opcode is right in isolation but say
nothing about how the subsystems interact. The golden ROMs prove the *system*
renders correctly but, being Luna's own output, can only catch *regressions*.
Together — plus the differential harness for timing — they cover correctness
from the single instruction up to the whole frame.
