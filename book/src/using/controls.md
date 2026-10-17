# Controller bindings

luna emulates **two controllers (Player 1 + Player 2)** from the keyboard,
both fully driven by the emulator (`$4016`/`$4017` manual reads + the
auto-read latch `$4218/$4219` for JOY1 and `$421A/$421B` for JOY2). The SNES
**Mouse** and the **Super Scope** are also supported — pick a device per port
under **Settings → Devices** in the GUI (the host mouse cursor and buttons
drive them), or from the CLI with `--port1`/`--port2` plus scripted
`--mouse`/`--superscope` input. (The CLI/MCP `set_joypad(port, mask)` API
injects pad bitmasks for either port for scripted/agent input; the GUI wires
both pad ports from the keyboard.)

A **Super Multitap** (Settings → Devices → Port 2 → Super Multitap) adds
players 3-5. They come from host gamepads only: the 3rd, 4th and 5th
connected gamepads drive players 3, 4 and 5 (players 1-2 keep their keyboard
bindings and the first two gamepads). From the CLI: `--port2 multitap` with
`--input3` … `--input5`.

## An empty port

`--port1 none` / `--port2 none` (or **Devices → Nothing (unplugged)**) runs
with nothing in the port, which is what you need to exercise a
"`is a controller connected?`" routine.

Be aware of what it can and cannot tell you. Auto-read gives `$0000` for an
empty port — which is also exactly what an idle connected pad gives, so
`$4218`/`$4219` alone **cannot** distinguish them. The difference appears
only past bit 15 of a manual serial read: a standard pad's data line idles
**high** once its 16 button bits and 4 signature bits have been clocked out,
while an empty port keeps reading **0** for ever. Clocking `$4016`/`$4017`
past the 16th bit and looking for a `1` is therefore the detection, and it
is the one luna models.

Both reference emulators agree on the empty-port value (ares returns 0 from
`ControllerPort::data()` when no device is allocated; Mesen2 masks the data
bits out of open bus and has nothing to OR back in). Treat it as a modelled
convention the two references share, not as a measurement of a floating line
on real silicon — if you have a console and a logic analyser, that one is
still worth checking.

Bindings are stored by physical `KeyCode` (layout-agnostic), so the key
*positions* hold on AZERTY/QWERTZ. Remap them per-player in the GUI under
**Settings → Input** (a Player 1 / Player 2 tab). That dialog also has a
**Preset** row that applies a whole layout to the active player in one click
(see *Presets* below); "Reset to defaults" is separate and always restores the
player's factory binding (P1 = Arrows, P2 = numpad).

## Player 1 keyboard layout (defaults)

Player 1 defaults to the arrow-key preset.

| Keyboard         | SNES button | JOY1 bit |
|------------------|-------------|---------:|
| `A`              | B           | 15       |
| `Z`              | Y           | 14       |
| `E`              | Select      | 13       |
| `D`              | Start       | 12       |
| `↑` `↓` `←` `→`  | D-pad       | 11..8    |
| `S`              | A           | 7        |
| `X`              | X           | 6        |
| `Q`              | L           | 5        |
| `W`              | R           | 4        |

### Presets

The Preset row offers one-click layouts (applied to whichever player tab is
showing):

- **Arrows** — the arrow-cluster layout above (the Player-1 factory default).
- **WASD** — d-pad on `WASD`, buttons on the surrounding cluster. Disjoint
  from the Player-2 default so both pads still coexist.

| Keyboard            | SNES button     |
|---------------------|-----------------|
| `W` `S` `A` `D`     | D-pad (U/D/L/R) |
| `F`                 | B               |
| `G`                 | A               |
| `Q`                 | Y               |
| `E`                 | X               |
| `R`                 | L               |
| `T`                 | R               |
| `C`                 | Select          |
| `V`                 | Start           |

## Player 2 keyboard layout (defaults)

There is no standard Player-2 keyboard preset, so this is luna's own default:
the numeric-keypad d-pad plus the right-hand `IJKL`/`UO`/`HN` cluster, chosen to
never collide with Player 1 so both pads work out of the box.

| Keyboard                    | SNES button     | JOY2 bit |
|-----------------------------|-----------------|---------:|
| `K`                         | B               | 15       |
| `J`                         | Y               | 14       |
| `H`                         | Select          | 13       |
| `N`                         | Start           | 12       |
| `Num8` `Num2` `Num4` `Num6` | D-pad (U/D/L/R) | 11..8    |
| `L`                         | A               | 7        |
| `I`                         | X               | 6        |
| `U`                         | L               | 5        |
| `O`                         | R               | 4        |

Hotkey: `F12` saves a screenshot, remappable under
**Settings → Hotkeys**.

## Auto-read + manual-mode behaviour

The SNES auto-read latch fires once per VBlank (line 225 NTSC, 240 PAL)
when `NMITIMEN.0` is set; the same pulse also re-arms the manual-mode
`$4016`/`$4017` shift register (matching the hardware's controller-port latch).

Real hardware physically locks out conflicting D-pad directions
(Up + Down, Left + Right) — luna drops both opposing bits when the
auto-read latches.

## Remap dialog

The GUI exposes a key-remap dialog under **Settings → Input**, where each
binding is stored by physical key position and persisted across sessions
(`input.json` and `hotkeys.json` in the settings folder — see
[Saves & save states](saves.md#where-luna-keeps-its-files)).

## Gamepads

Plug in up to five host gamepads (SDL-style mappings via `gilrs`): the
first connected pad drives Player 1, the second Player 2, merged with
the keyboard (both work at once); the third to fifth drive players 3-5
when a Super Multitap is on port 2. Fixed, Mesen2-like layout:

| Gamepad             | SNES        |
|---------------------|-------------|
| South (A/Cross)     | B           |
| East (B/Circle)     | A           |
| West (X/Square)     | Y           |
| North (Y/Triangle)  | X           |
| L1 / R1             | L / R       |
| Select / Start      | Select / Start |
| D-pad or left stick | D-pad       |

## Display & audio

- **Fullscreen**: `F11` (remappable hotkey) or *Emulation → Fullscreen*
  — borderless, toggle to return.
- **Volume**: *Settings → Audio* — a 0-100 slider plus *Mute*, applied
  live in the audio callback and persisted to
  `~/.config/luna/audio.json` (`%APPDATA%\luna\audio.json` on Windows).
- **Frame rate**: *Settings → Video → Show FPS* draws the emulated
  frame rate in the top-right corner of the picture, refreshed once a
  second (`60.1 fps` for an NTSC game at full speed, `50.0 fps` for a
  PAL one). A lower figure means the machine cannot keep up with the
  console. It is hidden while the emulation is paused, never appears in
  a screenshot, and the choice is kept in `~/.config/luna/video.json`
  (`%APPDATA%\luna\video.json` on Windows).
- **Video size**: *Settings → Video → Video Size → 1x … 10x* (`Alt+1` …
  `Alt+9`, `Alt+0` for 10x, as in Mesen2) sizes the window so the picture
  is that many screen pixels per console line, at the width the aspect
  ratio gives; the entry matching the current picture is ticked. In a
  maximised or fullscreen window the picture is shown at that size,
  centred. When the picture's shape changes (overscan, filter, aspect
  ratio) the window keeps the nearest whole size. *Fullscreen* sits at
  the bottom of the same menu.
- **Aspect ratio**: *Settings → Video → Aspect Ratio*, Mesen2's list.
  *No Stretching* (the default) shows square pixels; *NTSC (8:7 PAR)*
  and *PAL (11:8 PAR)* widen each pixel as a television did; *Auto*
  follows the cart's region; *Standard* and *Widescreen* force a 4:3 or
  16:9 picture; *Custom* uses the ratio typed under it (0.1 to 5). For
  example, to see a game as on an NTSC television at three times its
  size: *Aspect Ratio → NTSC (8:7 PAR)*, then `Alt+3`.
- **Video filter**: *Settings → Video → Video Filter*, Mesen2's list.
  *Prescale 2x … 10x* repeat each pixel (combine with *Use bilinear
  interpolation* for a soft picture with sharp pixel edges); *Scale 2x …
  4x* round off diagonals (the Scale2x / Scale3x algorithm); *LCD Grid*
  draws each pixel as four cells, three of them dimmed; *xBRZ 2x … 6x*
  follow the edges of the drawing and keep them sharp, the strongest
  smoothing of the list; *2xSai*, *Super2xSai* and *SuperEagle* smooth
  edges between blocks of colour by blending, each with its own
  weighting; *NTSC (blargg)* shows the picture as a composite NTSC
  television did, with its colour fringes on fine detail, dot crawl
  between frames and soft horizontal edges (for example, the waterfalls
  and dithered shadows of many games blend into the colours they were
  drawn for). *Use bilinear interpolation* smooths the final stretch
  instead of keeping square pixels. *NTSC (Bisqwit)* (NES-only in Mesen2)
  and HQx are listed greyed out: their only implementations are copyleft
  code, which luna's licence (MPL-2.0) does not take in, so they are being
  rewritten from their algorithms (NTSC (blargg), xBRZ and the 2xSaI
  family already give Mesen2's pictures to the pixel, NTSC at Mesen2's
  default settings). A
  screenshot goes through the filter, not the aspect ratio. Filter,
  aspect ratio and bilinear choice are kept in `video.json` with the
  frame-rate counter.
- The *Step frame* debugger hotkey moved from `F11` to `F6` (fullscreen
  claimed the universal key); saved hotkey configs keep working and can
  be rebound under *Settings → Hotkeys*.
