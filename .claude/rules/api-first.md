# API-First — every consumer drives the emulator through `luna-api` (auto-loaded)

IMPORTANT: `luna-api` (`crates/luna-api/`, the `Emulator` type) is the
**single contract** for both *driving* and *observing* the emulator. The
CLI, the MCP server, **and the GUI** are all thin consumers of that one
API. No consumer reaches into `luna-core::Snes` (or `luna-ppu`,
`luna-bus`, `luna-apu`, the CPU cores) directly.

This is a **defining pillar of luna** — the thing that sets it apart from
ares / Mesen2 / bsnes. Because every front-end shares one observation +
control surface, **what the CLI/MCP measure is exactly what the GUI
shows** — coherence by construction. A bug seen via `luna state` is the
same bug the user sees on screen; "test it in the GUI" is always
meaningful. Break this and the front-ends silently diverge (e.g. a
forced-blank frame the CLI renders black but the GUI skips), which makes
debugging untrustworthy.

## The rule

- **The GUI (`crates/luna-gui/`) depends on `luna-api`, not `luna-core`.**
  Driving the emulator — `step`, `set_joypad`, `reset`, `load_rom`,
  draining audio, rendering a frame, and **all** introspection — goes
  through `luna_api::Emulator`. The GUI never imports `luna_core::Snes`
  or pokes core fields (`snes.ppu.*`, `snes.cpu.*`, `snes.apu_real.*`).

- **Only the host I/O *libraries* are GUI-specific:** `cpal` (the audio
  output device + its ring buffer) and `eframe`/`egui` (the window,
  texture upload, input-event capture). Everything between the ROM and
  those libraries — emulation, timing, rendering-to-pixels, audio-sample
  production, register/memory introspection — is `luna-api`'s job.

- **Need core data the API doesn't expose yet? Add it to `luna-api`
  first, then consume it.** Never satisfy a GUI (or CLI, or MCP) need by
  reaching past the API into a lower crate. Example: the GUI's raw RGBA
  framebuffer is `Emulator::render_frame_rgba`, not
  `snes.ppu.framebuffer()`. A debugger panel reads `Emulator::state()` /
  `peek_*`, not `snes.cpu`.

- **Policy lives once, in `luna-api`.** The forced-blank render policy,
  frame-boundary detection, audio drain semantics, etc. are defined in
  the API so the CLI and GUI cannot disagree. If the GUI wants a display
  nicety (e.g. "hold the last non-blank frame to avoid flashing"), it
  layers that on top of the *same* API render output — it does not
  re-implement the render.

## Why the MCP "isn't used" is fine

`luna mcp` (the MCP server) exposes the *same* `luna_api::Emulator`
surface over stdio (`step` / `step_until_frame` / `get_state` /
`screenshot`). The CLI (`luna state` / `luna run`) is that same API over
argv. So driving the emulator from the CLI **is** driving it through the
API — the MCP adds a transport, not capability. Use whichever transport
is connected; they observe identical state.

## When this rule applies

- Any change to `crates/luna-gui/` that touches emulation, input, audio,
  or the framebuffer.
- Any new front-end feature that needs core state: extend `luna-api`'s
  surface (a method on `Emulator`, or a field on `EmulatorState`) first.
- Reviewing a GUI diff: a fresh `use luna_core::…` (or `luna_ppu` /
  `luna_bus` / `luna_apu`) in `luna-gui` is a red flag — route it through
  `luna-api` instead.

## Reference

`docs/` — the API surface is the dogfood path the CLI/GUI/MCP share. See
`crates/luna-api/src/lib.rs` for the `Emulator` methods and the
`EmulatorState` snapshot. `crates/luna-gui/src/emu_thread.rs` is the
canonical thin emu-loop over the API.
