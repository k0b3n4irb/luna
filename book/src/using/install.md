# Install & first run

## Prebuilt binaries (recommended)

Every [GitHub release](https://github.com/kobenairb/luna/releases/latest) ships
Linux binaries for **x86_64** and **aarch64** — no toolchain needed:

```bash
curl -LO https://github.com/kobenairb/luna/releases/latest/download/luna-v1.2.0-linux-x86_64.tar.gz
tar xzf luna-v1.2.0-linux-x86_64.tar.gz && cd luna-v1.2.0-linux-x86_64

./luna-gui "path/to/game.sfc"   # play in the graphical debugger
./luna --help                   # headless CLI: run · state · mcp …
```

Each tarball contains both binaries (`luna`, `luna-gui`) and a `.sha256`
checksum. For ARM64, swap `x86_64` → `aarch64`.

### Runtime requirements

- **`luna-gui`** needs a desktop Linux session: Vulkan or OpenGL, X11 or
  Wayland, and ALSA — all standard on any modern distro.
- **`luna`** (the headless CLI) needs none of those; it runs anywhere.

macOS and Windows are not yet built or supported — they may compile, but no
guarantees.

## Build from source

You need the Rust toolchain pinned in
[`rust-toolchain.toml`](https://github.com/kobenairb/luna/blob/main/rust-toolchain.toml)
(2024 edition), plus `libasound2-dev` on Linux:

```bash
git clone https://github.com/kobenairb/luna && cd luna
cargo run --release -p luna-gui -- "path/to/game.sfc"
```

## Firmware (DSP-1 games)

A handful of games (Super Mario Kart, Pilotwings) use the **DSP-1**
coprocessor, which needs a user-supplied `dsp1b.rom` dump. Luna looks for it
in three places, in order:

1. next to the ROM,
2. a path you pass with `--dsp1-rom` (CLI) or pick in the GUI prompt,
3. your config directory (`~/.config/luna/`).

The ROM is copyrighted and is not distributed with Luna — dump it from your own
cartridge.
