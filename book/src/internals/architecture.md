# Architecture overview

Luna is twelve crates in one Cargo workspace, in layers: a crate depends
only on crates of the layers below its own.

```text
front-ends    luna-cli   luna-gui
transport     luna-mcp-server
contract      luna-api
system        luna-core
components    luna-ppu   luna-apu   luna-cartridge
              luna-cpu-65c816   luna-cpu-spc700   luna-cpu-upd96050
foundation    luna-bus
```

## The crates

| Crate | Depends on | What it is |
|---|---|---|
| `luna-bus` | nothing | the `Bus` trait, 24-bit addresses, the memory map and one mapper per cartridge board (LoROM, HiROM, ExHiROM, SA-1, Super FX, S-DD1) |
| `luna-cartridge` | `luna-bus` | the ROM header, mapper detection, save RAM |
| `luna-cpu-65c816` | `luna-bus` | the main CPU, also instantiated a second time as the SA-1 |
| `luna-cpu-spc700` | nothing | the audio CPU |
| `luna-cpu-upd96050` | nothing | the NEC DSP that runs the DSP-1 program |
| `luna-ppu` | nothing | the picture: registers, VRAM / CGRAM / OAM, the per-line renderer |
| `luna-apu` | `luna-cpu-spc700` | the S-DSP synthesiser and the bridge between the two CPUs |
| `luna-core` | all of the above | the `Snes` machine: the scheduler, DMA / HDMA, the SA-1 and DSP-1 chips, power-on state |
| `luna-api` | `luna-core` and the components | the `Emulator` type: the one way to drive and observe the machine |
| `luna-mcp-server` | `luna-api` | the MCP transport, over stdio |
| `luna-cli` | `luna-api`, `luna-mcp-server` | the `luna` binary |
| `luna-gui` | `luna-api` | the `luna-gui` binary: window, input, audio device, debugger panels |

The three CPU cores, `luna-ppu` and `luna-bus` know nothing of the
console around them: each can be used, and is tested, alone.

## One contract: `luna-api`

The CLI, the MCP server and the GUI never touch `luna-core`. They call
`luna_api::Emulator`, and policy that two front-ends could disagree on
(what a forced-blank frame renders as, where a frame ends, how audio is
drained) is defined there once. What `luna state` measures is therefore
what the GUI shows and what an agent reads over MCP. The method list is
on [The `luna-api` Rust surface](../using/rust-api.md).

## How time advances

There is one clock, the console's master clock, and the main CPU drives
it. Every bus access and every internal cycle of a 65C816 instruction
charges its master cycles, and at each charge `luna-core` brings the
rest of the machine up to that instant: the PPU's beam position, the
SPC700 and the S-DSP, the coprocessor on the cartridge, the DRAM refresh
pause, the H/V interrupt comparators. DMA and HDMA run inside the same
accounting. Nothing is advanced a frame, or a scanline, at a time.

Every master cycle is also credited to who spent it (the CPU running,
the CPU waiting in `WAI`, a DMA burst, HDMA, the refresh), which is what
`luna profile` and the `stats` block of the state JSON report.

## Threads

The emulation core is single-threaded and deterministic: the same ROM,
inputs and power-on state give the same frames and the same samples
([Determinism guarantees](../method/determinism.md)). Only the GUI adds
threads. One runs the emulator and is paced by the video frame rate; the
audio device pulls samples from a ring buffer on its own callback, and a
resampler absorbs the small difference between the emulated rate and the
device's clock; the window thread repaints from the last finished frame
without waiting for the emulator.

## Targets

The nine crates from `luna-bus` up to `luna-api` build for
`wasm32-unknown-unknown`, and CI checks it on every push. The MCP
server, the CLI and the GUI are native only.

## Where to go next

- One page per subsystem follows this one, each opening with the same
  status block: [the PPU](ppu.md), [the APU](apu.md),
  [the CPUs](cpus.md), [memory, DMA and the bus](memory-dma-bus.md),
  [the coprocessors](coprocessors.md).
- Why the code is a translation of two reference emulators and not a
  design of its own: [Why "faithful port"](../method/faithful-port.md).
- [The founding design paper](founding-design.md) is the May 2026
  vision document. It explains the layer model and the reasons behind
  the API-first choice; its crate list and its API sketches are not the
  current tree.
