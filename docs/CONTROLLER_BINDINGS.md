# Controller bindings

luna emulates a **single controller (Player 1)** only. A second controller
(Player 2), the SNES **Mouse**, and the **Super Scope** are **not yet
supported**. (The CLI/MCP `set_joypad(port, mask)` API can inject bitmasks for
either port for scripted/agent input, but the GUI only wires Player 1 from the
keyboard.)

## Player 1 keyboard layout (defaults)

Defaults are the Mesen2 arrow-key preset; the source of truth is
`luna-gui/src/input.rs` (`KeyBindings::default`). Bindings are stored by
physical `KeyCode` (layout-agnostic), so the key *positions* hold on
AZERTY/QWERTZ. Remap them in the GUI's input-config dialog.

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

Hotkey: `F12` saves a screenshot (Mesen2-style), also remappable.

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
