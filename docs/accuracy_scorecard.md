# luna — Accuracy Scorecard (living document)

**One table, current truth.** Grades measure *behavioral correspondence to
ares + Mesen2* (the two reference emulators), not code quality. Rubric: **A**
= faithful port, residuals below the observable floor · **A−** = faithful with
named theoretical residuals (no known game impact) · **B+/B** = correct on
everything tested, unaudited corners remain · **C/D** = known divergences with
game impact.

> **Rule: any accuracy fix updates its row here in the same PR** (the HDMA
> pillar already mandates this for `hdma_ares_audit.md`; this generalises it).
> The full May-2026 review and its June re-grounding are preserved verbatim in
> [`archive/accuracy_scorecard_2026-05_regrounded_2026-06.md`](archive/accuracy_scorecard_2026-05_regrounded_2026-06.md).

## Scorecard

| Subsystem | Grade | Current state (one line) | Evidence / details | Last verified |
|---|:---:|---|---|:---:|
| CPU 65c816 | **A−** | Tom Harte 5.08M cases 100% + per-entry `cycles[]` bus-order oracle (94% entry-exact; the residual ~30 opcodes are ares-faithful — don't chase) | [`luna_65c816_gaps.md`](luna_65c816_gaps.md), `tests/tom_harte.rs` | 2026-06-20 |
| SPC700 | **A−** | All 254 opcodes cycle-stepped, byte/cycle-exact vs the atomic core; cooperative CPU↔SPC interleave at bus-access grain; `$F0` wait-state dividers modelled | [`luna_spc700_gaps.md`](luna_spc700_gaps.md), Tom Harte SPC700 100% | 2026-06-22 |
| S-DSP (audio) | **A** | Cycle-accurate ares port; BRR→PCM proven bit-exact vs an independent Mesen2-form decoder over 200k random groups; 10 PCM goldens CI-gated | [`luna_apu_gaps.md`](luna_apu_gaps.md), `luna-apu/src/dsp.rs` tests | 2026-06-23 |
| PPU | **A−** | Full feature set faithful (EXTBG, offset-per-tile, mosaic, interlace, hi-res 5/6, Mode 7, windows, color math). `$21xx` open bus = faithful two-chip MDR model incl. stale-bit partial updates (2026-07-26; residual: STAT78 bit-6 PIO gate). Open: gap #7 HiColor sub-scanline CGRAM timing (81% pixel-exact, `#[ignore]` tripwires keep it visible) | [`luna_bg_gaps.md`](luna_bg_gaps.md), [`luna_obj_gaps.md`](luna_obj_gaps.md), [`ppu_compositor_reference.md`](ppu_compositor_reference.md) | **2026-07-26** |
| DMA / HDMA | **A−** | **Pillar audit closed 2026-07-01**: every visual/behavioral row faithful (mid-frame enable = stale pointer, indirect last-active 1-byte quirk, count-0 header, MDMA preemption). Per-line cycle cost (#11) closed 2026-07-15 (faithful per-A-bus-read model, #117); residual = #13 edge interactions only (`$420C` mid-DMA, HDMA on the same line as MDMA) | [`hdma_ares_audit.md`](hdma_ares_audit.md), [`luna_dma_gaps.md`](luna_dma_gaps.md) | **2026-07-15** |
| SA-1 | **A−** | `conflict()` bus contention, faithful HV timer, per-access cycle cost; residual = batched (non-cothread) scheduler grain, not a value/feature bug | [`luna_sa1_gaps.md`](luna_sa1_gaps.md), [`sa1_status.md`](archive/sa1_status.md) | 2026-06-23 |
| Super FX (GSU) | **A−** | Engine proven byte-exact vs Mesen (single-step + trajectory differential harnesses); level-IRQ semantics fixed; Star Fox / Doom / Yoshi's Island / Stunt Race FX reach gameplay. Residual = batched scheduling grain (same class as SA-1) | [`superfx_reference.md`](superfx_reference.md), `luna-bus/src/superfx.rs` harnesses | 2026-06-20 |
| DSP-1 (uPD7725) | **B+** | Core implemented (`luna-cpu-upd96050`); Super Mario Kart Mode 7 and Pilotwings correct. No per-op differential vs a reference core yet — grade capped until one exists | `luna-cpu-upd96050/src/lib.rs`, [`firmware.md`](firmware.md) | 2026-06-19 |
| S-DD1 | **A−** | Decompressor proven byte-exact (staged differential); MMC banking faithful; Star Ocean and Street Fighter Alpha 2 play | [`sdd1_reference.md`](sdd1_reference.md) | 2026-06-22 |
| Bus / mappers | **B+** | ROM mirroring, open-bus MDR latch, `score_header` mapper detection, memory-speed table — all faithful and tested. Unaudited corners: exotic boards outside the supported set | `luna-bus/src/speed.rs`, `luna-cartridge` | 2026-06-17 |

## What "verified" means

Every grade above is backed by at least one *measurement* (never "looks right
on one screen"):

- **Exhaustive per-instruction suites** — Tom Harte SingleStepTests, both CPU
  cores, strict mode (`tom-harte.yml`, weekly + on demand).
- **Golden ROM suite** — ~92 framebuffer/PCM/mailbox SHA-256 goldens, CI-gated
  (`snes_test_roms.rs`).
- **Differential harnesses** — GSU single-step + trajectory vs Mesen, BRR→PCM
  decoder differential, NMI-cadence and WRAM-hash traces vs Mesen
  (`book/src/method/differential.md`).
- **Commercial-title regression net** — 15 game goldens + the HDMA corpus
  sweep (`tools/validate-hdma-corpus.sh`), developer-local (copyrighted ROMs
  are never committed).

## Open items (all below the observable floor)

1. PPU gap #7 — HiColor sub-scanline CGRAM timing (needs a per-dot CGRAM
   model; tripwire goldens in place).
2. ~~HDMA per-line cycle-count (#11 in the audit)~~ — **closed 2026-07-15**:
   faithful per-A-bus-read cost model (`hdma_cost`), `HDMA_OVERHEAD_MCLK`
   retired. #13's edge *interactions* (`$420C` mid-DMA) stay open.
3. SA-1 / Super FX scheduler grain — batched stepping vs ares' cothreads;
   engine outputs are exact, only stall placement differs.
4. P4 interrupt micro-timing (TIMEUP hold window, last-dot guard, htime=0
   delay). The Mesen differential shows the NMI/IRQ *cadence* matches, but
   that is not the same as the *registers* being right at every H-clock: the
   RDNMI (`$4210`) visibility window was observably wrong until 2026-07-13
   (#107 — a `BIT $4210 / BPL` poll loop passed twice per VBlank, so the whole
   PeterLemon corpus animated ~4/3x too fast). `$4211` TIMEUP has the same
   shape of hold window and is **not** yet measured against a reference —
   treat it as the next candidate, not as verified.
   **2026-07-15 update:** the CPU↔scanline phase work (#109) landed five
   faithful timing fixes — reset preamble (`//H=186`), DRAM refresh charged
   during DMA, DMA-clock-aligned refresh position, ares' MDMAEN/HDMA cost
   models, NTSC short scanline — then closed with ares' remaining two terms:
   the read **sample point** (`step(cost−4); read; step(4)`) and the
   **deferred `dmaEdge`** (a `$420B`-armed burst runs at the next access,
   charged to the next instruction). Verified by the per-instruction cycle
   differential vs Mesen2: on CPUBRA the two emulators are **cycle-identical
   over 841 386 instructions — zero per-instruction deltas differ**. The
   CPU↔scanline phase is **locked**: the faithful RDNMI pair (raise H=2,
   hold [2,6)) is live and `WaveHDMA` polls exactly once on 139/139 frames
   (#107's conservative masking is retired). `$4211` TIMEUP's hold window
   remains the un-measured sibling.
5. DSP-1 differential oracle — the one grade capped by *missing evidence*
   rather than a known divergence.
