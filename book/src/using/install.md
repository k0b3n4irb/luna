# Install & first run

## Prebuilt binaries (recommended)

Every [GitHub release](https://github.com/k0b3n4irb/luna/releases/latest) ships
prebuilt binaries — no toolchain needed:

| Platform | Asset |
|---|---|
| Linux x86_64 | `luna-v1.4.0-linux-x86_64.tar.gz` |
| Linux aarch64 | `luna-v1.4.0-linux-aarch64.tar.gz` |
| Windows x86_64 | `luna-v1.4.0-windows-x86_64.zip` |
| macOS Apple Silicon (arm64) | `luna-v1.4.0-macos-aarch64.tar.gz` |

```bash
# Linux / macOS (swap the asset name for your platform)
curl -LO https://github.com/k0b3n4irb/luna/releases/latest/download/luna-v1.4.0-linux-x86_64.tar.gz
tar xzf luna-v1.4.0-linux-x86_64.tar.gz && cd luna-v1.4.0-linux-x86_64

./luna-gui "path/to/game.sfc"   # play in the graphical debugger
./luna --help                   # headless CLI: run · state · mcp …
```

On Windows, download the `.zip`, extract it (Explorer opens it natively), and
run `luna-gui.exe` or `luna.exe`. Each archive contains both binaries and a
`.sha256` checksum.

### Runtime requirements

- **`luna-gui`** needs a desktop session with a GPU backend:
  - Linux — Vulkan or OpenGL, X11 or Wayland, and ALSA (all standard on any
    modern distro).
  - Windows — Direct3D 12 / Vulkan (any recent GPU driver).
  - macOS — Metal (built in).
- **`luna`** (the headless CLI) needs none of those; it runs anywhere.

> **macOS Gatekeeper:** the binaries are unsigned, so the first launch is
> blocked. Clear the quarantine flag once with
> `xattr -dr com.apple.quarantine luna-v1.4.0-macos-aarch64`, or right-click →
> *Open* in Finder and confirm.
>
> Intel Macs and 32-bit/ARM Windows are not built — build from source below.

## Build from source

You need the Rust toolchain pinned in
[`rust-toolchain.toml`](https://github.com/k0b3n4irb/luna/blob/main/rust-toolchain.toml)
(2024 edition), plus `libasound2-dev` on Linux:

```bash
git clone https://github.com/k0b3n4irb/luna && cd luna
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
