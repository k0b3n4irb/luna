# HDMA / DMA — faithful-port audit vs ares (living document)

**Status: OPEN / ongoing.** Reference: ares `ares/sfc/cpu/dma.cpp` +
`timing.cpp` (fetched 2026-06). Governed by
`.claude/rules/hdma-dma-faithful-audit.md`. luna impl:
`crates/luna-core/src/dma/{controller,channel,bus}.rs`.

Legend: ✅ faithful · 🔧 was wrong, now fixed · ⚠️ known divergence (open) ·
🔬 unaudited.

## Why this exists

Two distinct latent HDMA edge-case bugs surfaced within days (2026-06), each
from one commercial title, each passing all 59 golden ROM tests. HDMA is a
pillar subsystem; until every row below is ✅, treat it as an approximation.

## Per-behavior comparison

| # | ares behavior (`dma.cpp` / `timing.cpp`) | luna | Status |
|---|---|---|---|
| 1 | **Transfer-mode patterns** `transfer()` offsets (mode 1/5→bit0, 3/7→bit1, 4→index) + lengths `{1,2,2,4,4,4,2,4}` | `TransferMode::pattern()` | ✅ match |
| 2 | **`validA` masks** — A-bus blocks `$2100-21FF`, `$4000-41FF`, `$4200-421F`, `$4300-437F` | `valid_a()` | ✅ identical masks |
| 3 | **Direction** 0=A→B (readA/writeB), 1=B→A | `channel.rs` transfer/step | ✅ match |
| 4 | **WRAM→WRAM suppression** — `addressB==$80` + A in WRAM ⇒ invalid (no write) | `b_offset==0x80 && is_wram_a()` | ✅ match (the Kirby $2180 path) |
| 5 | **Line-counter model** — full 8-bit `lineCounter--`, reload when `(lineCounter & 0x7F)==0`; a `$80`/low-7-zero header = 128-line entry | full-byte decrement, reload on `& 0x7F == 0` | 🔧 fixed (PR #6 — was `(ntlr&0x7F).saturating_sub(1)` → 1-line; Contra III logo) |
| 6 | **`hdmaActive() = hdmaEnable && !hdmaCompleted`** — per-line gate on the *live* HDMAEN; a channel enabled mid-frame runs from that point | per-frame `hdma_started` lazy-start on first active line | 🔧 fixed (PR #3 — Yoshi's Island). NB: luna lazy-inits from source; ares keeps stale state + `hdmaDoTransfer=true`-for-all at setup. Validated equivalent for the known cases; see ⚠️ #9. |
| 7 | **Indirect address reload** — on a new entry read 2 bytes (`lo`, `hi`) into `indirectAddress` | reads `lo`+`hi` on reload | ✅ match (the common path) |
| 8 | **Frame-start init timing** — `hdmaSetup` at V=0 (`hcounter ≥ ~12`), `hdmaReset` clears completed/doTransfer for all | `hdma_init` at frame wrap | ✅ functionally; sub-dot H position not modelled (🔬 timing) |
| 9 | **`hdmaSetup` sets `hdmaDoTransfer=true` for ALL channels** (even disabled) when any HDMA is enabled (`dma.cpp:143`) | luna sets disabled channels `do_transfer=false`, uses lazy-start instead | ⚠️ structural difference — open. Equivalent output on YI/Contra III/corpus, but unverified vs every mid-frame toggle pattern. |
| 10 | **Indirect "last active channel" 1-byte quirk** (`dma.cpp:165`) — if `$43xA==0` on reload AND this is the last active HDMA channel, load only **1** byte for the indirect address (address ends one short, one fewer cycle) | luna always reads 2 bytes | ⚠️ **not implemented** — open. Rare (terminating indirect entry on the last channel); affects address + 1 read of timing. |
| 11 | **Per-line table read for timing** — `hdmaReload` does `readA` of the header **every** active line (`dma.cpp:153`), even gap lines | luna reads the next header only when the counter reaches 0 | ⚠️ timing approximation — luna folds per-line HDMA cost into the canonical 18-mclk/line `HDMA_OVERHEAD_MCLK`. Cycle count, not visual. |
| 12 | **HDMA vs MDMA arbitration / mid-DMA pause** (`hdmaTransfer`/`dmaRun` set `dmaEnable=false`) | a long sync DMA is driven in scanline-bounded segments; HDMA fires at each crossed visible line via `sched_one_line` | 🔧 fixed (Phase 5 inc 1) — HDMA now preempts a mid-frame MDMA at scanline boundaries instead of being deferred to after the whole burst. Test `hdma_preempts_a_long_mid_frame_dma_at_scanline_boundaries`. Sub-line position (ares dot-276 `hdmaPosition=1104`) is still line-granular — see #8/#11. |
| 13 | **`$420C` write mid-DMA, HDMA on the same line as MDMA, DMA during HDMA** edge interactions | 🔬 | unaudited |

## Fixed (regression-tested)

- **count-0 header = 128-line entry** — PR #6, test `hdma_header_low7_zero_is_a_128_line_entry`.
- **mid-frame HDMA enable** — PR #3, test `hdma_enabled_mid_frame_starts_from_source`.

## Open work (priority order)

1. ⚠️ #10 indirect last-active-channel 1-byte quirk — port `dma.cpp:162-169`
   faithfully (the `if(hdmaCompleted && hdmaFinished()) return;` branch).
2. ⚠️ #9 reconcile the mid-frame model with ares' `hdmaDoTransfer`-for-all
   semantics (or prove the lazy-start equivalent for all toggle patterns).
3. 🔬 #11/#13 cycle-accurate per-line HDMA timing + remaining edge
   interactions (`$420C` write mid-DMA, etc.). (#12 mid-DMA pause: fixed,
   Phase 5 inc 1.) The sub-line dot-276 `hdmaPosition` is Phase 5 inc 2.

## How to extend

Run `tools/validate-hdma-corpus.sh` after any DMA change and eyeball. When a
game reveals a new case: add a row here, a corpus title to the script, a
regression unit test in `crates/luna-core/src/dma/`, then fix faithfully.

> Note: the Tales of Phantasia battle sprite garble (2026-06-14) is an
> **OBJ/sprite** bug, NOT HDMA (its HDMA channels touch only BG scroll /
> CGADD / TM — no sprite register), and pre-existing. Tracked separately
> from this audit.
