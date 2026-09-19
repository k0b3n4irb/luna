# Install & first run

## Prebuilt binaries (recommended)

Every [GitHub release](https://github.com/k0b3n4irb/luna/releases/latest) ships
prebuilt binaries — no toolchain needed:

| Platform | Asset |
|---|---|
| Linux x86_64 | `luna-linux-x86_64.tar.gz` |
| Linux aarch64 | `luna-linux-aarch64.tar.gz` |
| Windows x86_64 | `luna-windows-x86_64.zip` |
| macOS Apple Silicon (arm64) | `luna-macos-aarch64.tar.gz` |

Each asset also exists under a versioned name
(`luna-v<version>-<os>-<arch>`) if you want to pin a release; the
unversioned names above always resolve to the latest one (from v1.25.0 on).

```bash
# Linux / macOS (swap the asset name for your platform)
curl -LO https://github.com/k0b3n4irb/luna/releases/latest/download/luna-linux-x86_64.tar.gz
tar xzf luna-linux-x86_64.tar.gz && cd luna-linux-x86_64

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
> `xattr -dr com.apple.quarantine luna-macos-aarch64`, or right-click →
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
