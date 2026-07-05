# Changelog

All notable user-facing changes to luna. Releases are cut from `main`
(tags `vX.Y.Z`, binaries attached by CI); day-to-day development happens on
`develop`. Format inspired by [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Fixed
- HDMA mid-frame enable is now a faithful port: a channel enabled after
  frame start runs from its stale table pointer (ares/Mesen2 semantics), not
  a re-read of the source address — closes audit row #9 (#58).
- HDMA indirect "last active channel" 1-byte reload quirk — closes audit
  row #10 (#57). The HDMA/DMA pillar audit is now ✅ on every
  visual/behavioral row.

### CI / docs
- CI clippy now runs `--all-features`, matching the local gate; Tom Harte
  CPU suites run weekly on a cron (#59).
- README homebrew demo grid (Mode 7 / HDMA wave / windows / gradient) — no
  commercial-game imagery (#60); repo description + topics set.
- Living accuracy scorecard (single current-truth table; the May/June
  review archived), this CHANGELOG, and CONTRIBUTING.md.

## [1.4.0] — 2026-06-28

### Added
- **Windows x86_64 and macOS Apple-Silicon (arm64) binaries** — the release
  matrix now builds four native targets (zip on Windows, tar.gz elsewhere,
  SHA-256 sidecars) (#54, #56).
- Install docs for all four platforms, incl. the macOS Gatekeeper
  quarantine step (#55).

## [1.3.0] — 2026-06-26

### Added
- **Faithful Mesen2 Event Viewer** in the GUI debugger: register-access
  events plotted as coloured dots over the live frame at `(scanline,
  H-clock)`, 25-checkbox filter panel, decoded event list; captures PPU/CPU
  register writes, DMA and HDMA per-channel, with exact H-clock timestamps
  (#49, #51).

## [1.2.0] — 2026-06-24

### Fixed
- SA-1 HV-mode timer (faithful `SA1::step` port), faithful `nmiLine`
  ($4210 clear at VBlank end), and late-NMITIMEN.7 NMI delivery (#37, #39,
  #40).

### Added
- Self-contained Mesen interrupt-timing differential — confirms luna's
  observable NMI/IRQ cadence matches the reference (#41).

## [1.1.0] — 2026-06-22

### Added
- **SNES Mouse and Super Scope**, faithful ares ports: GUI device picker
  (Settings → Devices) with live pointer capture, plus headless scripting
  (`--port1/2`, `--mouse`, `--superscope`) (#27–#30).
- OpenSNES follow-up RFEs: `force_blank` trace column, `--until-frame`,
  `--assert-cgram`, trace-determinism guarantees doc (#31–#33).

## [1.0.0] — 2026-06-22

First stable release.

### Added
- Battery SRAM persistence (sidecar `.srm`) with periodic auto-flush;
  versioned, ROM-hash-guarded save states.
- Video-as-clock frame limiter — kills motion judder in the GUI.
- Commercial-title regression net (15 hardware-coverage goldens) and the
  SPC700 memory-result oracle.

## [0.3.x] — 2026-06-21 … 2026-06-22

- **0.3.3** — SPC700 audio accuracy finished (`$F0` wait-state dividers);
  reproducible accuracy corpus.
- **0.3.2** — frame/line/blank trace columns + mem-trace address filter
  (OpenSNES SDK RFEs).
- **0.3.1** — Star Ocean (S-DD1) plays past the intro.
- **0.3.0** — OpenSNES SDK harness: native asserts, SRAM persistence,
  memory breakpoints, poke/search/run-until/set-register, MCP tool
  promotion; **S-DD1 coprocessor**.

## [0.2.0] — 2026-06-20

### Added
- **First binary release**: Linux x86_64 + aarch64 tarballs with SHA-256
  sidecars, built natively by CI on `v*` tags.
- SA-1 bus-conflict contention; PPU OBJ-cache rendering performance.

## [0.1.0] — 2026-06-19

### Added
- Cycle-stepped SPC700 core (mid-instruction resumable) on a 2× SPC clock
  domain; SA-1 `$2225` per-side BW-RAM banking (fixes the Super Mario RPG
  attract demo).

## [0.0.1 – 0.0.4] — 2026-06-15 … 2026-06-17

Early development: first public tags while the emulator grew from a booting
core to a playable machine — 65c816/SPC700/PPU/APU cores, LoROM/HiROM/ExHiROM
+ SA-1/Super FX/DSP-1 mappers, the `luna-api` introspection surface, GUI
debugger, MCP server, golden ROM suite and the differential-harness method.
