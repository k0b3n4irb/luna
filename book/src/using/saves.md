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
