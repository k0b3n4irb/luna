# Testing & validation

Luna's accuracy is held in place by **two complementary tiers** of automated
tests (a third, `luna test`, puts the same machinery in homebrew developers'
hands), plus the differential harness from the [previous chapter](differential.md).
The exact commands and file locations live with the code (see the repository's
`tests/` directories and contributing notes); this chapter describes *what* is
validated and *why* it is trustworthy.

## Tier 1 — per-instruction CPU suites (hardware truth)

Both CPU cores are checked against **exhaustive single-instruction
state-transition vectors** — ten thousand cases per opcode for the 65C816
(in each of its two modes, native and emulation), one thousand for the
SPC700 — covering every addressing mode and flag combination. Each case sets the processor to a
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

- **Display ROMs** hash the 256×224 framebuffer (256×239 under overscan) — opcode result screens (every
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

### A nuance worth knowing: the video standard

The video standard is chosen per family. The **PPU demos run as PAL**, the
convention of the `twvd/siena` suite and of krom's reference captures. The
**CPU test family runs as NTSC**: each of those ROMs waits for one vblank, then
writes its whole result table in a single burst. In PAL the burst overruns
vblank and the table is truncated mid-row; in NTSC the full table lands. Mesen2
agrees on both: its PAL screen is pixel-identical to Luna's truncated one, and
in NTSC both render the full all-PASS table. (The family used to run as PAL,
on the opposite theory, calibrated against an old boot that ran too fast.)

## Why two tiers

The per-instruction suites prove each opcode is right in isolation but say
nothing about how the subsystems interact. The golden ROMs prove the *system*
renders correctly but, being Luna's own output, can only catch *regressions*.
Together — plus the differential harness for timing — they cover correctness
from the single instruction up to the whole frame.

## Tier 3 — user-facing: `luna test` (homebrew CI)

The first two tiers test *luna*; `luna test` lets homebrew developers test
*their game* with the same machinery — TOML manifests asserting on symbols,
the SDK assert/log channels and displayed-frame hashes, run in-process
through `luna-api` with a CI exit-code contract. See
[Developing homebrew with luna](../using/homebrew-ci.md).
