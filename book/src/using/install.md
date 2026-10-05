# Install & first run

## Prebuilt binaries (recommended)

Every [GitHub release](https://github.com/k0b3n4irb/luna/releases/latest) ships
prebuilt binaries — no toolchain needed. One zip per platform, named after
the version (`vX.Y.Z` below is the one on the release page):

| Platform | File | Architecture |
|----------|------|--------------|
| **Linux** | `luna_vX.Y.Z_linux_x86_64.zip` | x86_64 |
| **Linux** | `luna_vX.Y.Z_linux_arm64.zip` | arm64 (aarch64) |
| **macOS** | `luna_vX.Y.Z_darwin_arm64.zip` | arm64 (Apple Silicon) |
| **Windows** | `luna_vX.Y.Z_windows_x86_64.zip` | x86_64 |

Only the five most recent versions keep a release with binaries. To run an
older one, build it from its tag (every tag is kept):

```bash
git checkout v1.24.0 && cargo build --release -p luna-cli
```

```bash
# Linux / macOS (swap the version and the platform suffix)
curl -LO https://github.com/k0b3n4irb/luna/releases/download/vX.Y.Z/luna_vX.Y.Z_linux_x86_64.zip
unzip -q luna_vX.Y.Z_linux_x86_64.zip && cd luna_vX.Y.Z_linux_x86_64

./luna-gui "path/to/game.sfc"   # play in the graphical debugger
./luna --help                   # headless CLI: run · state · mcp …
```

On Windows, download the `.zip`, extract it (Explorer opens it natively), and
run `luna-gui.exe` or `luna.exe`. Each zip contains both binaries plus
`LICENSE` and `README.md`; the release page shows the SHA-256 digest of each
file. Every release that keeps binaries uses these names (the five
highest versions; the older pages were republished on 2026-10-05).

### Runtime requirements

- **`luna-gui`** needs a desktop session with a GPU backend:
  - Linux — Vulkan or OpenGL, X11 or Wayland, and ALSA (all standard on any
    modern distro).
  - Windows — Direct3D 12 / Vulkan (any recent GPU driver).
  - macOS — Metal (built in).
- **`luna`** (the headless CLI) needs none of those; it runs anywhere.

> **macOS Gatekeeper:** the binaries are unsigned, so the first launch is
> blocked. Clear the quarantine flag once with
> `xattr -dr com.apple.quarantine luna_vX.Y.Z_darwin_arm64`, or right-click →
> *Open* in Finder and confirm.
>
> Intel Macs and 32-bit/ARM Windows are not built — build from source below.

## Build from source

You need the Rust toolchain pinned in
[`rust-toolchain.toml`](https://github.com/k0b3n4irb/luna/blob/main/rust-toolchain.toml)
(2024 edition), plus `libasound2-dev` and `libudev-dev` on Linux:

```bash
git clone https://github.com/k0b3n4irb/luna && cd luna
cargo run --release -p luna-gui -- "path/to/game.sfc"
```

## Firmware (DSP-1 games)

A handful of games (Super Mario Kart, Pilotwings) use the **DSP-1**
coprocessor, which needs a user-supplied `dsp1b.rom` dump. Luna looks for it
in three places, first hit wins:

1. embedded in the ROM dump (some `.sfc` files append the 8 KB firmware),
2. next to the ROM (`dsp1b.rom` in the same folder),
3. luna's firmware folder — `~/.config/luna/firmware/dsp1b.rom` on Linux
   (`<config>/luna/firmware` per platform).

If none is found, `--dsp1-rom <path>` (CLI) or the GUI's "locate firmware"
prompt installs the file into that firmware folder once, for every later run.

The ROM is copyrighted and is not distributed with Luna — dump it from your own
cartridge.

## Unsupported coprocessors

Luna emulates **SA-1, Super FX, DSP-1 and S-DD1**. A game that needs any
other chip — DSP-2/3/4, Cx4 (Mega Man X2/X3), OBC1, S-RTC, ST-010/011,
ST-018, SPC7110, Super Game Boy — is refused at load with an error naming
the chip, rather than booting and hanging:

```text
$ luna run "Mega Man X2 (USA).sfc"
error: cartridge requires the Cx4 coprocessor, which luna does not emulate yet …
```

`--force-mapper lorom` (or `hirom`) loads the ROM anyway, without the chip —
useful for poking at its header or code, not for playing it.
