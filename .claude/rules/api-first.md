---
paths:
  - "crates/luna-gui/**"
  - "crates/luna-api/**"
  - "crates/luna-cli/**"
  - "crates/luna-mcp-server/**"
---
# API-first — every consumer drives the emulator through `luna-api`

`luna-api` (`crates/luna-api/`, the `Emulator` type) is the single
contract for driving and observing the emulator. The CLI, the MCP server
and the GUI are thin consumers; none reaches into `luna-core::Snes`,
`luna-ppu`, `luna-bus`, `luna-apu` or the CPU cores. This is what sets luna
apart: what the CLI/MCP measure is exactly what the GUI shows, so a bug seen
via `luna state` is the bug on screen.

- **The GUI depends on `luna-api`, not `luna-core`**: step, joypad, reset,
  load, audio drain, frame rendering and all introspection go through
  `luna_api::Emulator` (`render_frame_rgba`, `state()`, `peek_*`). Only the
  host I/O libraries are GUI-specific: `cpal` (audio device, ring buffer)
  and `winit` + `pixels` + `egui-wgpu` (window, texture, input events).
- **Missing core data: add it to `luna-api` first**, then consume it. Never
  satisfy a front-end by reaching past the API.
- **Policy lives once, in `luna-api`** (forced-blank render, frame
  boundaries, audio drain). A GUI nicety layers on the same API output; it
  does not re-implement the render.
- `luna mcp` and the CLI are the same API over stdio and argv: use
  whichever is connected.
- Review red flag: a fresh `use luna_core::…` (or `luna_ppu` / `luna_bus` /
  `luna_apu`) in `luna-gui`. Canonical thin loop:
  `crates/luna-gui/src/emu_thread.rs`.
