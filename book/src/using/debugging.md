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
| **Debugger** | Mesen2's Debugger window for the 65C816 (below) |

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
- **Step**: `F10` executes one instruction, `F6` runs to the next frame
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

## The Debugger window

*Debug ▸ Debugger* opens Mesen2's Debugger window for the 65C816, laid
out as Mesen2's: the code on the left, **Status** above the
**Labels / Functions / Find Results / Controllers** tabs on the right,
**Watch / Breakpoints / Call Stack** along the bottom, a status bar, and
the settings panel at the right edge (`Ctrl+Q`). While it is open the
code log and the call stack are recorded (a few percent of speed); by
default it breaks when it opens.

**The code** is the listing the MCP `listing` tool returns (see [The code
listing](mcp.md#the-code-listing)): the whole address space, the code that
ran disassembled with the widths it ran with, code decoded ahead of it on
a green tint, what is not code collapsed into *unidentified* / *unmapped*
blocks, labels and comments in their rows, and after each operand the
address it would reach now and the value there (`LDA $10 [counter] =
$1234`). The PC's instruction has a yellow box and an arrow; a breakpoint
a coloured dot (red exec, green write, blue read, grey forbid; hollow when
disabled, `+` with a condition).

- **Mouse**: click a row to select it; click the gutter to toggle an exec
  breakpoint, middle-click for a forbid one; double-click a row to go to
  the address it reaches; the wheel scrolls, the scrollbar on the right
  spans the whole space with the PC (yellow) and the breakpoints marked;
  the mouse's back / forward buttons walk the navigation history.
- **Right-click** a row: toggle breakpoint, add to watch, edit label,
  find all occurrences, move program counter, run to location, go to
  location, navigate backward / forward, copy.
- **Keys**, Mesen2's: `F5` continue, `Shift+F5` break, `Esc` either;
  `F11` step into, `F10` step over, `Shift+F11` step out, `F7` one
  scanline, `F8` one frame, `Alt+B` run to a scanline, `Ctrl+B` break in
  N instructions / scanlines / frames; `Ctrl+G` go to (an address, a
  label, an expression), `Ctrl+,` go to all (labels by name), `Ctrl+F`
  find (`F3` / `Shift+F3`), `Ctrl+Shift+F` find all occurrences; `F9`
  toggle breakpoint, `F2` edit label, `;` edit comment, `Ctrl+F11` run to
  the selected row, `Ctrl+Shift+F10` move the PC there, `Ctrl+-` /
  `Ctrl+Shift+-` back / forward.
- **Status** edits the registers and flags while paused; it shows the
  pending interrupts, the stack, the PPU's position and its VRAM / OAM /
  CGRAM addresses.
- **Watch** takes Mesen2's expressions and formats (`[player_hp]`,
  `{$7E0010}, S2`, `[table, 8]`); a value that changed since the last stop
  is red. **Breakpoints** lists, enables, marks, edits (memory, range,
  read / write / execute, condition) and removes them. **Call Stack**
  reads as Mesen2's, `[nmi]` / `[irq]` for the interrupts.
- **File ▸ Workspace** imports labels (`.mlb`, `.sym`) and exports the
  `.mlb`, imports and exports the watch list, resets the workspace.
- **Controllers** forces buttons pressed on ports 1 and 2.
- **The workspace**: the labels you set, the breakpoints, the watches
  and the window's settings are kept per ROM, as Mesen2 keeps them, in
  `<settings>/debugger/<rom name>.json` (`~/.config/luna/debugger/` on
  Linux, `%APPDATA%\luna\debugger\` on Windows), and come back at the
  next load of a ROM of that name — over its `<rom>.mlb`, so what you set
  last wins. It is written when the window closes, when another ROM loads,
  on quit, and at most once a minute while you work. The MCP
  `workspace_save` / `workspace_load` read and write the same file.

Not there yet: step back (rewind), the CPU-cycle and PPU-cycle steps
(luna runs instructions whole), the code/data logger's menu, the
assembler, the memory viewer link, the SPC700 and coprocessor windows.

## GUI conveniences

A few workflow helpers in the **File** and **Emulation** menus:

- **Force mapper** (*File ▸ Force mapper*): load a ROM whose internal checksum
  is blank/invalid (much of the homebrew test corpus) — auto-detection refuses
  to guess LoROM vs HiROM without a valid checksum, so on a failed load an
  inline *"couldn't detect the mapper — load as…?"* prompt loads it in one
  click, and the submenu pre-sets a sticky default for a whole test corpus.
- **Record input** (*Emulation ▸ ● Record input*): capture what you play as a
  replayable `frame:mask` script. A red **⏺ REC** badge shows while recording;
  stopping writes a `.input` file to `~/.local/luna/recordings/`
  (`%APPDATA%\luna\recordings\` on Windows) that replays
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
