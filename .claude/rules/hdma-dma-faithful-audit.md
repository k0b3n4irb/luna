# HDMA / DMA — faithful port + systematic ares audit (PILLAR, auto-loaded)

IMPORTANT: The DMA/HDMA controller is a **pillar** subsystem — shared by
every game, and the source of repeated *silent, game-specific* rendering
bugs. In June 2026 two distinct latent HDMA edge-case bugs surfaced within
days, each from a single commercial title, each invisible to the golden
test suite:

- **mid-frame HDMA enable** ignored — Yoshi's Island intro text (`$420C`
  written at scanline ~12, not vblank). Fixed: live `hdmaActive()` gating.
- **count-0 line-count header** treated as 1 line instead of 128 — Contra
  III title logo (`$80` header). Fixed: full-8-bit decrement, reload when
  `& 0x7F == 0`.

Two bugs in two days, found by *eyeballing real games*, means the HDMA
implementation is **not yet a faithful port** — it is an approximation with
an unknown number of remaining edge cases. Treat it accordingly.

## The mandate

1. **ares `ares/sfc/cpu/dma.cpp` + `timing.cpp` are the spec.** Any change to
   `crates/luna-core/src/dma/` — and any HDMA/DMA bug — is resolved by
   reading the matching ares code and translating it faithfully, including
   the edge cases (count-0, mid-frame enable/disable, the indirect
   "last active channel" 1-byte reload quirk, `validA` masks, transfer-mode
   patterns + lengths, vblank/HDMA-init ordering). This extends
   `.claude/rules/faithful-port-and-dichotomy.md` to make DMA a named pillar.

2. **Keep `docs/hdma_ares_audit.md` current.** It is a living, line-by-line
   comparison of luna's HDMA against ares: every behavior, its status
   (✅ match / ⚠️ gap / 🔧 fixed), and the open items. Before touching HDMA,
   read it; after a faithful fix, update it. New games that misbehave on
   HDMA get a row.

3. **The golden suite is NOT sufficient for HDMA.** Both 2026-06 bugs passed
   all 59 golden ROM tests (no golden exercised count-0 or mid-frame enable).
   Visual regression on real titles is mandatory: run
   **`tools/validate-hdma-corpus.sh`** (gradients, raster/status-bar splits,
   Mode 7, mid-frame splits) and eyeball the output for any HDMA change. Add
   a title to that script whenever a new HDMA case is found.

4. **A confirmed HDMA divergence gets a regression unit test** in
   `crates/luna-core/src/dma/` (see `hdma_enabled_mid_frame_starts_from_source`,
   `hdma_header_low7_zero_is_a_128_line_entry`) — a synthetic table that
   isolates the exact edge case, so the golden gap is closed permanently.

## When this rule applies

- Any edit to `crates/luna-core/src/dma/` (controller, channel, bus).
- Any game-specific rendering bug where a layer/effect appears/disappears on
  a screen split, status bar, gradient, or raster effect — suspect HDMA and
  consult the audit doc before theorising.
- The DMA/HDMA audit is **open and ongoing**; closing it (faithful parity
  with ares, all rows ✅) is a standing goal, not a one-off task.
