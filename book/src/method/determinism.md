# Trace & snapshot determinism guarantees

This table tells a headless consumer (CI gate, SDK test harness) **how strong an
assertion it may safely write** against each luna output — exact-equality vs
same-arch-only vs visual-anchor-only. It answers OpenSNES follow-up RFE-4.

## Why luna is deterministic at all

luna's entire **headless** path — every CPU core (65C816, SPC700, uPD96050,
GSU), the PPU, S-DSP, DMA/HDMA, and the coprocessors — is **integer-only**.
There is no floating point, no wall-clock time source, no RNG, and no hash-map iteration on
the emulation path. So for a fixed `(ROM, mapper, input script, start state)`
the execution is **bit-identical every run**. (The only floating point in the
project is the *GUI's* audio resampler and frame pacing — neither touches the
headless trace/state/screenshot outputs.)

The framebuffer hash (`--print-fbhash`) is a pure function of that full
execution state, and it is **verified identical across architectures** (it is
the cross-arch visual-regression gate). That makes it the **anchor**: if any
arch-dependent divergence existed anywhere in the emulation, the fbhash would
already differ — it does not. Outputs that *derive from the same execution* are
therefore stable by the same mechanism.

## The table

| Output | Run-to-run (same build) | Cross-architecture | Safe assertion |
|---|---|---|---|
| `--print-fbhash` (`fbhash=<16 hex>`) | **Exact** | **Exact — verified** (the visual gate) | Assert exact equality. |
| Trace **event counts** — `--superfx-trace` / `--sa1-trace` instruction counts, `--dma-trace` / `--mem-trace` row counts, `instructions_executed`, `frame_count`, `nmis_serviced` | **Exact** | **Exact** — a count is a direct consequence of the execution path the fbhash already pins; a divergence would perturb the fbhash | Assert exact counts (a much stronger gate than `> 0`). |
| Trace **row content** — per-row `pc`/`addr`/`value`/`vram_word`, the `blank`/`force_blank` flags, mailbox/SA-1 side events | **Exact** | **Exact** — same integer execution; same anchor argument | Assert exact, OR diff the whole CSV against a committed golden. |
| **WRAM / ARAM byte dumps** (`--assert`, `--assert-aram`, `--assert-vram`, `--assert-cgram`, `peek_*`) | **Exact** | **Exact in practice** (same integer core), but **not yet pinned by a standing cross-arch differential** — the cross-arch WRAM harness needs an x86_64 host luna's CI does not yet have | Assert exact same-arch. Cross-arch, treat fbhash as the guaranteed gate and these as expected-equal-but-unpinned. |
| `--print-fbhash` timing fields, wall-clock, any GUI audio/pacing | n/a (host-dependent) | not stable | Never assert. |

## Practical guidance

- **Strongest cross-arch gate:** `--print-fbhash`. Already your visual anchor.
- **Now also safe to assert exactly, cross-arch:** trace **event counts** and
  **row content** — they cannot diverge without also moving the fbhash. Replace
  `> 0 instructions executed` with the exact count.
- **WRAM/ARAM/CGRAM byte assertions:** rock-solid run-to-run and same-arch.
  Cross-arch they are expected-identical (same integer core) but luna has not
  yet *run* the cross-arch byte differential (no x86_64 CI host); until it does,
  prefer the fbhash for the cross-arch leg and keep byte asserts same-arch.

If you ever observe a cross-arch mismatch in a count or row that the fbhash
agrees on, that is a luna bug — please report it; the anchor argument says it
should not happen.
