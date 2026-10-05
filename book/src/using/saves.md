# Saves & save states

Luna keeps your progress two ways.

## Battery (cartridge) saves — automatic

A game's in-cartridge save (the kind the original cartridge kept alive with a
battery) is written to a `<rom>.srm` sidecar next to the ROM whenever you close
Luna or switch games, and restored the next time you load that ROM.

It is the standard `.srm` format, so your saves **interchange with other
emulators**.

## Save states — full snapshots, 9 slots

A save state captures the *entire* machine — every register, all of RAM, the
PPU and APU — into one of nine slots.

| Key | Action |
|---|---|
| `F5` | Save to the current slot |
| `F9` | Load the current slot |
| `F2` | Pause |
| `F3` | Reset |
| `F12` | Screenshot |

Pick a slot from **Emulation → Save state / Load state**. Every hotkey is
remappable in **Settings → Hotkeys**.

## Where Luna keeps its files

The GUI writes to two fixed folders, whatever directory it was started from:

| What | Linux, macOS | Windows |
|---|---|---|
| Save-state slots (`<rom-slug>.slot<N>.luna`) | `~/.local/luna/states/` | `%APPDATA%\luna\states\` |
| Screenshots (`<rom>_NNN.png`) | `~/.local/luna/screenshots/` | `%APPDATA%\luna\screenshots\` |
| Input recordings (`<rom>_NNN.input`) | `~/.local/luna/recordings/` | `%APPDATA%\luna\recordings\` |
| Settings: `input.json` (pad bindings), `hotkeys.json`, `audio.json`, `last_rom_dir` | `~/.config/luna/` | `%APPDATA%\luna\` |

On Linux and macOS the settings folder follows `$XDG_CONFIG_HOME` when it is
set (`$XDG_CONFIG_HOME/luna/`); the three other folders do not. For example,
after `F5` on slot 1 of `game.sfc`:

```bash
ls ~/.local/luna/states/        # game.slot1.luna
luna state --load-state ~/.local/luna/states/game.slot1.luna --out - game.sfc
```
