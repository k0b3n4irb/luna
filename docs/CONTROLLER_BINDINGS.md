# Controller bindings

## Player 1 keyboard layout

Wired in `luna-gui/src/app.rs`:

| Keyboard         | SNES button | JOY1 bit |
|------------------|-------------|---------:|
| `Z`              | B           | 15       |
| `A`              | Y           | 14       |
| Right `Shift`    | Select      | 13       |
| `Enter`          | Start       | 12       |
| `↑` `↓` `←` `→`  | D-pad       | 11..8    |
| `X`              | A           | 7        |
| `S`              | X           | 6        |
| `Q`              | L           | 5        |
| `W`              | R           | 4        |

## Auto-read + manual-mode behaviour

The SNES auto-read latch fires once per VBlank (line 225 NTSC, 240 PAL)
when `NMITIMEN.0` is set; the same pulse also re-arms the manual-mode
`$4016`/`$4017` shift register (per ares' `controllerPort.latch()`).

Real hardware physically locks out conflicting D-pad directions
(Up + Down, Left + Right) — luna drops both opposing bits when the
auto-read latches.

## Remap dialog

The GUI exposes a key-remap dialog (mirror of Mesen2's default keymap).
See `luna-gui/src/remap.rs` for the binding-storage shape and the
serialisation format.
