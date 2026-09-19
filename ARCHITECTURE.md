# Luna — Architecture

> **This file is a pointer, not the document (2026-09-18).** Luna's founding
> architecture / vision paper (May 2026) used to live here **and** in the
> book, and the two ~1 800-line copies had drifted apart. The maintained
> copy is the book chapter:
>
> **[`book/src/internals/architecture.md`](book/src/internals/architecture.md)**
>
> The previous full text of this file is in git history
> (`git log --follow -- ARCHITECTURE.md`; last full revision `2ec94fa`).

## What that document is

A **historical design document**: kept for the rationale and the layer
model, **not** a description of the current tree — where they differ, the
code wins. For the current workspace layout see `CLAUDE.md` (12 crates)
or `ls crates/`; for the current accuracy status see
[`docs/accuracy_scorecard.md`](docs/accuracy_scorecard.md); for the real
CLI flags and MCP tool catalogue see `book/src/using/`.

## `ARCHITECTURE.md §N` references in the source

Source comments (`crates/*/src/**`, `.github/workflows/ci.yml`) cite
sections as "`ARCHITECTURE.md` §N". Those **§ numbers refer to the book
copy**, whose numbering is identical to the text that used to be here:

| § | Section |
|---|---|
| 1 | Vision & goals |
| 2 | Non-goals |
| 3 | Overview (3.1 layered architecture, 3.2 execution modes) |
| 4 | Rust workspace organization (4.1 cross-target async strategy) |
| 5 | Layer 1 — Bus & memory (`Bus` trait, cartridge mappers, memory map) |
| 6 | Layer 2 — Emulation core (6.1 65C816, 6.2 PPU, 6.3 APU / SPC700, 6.4 DMA & HDMA, 6.5 coprocessors, 6.6 scheduler & cycle-accurate sync) |
| 7 | Layer 3 — Control & introspection API (7.1–7.4) |
| 8 | Layer 4 — MCP server (8.1–8.5) |
| 9 | API-first & ecosystem of use cases (9.1–9.5) |
| 10 | Threading model (10.1–10.3) |
| 11 | Determinism & reproducibility |
| 12 | Testing strategy |
| 13 | Build, distribution, license |
| 14 | Roadmap & phasing |
| 15 | Risks & open questions |
| 16 | Glossary |
