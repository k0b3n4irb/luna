# Controller bindings

luna emulates **two controllers (Player 1 + Player 2)** from the keyboard,
both fully driven by the emulator (`$4016`/`$4017` manual reads + the
auto-read latch `$4218/$4219` for JOY1 and `$421A/$421B` for JOY2). The SNES
**Mouse** and the **Super Scope** are **not yet supported**. (The CLI/MCP
`set_joypad(port, mask)` API injects bitmasks for either port for
scripted/agent input; the GUI wires both ports from the keyboard.)

Bindings are stored by physical `KeyCode` (layout-agnostic), so the key
*positions* hold on AZERTY/QWERTZ. Remap them per-player in the GUI under
**Settings → Input** (a Player 1 / Player 2 tab). The source of truth is
`luna-gui/src/input.rs` (`KeyBindings::default`). That dialog also has a
**Preset** row that applies a whole layout to the active player in one click
(see *Presets* below); "Reset to defaults" is separate and always restores the
player's factory binding (P1 = Arrows, P2 = numpad).

## Player 1 keyboard layout (defaults)

Player 1 defaults to the Mesen2 arrow-key preset.

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

Mesen2 ships no Player-2 keyboard preset (it leaves the second pad unbound),
so this is luna's own default: the numeric-keypad d-pad plus the right-hand
`IJKL`/`UO`/`HN` cluster, chosen to never collide with Player 1 so both pads
work out of the box.

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

Hotkey: `F12` saves a screenshot (Mesen2-style), remappable under
**Settings → Hotkeys**.

## Auto-read + manual-mode behaviour

The SNES auto-read latch fires once per VBlank (line 225 NTSC, 240 PAL)
when `NMITIMEN.0` is set; the same pulse also re-arms the manual-mode
`$4016`/`$4017` shift register (per ares' `controllerPort.latch()`).

Real hardware physically locks out conflicting D-pad directions
(Up + Down, Left + Right) — luna drops both opposing bits when the
auto-read latches.

## Remap dialog

The GUI exposes a key-remap dialog (mirror of Mesen2's default keymap).
See `luna-gui/src/input.rs` for the binding-storage shape and the
serialisation format (`KeyBindings`), and `luna-gui/src/ui.rs` for the dialog.
