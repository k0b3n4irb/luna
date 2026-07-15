# Debugging tools

`luna-gui` is a **debugger first, a player second**. Beyond the game window it
opens a set of live inspection panels — every one reads the *same* emulator
state the game runs on, so what a panel shows is exactly what is on screen.

Open any tool from the **Debug** menu. Each appears in its own window, updating
in real time as the game runs.

| Group | Panels |
|---|---|
| **CPU** (65C816) | State (registers/flags), Memory (hex), Disassembly |
| **SPC700** (audio CPU) | State, Memory (ARAM hex), Disassembly |
| **PPU** | Sprites (OAM), Palette (CGRAM), Tilemap, **Event Viewer** |
| **System** | Registers (full snapshot) |

The disassemblers are M/X-aware for the 65C816 (operand widths follow the
current register sizes); the memory viewers show the live CPU bus and ARAM.

## Breakpoints & stepping

Luna's GUI debugger halts the emulation at full speed on breakpoints and
steps it instruction by instruction — the same registry the MCP tools use.

- **Set a breakpoint**: open *Debug → CPU disassembly*, click any row — a
  red dot marks it. Click again to remove. The *Debug → Breakpoints* panel
  lists every breakpoint (with its symbol when a `.sym` table is loaded),
  removes them individually (✕) or all at once, and adds **memory
  watchpoints** over an address range, firing on reads and/or writes.
- **Run to the hit**: resume (`F2`). When a breakpoint fires the emulation
  auto-pauses, an orange **⏸ Break** banner appears in the menu bar with
  the hit's PC (and address/value for a watchpoint), the CPU disassembly
  jumps to the halt PC with the line highlighted, and the Event Viewer
  shows the hit as a *Breakpoint* dot at the exact scanline/H-clock.
- **Step**: `F10` executes one instruction, `F11` runs to the next frame
  boundary (both also in the *Emulation* menu, and both pause first if the
  game is running). The framebuffer and all debug panels follow each step.

Exec breakpoints halt *before* their instruction executes and are
resume-friendly: resuming from a hit moves past it. Watchpoints halt right
*after* the accessing instruction, reporting its exact PC, the address and
the byte. With no breakpoints set, the emulation hot path is unchanged.

Memory watchpoints are **mirror-folded**: a watch on a WRAM or MMIO address
also fires when the game reaches the same byte through an address mirror — so
`$7E:0500` catches an access via `$00:0500`, and `$00:2100` catches a FastROM
access via `$80:2100`. You don't need to know the executing bank.

Driving this from a script (MCP), a run needs no step budget: `run` executes
until a breakpoint, a `STOP`, or a `pause` from another call, then returns
`interrupted: true` — so a debugger client can "continue" and "pause" without
polling.

## GUI conveniences

A few workflow helpers in the **File** and **Emulation** menus:

- **Force mapper** (*File ▸ Force mapper*): load a ROM whose internal checksum
  is blank/invalid (much of the homebrew test corpus) — auto-detection refuses
  to guess LoROM vs HiROM without a valid checksum, so on a failed load an
  inline *"couldn't detect the mapper — load as…?"* prompt loads it in one
  click, and the submenu pre-sets a sticky default for a whole test corpus.
- **Record input** (*Emulation ▸ ● Record input*): capture what you play as a
  replayable `frame:mask` script. A red **⏺ REC** badge shows while recording;
  stopping writes a `.input` file to `~/.local/luna/recordings/` that replays
  with `luna state --input @<file>`.
- **Reload ROM** (*File ▸ Reload ROM*) + **Auto-reload on file change**: reboot
  the current ROM from disk in place — turn on auto-reload and an external SDK
  build (a rewritten `.sfc`) restarts the running game with no reopen.

## The Event Viewer

The Event Viewer answers one question the other panels can't: **where in the
frame does each hardware access happen?** It plots every register access as a
coloured dot over the running picture, at the exact `(scanline, cycle)` where
it occurred — so a raster split, a gradient, or a mid-frame DMA burst is
visible *as a shape on the frame*, not just a number.

Open it from **Debug → Event Viewer**. It has three regions:

- **Overlay** (centre) — the live framebuffer with event dots drawn on top.
  Each category has its own colour. The horizontal position is the access's
  master-clock within the scanline, at full precision, so events line up with
  the pixels they affect.
- **Filter panel** (right) — checkboxes to show or hide categories, grouped as
  **PPU register writes** (VRAM, CGRAM, OAM, Mode 7, BG Options, BG Scroll,
  Window, Others), **Other events** (PPU/SPC/CPU/WRAM reads and writes, IRQ,
  NMI, marked breakpoints), and **DMA channels** (0–7, filtered individually).
  *Select all* / *Deselect all* toggle the whole set.
- **List** (bottom, optional) — every captured event decoded into a table:
  scanline, cycle, program counter, type, and register address. Toggle it with
  *Show list view*.

Two more toggles help read busy frames: **Show previous frame events** fills in
the part of the frame the current one hasn't reached yet, and the per-channel
**DMA** filters isolate a single transfer.

### What it captures

The viewer records both **CPU register I/O** and **DMA B-bus writes** — the CPU
is halted during a transfer, so capturing the DMA side is what makes an OAM or
VRAM upload show up at all. **HDMA** is captured the same way, so per-scanline
effects — colour-math gradients, window splits, status-bar raster — appear
spread down the frame exactly where they fire, instead of leaving the middle of
the picture blank.

### Reading it

A few patterns to look for:

- **DMA bursts** cluster in the top/bottom bands (vblank), where games upload
  VRAM and OAM.
- **HDMA effects** form vertical streaks down the visible frame — one dot per
  scanline as the channel re-writes a scroll, window, or colour register.
- A **raster split** shows up as a horizontal line of events at the scanline
  where the screen mode changes.

Because the capture is passive, the Event Viewer never alters timing or
rendering — turning it on does not change what the game does.
