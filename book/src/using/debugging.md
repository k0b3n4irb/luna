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
