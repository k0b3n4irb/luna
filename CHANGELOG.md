# Changelog

All notable user-facing changes to luna. Releases are cut from `main`
(tags `vX.Y.Z`, binaries attached by CI); day-to-day development happens on
`develop`. Format inspired by [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added
- `luna test`: `[asserts.oam]` — decoded sprite asserts over the same
  OAM decode the sprite viewer uses (#218): `visible = N` counts
  on-screen sprites (`0 <= y < 224`, `-32 < x < 256`) and
  `[asserts.oam.sprites.N]` asserts per-sprite `x`/`y`/`tile`/
  `palette`/`priority`/`w`/`h` (comparator grammar) plus
  `hflip`/`vflip` booleans — retiring the last raw-OAM golden
  workaround.

### Fixed
- `luna test`: `[asserts.dma]` now buckets exactly like the probes'
  `--dma-trace` CSV parse (#217). Both ceilings count only the VRAM
  data ports (`$2118`/`$2119`) — OAM/CGRAM/scroll-register (H)DMA
  writes no longer inflate `unsafe_writes` — and forced-blank bytes
  are excluded from `max_vblank_bytes` (no VBlank deadline under
  forced blank). A failing `unsafe_writes` now names the first
  offending write (frame, line, channel, VRAM word, source).

## [1.16.0] — 2026-08-09

The adoption-feedback release, same-day: the audio-RMS oracle reads the
real stream (#211), block asserts get free labels (#210), the six final
manifest capabilities land (#212) — every remaining OpenSNES Python
probe now has its manifest shape — and STAT78 reports PPU2 revision 3
like both references (#207).

### Added
- `luna test`: the final manifest capabilities (#212) — per-leg
  `mouse`/`superscope` scripts (the `--mouse`/`--superscope` grammars,
  ports 1/2), `[asserts.dsp]` on the S-DSP register file (names or hex
  indices; new `Emulator::dsp_registers()`), `[asserts.footprint]`
  non-zero-byte floors per space, `[asserts.dma]` discipline ceilings
  (`unsafe_writes`, `max_vblank_bytes` — classified from the DMA
  trace), `srm_in`/`srm_out` for battery power-cycle tests, and a
  `firmware = "dsp1b.rom"` gate that SKIPs (never fails) when the blob
  is absent — with `SKIP` lines, a skipped count, and JSON `skipped`
  fields.
- `luna test`: `[asserts.blocks]` entries accept an explicit `offset`
  field, turning the TOML key into a free label — two spaces at the
  same offset (e.g. VRAM[0] and CGRAM[0] after a double DMA) can now
  share one manifest (#210). The key-as-offset form is unchanged.

### Fixed
- `luna test`: `audio_rms_min` no longer reads a silent ring for a ROM
  that is audibly playing (#211). The APU sample ring holds 512 ms and
  drops **new** samples when full, so the runner's single end-of-run
  drain only ever saw the boot silence; it now drains during the run
  (frame-at-a-time under a `frames` bound, chunked under `steps`) and
  computes the RMS over the whole pooled stream — the same audio
  `luna run --audio-out` captures.
- STAT78 (`$213F`) bits 0-3 report PPU2 (5C78) revision **3**, matching
  both references (ares defaults `versionPPU2` to 3; Mesen2 reports 3).
  luna reported 2 through v1.15.0 — an unintentional off-by-one from
  the original diagnostic-registers commit, spotted by OpenSNES's
  open-bus sweep (#207). A game branching on the PPU version bits now
  takes the same path as under the references.

## [1.15.0] — 2026-08-09

`luna test` asserts v2 — the direct answer to OpenSNES's v1.14.0
adoption feedback (#205): the five assert kinds needed for their
remaining ~13 Python probes to become manifests, completing the
harness-retirement arc #181 started.

### Added
- `luna test` asserts v2 (#205, from OpenSNES's v1.14.0 adoption): the
  five assert kinds needed to retire their remaining Python probes —
  `[[checkpoint]]` tables with `delta` directions
  (`increased`/`decreased`/`changed`/`unchanged` vs the previous
  checkpoint) and per-checkpoint values; `{eq|ne|ge|gt|le|lt, width?}`
  comparator tables in `[asserts.values]` (bare ints stay `eq`);
  `[asserts.blocks]` byte-range equality in any space
  (`wram`/`vram`/`cgram`/`oam`/`aram`); `[asserts.trace]` minimum event
  counts (coprocessor liveness); and `audio_rms_min` (the "music is
  playing" oracle).

## [1.14.0] — 2026-08-08

The OpenSNES DX release: everything the SDK team asked for in issues
#168–#181, in one coherent drop. Full CLI↔MCP parity (an agent over MCP
now sees every trace, oracle and channel the CLI does), a much richer
debugging API (symbols v2 with an SPC address space, breakpoints v2,
narrowing search sessions, pokes for every memory space, per-frame
freezes, a tracked call stack), and `luna test` — the manifest-driven
test runner that finally gives homebrew a real CI story.

### Added
- `luna test` (#181) — the manifest-driven homebrew test runner: one
  TOML per test (`rom`, optional `sym`/`input`/`screenshot`, a
  `frames`/`steps` bound, and asserts on `wdm_empty`,
  `nocash_contains`, `fbhash` and symbol values), run in-process
  through `luna-api` with the CI exit-code contract (0 pass / 1 fail /
  2 usage). `--update` regenerates `fbhash` goldens preserving manifest
  comments; `--only` filters; `--report json` emits a machine-readable
  summary. New book chapter "Developing homebrew with luna" with a
  copyable GitHub Actions recipe.
- Tracked 65C816 call stack (#180) — opt-in `enable_call_stack` +
  `call_stack()` on `luna_api::Emulator` (JSR/JSL/RTS/RTL, BRK/COP and
  NMI entries; bounded at 256 frames; RTS-without-JSR tolerant),
  symbol-annotated, exposed as MCP tools, embedded in the state JSON
  while tracking (`call_stack`), and on the CLI as `luna state
  --call-stack`. Maintained API-side by the run loops — the CPU core
  is untouched.
- Pokes beyond WRAM + per-frame freezes (#178) — `poke_vram` /
  `poke_cgram` / `poke_oam` / `poke_aram` on `luna_api::Emulator` and
  as MCP tools (previously `poke_memory` silently skipped everything
  but WRAM), plus `freeze_add` / `freeze_remove` / `freeze_list`:
  cheat-style WRAM pinning re-applied at every frame boundary in every
  run path — including the GUI's interruptible run loop, so the three
  front-ends can never disagree about a frozen value.
- Narrowing memory-search sessions (#177) — `search_begin(width)` /
  `search_refine(op, value?)` / `search_results(limit)` on
  `luna_api::Emulator` and as MCP tools: the classic "find my variable"
  loop (`eq`/`ne`/`lt`/`gt` against a value, `changed`/`unchanged`
  against the previous snapshot).
- Breakpoints v2 (#176) — `bp_set_enabled` (disable without losing id /
  name / hit count), per-breakpoint hit counts (mem watches count at
  most one hit per instruction, matching the first-hit rule), an
  exposed `mirror` flag on watch creation (previously hardcoded on),
  and display names defaulting to the creating symbol. `bp_list`
  reports it all. API note: `Emulator::bp_add_exec` / `bp_add_mem`
  signatures grew `name` (and `mirror`) parameters.
- Symbols v2 (#179, closing the ARAM half of #171): the symbol table
  carries two address spaces (24-bit CPU bus + SPC700 ARAM) — load a
  wla-spc700 driver's `.sym` with `space: "aram"` (API:
  `load_symbols_spc(_str)`) without clobbering the game's CPU symbols;
  `disasm_spc` is now annotated and, with `peek_aram`, accepts ARAM
  `symbol` names. WLA-DX `[definitions]` constants resolve by name (and
  never annotate addresses). Name resolution is a binary search and
  parse dedup is sort-based — the old linear/quadratic scans are gone,
  so large `.sym` files stay fast.
- `luna state`: `--peek` results are mirrored into the `--out` JSON as a
  `peeks` array (`{spec, space, addr, bytes_hex, error?}`) so harnesses
  read peeks from the same machine-readable channel as the state instead
  of regex-parsing the stderr hexdump (#175). Existing top-level JSON
  keys are unchanged; failed peeks keep their slot with an `error`
  string. The stderr hexdump stays for humans.
- `luna mcp` preload flags (#174) — `--rom` (with beside-ROM `.sym`
  auto-detection), `--sym`, `--force-mapper`, `--force-region`: the
  session starts with the ROM loaded, so an MCP client's first `state`
  works without a `load_rom` round-trip. The MCP handshake now
  identifies the server as `luna` with luna's real version (previously
  rmcp's) and carries workflow instructions.
- MCP: persistence + media parity (#173) — `sram_get` / `sram_set`
  (base64 `.srm`, the `--srm-out` / `--srm-in` pair), `export_spc`
  (standard `.spc` v0.30 snapshot), `decode_sprites` (structured OAM
  list), and `screenshot` gained `native` (512×448) and `bg` (single
  layer 1..=4) modes. `Emulator::peek_vram` / `peek_aram` `count` is
  now `u32` (capped at `0x10000`) so a full 64 KB dump is one call —
  the CLI `--peek APU:0:10000` form works too.
- MCP: symbol parity (#171, CPU-space half) — new `load_symbols_str`
  (load `.sym` text with no host file), `clear_symbols`, and
  `symbol_for_addr` (reverse lookup) tools; `disasm_cpu` and
  `enable_mem_trace` now accept a `symbol` argument, and `bp_add` gained
  `hi_symbol` for symbol-bounded watch ranges. The ARAM-space tools
  (`disasm_spc`, `peek_aram`) stay numeric until symbols v2 adds an SPC
  address space (#179).
- MCP: full trace parity with the CLI (#172) — mechanical `enable_*` /
  `take_*` tool pairs for the dma, dsp (S-DSP writes), mailbox
  (`$2140-43`), `sa1_log`, `sa1_side_log`, `sa1_trace`, superfx, dsp1,
  and spc700 traces. PC-carrying events are symbolised like the CPU/mem
  traces; `take_dsp1_trace {decode_commands}` additionally returns the
  decoded DSP-1 command transactions (the `--dsp1-trace-commands` view).
- MCP: the CI determinism oracles are now reachable over MCP (#170) —
  `frame_hash {force_display, native}` (the CLI `fbhash=` value, 16 hex
  chars), `set_native_capture`, `wram_page_hashes {page_size}`,
  `wram_snapshot {include_data}` (stable FNV-1a-64 + optional base64
  image), and `loop_probe {max_steps}`. Hashes travel as hex strings
  because JSON numbers can't carry a full u64.
- MCP: headerless / checksum-invalid homebrew is now loadable over pure MCP
  (#169) — `load_rom` gained optional `force_mapper` / `force_region`
  params (the CLI `--force-mapper` / `--force-region` vocabulary), a new
  `load_rom_bytes` tool loads a base64 image with no host file (note: no
  firmware-folder lookup — check `missing_firmware`), and a new
  `set_port_device` tool plugs `joypad` / `mouse` / `superscope` into a
  port without touching the GUI or CLI (the `set_mouse` /
  `set_superscope` descriptions now point at it).
- MCP: the two SDK assert/log channels are now reachable over MCP (#168) —
  `enable_nocash_log` / `take_nocash_log` (the `$21FC` Nocash TTY, drained
  as `{text, base64}`) and `enable_wdm_log` / `take_wdm_log` (the `WDM`
  assert channel, drained as `[{pc, operand, symbol}]`). Parity with the
  CLI's `--nocash-out` / `--wdm-out`.

### Changed
- **BREAKING — save-state format v5** (#167): the ROM-identity hash binding
  a state to its ROM is now an explicit FNV-1a-64 over the raw ROM bytes
  (previously `std`'s `DefaultHasher`, unspecified across toolchains — a
  Rust upgrade could silently orphan every saved state), and the container
  plus all mapper/coprocessor blobs moved from bincode 1.x (EOL) to
  bincode 2. Both breaks share the one version bump: v4 and older `.luna`
  blobs are rejected with a clean version error — re-save from a live run.
  States are now portable across luna builds and toolchains
  (`docs/trace_determinism.md` gained a save-state section).

### Fixed
- `search_memory` hits in the WRAM high half are reported as `$7F:xxxx`
  — previously they leaked as impossible `$7E:1xxxx` addresses (#177).
- `wram_page_hashes` no longer panics the transport on an invalid
  `page_size` — it returns a typed `BadArg` error (#167, #182).
- `run_until_pc` catches core panics like every other run path — a
  crashing ROM surfaces as an error instead of aborting the CLI/MCP/GUI
  (#167, #183).
- `run_until_mem_read`/`run_until_mem_write` no longer clobber a
  caller-enabled memory trace: they ride the breakpoint registry (and are
  panic-safe by construction) instead of hijacking the mem-trace buffer
  (#167, #184).
- "Native capture is not enabled" is a typed usage error (`BadArg`), not
  a fake I/O error; fixed the stale `set_port2_mouse` rustdoc link
  (#167, #185).

## [1.13.0] — 2026-08-03

DSP-1 visibility, end to end. The cart coprocessor was the last one with
no trace surface: Super FX and SA-1 could both be proven to have
executed, the DSP-1 could not, so a divergence there had no
confound-free oracle to bisect against. It now has one, and it goes
further than a raw instruction dump — the byte stream can be read back
as command transactions, checked against the OpenSNES command table
without ever letting that table decide what it is looking at.

### Removed
- **Dependabot** (`.github/dependabot.yml`) — the bot was never
  authorised by the maintainer. It arrived bundled inside the
  supply-chain lot (#129) rather than as a decision of its own, and a
  third-party app opening pull requests against this repository is
  exactly the kind of change that needs to be asked for, not inferred.
  Dependency updates are now manual; `cargo deny` (CI + weekly) remains
  the gate that *surfaces* advisories, and acting on one is a maintainer
  call. Dependabot alerts and automated security fixes were already off
  at the repository level and stay off.

### Added
- **`--dsp1-trace` for DSP-1 (µPD77C25) visibility**
  ([#158](https://github.com/k0b3n4irb/luna/issues/158)) — parity with
  `--superfx-trace` / `--sa1-trace`, so a headless harness can prove the
  DSP-1 executed the same way it already can for the other two
  coprocessors. Three parts:
  - `dsp1.instructions_executed` in the `state` JSON: the
    coproc-liveness counter, readable **without** enabling any trace.
  - `--dsp1-trace` (+ `--dsp1-trace-max`): microcode execution **and**
    the CPU-side DR/SR port traffic in ONE interleaved stream
    (`seq,kind,pc,opcode,value,a,b,dr,sr,rqm`, `kind` = E/W/R/S) —
    because the question a driver author asks is "did my command byte
    land before or after the chip cleared RQM?", which two separate logs
    cannot answer.
  - `--dsp1-trace-ports`: restrict to the DR/SR transactions. The stock
    firmware idles in a two-instruction RQM wait loop, so a full trace
    spends its entire budget on idle spin before the interesting command
    lands (observed on Super Mario Kart: 200 000 events, all idle).
  On a port row `pc` is the **microcode** PC at the moment the CPU
  touched the port, not a CPU address — grouping by it is what makes a
  handshake legible (Super Mario Kart: four `R` sites hit 21 462 times
  each, i.e. a four-word result handed back one word per site, with the
  `S` polls clustered on the first of them).
  - `--dsp1-trace-commands`: the same capture grouped into **command
    transactions** — one row per command byte with the input words it
    consumed and the output words it produced, against the `OpenSNES`
    command table. Transaction boundaries come from the protocol (an
    8-bit `DRC` write opens a command), **never** from the table, so a
    stale word count surfaces as `status=mismatch` on that one row
    instead of silently mis-grouping the rest of the capture — a word
    count is documentation, and documentation must not be able to make
    the emulator look broken. Each row carries a `confidence`
    (`verified` / `documented` / `provisional`) so a disagreement can be
    weighed rather than taken as a verdict; open-ended operations
    (Raster, ROM dump) report their observed length and assert nothing.
    Two rows were settled by measuring rather than by reading a doc:
    `$02` Parameter is 7-in/4-out (stable over 112 consecutive Super
    Mario Kart transactions, promoted upstream on that evidence), and
    `$80` is Sync/Reset — 0/0, hammered 128x at boot to force the chip
    into command-wait. Only `$80` is named: an unrecognised byte
    behaves identically in the reference HLE dispatch, but identical
    behaviour is not identical meaning, so it stays `unknown`.
  Note the neighbouring flag names: `--dsp-trace` is the **audio**
  S-DSP, `--dsp1-trace` the **cart coprocessor**.

## [1.12.0] — 2026-08-01

Player comfort and debugging reach. The GUI gains the three things
anyone actually reaches for (fullscreen, gamepads, volume); the
audio-driver blind spot two downstream reports pointed at is now
instrumented; and the untrusted-input surface, the MCP wire contract
and the H/V-counter latch each gained the evidence they were missing.

### Changed
- **rmcp 0.8 → 3.1** (MCP 2026-07-28 support). Taken with evidence, not
  on a green compile: the new protocol contract tests exercise the
  catalogue, an argument/result round trip, the JSON-RPC error path,
  unknown-tool rejection and a full `load_rom` → `screenshot` → `state`
  agent loop — all pass unchanged against 3.1, so luna's observable MCP
  behaviour is preserved. The client-side `CallToolRequestParams` is now
  non-exhaustive and built through `::new(..).with_arguments(..)` (test
  code only; the server's tool definitions are untouched thanks to the
  macros). Bonus: this retires the `RUSTSEC-2026-0189` ignore in
  `deny.toml` — the DNS-rebinding advisory against rmcp 0.8's HTTP
  transport is fixed upstream, so the exception is gone rather than
  merely justified.

### Added
- **DSP / APU visibility for audio-driver debugging**
  ([#122](https://github.com/k0b3n4irb/luna/issues/122), asked for by
  OpenSNES while debugging its SPC700 arc through WAV captures alone):
  1. `state` JSON gains **`apu.dsp`** — the eight voices as objects
     (`keyed_on`, VOL L/R, pitch, SRCN, ADSR1/2, GAIN, ENVX, OUTX, live
     envelope + phase, BRR address, pitch accumulator) plus master
     state (MVOL/EVOL, EFB, KON/KOFF, FLG, ENDX, PMON/NON/EON, DIR,
     ESA, EDL), instead of eight parallel arrays. The flat `voice_*`
     arrays stay for existing consumers.
  2. **`--peek APU:OFFSET:COUNT`** reads ARAM instead of the CPU bus —
     verify an uploaded driver image or the `$F0-$FF` register page.
  3. **`--dsp-trace <PATH>`** (+ `--dsp-trace-max`) captures every DSP
     register write with an SPC-cycle timestamp as CSV
     `spc_cycles,reg,name,value`, `name` decoded (`V0_ADSR1`, `KON`,
     `FLG`, …) — the sequencing oracle for "did my KON/KOFF pulses
     reach the chip in the order I intended?", which is invisible in a
     WAV.
- **Fuzzing for the ROM-parsing surface** (`fuzz/`, cargo-fuzz): three
  targets covering `Cartridge::from_bytes` (auto-detect, header scoring,
  SMC/firmware stripping), the checksum-skipping `from_bytes_forced`
  path across all 8 mapper kinds, and the full parse → `Snes` →
  `reset` → step chain where an accepted-but-malformed cart reaches the
  mapper shims. First campaign: **~67 million executions, zero crashes**
  — the parser's clamps and mirrored indexing hold under adversarial
  input. Weekly in CI (+ on any PR touching `luna-cartridge` or
  `fuzz/`), with crash reproducers uploaded as artifacts and a minimized
  seed corpus committed so a fresh clone starts from real coverage.
- **MCP protocol contract tests** — the audit's "only front-end without
  a contract test" is closed. `luna-mcp-server/tests/protocol.rs` runs
  the real server against a real rmcp client over an in-memory
  `tokio::io::duplex` pair (no process, no ports, deterministic) and
  locks the wire behaviour: the discovered tool catalogue (plus every
  tool having a description and an input schema), an argument/result
  round trip, errors surfacing as JSON-RPC errors carrying luna's
  message, unknown-tool rejection, and a full `load_rom` → `screenshot`
  → `state` agent loop on a synthetic ROM. This is the evidence the
  pending rmcp 3.0 (MCP 2026-07-28) bump was waiting on.

### Fixed
- **`--input` checkpoints no longer overrun the `-n` budget**
  ([#126](https://github.com/k0b3n4irb/luna/issues/126), reported
  downstream by OpenSNES). Chasing a checkpoint's frame stepped the
  emulator *unbounded* and only then spent `-n`, so
  `luna state -n 100000 --input "900:0x8000"` ran to frame **910**
  instead of frame 12 — a run 75x longer than requested, in which a
  press scheduled far beyond the requested window still reached the
  ROM (the reported "first checkpoint latched at boot"). Checkpoint
  chasing now spends from the same budget as the run: `-n` is the total
  length with or without `--input`, and a checkpoint the run never
  reaches simply never fires. Fixed identically in `state`, `spc-dump`
  and `assets-dump` (`wram-trace` and `bench` already applied
  checkpoints inside their frame loops). CLI-level regression tests
  included.

- **The H/V-counter latch subsystem is now faithful end-to-end** (ares
  `cpu/io.cpp` + Mesen2 agree on all four): the WRIO (`$4201`) latch
  fires on the **falling** edge of bit 7 (luna had the polarity
  inverted), WRIO powers up high (`$FF`), SLHV (`$2137`) only latches
  while the line is high, and STAT78 bit 6 reads forced-1 without
  clearing the latch flag while the line is held low — closing the last
  named residual of the scorecard's PPU row.

### Added
- **GUI comfort trio** — the three most-visible player features:
  **fullscreen** (`F11` remappable hotkey + Emulation menu, borderless),
  **gamepad support** (up to two pads via gilrs, SDL-style mappings,
  fixed Mesen2-like layout, first pad = Player 1, merged with the
  keyboard), and a **volume slider + mute** (Settings → Audio, applied
  live in the audio callback, persisted to `~/.config/luna/audio.json`).
  The *Step frame* debugger hotkey moved from `F11` to `F6`.

## [1.11.0] — 2026-08-01

The audit release: an eight-PR sweep out of a full-project review — one
foundational rendering discovery (the hardware framebuffer line origin,
dormant under every frame since the project began), three accuracy
chantiers, a GUI-robustness batch, and the supply-chain/ops layer. The
accuracy scorecard now has no row below A−, with the PPU at **A**.

### Fixed
- **The framebuffer line origin is now the hardware's** — the root cause
  of PPU gap #7, found by pinning luna's HiColor chart against the
  hardware reference and a Mesen2 capture (luna's frame was pixel-perfect
  but one scanline late; the luna↔Mesen2 CGRAM write timeline was
  byte-identical, 3597/3598 events). Hardware displays PPU lines 1..=224:
  framebuffer row r is scanned during line r+1, and line 0 is the
  pre-render line (why real games set BGVOFS = -1). Three coordinated
  changes: `Ppu` renders line V into row V-1; a DMA B-bus write landing
  mid-line now partial-flushes the in-progress row with the pre-write
  state (the CPU-path flush, mirrored on the DMA path); and each line's
  HDMA transfer fires at the END of its line (ares `hcounter() >= 1104` —
  after the visible pixels of the row being scanned), lifting the
  2026-06-17 dot-276 deferral. Result: **16 PeterLemon corpus tests are
  now pixel-exact against their hardware reference PNGs** (WindowHDMA
  30938→0, Mode7HDMA, Perspective, Rings, HiColor64/3840/575Myst, the
  BGMap family, …; 36/42 improved, 3 within animation-phase noise), the
  HiColor64 tripwire is un-ignored as an active golden, and the whole
  golden suite re-anchored (verified against references + the commercial
  HDMA corpus: F-Zero, SCV4, Yoshi's Island, FF6, SMRPG, RPM Racing).
- **`$4211` TIMEUP is now the faithful ares model** — the last three
  deferred interrupt micro-timing terms are ported (ares `irq.cpp`):
  the H/V-IRQ assert point sits 10 clocks after the counters match
  (the detect→assert pipeline, including its wrap into the next line
  for `htime` near the line end), IRQs cannot trigger across a field
  boundary, a `$4211` read landing within 4 clocks of the raise sees
  the flag without acknowledging it (the RDNMI-hold mirror), and
  disabling both IRQ sources in NMITIMEN drops a held flag at once.
  Also found by measurement vs Mesen2: `$4210`/`$4211`/`$4212` now
  pass the CPU open-bus (MDR) bits through their undriven bit
  positions (bits 4-6 / 0-6 / 1-5) instead of returning zeros.- **PPU open bus is now the real two-chip MDR model** (ares
  `ppu1.mdr`/`ppu2.mdr`, Mesen2 agrees). Reads of the PPU1 write-only
  family (`$2104-06/08-0A/14-16/18-1A/24-26/28-2A`) return PPU1's
  data-bus latch; every other write-only `$21xx` register and SLHV
  return the **CPU** MDR (previously a single PPU-side latch answered
  for everything, updated even by writes); CGDATAREAD, OPHCT/OPVCT and
  STAT77/STAT78 now perform their partial updates, leaving the
  documented stale bits (CGRAM high-read bit 7, counter high-read bits
  1-7, STAT77 bit 4, STAT78 bit 5). Games that read write-only `$21xx`
  registers and depend on 65c816 open-bus behaviour now see the right
  byte. Save-state format bumped to v4 (PPU field layout changed).

- **GUI: emulation no longer depends on the audio stack** (#130). With
  no output device the ROM used to look loaded but nothing ever stepped
  the core (permanent black screen); the emu thread now always spawns
  (video-as-clock pacing, "running silent" menu notice), a dead cpal
  stream is rebuilt automatically every ~3 s and hot-swapped into the
  running thread (sound drops and comes back on its own when the device
  returns), a panicked emu thread no longer disables audio for every
  later ROM, keys no longer stay pressed after Alt-Tab, and hotkeys no
  longer fire while typing in a modal.

### Added
- **DSP-1 differential oracle** — the scorecard's last "grade capped by
  missing evidence" item is closed. New harness
  (`tests/dsp1_port_differential.rs` + `tools/mesen-dsp1-port-trace.lua`)
  compares the DSP-1's complete observable behaviour — the DR-port
  command/result byte stream — against a Mesen2 reference capture:
  **byte-identical over 380 783 events** across Super Mario Kart's
  title + demo race (60 s, no input). DSP-1 grade: B+ → A−.
- **Supply-chain & repo hardening** (#127-#129): cargo-deny (advisories
  / licenses / sources) in CI + weekly, Dependabot, SECURITY.md, issue
  and PR templates, a CI badge, and the Tom Harte suites now gate every
  PR that touches a CPU core. The declared MSRV is now the tested
  toolchain (1.95 — the old 1.85 claim never compiled), stale founding
  docs are bannered as historical, and closed investigations are
  archived.

### Changed
- **Save states**: format v4 (the PPU layout changed with the two-chip
  MDR model and the line-origin work). Older `.luna` states are
  rejected with a clear error — re-create them from the current build.

## [1.10.1] — 2026-07-18

Hotfix for a GUI display regression: the game frame sat under the menu bar,
leaving a black letterbox bar at the bottom of the window on every ROM.

### Fixed
- **GUI: the game no longer hides under the menu bar / bottom letterbox** (#123).
  The frame was centered in the *whole* window by the `pixels` scaling
  renderer, which knows nothing about the 28-px egui menu bar — so the top
  of the picture sat under the menu and a matching black bar appeared at the
  bottom (visible on every ROM next to Mesen2). The game is now drawn by
  egui itself: the emulator texture is registered with the egui renderer and
  painted aspect-correct in the area *below* the menu bar, so letterboxing
  is symmetric and resize-safe. Two follow-on fixes landed with it: the
  frame texture is created as `Rgba8Unorm` (non-sRGB) because egui-wgpu's
  `register_native_texture` contract expects gamma-space samples — the
  sRGB default double-converted and darkened every color — and the
  mouse→SNES-pixel mapping (Mouse / Super Scope) now derives from the
  actual on-screen game rect instead of the old `pixels` surface transform.

## [1.10.0] — 2026-07-15

The accuracy release. The CPU↔scanline phase work (#107 → #109) lands end to
end: on krom's `CPUBRA`, luna and Mesen2 now execute a **cycle-identical
instruction stream over 841 386 instructions** — and the `$4210` deviation
that opened the chain is retired for the faithful hardware rule. Around it:
`--force-region`, native 512×448 output for the OpenSNES interlace port, and
a golden-test net re-anchored on frames so it survives accuracy work instead
of penalising it.

### Fixed
- **CPU↔scanline phase locked — ares' two remaining timing terms** (#109,
  closing the chain opened by #107):
  - **read sample point**: the bus is now sampled four master clocks before
    the access ends (`step(cost−4); read; step(4)`, ares `cpu/memory.cpp`,
    Mesen2 `SnesMemoryManager::Read`) instead of at the end — every H-clock-
    dependent register read (`$4210`, `$4212`, `$2137`) observes the bus where
    hardware does, and traces/watchpoints timestamp at that point;
  - **deferred `dmaEdge`**: a `$420B` write only *arms* the transfer (ares
    `dmaPending`); the burst executes at the next bus access and is charged to
    the next instruction, where Mesen2's per-instruction trace places it. The
    DMA/HDMA realignment step now also uses the true in-flight access cost.
  Result: on `CPUBRA`, luna and Mesen2 are **cycle-identical over 841 386
  instructions — not a single per-instruction delta differs**. The phase is
  locked, so the faithful RDNMI rule (raise at H=2, readable-set hold in
  [2,6)) replaces #107's conservative masking: `WaveHDMA` polls exactly once
  on 139/139 frames while keeping the Terranigma / Chrono Trigger protection,
  and its HDMA table advances +3/frame on 446/449. Save-state version bumped
  (a pending-DMA field is serialized). Nine goldens re-baselined (eyeballed;
  the two moved PCM goldens have identical loudness/attack stats — LSB-level
  differences only).
- **CPU↔scanline phase: five faithful timing fixes** (#109), each read out of
  ares and verified by a per-instruction master-clock differential against
  Mesen2 — on krom's `CPUBRA` the two emulators now execute an **identical PC
  stream over 841 386 instructions with a total drift of −8 master clocks**:
  - the CPU's **reset sequence** (132 clocks + vector fetch, ares' `//H=186`)
    was never charged, so luna's entire CPU-vs-scanline phase ran 186 clocks
    early;
  - the once-per-scanline **DRAM refresh was not charged during DMA** — a
    64 KiB VRAM clear finished 15 840 clocks early;
  - the **refresh position** was pinned at 538 instead of ares'
    `530 + 8 − dmaCounter()` (531..538, aligned to the DMA clock), and fired
    one chunk late;
  - `STZ $420B` (no channels) paid an 8-clock DMA overhead hardware doesn't,
    and a real burst was missing ares' alignment + per-channel steps;
  - the **HDMA per-line cost** (scorecard/audit item #11) is now ares' model —
    8 clocks per A-bus read including the every-line table read, replacing the
    flat `18 + 8×bytes` — closing ~1 000 clocks of phase error per frame on
    HDMA-heavy scenes.
  The NTSC **short scanline** (line 240 of odd fields = 1360 clocks) is now
  representable: (H, V) derive from incremental counters instead of dividing
  the master clock. Audio validated by ear in the GUI; 18 goldens
  re-baselined (one-frame shifts, each eyeballed); the full HDMA commercial
  corpus swept clean.
- **Golden harness: the `CPUTest` family now runs as NTSC.** The old PAL
  forcing was calibrated against luna's too-fast boot: with cycle-exact
  timing (and per Mesen2, pixel-for-pixel), the ROMs' single-burst result
  table genuinely overruns PAL's VBlank and truncates mid-row. In NTSC both
  emulators render the full all-PASS table — and all 23 goldens pass with
  their existing hashes, now timing-invariant.

### Added
- **CLI: `--force-region <ntsc|pal>`** on every ROM-loading subcommand (`run`,
  `state`, `frames`, `wram-trace`, the dump commands), backed by
  `Emulator::set_forced_region` in `luna-api`. Overrides the cartridge header's
  country byte, changing the scanline count (262/312) and frame rate. Exists
  because the golden harness runs the homebrew corpus as PAL to match krom's
  reference captures, and a timing investigation (#109) must be able to
  reproduce that exact configuration — with `--mem-trace`, `--cpu-trace` and
  the Mesen2 differential attached — from the command line.

### Fixed
- **CPU: `$4210` (RDNMI) was readable too early, so `WaitNMI` poll loops ran
  the game twice per frame** (#107). The classic no-NMI idiom — `WaitNMI: BIT
  $4210 / BPL WaitNMI` — passed **twice** in one VBlank whenever its read
  landed in the first clocks of the VBlank scanline: luna handed the flag back
  *set* there but, being inside the window where the S-CPU forbids clearing it,
  could not clear it, so the very next poll passed again. Every demo in the
  PeterLemon corpus idles on that macro, so they all animated ~4/3x too fast
  (krom's `WaveHDMA` advanced its HDMA table +4 bytes/frame instead of +3),
  which made luna useless as a behavioural reference for porting them.
  luna now masks the flag below H-clock 6 of the VBlank scanline: a `$4210`
  read there sees it **clear** and still cannot clear it, which keeps the
  protection that an NMI handler acknowledging `$4210` must not starve a
  mainline poll of the same flag (Terranigma, Chrono Trigger). Verified
  against a Mesen2 headless trace of `WaveHDMA`: exactly one pass per frame,
  HDMA table pointer +3/frame. Note this masking is a **deliberate deviation**
  from ares and Mesen2, which both hand the flag back *set* in that window: the
  outcome actually hinges on where the poll loop lands relative to the
  scanline, and luna's lands inside the window where hardware's does not. The
  faithful rule needs cycle-exact CPU-vs-scanline timing first (#109); the
  masking is conservative — it can only make a poll retry, never double-fire.
  Three PeterLemon goldens were re-baselined (same clean picture, correct
  animation phase); no commercial-title golden moved.

## [1.9.0] — 2026-07-12

Debugger + toolchain polish from a joint review with Cooper (the OpenSNES
IDE): a leaner MCP debug surface (raw OAM, capabilities, interruptible run +
pause, mirror-folded watchpoints), a friendlier GUI (force-mapper, in-place
reload), and CLI flag parity for the PeterLemon reference-port workflow.

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
- **MCP `peek_oam`** (#89): read all 544 OAM bytes (512-byte low table +
  32-byte high table) directly, instead of pulling the whole `state()`
  snapshot for a sprite/OAM viewer. New `Emulator::peek_oam()` API + MCP tool,
  mirroring `peek_cgram`.
- **MCP `capabilities`** (#90): reports the luna release `version` and the
  live tool catalogue, so a client can feature-detect. (The handshake's
  `serverInfo.version` reports the rmcp library version, not luna's — this
  tool gives the real one.)
- **Interruptible run + `pause`** (#92): the MCP surface gains a `run` tool
  (no mandatory step budget — runs until a breakpoint, a `STOP`, or a
  `pause`) and a `pause` tool that stops an in-flight run. `pause` raises a
  shared flag *without* taking the emulator lock, so it lands while `run`
  holds it (rmcp dispatches each request on its own task); the run then
  returns with `interrupted: true`. Backed by
  `Emulator::run_until_break_interruptible(max_steps, &AtomicBool)` and a new
  `RunOutcome::interrupted`.
- **luna-gui: reload the ROM in place** (#93). *File ▸ Reload ROM* reboots the
  current ROM from disk, and *File ▸ Auto-reload on file change* watches the
  loaded ROM file and reboots when it changes — the watch-mode loop for an
  external SDK build (rebuild the `.sfc`, the running game restarts, no
  reopen). Off by default; the mtime poll is throttled and debounced one cycle
  so a mid-rebuild write isn't read half-written.
- **CLI flag parity** (#95, #85): `luna run` now accepts `--force-mapper`
  (like `state`/`frames`), so a checksum-invalid reference ROM can reach
  `--print-fbhash`; and `luna state` now accepts `--print-fbhash` and
  `--wdm-out`, so an input-driven (`--input`) test can also emit the
  cross-arch visual baseline and keep the WDM/`SNES_ASSERT` oracle. `run` and
  `state` produce the same `fbhash` for the same displayed frame.

### Fixed
- **Memory watchpoints now fold address mirrors** (#91). A watchpoint on a
  WRAM or MMIO address used to fire only on the exact 24-bit bank, so a game
  touching the same byte through a mirror (`$00:0500` vs `$7E:0500`, or
  `$00:2100` vs FastROM `$80:2100`) slipped past silently. Watches set through
  the API/MCP now match every mirror of the WRAM low 8 KB and the MMIO windows
  by default; bank-exact matching is still available at the registry level.

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
