# Changelog

All notable user-facing changes to luna. Releases are cut from `main`
(tags `vX.Y.Z`, binaries attached by CI); day-to-day development happens on
`develop`. Format inspired by [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added
- **luna-gui: force the cartridge mapper** (#88). A ROM whose internal
  checksum is blank/invalid (much of the PeterLemon homebrew test corpus)
  used to load as a black screen, because layout auto-detection refuses to
  guess LoROM vs HiROM without a valid checksum and the GUI had no override.
  Now, on a detection failure an inline **"couldn't detect the mapper — load
  as LoROM / HiROM / ExHiROM / SA-1?"** prompt appears and loads the ROM in
  one click (no re-open); a **File ▸ Force mapper** submenu also pre-sets a
  sticky default for opening a whole test corpus. Auto-detect stays the
  default. New API `Emulator::load_rom_forced(path, mapper)` (the path-based
  sibling of `load_rom_bytes_forced`) backs it.

## [1.8.0] — 2026-07-07

Input recording — the record half of luna's input replay, requested and
validated by Cooper (the OpenSNES IDE) for one-click bug repros and
gameplay regression tests.

### Added
- **Input recording** (#83): capture what you actually play and export it
  as the existing `frame:mask` `--input` script — the record half to match
  luna's replay half. In the GUI, *Emulation → ● Record input* toggles
  capture (with a red **⏺ REC** badge) and writes a replayable `.input`
  file to `~/.local/luna/recordings/` on stop. New `luna-api` surface
  (`start_input_capture` / `take_input_capture` / `is_capturing_input`,
  `input_capture_to_script`) records only per-port mask *changes*, so files
  stay tiny; also exposed as the MCP `start_input_capture` /
  `take_input_capture` tools. Only Player 1 changes are recorded (Player 2
  is captured but written commented-out, since `--input` is single-port).
- `--input` now also accepts `@<file>` to read a script from a file, and
  the script grammar allows `#` comments and newlines — so an exported
  recording replays directly with `luna state --input @recording.input`
  (pair a save state via `--load-state` for a mid-game capture).

## [1.7.0] — 2026-07-06

The GUI interactive debugger — breakpoints, watchpoints, and stepping
(epic #63 P4, closing the interactive-debugger epic on the luna side) —
plus CLI symbol-name assertions.

### Added
- **GUI breakpoints & stepping** (#68 — epic #63 P4, completing the
  interactive-debugger parity): click a CPU-disassembly row to toggle an
  exec breakpoint (red gutter dot); a Breakpoints panel lists/removes them
  and adds memory watchpoints; on a hit the emulation auto-pauses with a
  halt banner, the disassembly jumps to the PC, and the Event Viewer's
  *Breakpoint* category shows the hit at its exact position. `F10` steps
  one instruction, `F11` one frame. The no-debugger hot path is unchanged;
  `run_until_break` now counts instructions and catches core panics like
  `step`.
- CLI `--assert` / `--peek` accept WLA-DX label names (`--assert
  r_done=EFBE`, `--peek monster_x:2`) resolved through the loaded `.sym`
  table — parity with the MCP `symbol:` args; the numeric forms are
  unchanged (#77). Frees the last downstream `.sym` parser.

## [1.6.0] — 2026-07-05

Interactive-debugger parity (epic #63, P1–P3) — the MCP surface now covers
the OpenSNES snesdbg workflows end-to-end.

### Added
- **WLA-DX `.sym` symbol support** (#67): `<rom>.sym` auto-detected next
  to the ROM (CLI `--sym` overrides); `disassemble_cpu` lines and the MCP
  cpu/mem traces annotate with the nearest label (`name+0xNN`); the
  address-taking MCP tools accept a `symbol` name; new
  `load_symbols` / `resolve_symbol` API + MCP surface — the two symbol
  parsers duplicated in the OpenSNES tooling become deletable
  (epic #63, phase P3).
- **First-class breakpoint/watchpoint registry** (#66): exec breakpoints
  and read/write memory watchpoints halt at full emulation speed via the
  existing trace hook points (zero overhead when unused). New API
  (`bp_add_exec/mem`, `bp_remove/clear/list`, `run_until_break` →
  `RunOutcome`) and MCP tools (`bp_add`, `bp_remove`, `bp_clear_all`,
  `bp_list`, `run_until_break`). Exec bps halt before their instruction
  (resume-friendly); watchpoints report the exact accessing PC, address
  and value (epic #63, phase P2).
- **15 new MCP tools** (#65): CPU/SPC700 disassembly with live-PC/M/X
  defaults, save/load state over the wire, CGRAM peek, the four debug
  renders (tilemap / VRAM tiles / palette / sprite sheet) as base64 PNGs,
  CPU + memory trace enable/drain with filters, and Mouse / Super Scope
  input — the MCP surface now covers the interactive-debugger workflows
  (epic #63, phase P1).

## [1.5.0] — 2026-07-05

The HDMA pillar closure + the 2026-07-05 project-review remediation.

### Fixed
- HDMA mid-frame enable is now a faithful port: a channel enabled after
  frame start runs from its stale table pointer (ares/Mesen2 semantics), not
  a re-read of the source address — closes audit row #9 (#58).
- HDMA indirect "last active channel" 1-byte reload quirk — closes audit
  row #10 (#57). **The HDMA/DMA pillar audit is now faithful on every
  visual/behavioral row**; the scorecard row moves B− → A−.

### Changed
- `luna-cli` internals split into focused modules with the crate's first
  unit tests — 19 tests pin every flag-parser (`--input`, `--mouse`,
  `--peek`, `--assert*`, `--mem-trace-*`). No behavior change (proven:
  `--help` and framebuffer hashes byte-identical) (#62).

### CI / docs / project
- CI clippy now runs `--all-features`, matching the local gate; Tom Harte
  CPU suites run weekly on a cron; new weekly headless-throughput perf
  guard (#59, #64).
- README homebrew demo grid (Mode 7 / HDMA wave / windows / gradient) — no
  commercial-game imagery (#60); repo description + topics set.
- Living accuracy scorecard: one current-truth table, Super FX / DSP-1 /
  S-DD1 graded for the first time, May/June review archived; accuracy
  fixes must now update their row in the same PR (#61).
- This CHANGELOG and CONTRIBUTING.md (#61); `.mailmap`; internal
  version-pin cleanup (#64).

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
