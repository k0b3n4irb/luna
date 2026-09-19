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
| CPU 65c816 | **A−** | Tom Harte 5.08M cases 100% + per-entry `cycles[]` bus-order oracle (94% entry-exact; the residual ~30 opcodes are ares-faithful — don't chase). **Named open residuals** (timing only, no state effect — [`luna_65c816_gaps.md`](luna_65c816_gaps.md) #1-#4): hardware NMI/IRQ entry omits the two leading dummy cycles of ares `interrupt()` (`read(PC); idle()`), so it takes 6 bus cycles in native mode where hardware takes 8; NMI/IRQ are polled at the instruction boundary (`step()`), not on the instruction's last cycle; no one-instruction recognition delay after `CLI`/`SEI`/`PLP`; `WAI` wakes on an 8-mclk grid | [`luna_65c816_gaps.md`](luna_65c816_gaps.md), `tests/tom_harte.rs` | 2026-06-20 |
| SPC700 | **A−** | All 254 opcodes cycle-stepped, byte/cycle-exact vs the atomic core; cooperative CPU↔SPC interleave at bus-access grain; `$F0` wait-state dividers modelled. **2026-09-11:** the CPU→SPC clock ratio now uses the console's master clock — PAL ran the SPC against the NTSC clock (~0.9 % slow, flat music); `$F3` writes through `$F2 ≥ $80` dropped (read-only mirror); CPU mailbox mirrored across `$2140-$217F` | [`luna_spc700_gaps.md`](luna_spc700_gaps.md), Tom Harte SPC700 100% | **2026-09-11** |
| S-DSP (audio) | **A** | Cycle-accurate ares port; BRR→PCM proven bit-exact vs an independent Mesen2-form decoder over 200k random groups; 9 PCM goldens (8 CI-gated, PitchMod `#[ignore]`d — not a luna bug) | [`luna_apu_gaps.md`](luna_apu_gaps.md), `luna-apu/src/dsp.rs` tests | 2026-06-23 |
| PPU | **A** | Full feature set faithful (EXTBG, offset-per-tile, mosaic, interlace, hi-res 5/6, Mode 7, windows, color math) **on the hardware line origin** (picture = PPU lines 1..=224, row r scanned during line r+1 — gap #7 root cause, found via the HiColor chart): **16 corpus tests pixel-exact vs hardware reference PNGs** (WindowHDMA, Mode7HDMA, Perspective, Rings, HiColor64/3840/575Myst, BGMap family, …). `$21xx` open bus = two-chip MDR (2026-07-26). Residual: HiColor128 band-group parity (91%, gap #7b tripwire; STAT78 PIO gate closed 2026-08-01 along with the WRIO falling-edge latch + SLHV gate). **2026-09-11 audit:** Mode 5/6 priority tables and OAM priority rotation (`byte >> 2`, live address) fixed; sprites now fetched one line ahead of the row they appear on (2026-09-12 — the KDL3 last-row strip); VBlank entry now follows SETINI overscan in both regions (2026-09-12: `vdisp` = 225 / 240, recomputed on the `$2133` write like ares — PAL was hardcoded to 240, so every PAL title without overscan got its NMI 15 lines late and 15 extra HDMA lines); `$2104` / `$2138` now redirect to the sprite being evaluated and VRAM reads return 0 while the picture is drawn (2026-09-12); clip-to-black now runs before the math with halving off (2026-09-12); **2026-09-14:** CGRAM accesses during the picture (display on, lines 1..vdisp, dots 22..274) land on the entry the PPU last fetched for the pixel under the beam, CGADD still advancing (ares `io.cpp:47-61`, `dac.cpp:158`; Mesen2 `InternalCgramAddress`); the picture is 239 rows when the frame starts in overscan (ares `main.cpp:4`, Mesen2 `_overscanFrame`) — the last two rows of the 2026-09-11 audit, closed | [`luna_bg_gaps.md`](luna_bg_gaps.md), [`luna_obj_gaps.md`](luna_obj_gaps.md), [`ppu_compositor_reference.md`](ppu_compositor_reference.md) | **2026-09-14** |
| DMA / HDMA | **A−** | **Pillar audit closed 2026-07-01**: every visual/behavioral row faithful (mid-frame enable = stale pointer, indirect last-active 1-byte quirk, count-0 header, MDMA preemption). Per-line cycle cost (#11) closed 2026-07-15 (faithful per-A-bus-read model, #117); residual = #13 edge interactions only (`$420C` mid-DMA, HDMA on the same line as MDMA). **2026-09-11:** HDMA honours the `$43x0` direction bit (row #3 had claimed ✅), `$420B/$420C` cleared on reset (row #14); **2026-09-18:** every DMA/HDMA read latches the MDR and unmapped reads return it (was `$FF`), the APU mailbox is reachable on the DMA B-bus (row #18); **open:** segmented-path cost/realign, DMA start edge, same-channel HDMA abort | [`hdma_ares_audit.md`](hdma_ares_audit.md), [`luna_dma_gaps.md`](luna_dma_gaps.md) | **2026-09-18** |
| SA-1 | **A−** | Register, memory and DMA model ported line for line from ares (`io.cpp`, `memory.cpp`, `bwram.cpp`, `dma.cpp`): side-split register dispatch, the three BW-RAM views (linear `$40-$5F`, bitmap `$60-$6F`, the CBM window in both modes), per-device DMA charging the SA-1 its steps, VLBP on the `$2258` write / `$230D` read, BW-RAM protection armed at power-on, CC1 / CC2, RDYB, ROM mirroring, CIV/CNV interrupt delivery, `conflict()` contention, faithful HV timer. **Residual:** batched (non-cothread) scheduler grain — the same class as Super FX — plus the deliberate CIWP/SIWP `$FF` default ([`luna_sa1_gaps.md`](luna_sa1_gaps.md) #3/#4) | [`luna_sa1_gaps.md`](luna_sa1_gaps.md), [`sa1_status.md`](archive/sa1_status.md) | **2026-09-14** |
| Super FX (GSU) | **A−** | Engine proven byte-exact vs Mesen (single-step + trajectory differential harnesses); level-IRQ semantics fixed; Star Fox / Doom / Yoshi's Island / Stunt Race FX reach gameplay. Residual = batched scheduling grain (same class as SA-1) | [`superfx_reference.md`](superfx_reference.md), `luna-bus/src/superfx.rs` harnesses | 2026-06-20 |
| DSP-1 (uPD7725) | **A−** | Port-level differential vs Mesen2: the complete DR command/result byte stream is **byte-identical over 380 783 events** (SMK title + demo race, 60 s no-input; SR polling excluded as timing-sensitive). Validates the uPD7725 core + firmware decode + mapper glue end-to-end. Residual = no per-op internal-state oracle (Mesen2 Lua doesn't expose NecDsp registers) | `tests/dsp1_port_differential.rs`, `tools/mesen-dsp1-port-trace.lua`, [`firmware.md`](firmware.md) | **2026-07-26** |
| S-DD1 | **A−** | Decompressor proven byte-exact (staged differential); MMC banking faithful; Star Ocean and Street Fighter Alpha 2 play | [`sdd1_reference.md`](sdd1_reference.md) | 2026-06-22 |
| Bus / mappers | **B+** | ROM mirroring, open-bus MDR latch, `score_header` mapper detection, memory-speed table — all faithful and tested. Unaudited corners: exotic boards outside the supported set. **2026-09-11:** map mode accepted only as exact values, else the header location (ares `board()`) — Contra III (`$53`) was loaded as SA-1; `$420D` MEMSEL writes now persist and MEMSEL powers up slow. **2026-09-18:** unemulated coprocessors are named and refused instead of booting bare — DSP-2/3/4 (were misdetected as DSP-1 and handed `dsp1b.rom`; told apart by title as ares `firmwareNEC()` does), OBC1, S-RTC, SGB, ST-010/011, ST-018, Cx4, SPC7110 (ares `board()` chipset + `$FFBF` sub-type); `--force-mapper` still loads them chipless. **Open:** LoROM SRAM ≤ 2 MB mapping, checksum hard-reject | `luna-bus/src/speed.rs`, `luna-cartridge` | **2026-09-18** |
| Power-on / reset state | **A−** | RAM arrays (WRAM, VRAM, CGRAM 15-bit, OAM, ARAM) selectable `zero` / `ones` / seeded `random` at power-on (ares `cpu.cpp:92`, `ppu.cpp:99,123`, `dsp.cpp:199`; Mesen2 `InitializeRam`); every array persists across a soft reset (ARAM fixed 2026-09-08). `--power-on random` also randomises the PPU's registers, latches and both chip MDRs (ares `PPU::power`, 2026-09-12), and DMA channel registers power up at `$FF` in every mode (ares `cpu.hpp:217-251`, Mesen2's constructor) — issue #224 closed. **2026-09-11:** reset keeps the controller port devices (Mouse / Super Scope) and host inputs, returns MEMSEL to slow and clears HDMAEN | `luna-core/src/power.rs`, `Snes::apply_power_on` tests | **2026-09-11** |
| Controllers | **B+** | Standard pad (auto-read `$4218-$421B` + serial `$4016/$4017`, idle-high data line after the 16 shifts), SNES Mouse and Super Scope (ares `controller/mouse`, `controller/super-scope` ports; PPU H/V latch through WRIO; the Super Scope's held-button turbo / trigger-lock state machine is deliberately simplified to per-frame scripted states), selectable per port, scriptable from CLI / MCP / GUI. **2026-09-18:** Super Multitap (ares `controller/super-multitap`: detection while latched, iobit-selected pad pairs on d0/d1, per-pad serial shifters; `$421C-$421F` carry the d1 lines) on either port — players 3-5 via CLI `--input3..5`, MCP `set_joypad {port: 2..4}`. **Not emulated:** the Justifier, the two-tap 8-player setup; the `$4017` read leaves bits 2-4 low (ares ties them high). No differential oracle for the serial devices (unit tests + in-game validation only) | `luna-core/src/controller.rs`, `luna-core/src/cpu_regs.rs` | **2026-09-18** |

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

1. ~~PPU gap #7 — HiColor sub-scanline CGRAM timing~~ — **cracked
   2026-07-26**: never a CGRAM-timing bug (the luna↔Mesen2 write timeline
   was byte-identical); it was the framebuffer LINE ORIGIN (hardware
   displays lines 1..=224 → row r is scanned during line r+1). Fixed with
   the hardware origin + the DMA-path partial flush + the HDMA end-of-line
   application point; HiColor64 and 15 other corpus refs are now
   pixel-exact. Residual **#7b** (HiColor128, 91% exact, tripwire in
   place): in the bottom half every SECOND 8-line tile-row band (y
   168-175, 184-191, 200-207, 216-223, plus rows 95/111) renders exactly
   ONE LINE EARLY — an alternating palette-GROUP parity around the
   every-16-lines CGADD reset. **Not** a per-byte burst-clock issue: that
   first lead was retracted after calibrating the two emulators' line
   phase on the `$FFEA` NMI-vector fetch, which showed luna's IRQ entry,
   handler and CGRAM-burst positions all cycle-aligned with Mesen2 (the
   deltas-not-absolutes rule; the full measurement lives in the
   `ppu_hdma_hicolor128` tripwire).
2. ~~HDMA per-line cycle-count (#11 in the audit)~~ — **closed 2026-07-15**:
   faithful per-A-bus-read cost model (`hdma_cost`), `HDMA_OVERHEAD_MCLK`
   retired. #13's edge *interactions* (`$420C` mid-DMA) stay open.
3. SA-1 / Super FX scheduler grain — batched stepping vs ares' cothreads;
   engine outputs are exact, only stall placement differs.
4. ~~P4 interrupt micro-timing (TIMEUP hold window, last-dot guard,
   htime=0 delay)~~ — **ported 2026-07-26**, see the update at the end of
   this item; the history below is kept because it explains *why* the
   RDNMI half had to land first.
   The Mesen differential shows the NMI/IRQ *cadence* matches, but
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
   (#107's conservative masking is retired).
   **2026-07-26 update — the three deferred TIMEUP siblings are ported**
   (ares irq.cpp): the 10-clock detect→assert counter-sampling delay
   (vcounter(10)/hcounter(10), incl. its next-line wrap), the
   "no IRQ on the last dot of a field" guard, the 4-clock `$4211` hold
   window (mirror of the RDNMI hold), the irqLine drop on NMITIMEN
   IRQ-disable, and the $4210/$4211/$4212 CPU-open-bus passthrough bits
   (found by measurement: Mesen returns MDR low bits, luna returned
   zeros). Unit-tested per edge case; 89/89 goldens (CPUPHL re-anchored,
   verified against the hardware reference PNG); Doom NMI/TIMEUP cadence
   unchanged vs Mesen; Tales of Phantasia (pure V-IRQ) plays clean.
   Residual: the hold is positional per-raise, not a live 4-clock poll
   grid — below the bus-access observable floor.
5. ~~DSP-1 differential oracle~~ — **closed 2026-07-26**: port-level DR
   stream differential vs Mesen2, byte-identical over 380 783 events
   (`tests/dsp1_port_differential.rs`; Mesen2's Lua does not expose the
   NecDsp registers, so the DR protocol stream — the chip's complete
   observable behaviour — is the oracle). Grade flipped B+ → A−.
