# Luna — Architecture

> A SNES emulator in Rust with an introspection API and a built-in MCP
> server, designed so that an AI agent can **play**, **develop** and
> **debug** Super Nintendo games autonomously.

---

## Table of contents

- [1. Vision & goals](#1-vision--goals)
- [2. Non-goals](#2-non-goals)
- [3. Overview](#3-overview)
  - [3.1 Layered architecture](#31-layered-architecture)
  - [3.2 Execution modes](#32-execution-modes)
- [4. Rust workspace organization](#4-rust-workspace-organization)
  - [4.1 Cross-target async strategy](#41-cross-target-async-strategy)
- [5. Layer 1 — Bus & memory](#5-layer-1--bus--memory)
- [6. Layer 2 — Emulation core](#6-layer-2--emulation-core)
  - [6.1 65C816 CPU](#61-65c816-cpu)
  - [6.2 PPU](#62-ppu)
  - [6.3 APU / SPC700](#63-apu--spc700)
  - [6.4 DMA & HDMA](#64-dma--hdma)
  - [6.5 Coprocessors](#65-coprocessors)
  - [6.6 Scheduler & cycle-accurate sync](#66-scheduler--cycle-accurate-sync)
- [7. Layer 3 — Control & introspection API](#7-layer-3--control--introspection-api)
  - [7.1 Control plane](#71-control-plane)
  - [7.2 Debug API](#72-debug-api)
  - [7.3 Semantic API (for the AI)](#73-semantic-api-for-the-ai)
  - [7.4 Events & subscriptions](#74-events--subscriptions)
- [8. Layer 4 — MCP server](#8-layer-4--mcp-server)
  - [8.1 Transport & runtime](#81-transport--runtime)
  - [8.2 Tool catalogue](#82-tool-catalogue)
  - [8.3 Resource catalogue](#83-resource-catalogue)
  - [8.4 Notifications & streaming](#84-notifications--streaming)
  - [8.5 Token economy & MCP costs](#85-token-economy--mcp-costs)
- [9. API-first & ecosystem of use cases](#9-api-first--ecosystem-of-use-cases)
  - [9.1 The API is the product, not MCP](#91-the-api-is-the-product-not-mcp)
  - [9.2 Transport catalogue](#92-transport-catalogue)
  - [9.3 Unlocked product use cases](#93-unlocked-product-use-cases)
  - [9.4 Architectural implications](#94-architectural-implications)
  - [9.5 `luna-api` as a stable public contract](#95-luna-api-as-a-stable-public-contract)
- [10. Threading model](#10-threading-model)
  - [10.1 Native target](#101-native-target-linux--macos--windows)
  - [10.2 WASM target (Luna Studio Web — V2)](#102-wasm-target-luna-studio-web--v2)
  - [10.3 Strict discipline](#103-strict-discipline)
- [11. Determinism & reproducibility](#11-determinism--reproducibility)
- [12. Testing strategy](#12-testing-strategy)
- [13. Build, distribution, license](#13-build-distribution-license)
- [14. Roadmap & phasing](#14-roadmap--phasing)
- [15. Risks & open questions](#15-risks--open-questions)
- [16. Glossary](#16-glossary)

---

## 1. Vision & goals

**Luna** is a SNES emulator in Rust that exposes the console as a
**first-class programmable environment** for AI agents. Where traditional
emulators treat AI as a secondary use case (to be bolted on via OCR over
screenshots), Luna makes the agent ↔ machine dialogue a central design goal.

**Goals**

1. **High hardware fidelity**: cycle-accurate emulation of the 65C816 CPU,
   the PPU, the SPC700 and the main coprocessors (SA-1, Super FX, DSP-1
   as a priority).
2. **Rich introspection API**: expose the full machine state (registers,
   VRAM, OAM, palette, scroll, tilemap, sprites) in a structured form.
3. **Built-in MCP server**: an AI agent (Claude, Cursor, etc.) can drive
   the emulator through a catalogue of standardized JSON-RPC *tools*.
4. **Three assumed usage modes**:
   - 🎮 **Play mode** — the agent plays an existing game.
   - 🛠️ **Dev mode** — the agent develops a homebrew (hot-reload, profiler).
   - 🐛 **Debug mode** — the agent debugs a ROM hack (breakpoints, trace,
     time-travel).
5. **Triple execution mode**: *headless* (for AI in production / CI),
   *standalone* (for a human who plays), *spectator* (the AI plays, the
   human observes with visual feedback and activity overlays).
6. **MCP token economy**: intentional design so that a multi-hour AI
   session fits within a reasonable budget (see §8.5).
7. **Strict determinism** in `replay` mode: the same input + same seed
   produces the same sequence of frames bit for bit.
8. **API-first**: MCP is just one of the transports. Layer 3
   (`luna-api`) is designed from the start as a stable public contract
   that can be exposed via REST, WebSocket, WASM, FFI… to unlock an
   ecosystem of third-party tools (homebrew web IDE, desktop dev studio
   client, VSCode extensions, etc. — see §9).

**Measurable success criteria**

| Metric                                 | Target                             |
|----------------------------------------|------------------------------------|
| SNES compatibility (test suite)        | ≥ 99% of commercial ROMs           |
| Hardware-accuracy tests passed         | ≥ 95%                              |
| Performance (release, modern x86-64)   | 60 fps cycle-accurate at < 30% CPU |
| MCP tool round-trip latency            | < 5 ms (local stdio)               |
| Cold start → ROM loaded                | < 200 ms                           |
| Binary size (release, stripped)        | < 15 MB                            |
| Token budget / hour (balanced profile, active gameplay) | < 10M tokens      |
| Spectator GUI latency (event → render) | < 16 ms (1 frame)                  |

---

## 2. Non-goals

- **Speed at the expense of accuracy**: we systematically favor fidelity
  over raw throughput.
- **Online multiplayer netplay**: out of scope for V1.
- **Emulation of other consoles**: SNES only. (A future factorization is
  possible but it is not a goal.)
- **Immersive, complex GUI**: `luna-gui` is deliberately minimal and
  functional (framebuffer + debug overlays). We do not compete with
  RetroArch on shaders, post-processing, or multimedia frontends.
- **Compatibility with low-level hacks** (overclocking, MSU-1,
  widescreen patches): possible in V2.

---

## 3. Overview

### 3.1 Layered architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                  Layer 4 — MCP server (luna-mcp)                   │
│         JSON-RPC 2.0 over stdio / SSE / Streamable HTTP             │
│                            (tokio async)                            │
├─────────────────────────────────────────────────────────────────────┤
│        Layer 3 — Control & Introspection API (luna-api)            │
│   ┌───────────────┬────────────────┬─────────────┬──────────────┐   │
│   │ Control plane │   Debug API    │ Semantic API│   Events     │   │
│   │ (lifecycle)   │ (breakpoints,  │ (sprites,   │  (vblank,    │   │
│   │               │  registers,    │  tilemap,   │   irq, bp    │   │
│   │               │  trace, mem)   │  scroll…)   │   hits, …)   │   │
│   └───────────────┴────────────────┴─────────────┴──────────────┘   │
├─────────────────────────────────────────────────────────────────────┤
│             Layer 2 — Emulation core (luna-core)                   │
│  ┌────────┬────────┬────────────┬─────┬───────────┬───────────────┐ │
│  │ 65C816 │  PPU   │ SPC700/DSP │ DMA │ Coproc.   │   Scheduler   │ │
│  │        │        │            │     │ (SA-1, FX)│ (coroutines)  │ │
│  └────────┴────────┴────────────┴─────┴───────────┴───────────────┘ │
├─────────────────────────────────────────────────────────────────────┤
│            Layer 1 — Bus & memory map (luna-bus)                   │
│        Mappers (LoROM, HiROM, ExHiROM, SA-1, SDD-1, …)              │
└─────────────────────────────────────────────────────────────────────┘
                            ▲
                            │
                   ┌────────┴────────┐
                   │   luna-cli      │   luna-gui (egui/wgpu)
                   │ (headless run)  │   standalone & spectator
                   └─────────────────┘
```

The layers communicate only through Rust contracts (traits + serializable
types). No direct dependency from a lower layer to a higher one.

### 3.2 Execution modes

Luna is designed to run under **four** combinable modes, which are not
separate binaries but configurations of the same `luna` binary. This stems
from the principle that **the emulation core, the introspection API and the
GUI are fully decoupled**: each "consumer" of the core can be turned on or
off independently.

#### Mode 1 — Headless (AI production, CI)

```bash
$ luna mcp --rom game.sfc
```

- No window, no graphics dependency (on Linux, no need for X or Wayland).
- MCP stdio server on standard output.
- Inputs only via the `emu_send_input` MCP tool.
- Outputs only via MCP tools/resources.
- **Use**: integration into Claude Code/Cursor, cloud deployment, batch AI
  benchmarks, CI test suite.

#### Mode 2 — Standalone (human plays)

```bash
$ luna run game.sfc
```

- Native window, framebuffer at 60 fps, audio.
- Keyboard/gamepad inputs.
- No MCP server started.
- Menu: save states, reset, load ROM, video options (integer filter, 4:3
  ratio, etc.).
- **Use**: classic retrogaming, manual verification of a behavior, human
  debugging.

#### Mode 3 — Spectator (the AI plays, the human observes)

```bash
$ luna mcp --rom game.sfc --spectate
```

- The MCP server is active (the AI is in control).
- **A GUI window is open in parallel**, subscribed to the same event bus
  as the agent.
- The human sees in real time:
  - the framebuffer (what the agent sees)
  - **an "Agent activity" panel**: a timeline of recent tool calls
    (`emu_send_input(B, 30 frames)`, `sem_get_sprites()`, …)
  - **visual overlays**: highlighting of the sprites/memory regions the
    agent queried in the last N seconds
  - event notifications (`BreakpointHit`, `RomLoaded`)
- The human can at any time:
  - **pause** (the agent sees its next request queued)
  - **inspect** the state (registers, memory) side by side with the agent
  - **take over** (toggle "human override") to replay a difficult section
- **Use**: **debugging the agent itself** (why did it choose that input?),
  public demos, educational observation.

#### Mode 4 — Coop (human + AI simultaneously, V2)

```bash
$ luna mcp --rom game.sfc --spectate --coop
```

- Human inputs + MCP inputs merged.
- Use case: human drives P1, AI drives P2 in a coop game (Joe & Mac,
  Sunset Riders…), or the AI suggests and the human validates.
- Out of scope for V1, but the architecture allows it natively (the input
  subsystem already aggregates multiple sources).

#### Decoupling architecture

```
                      Emulation core
                  ┌─────────────────────┐
                  │     Event bus       │ (tokio broadcast)
                  └─────────┬───────────┘
                            │
            ┌───────────────┼───────────────┐
            │               │               │
            ▼               ▼               ▼
       ┌─────────┐    ┌─────────┐    ┌──────────┐
       │   MCP   │    │   GUI   │    │  Replay  │
       │ server  │    │ (egui)  │    │ recorder │
       └─────────┘    └─────────┘    └──────────┘
       (optional)    (optional)     (optional)
```

Each consumer is **opt-in**. The `--spectate` mode simply turns on GUI +
MCP simultaneously. The GUI **never** goes through MCP: it talks to the
core via the internal bus (latency < 1 ms, zero token cost).

---

## 4. Rust workspace organization

Cargo workspace with ~15 crates. Each crate is annotated **cross-target**
(compiles natively and to `wasm32-unknown-unknown`) or **native-only**
(forbidden under WASM). This discipline is verified in CI: `cargo check
--target wasm32-unknown-unknown` on the cross-target crates.

```
luna/
├── Cargo.toml                       # workspace root
├── ARCHITECTURE.md                  # this document
├── README.md                        # project introduction (front door)
├── RESEARCH.md                      # pre-Phase-0 research synthesis
├── docs/emulator_landscape.md       # comparative survey of SNES emulators
│
├── crates/
│   │── # ──────────── EMULATION CORE (cross-target, !Send, no_std-ready) ────
│   ├── luna-bus/                    # ✅ memory map, cartridge mappers
│   ├── luna-cpu-65c816/             # ✅ main CPU, cycle-accurate
│   ├── luna-cpu-spc700/             # ✅ audio CPU
│   ├── luna-ppu/                    # ✅ Picture Processing Unit
│   ├── luna-apu/                    # ✅ SPC700 + audio DSP (orchestrates spc700)
│   ├── luna-dma/                    # ✅ DMA + HDMA
│   ├── luna-coproc/                 # ✅ SA-1, Super FX, DSP-1/2/3/4, etc.
│   ├── luna-cartridge/              # ✅ ROM parsing, header detection, SRAM
│   ├── luna-core/                   # ✅ assembles the components, scheduler
│   │
│   │── # ──────────── CROSS-TARGET ABSTRACTIONS ─────────────────────
│   ├── luna-async/                  # ✅ runtime facade (spawn/sleep/channels)
│   ├── luna-api/                    # ✅ ★ stable public contract (layer 3)
│   │
│   │── # ──────────── TRANSPORTS (mix cross-target / native-only) ────
│   ├── luna-mcp-core/               # ✅ Tool/Resource types, schemas
│   ├── luna-mcp-server/             # ❌ rmcp + tokio mainline (native-only)
│   ├── luna-mcp-client/             # ✅ cross-target WebSocket transport
│   ├── luna-rest/                   # ❌ axum + OpenAPI (V1.1, native-only)
│   ├── luna-ws/                     # ❌ tokio-tungstenite (V1.1, native-only)
│   ├── luna-wasm/                   # ⚠️ WASM-only, JS bindings (V2)
│   ├── luna-ffi/                    # ❌ cdylib C/Python (V2, native-only)
│   ├── luna-libretro/               # ❌ libretro core (V2, native-only)
│   │
│   │── # ──────────── BINARIES & GUI ───────────────────────────────
│   ├── luna-cli/                    # ❌ `luna` binary, dispatches the modes
│   ├── luna-gui/                    # ⚠️ egui/wgpu (native + WASM via eframe)
│   └── luna-overlay/                # ⚠️ spectator overlays (native + WASM)
│
├── tests/
│   ├── roms/                        # homebrew hardware-test ROMs
│   ├── cpu-tests/                   # JSON per-instruction test suite for the 65C816
│   └── golden/                      # reference frames for visual tests
│
└── tools/
    └── disasm/                      # standalone 65C816 disassembler
```

Legend: ✅ cross-target / ⚠️ cross-target with cfg-gated features /
❌ native-only.

**Key dependency choices** (revised after research, see RESEARCH.md)

| Domain              | Crate(s)                                       | Rationale                                                   |
|---------------------|------------------------------------------------|-------------------------------------------------------------|
| Native async runtime| `tokio` (rt-multi-thread, sync, macros)        | De facto standard                                           |
| Web async runtime   | `wasm-bindgen-futures` + `gloo-timers`         | Single-thread, microtask queue                              |
| **Async facade**    | **`luna-async`** (in-house crate)              | **Avoids `#[cfg(target_arch)]` everywhere — mandatory**     |
| Channels (cross)    | `futures::channel::mpsc` / `async-channel`     | `crossbeam-channel` **panics** under WASM (see RESEARCH.md) |
| Serialization       | `serde` + `serde_json`                         | Essential for MCP                                           |
| MCP server          | `rmcp` (official Anthropic) — native only      | No `wasm32-unknown-unknown` support                         |
| Schemas             | `schemars` + `ts-rs` (build-time) + `utoipa`   | JSON Schema / TS / OpenAPI generation                       |
| Rendering (gui)     | `wgpu` + `egui` / `eframe`                     | Cross-platform native + WASM via WebGPU/WebGL               |
| Native audio        | `cpal`                                         | Cross-platform low-latency                                  |
| Web audio           | `cpal` (wasm-bindgen backend, output only)     | Web Audio API bridge; ~50-100ms latency                     |
| 65C816 testing      | per-instruction state-vector suite (JSON)      | Exhaustive opcode-level conformance vectors                 |
| Visual tests        | `image` + `pixelmatch`                         | Golden-frame comparison                                     |
| Tracing             | `tracing` + `tracing-subscriber`               | Structured logs                                             |
| CLI args            | `clap` (derive)                                | Standard                                                    |
| Coroutines          | **none** (`genawaiter` rejected)               | Static-dispatch pattern preferred, see §6.6                 |

**Decision on the internal architecture**: unlike designs that use
cooperative cothreads, and unlike some Rust emulators that attempt
`#[coroutine]` (nightly, broken save-states), Luna uses the
**CPU-driven master-clock catch-up** pattern. See §6.6 for the details.

### 4.1 Cross-target async strategy

The core (`luna-core`, `luna-api`, `luna-mcp-core`) must be able to compile
to `wasm32-unknown-unknown`. However:

- mainline `tokio` only partially supports WASM
  (`tokio::time` *panics* at runtime).
- `crossbeam-channel` does **not** work under WASM (parking primitive
  absent, panic "unreachable").
- `std::thread::spawn` is unavailable under single-thread WASM.

**Adopted solution**: a `luna-async` crate that exposes a minimal API
(`spawn`, `sleep`, `yield_now`, `mpsc`, `oneshot`) with two conditional
implementations:

```rust
// crates/luna-async/src/lib.rs
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    pub use tokio::task::spawn;
    pub use tokio::time::sleep;
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::future::Future;
    pub fn spawn<F: Future<Output = ()> + 'static>(f: F) {
        wasm_bindgen_futures::spawn_local(f);
    }
    pub async fn sleep(d: std::time::Duration) {
        gloo_timers::future::TimeoutFuture::new(d.as_millis() as u32).await;
    }
}

pub use imp::{spawn, sleep};

// Cross-target channels — `futures::channel` works everywhere
pub use futures::channel::{mpsc, oneshot};
```

**`!Send` discipline throughout the core**: single-thread, WASM-compatible.
Native parallelism goes through explicit workers (dedicated threads) in the
native `luna-mcp-server`, never in `luna-core`.

**Consequence for the 60 Hz loop**: see §10 (dedicated thread on native,
`requestAnimationFrame` under WASM, abstracted by a `Frontend` trait).

---

## 5. Layer 1 — Bus & memory

The **bus** is the central object that routes reads/writes to the right
components according to the 65C816's 24-bit address (`$bb:aaaa`).

### `Bus` trait (the view exposed to the CPU)

```rust
pub type MCycles = u64;

pub trait Bus {
    /// Reads a byte. TRIGGERS `io_cycle()` internally with the access
    /// cost (SLOW=8 / FAST=6 / XSLOW=12 mclk depending on the region).
    fn read(&mut self, addr: u32) -> u8;

    /// Writes a byte. May have side effects (MMIO registers).
    /// Also triggers `io_cycle()` with the access cost.
    fn write(&mut self, addr: u32, value: u8);

    /// **KEY PRIMITIVE FOR MID-INSTRUCTION ACCURACY.**
    /// Called by the CPU on every bus access (read, write, or internal
    /// cycle without an access). The bus uses it to immediately catch
    /// the PPU up, process HDMA, and test NMI/IRQ.
    ///
    /// This is what makes Mario Kart, F-Zero, and all games with
    /// HDMA/mid-frame effects correct.
    fn io_cycle(&mut self, mcycles: MCycles);

    /// Probes the interrupt lines after accumulation via io_cycle.
    fn nmi_pending(&self) -> bool;
    fn irq_pending(&self) -> bool;
}

/// Trait for components stored behind the bus (PPU, DMA, etc.)
pub trait BusDevice {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
    fn snapshot(&self) -> Vec<u8>;
    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError>;
}
```

**Why `io_cycle()`?** It is the primitive that separates a "moderately
accurate" SNES emulator from a truly cycle-accurate one. Without it, the
PPU is only caught up between CPU instructions — which misses HDMA
effects, the exact timing of H/V IRQs, and the Mario Kart bugs. With it,
every CPU memory access triggers a PPU catch-up to the exact cycle, which
guarantees accuracy while staying zero-alloc in the hot loop.

### Cartridge mappers

A `Mapper` trait that implements `BusDevice` and exposes the topology
specific to the cartridge type:

- `LoRom` (mode 20)
- `HiRom` (mode 21)
- `ExHiRom` (mode 25)
- `Sa1Mapper`
- `SuperFxMapper`
- `SDD1Mapper`
- `SPC7110Mapper`

Detection is done via `luna-cartridge::detect_mapper(&rom_bytes)` which
parses the SNES internal header (offset 0x7FC0 or 0xFFC0).

### Memory map summary

```
$00–$3F:$0000–$1FFF  → WRAM mirror (LowRAM)
$00–$3F:$2100–$213F  → PPU registers
$00–$3F:$2140–$2143  → APU communication ports
$00–$3F:$4200–$421F  → CPU registers (NMI, IRQ, DMA)
$00–$3F:$4300–$437F  → DMA channels
$00–$3F:$8000–$FFFF  → ROM (via mapper)
$7E–$7F:$0000–$FFFF  → WRAM (128 KB)
$F0–$FF:...          → SRAM (via mapper)
```

---

## 6. Layer 2 — Emulation core

### 6.1 65C816 CPU

Cycle-accurate implementation of the Western Design Center 65C816 (the
16-bit 6502 variant used in the SNES).

**65C816 quirks to handle correctly**

- Independent 8/16-bit modes for A and X/Y (via the M and X flags of the
  status register).
- Separate 64 KB banks for program (PB) and data (DB).
- Emulation mode (E) that makes it behave like a 6502.
- All the exotic addressing modes (direct page, stack relative, long
  indexed…).

**Core of the implementation**

```rust
pub struct Cpu65C816 {
    // Registers
    a: u16, x: u16, y: u16,
    pc: u16, pb: u8, db: u8,
    sp: u16, dp: u16,
    p: StatusFlags,        // N V M X D I Z C + E
    
    // Cycle-accurate stepping state
    pending_cycles: u8,
    current_instr: Option<Instruction>,
    micro_op_index: u8,
}

impl Cpu65C816 {
    /// Advances by one master cycle. May be in the middle of an instruction.
    pub fn tick(&mut self, bus: &mut Bus) -> CycleResult;
}
```

Each instruction is broken down into cycle-dated **micro-ops**, which
allows:
- cycle-precise breakpoints
- an interrupt (IRQ/NMI) handled at the exact hardware timing
- a "step" debugger that can step by instruction or by cycle

### 6.2 PPU

The SNES PPU is *complex*: 8 graphics modes (including the famous Mode 7),
4 tile planes, 128 sprites, OAM, CGRAM palette, masking windows, mosaic,
color math…

**Breakdown into sub-modules**

```
luna-ppu/
├── src/
│   ├── lib.rs           # struct Ppu, tick()
│   ├── modes/           # rendering of modes 0–7
│   │   ├── mode0.rs ... mode7.rs
│   ├── sprites.rs       # OAM, sprite renderer
│   ├── window.rs        # window masking, color math
│   ├── vram.rs          # VRAM 64 KB
│   ├── cgram.rs         # palette 512 bytes
│   └── registers.rs     # $2100-$213F
```

**Rendering**: scanline-based in V1 (simpler and 99% sufficient),
upgradable to dot-based for the demos that change registers in the middle
of a scanline.

**Exposed framebuffer**: `[u8; 256 * 224 * 4]` (RGBA8) accessible
read-only via layer 3.

### 6.3 APU / SPC700

The SNES APU is a near-independent subsystem: an SPC700 CPU (an 8-bit
variant derived from the 6502) with its own 64 KB RAM and an audio DSP. It
communicates with the main CPU via 4 "mailbox" registers.

**Critical architectural implication**: the SPC700 runs at 1.024 MHz while
the 65C816 runs at ~3.58 MHz, and the two must stay synchronized. That is
the scheduler's job (§6.6).

```rust
pub struct Apu {
    spc700: Spc700,
    dsp: AudioDsp,
    ram: [u8; 65536],
    ports: [u8; 4],      // $2140–$2143 on the CPU side
}
```

### 6.4 DMA & HDMA

8 DMA channels (memory ↔ MMIO burst transfers) + their HDMA equivalents
(transfers synchronized to PPU rendering, scanline by scanline).

This is crucial: nearly all SNES visual effects (parallax, dynamic mode 7,
color math over windows) rely on HDMA. Incorrect emulation breaks Final
Fantasy VI, Chrono Trigger, etc.

### 6.5 Coprocessors

| Chip         | Iconic games                  | Priority |
|--------------|-------------------------------|----------|
| SA-1         | Super Mario RPG, Kirby Super Star | V1   |
| Super FX     | Star Fox, Yoshi's Island, Doom | V1     |
| DSP-1        | Super Mario Kart, Pilotwings  | V1       |
| DSP-2/3/4    | Dungeon Master, SD Gundam GX  | V2       |
| Cx4          | Mega Man X2, X3               | V2       |
| SPC7110      | Far East of Eden Zero         | V3       |
| S-DD1        | Star Ocean, Street Fighter Alpha 2 | V2  |
| OBC1, ST010+ | A few niche titles            | V3       |

Each coprocessor is a crate-feature of `luna-coproc`, which allows building
a minimal build when targeting a specific game.

### 6.6 Scheduler & cycle-accurate sync

**The problem**: advance, in the right order, a main CPU (21.477 MHz NTSC),
a PPU (clocked by dots/scanlines), an APU at an independent frequency
(3.072 MHz SPC700), DMAs that steal cycles from the CPU, and potentially a
coprocessor — all while staying deterministic and performant.

**Decision**: we adopt the **CPU-driven master-clock catch-up** pattern,
considered the Rust state of the art in cycle-accurate emulation.

**Rejected patterns and why**

| Pattern | Verdict | Reason |
|---|---|---|
| Event-queue `BinaryHeap` | ❌ | At 21M cycles/s, the heap + `Box<dyn>` overhead eats 50% of the cycle budget |
| Coroutines `#[coroutine]` | ❌ | Nightly only, save-states impossible (non-serializable closures), marginal perf |
| `genawaiter` (stable, but...) | ❌ | LLVM struggles to inline it; broken save-states |
| Naive instruction-step | ❌ | No mid-instruction accuracy, breaks Mario Kart |
| Lazy + `next_event` | ⚠️ | To be used **in addition** to optimize WAI/STP |
| **CPU master-clock catch-up + `io_cycle()`** | ✅ | **Our choice** |
| Pure per-cycle state-machine | ⚠️ | Excellent for the isolated CPU, to be used in `luna-cpu-65c816` |

**Adopted pattern — overview**

```
loop {
  delta_mclk = if memory_refresh_pending { 40 }                  // DRAM refresh
              else if dma.active() { dma.tick(bus) }             // DMA steals the cycles
              else { cpu.step(bus) }                             // 1 CPU instruction
                       ↑ during this step, the CPU calls
                         bus.io_cycle(n) on every access,
                         which catches PPU/HDMA up mid-instruction
  apu.tick(delta_mclk)                                           // rational catch-up
  ppu.catch_up_to(total_mclk + delta_mclk)                       // internal-cycle residue
  total_mclk += delta_mclk
  if ppu.frame_complete() { return }
}
```

**Full Rust sketch**

```rust
// crates/luna-core/src/scheduler.rs

pub type MCycles = u64;
pub const NTSC_MASTER_HZ: u64 = 21_477_272;
pub const APU_MASTER_HZ:  u64 = 24_576_000;

pub struct Snes {
    pub cpu: Cpu65816,
    pub ppu: Ppu,
    pub apu: Apu,             // SPC700 + DSP, independent frequency
    pub dma: DmaUnit,
    pub cart: Cartridge,
    pub wram: Box<[u8; 0x20000]>,
    pub total_mclk: MCycles,
    pub frame_mclk: MCycles,
    pub memory_refresh_pending: bool,
}

impl Snes {
    /// One iteration = either 1 CPU instruction, or 1 DMA cycle, or
    /// 1 DRAM refresh. Zero-alloc in the hot loop.
    #[inline]
    pub fn step(&mut self) -> TickEffect {
        let delta = if self.memory_refresh_pending {
            self.memory_refresh_pending = false;
            MEMORY_REFRESH_CYCLES                                    // ~40 mclk
        } else if self.dma.active() {
            // DMA steals cycles from the CPU. Unit = 8 mclk (1 byte transfer)
            self.dma.tick(&mut self.snes_bus())
        } else {
            // CPU executes ONE instruction. During `step`, bus.io_cycle()
            // immediately catches up PPU + HDMA + IRQ check.
            let mut bus = self.snes_bus();
            self.cpu.step(&mut bus);
            bus.access_master_cycles_total
        };

        // APU at a different frequency: catch-up using RATIONAL u64
        // arithmetic (no float, no drift).
        let apu_eff = self.apu.tick(delta);

        // PPU residue (internal CPU cycles with no bus access → not caught
        // up by io_cycle). Usually 0-2 mclk.
        let ppu_eff = self.ppu.catch_up_to(self.total_mclk + delta);

        self.total_mclk += delta;
        self.frame_mclk += delta;

        TickEffect {
            frame_complete: ppu_eff.frame_complete,
            audio_samples: apu_eff.audio_samples,
        }
    }

    pub fn run_to_frame(&mut self, audio_out: &mut Vec<(f32, f32)>) {
        loop {
            let e = self.step();
            audio_out.extend(e.audio_samples);
            if e.frame_complete { return; }
        }
    }
}

// APU catch-up using rational arithmetic — NO FLOAT
impl Apu {
    pub fn tick(&mut self, main_mcycles: MCycles) -> TickEffect {
        // master CPU = 21.477272 MHz, master APU = 24.576 MHz
        self.numerator += main_mcycles * APU_MASTER_HZ;
        while self.numerator >= NTSC_MASTER_HZ {
            self.numerator -= NTSC_MASTER_HZ;
            self.spc700.step(&mut self.bus);
            self.timer0.tick(); self.timer1.tick(); self.timer2.tick();
            if self.sample_divider.tick() { self.emit_sample(); }
        }
        TickEffect::default()
    }
}
```

**Why this works** (to keep in mind while implementing):

1. **Zero-alloc in the hot loop** — no `Box<dyn>`, no `BinaryHeap`, no
   `Vec::push` per cycle. Static dispatch everywhere.
2. **Free mid-instruction accuracy** — `bus.io_cycle()` catches up the PPU
   on every access, so scanline-precise HDMA, exact H/V IRQs, and the
   Mario Kart bugs are correctly reproduced.
3. **No APU/CPU drift** — rational u64 arithmetic (no float). Verifiable:
   after 1h of emulation, `apu.cycle_count() ≈ apu_freq * elapsed`.
4. **Trivial save states** — all fields are concrete `serde::Serialize`
   structs (impossible with coroutines/closures).
5. **Run-ahead / netplay possible** — `step()` is pure, the entire state
   can be cloned and replayed.
6. **Borrow-checker compatible** — `SnesBus<'a>` pattern created on every
   step that borrows the fields separately (`&mut self.ppu, &mut
   self.wram, …`). No `Rc<RefCell>` in the hot loop.

**Residual risks & mitigations**: see §15.

---

## 7. Layer 3 — Control & introspection API

This is the layer that defines **what you can do with the machine** without
yet talking about MCP. It is exposed by the `luna-api` crate as async Rust
traits, independent of the protocol.

### 7.1 Control plane

```rust
#[async_trait]
pub trait EmulatorControl {
    async fn load_rom(&self, path: &Path) -> Result<RomInfo>;
    async fn load_rom_bytes(&self, bytes: Vec<u8>) -> Result<RomInfo>;
    async fn reset(&self) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn step_instructions(&self, count: u32) -> Result<StepResult>;
    async fn step_cycles(&self, count: u64) -> Result<StepResult>;
    async fn step_frames(&self, count: u32) -> Result<StepResult>;

    async fn save_state(&self) -> Result<SaveStateId>;
    async fn load_state(&self, id: SaveStateId) -> Result<()>;
    async fn list_states(&self) -> Result<Vec<SaveStateInfo>>;

    async fn screenshot(&self) -> Result<Screenshot>;  // PNG bytes
    async fn send_input(&self, port: u8, buttons: Buttons, frames: u32) -> Result<()>;
}
```

### 7.2 Debug API

```rust
#[async_trait]
pub trait EmulatorDebug {
    // Registers
    async fn cpu_registers(&self) -> CpuRegisters;
    async fn apu_registers(&self) -> ApuRegisters;
    async fn ppu_registers(&self) -> PpuRegisters;

    // Memory
    async fn read_memory(&self, space: MemSpace, addr: u32, len: u32) -> Vec<u8>;
    async fn write_memory(&self, space: MemSpace, addr: u32, data: Vec<u8>) -> Result<()>;

    // Breakpoints
    async fn add_breakpoint(&self, bp: Breakpoint) -> Result<BpId>;
    async fn remove_breakpoint(&self, id: BpId) -> Result<()>;
    async fn list_breakpoints(&self) -> Vec<BreakpointInfo>;

    // Disassembly
    async fn disassemble(&self, addr: u24, count: u32) -> Vec<DisasmLine>;

    // Trace
    async fn start_trace(&self, filter: TraceFilter) -> Result<TraceId>;
    async fn stop_trace(&self, id: TraceId) -> Result<TraceLog>;
}

pub enum MemSpace { Wram, Vram, Oam, Cgram, Sram, ApuRam, Rom }

pub enum Breakpoint {
    Exec   { addr: u24, condition: Option<Expr> },
    Read   { addr: u24, len: u32 },
    Write  { addr: u24, len: u32, value_match: Option<u8> },
    Vblank,
    Hblank { scanline: u16 },
    DmaStart { channel: u8 },
}
```

### 7.3 Semantic API (for the AI)

**This is where Luna differentiates itself from every other emulator.** We
expose the *semantics* of the current frame, not just its pixels, so that
an agent can "understand" the scene without a vision pipeline.

```rust
#[async_trait]
pub trait EmulatorSemantic {
    /// All OAM sprites with their decoded state.
    async fn sprites(&self) -> Vec<Sprite>;

    /// The 4 backgrounds, their mode, their scroll registers.
    async fn backgrounds(&self) -> [Background; 4];

    /// The currently visible tilemap region for a given BG.
    async fn visible_tilemap(&self, bg: u8) -> Tilemap;

    /// CGRAM palette decoded into RGB colors.
    async fn palette(&self) -> [Color; 256];

    /// Active graphics mode ($2105).
    async fn graphics_mode(&self) -> GraphicsMode;

    /// Window and color-math state.
    async fn window_state(&self) -> WindowState;
}

pub struct Sprite {
    pub index: u8,
    pub x: i16, pub y: i16,
    pub size: SpriteSize,
    pub tile_index: u16,
    pub palette: u8,
    pub priority: u8,
    pub flip_h: bool, pub flip_v: bool,
    pub on_screen: bool,
}
```

**Bonus**: an optional system of **per-game annotations** (`game_maps/`),
which maps known RAM addresses to semantic names:

```toml
# game_maps/super_mario_world.toml
[memory.ram]
"player_x"     = { addr = 0x7E0094, type = "u16le" }
"player_y"     = { addr = 0x7E0096, type = "u16le" }
"player_state" = { addr = 0x7E0071, type = "u8" }
"score"        = { addr = 0x7E0F34, type = "u24le" }
"coins"        = { addr = 0x7E0DBF, type = "u8" }
"lives"        = { addr = 0x7E0DBE, type = "u8" }
```

The agent can then call `read_named("player_x")` instead of memorizing hex
addresses.

### 7.4 Events & subscriptions

Many use cases require the agent to react to an event rather than poll. We
expose an async event channel:

```rust
#[async_trait]
pub trait EmulatorEvents {
    async fn subscribe(&self, filter: EventFilter) -> EventStream;
}

pub enum EmulatorEvent {
    FrameComplete { frame_number: u64 },
    VBlankStart,
    BreakpointHit { id: BpId, pc: u24 },
    MemoryWatchTriggered { addr: u24, old: u8, new: u8 },
    DmaTransferComplete { channel: u8 },
    Crash { reason: CrashReason },
    RomLoaded { info: RomInfo },
}
```

On the MCP side, these events are published as JSON-RPC **notifications**
(unsolicited server→client messages).

---

## 8. Layer 4 — MCP server

### 8.1 Transport & runtime

**Supported transports** (in this priority order):

1. **stdio** — for local integration with Claude Code, Cursor, etc.
2. **Streamable HTTP** — for web / cloud integration.
3. **SSE** — historical fallback.

The `luna` binary launches the MCP server in stdio mode by default:

```bash
$ luna mcp                            # stdio mode (default)
$ luna mcp --http --port 7878         # HTTP mode
$ luna mcp --rom path/to/game.sfc     # load the ROM at startup
```

The multi-thread tokio runtime handles concurrency: a dedicated thread for
the emulation core (clocked at 60 fps), N threads for the MCP handlers that
talk to the core via crossbeam channels.

### 8.2 Tool catalogue

Each MCP tool is a thin JSON ↔ `luna-api` call mapping layer. JSON schemas
generated from the Rust structs via `schemars`.

**"Control" tools**

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `emu_load_rom`          | Loads a ROM from a path                      |
| `emu_reset`             | Resets the console                           |
| `emu_pause` / `emu_resume` | Pause/resume                              |
| `emu_step`              | Advances by N instructions / cycles / frames |
| `emu_send_input`        | Sends a button sequence                      |
| `emu_screenshot`        | PNG of the current framebuffer               |
| `emu_save_state`        | Creates a save state, returns an ID          |
| `emu_load_state`        | Restores a save state                        |

**"Debug" tools**

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `dbg_read_memory`       | Reads N bytes in a memory space              |
| `dbg_write_memory`      | Writes N bytes                               |
| `dbg_get_registers`     | All CPU/PPU/APU registers                    |
| `dbg_add_breakpoint`    | Sets a typed breakpoint                      |
| `dbg_remove_breakpoint` | Removes a breakpoint                         |
| `dbg_list_breakpoints`  | Lists the active breakpoints                 |
| `dbg_disassemble`       | Disassembles N instructions at an address    |
| `dbg_trace_start`       | Starts a filtered trace log                  |
| `dbg_trace_stop`        | Stops and returns the trace                  |

**"Semantic" tools** (Luna's differentiating advantage)

| Tool                    | Description                                  |
|-------------------------|----------------------------------------------|
| `sem_get_sprites`       | Structured list of the 128 active sprites    |
| `sem_get_backgrounds`   | The 4 BGs with mode + scroll                 |
| `sem_get_tilemap`       | Visible tilemap for a BG                     |
| `sem_get_palette`       | Decoded CGRAM palette                        |
| `sem_read_named`        | Reads an address via the game's named mapping|
| `sem_load_game_map`     | Loads an annotation file                     |

### 8.3 Resource catalogue

MCP **resources** expose content the agent can "read" (different from
tools, which are actions).

| URI                                     | Content                              |
|-----------------------------------------|--------------------------------------|
| `luna://state/cpu`                      | JSON CPU registers                   |
| `luna://state/ppu`                      | JSON PPU registers                   |
| `luna://state/framebuffer.png`          | Current frame as PNG                 |
| `luna://state/sprites`                  | JSON OAM sprites                     |
| `luna://memory/wram?addr=…&len=…`       | Memory dump                          |
| `luna://disasm?addr=…&count=…`          | Text disassembly                     |
| `luna://docs/65c816-opcodes`            | Built-in 65C816 opcode reference     |
| `luna://docs/ppu-registers`             | PPU register reference               |

These built-in docs let the agent consult the spec without a network,
which dramatically speeds up debug iterations.

### 8.4 Notifications & streaming

The server emits JSON-RPC notifications for subscribed events. The MCP
client receives them as a push:

```json
{
  "jsonrpc": "2.0",
  "method": "luna/event",
  "params": {
    "type": "BreakpointHit",
    "id": "bp_4",
    "pc": "0x808012",
    "cycle": 1234567
  }
}
```

On the agent side, this enables the pattern:

```
1. add_breakpoint(exec, 0x808012)
2. resume()
3. (passive wait for the "BreakpointHit" notification)
4. get_registers() / read_memory() / disassemble()
5. step / continue
```

### 8.5 Token economy & MCP costs

#### 8.5.1 The problem

An agent driving an emulator can saturate a token quota very quickly if the
API is designed naively. A few orders of magnitude to set the scene (basis:
~4 characters per token, base64 encoding adds ~33% of volume):

| Raw SNES data                           | Size    | Tokens (naive) |
|-----------------------------------------|---------|----------------|
| RGBA framebuffer 256×224                | 224 KB  | ~76,000        |
| PNG framebuffer (limited colors)        | 5–20 KB | ~1,700–6,800   |
| Full VRAM dump                          | 64 KB   | ~22,000        |
| Full WRAM dump                          | 128 KB  | ~44,000        |
| Full OAM (128 sprites, raw)             | 544 B   | ~180           |
| 1 second of unfiltered CPU trace        | ~1 MB   | ~340,000       |

For comparison, a typical Claude Sonnet call has a *context window* on the
order of 200k tokens. **A raw screenshot would already consume ~38% of that
budget**; a raw trace log, more than the entire budget. Without discipline,
an agent playing for 5 minutes can consume several million tokens.

#### 8.5.2 Seven design principles

1. **Semantics before pixels**: by default, return decoded structures
   (sprites, scroll, named RAM), not bytes.
2. **Filter server-side**: `visible_only`, `region`, `since_frame`,
   `kind` — it is not the agent's job to throw away what it did not ask
   for.
3. **Hash + diff**: before a large payload, expose a hash of the state;
   the agent only fetches if it changed.
4. **Resources rather than inline**: large blobs (PNG, memory dumps) are
   exposed as **MCP resources** (URI), not inline in the response — the
   agent only pays the cost if it explicitly chooses to read the resource.
5. **Explicit detail levels**: every potentially costly tool exposes a
   `detail: "thumbnail" | "low" | "medium" | "full"` parameter, with `low`
   as the default.
6. **Hard caps**: each tool has an internal `max_bytes` and truncates with
   a structured warning rather than returning 100 KB without notice.
7. **Announced budget**: every response includes an
   `estimated_output_tokens` field (computed server-side) that lets the
   agent and the human track consumption in real time.

#### 8.5.3 Concrete strategies per tool

| Tool                | Naive                  | With the Luna strategy    | Savings   |
|---------------------|------------------------|---------------------------|-----------|
| `emu_screenshot`    | Inline base64 PNG (~5k)| Resource URI (~50 tokens); PNG available via `luna://state/framebuffer.png` if needed | ~99% |
| `sem_get_sprites`   | 128 sprites all fields (~3k) | `{visible_only: true, fields: ["x","y","tile"]}` → ~500 | ~85% |
| `dbg_read_memory`   | 1 KB of bytes (~340)   | Hash if unchanged (~30); bytes if changed | ~90% in steady state |
| `dbg_trace_start`   | Raw (~340k/s)          | Filter `{pc_range, ops}` + `max_lines` limit | ~99% |
| `sem_get_tilemap`   | Full tilemap 32×32×4 (~5k) | Auto-crop to the visible region (~1k) | ~80% |
| `dbg_get_registers` | All registers in detail (~600) | Categories: `cpu`, `ppu_minimal`, `apu` (~150 each) | ~75% |

#### 8.5.4 Mechanisms implemented in the API

**a) Standardized detail levels**

```rust
#[derive(Deserialize)]
pub struct ScreenshotParams {
    /// "thumbnail" (32×28, ~150 tokens),
    /// "low" (128×112, ~1.5k tokens),
    /// "full" (256×224, via resource URI only)
    #[serde(default = "default_low")]
    detail: DetailLevel,
    /// If true, return just a hash if the frame has not changed
    /// since the last call
    #[serde(default)]
    if_changed_since: Option<FrameHash>,
}
```

**b) Hash-then-fetch pattern**

```rust
#[derive(Serialize)]
pub struct MemoryReadResponse {
    pub addr: u32,
    pub len: u32,
    pub hash: u64,             // always returned
    pub data: Option<Vec<u8>>, // None if hash == previous_hash (savings)
    pub estimated_output_tokens: u32,
}
```

The agent can therefore say: "read 1KB at 0x7E0000, but just the hash if
nothing changed". On a polling loop, this cuts the cost by a factor of
10–100x.

**c) Resources for large payloads**

Rather than inlining a PNG in a tool response, Luna exposes:

```
luna://state/framebuffer.png        → full PNG
luna://state/vram.bin               → 64 KB VRAM
luna://state/sprites.json           → detailed JSON of all sprites
luna://state/disasm?addr=…&count=…  → text disassembly
```

The `emu_screenshot` tool returns, by default, **only** the resource URI +
a thumbnail. The agent decides whether to "open" the resource. MCP clients
like Claude Code can even preview without loading into context.

**d) Standardized filters**

All "list" tools support uniform filters:

```jsonc
{
  "visible_only": true,        // on-screen sprites/tiles only
  "region": { "x": 0, "y": 0, "w": 128, "h": 128 },
  "since_frame": 1234,          // delta since a frame
  "fields": ["x", "y", "tile"], // projection (major savings)
  "limit": 50
}
```

**e) Subscriptions rather than polling**

Polling is expensive (1 tool call/frame × 60 frames/s). We encourage the
agent to use MCP notifications for frequent events:

```
✘ Bad (polling):
  while True: screenshot(); analyze(); sleep(...)
  → 60 tools/s × 1k tokens = 60k tokens/s

✓ Good (event-driven):
  subscribe("FrameComplete", every=30)
  → 2 notifications/s × 200 tokens = 400 tokens/s
```

**f) Transparent budget tracking**

Each response contains:

```json
{
  "data": "...",
  "_meta": {
    "estimated_output_tokens": 142,
    "session_tokens_used": 28430,
    "session_tokens_budget": 200000
  }
}
```

The agent (and the spectator GUI) can display consumption in real time.
When the budget is approached, we can either alert or degrade gracefully
(automatically force `detail: thumbnail`).

#### 8.5.5 Configurable cost modes

At MCP server startup, the user picks a profile:

```bash
$ luna mcp --rom game.sfc --cost-profile economy
$ luna mcp --rom game.sfc --cost-profile balanced     # default
$ luna mcp --rom game.sfc --cost-profile generous
```

| Profile     | Screenshot default | Memory default      | Trace default        |
|-------------|--------------------|---------------------|----------------------|
| `economy`   | thumbnail          | hash-only           | refused without filter|
| `balanced`  | low                | hash-then-data      | 1k lines max         |
| `generous`  | medium             | full data           | 10k lines max        |

The `economy` profile is designed so that a multi-hour session fits within
a reasonable budget (typically < 5M tokens / hour of active gameplay).

#### 8.5.6 Session budget estimate

On a typical "agent learning to play Super Mario World" use case with the
`balanced` profile and an event-driven loop:

| Action                            | Frequency       | Tokens/call  | Total/min  |
|-----------------------------------|-----------------|--------------|------------|
| FrameComplete subscription        | 2/s (filtered)  | 200          | 24,000     |
| sem_get_sprites (visible)         | 2/s             | 500          | 60,000     |
| sem_read_named (player_x/y/lives) | 2/s             | 80           | 9,600      |
| emu_send_input                    | ~5/s            | 50           | 15,000     |
| emu_screenshot (low) occasional   | 0.1/s           | 1,500        | 9,000      |
| **Total**                         |                 |              | **~120k/min** |

→ **~7M tokens/hour** of an active agent on this profile. That is
sustainable on an Anthropic "pro" API plan, and far below the ~80M
tokens/hour a naive design based on full PNGs + RAM dumps would consume.

---

## 9. API-first & ecosystem of use cases

The AI agent via MCP is only one possible client among many. Exposing Luna
as a stable API opens up a whole range of tools the SNES community has
never had: a web IDE for homebrew, a desktop development client, CI for ROM
hacks, a TAS platform, a VSCode extension, etc. This section spells out that
openness and its implications.

### 9.1 The API is the product, not MCP

Looking at the layered architecture (§3.1), one notices that layers 1 to 3
**never** depend on layer 4. The MCP server is only an **adapter** that
translates JSON-RPC ↔ `luna-api` Rust calls.

```
   Naive view                         Luna view
   ──────────                         ─────────
   ┌─────────────┐                    ┌─────────────┐
   │  Emulator   │                    │ Stable API  │ ← the public product
   └──────┬──────┘                    ├─────────────┤
          │                           │  Emulator   │ ← the implementation
   ┌──────▼──────┐                    └─────────────┘
   │   MCP API   │ ← the product
   └─────────────┘            ┌─MCP─┬─REST─┬─WS─┬─WASM─┬─FFI─┐
                              └─────┴──────┴────┴──────┴─────┘
                                     ↑ interchangeable adapters
```

This is the **Ports & Adapters** pattern (hexagonal architecture), adapted
to a product where the core (the emulation) must outlive the evolution of
access protocols. Concretely:

- The `luna-api` crate imports **nothing MCP-specific**.
- The public types are serializable with `serde` but format-agnostic
  (JSON, MessagePack, bincode, protobuf possible).
- Every new transport is a `luna-transport-X` crate that depends only on
  `luna-api`, never the other way around.

### 9.2 Transport catalogue

| Transport          | Typical use case                         | Status    |
|--------------------|------------------------------------------|-----------|
| **MCP stdio**      | Local AI agent (Claude Code, Cursor)     | V1        |
| **MCP HTTP/SSE**   | Remote AI agent, multi-client            | V1        |
| **REST / HTTP**    | Web frontends, enterprise integrations   | V1.1      |
| **WebSocket**      | Real-time web (Luna Studio Web)          | V1.1      |
| **gRPC**           | High-perf clients, microservices         | V2        |
| **WASM / JS bindings** | Emulator in the browser              | V2        |
| **FFI / cdylib**   | C / Python / Lua / … integrations        | V2        |
| **libretro core**  | RetroArch integration                    | V2        |

**Principle**: *one source schema, several generated adapters*. From the
`luna-api` types annotated with `schemars::JsonSchema`, we automatically
derive:

- JSON Schema for the MCP tools.
- OpenAPI 3 for REST (via `utoipa`).
- `.proto` files for gRPC.
- TypeScript types for web clients (via `ts-rs`).
- Python bindings (via `pyo3`).

A single source of truth, several surfaces. The risk of desynchronization
between client and server is eliminated at compile time.

**⚠️ Important WASM constraint**: `rmcp` (the official Rust MCP SDK) does
not support `wasm32-unknown-unknown` (it depends on mainline `tokio` with
non-WASM features). Consequence for Luna Studio Web:

- The WASM binary **does not embed an MCP server**.
- The remote AI agent connects to a **native** Luna (which hosts the
  official MCP server), via WebSocket relayed by the web client.
- Target architecture for the web V2:

  ```
  AI agent ──MCP stdio──► native Luna ──WebSocket──► Luna Studio Web (WASM)
                                                          │
                                                          ▼
                                                   Shared view of the same
                                                   emulation state
  ```

- Future alternative: wait for `wasm32-wasip2` + the Component Model
  (maturity mid-2026 per paiml/rust-mcp-sdk).

See RESEARCH.md for the details of the WASM audit.

### 9.3 Unlocked product use cases

Beyond the AI agent, here is the ecosystem of tools the API makes possible.
Listed by impact potential for the SNES community.

#### A — Luna Studio Web (homebrew IDE in the browser)

**High priority** post-V1. An integrated environment in the browser to
develop your own SNES game:

- Code editor (Monaco/CodeMirror) with 65C816 syntax highlighting.
- Built-in assembler (`wla-dx`, `ca65`) compiled to WASM.
- **Luna emulator in WASM** on the same page, local execution.
- **Hot-reload**: `Ctrl+R` re-assembles and relaunches the current ROM.
- Visual debug tools: VRAM viewer, palette editor, sprite editor, tilemap
  painter.
- Git versioning via libgit2 (in-browser) or a server backend.
- Project sharing via URL (sandboxed).

The entire IDE is a SPA that talks to Luna WASM via JS bindings — no
network latency in the dev/test loop. **This is by far the most impactful
use case for the SNES homebrew community**, which today has no equivalent
to Godot/Unity for its needs.

#### B — Luna Studio Desktop (heavy dev-studio client)

For devs who want native performance and system integration:

- Cycle-accurate with no JS/WASM overhead.
- Native filesystem, native Git, pluggable build pipelines.
- Plugin system (Aseprite, Tiled, Pyxel importers…).
- More powerful debugger (multi-window memory inspector, rich conditional
  watchpoints).

Built with `egui` + direct calls to `luna-api` (no JSON transport, just
Rust ↔ Rust). It is the SNES equivalent of "Visual Studio Code + integrated
emulator extension".

#### C — CI integration tests for ROM hacks and homebrew

A `luna-test` crate that lets you write tests for your own game:

```rust
#[luna_test]
fn level_1_can_be_completed() {
    let mut emu = Luna::new().load("game.sfc");
    emu.advance_to_title_screen();
    emu.send_inputs(&["Start", "Start"]);
    emu.run_until_event(Event::LevelComplete, max_frames: 18_000)?;
    assert_eq!(emu.read_named("score"), 12_500);
    assert_eq!(emu.read_named("lives"), 3);
}
```

Homebrew devs have no serious CI today. Luna brings a standard: commit →
GitHub Actions → tests played on a cycle-accurate emulator → result in
≤ 30s.

#### D — Lightweight cloud streaming

WebSocket + compressed framebuffer (PNG diff or simple H.264) → stream a
Luna session from a server to a thin web client. Not a Stadia competitor,
but useful for: "open this ROM in a tab without installing anything"
(demos, sharing, interactive archives).

#### E — VSCode extension

A plugin that detects SNES homebrew projects and:
- Launches Luna as a subprocess (local REST transport).
- Displays the framebuffer in a webview panel.
- Wires the VSCode debugger to the breakpoint/register API.
- Allows edit → assemble → test without leaving the editor.

#### F — Educational platform

16-bit machine architecture courses, Luna as an interactive, real-time
sandbox: students simultaneously see the state of the CPU registers, the
VRAM, the instruction fetch, the pixel-by-pixel effect.

#### G — Modern speedrunning & TAS

Deterministic `replay` mode + frame-stepping + save states + scripting → a
Tool-Assisted Speedrun platform for the SNES, with a modern Rust ecosystem
and a scriptable API.

#### H — Automated tournament refereeing

Multiple Luna instances in parallel (one container per match) +
cryptographically signed replays → SNES tournaments with integrity proofs.
Eliminates client-side cheating.

#### I — Embedded / hardware

Once `luna-core` is stable in `no_std` (V2 goal), a port becomes possible
on a Raspberry Pi-type SBC in "smart retro console" mode: emulator + local
MCP server that answers a voice assistant's requests ("Claude, save my
state before the boss").

### 9.4 Architectural implications

For this ecosystem to stay coherent and maintainable:

1. **Zero application logic in the transports**: the `luna-mcp`,
   `luna-rest`, `luna-wasm` crates do *only* marshalling. All business
   logic stays in `luna-api`.

2. **Single source schema**: all `luna-api` public types are annotated
   `JsonSchema`. The OpenAPI docs, TS types, and `.proto` files are all
   **generated**, not hand-written.

3. **Conditional compilation**: each transport is a toggleable Cargo
   feature. "Minimal headless" build = MCP only. "Luna Studio Desktop"
   build = everything enabled.

4. **Authentication & authorization**: critical as soon as we leave local
   stdio. V1.1 design:
   - API token (`Authorization: Bearer …`).
   - Granular capabilities (`read_state`, `write_memory`, `load_rom`).
   - Rate limiting + per-session quotas.

5. **Multi-tenancy**: V1 = one binary, one emulation. V1.1+ considers a
   **session manager mode** for the cloud (N isolated sessions per server
   instance, each with its own emulation core in a thread).

6. **API versioning**: version pin in requests, clean deprecation (≥ 1
   minor of transition), announced breaking changes.

7. **Observability**: structured tracing (`tracing` crate), optional
   Prometheus metrics for server deployments. Essential in multi-tenancy.

### 9.5 `luna-api` as a stable public contract

Direct consequence: `luna-api` becomes the **flagship crate** of the
ecosystem, the one that must have the strongest stability. We apply a
discipline to it that is stricter than the other crates:

- **Strict SemVer**: no breaking change without a major bump.
- **Deprecation policy**: ≥ 1 minor with `#[deprecated]` before removal.
- **Public API tests**: `cargo-public-api` in CI, detects any undocumented
  change.
- **Exhaustive documentation**: every trait/struct documented, examples in
  `///` doctests run as tests.
- **Strategic re-exports**: `luna::api::prelude::*` gathers the types
  clients need, isolating internal details.
- **Maintained changelog** in the Keep a Changelog format, with an "API
  changes" section separate from the rest.

It is the only crate whose API stability is guaranteed at the "release
product 1.0" level. The others (`luna-cpu`, `luna-ppu`, …) can evolve more
freely between versions as long as `luna-api` stays stable.

---

## 10. Threading model

The model differs depending on the target (native vs WASM). The shared code
goes through the `luna-async` facade (§4.1) to stay cross-target.

### 10.1 Native target (Linux / macOS / Windows)

```
┌────────────────────────────────────────────────────────────────┐
│            "emulation" thread (dedicated, 60 Hz)              │
│  - CPU master-clock catch-up scheduler (§6.6)                 │
│  - bus.io_cycle() catches PPU/HDMA up mid-instruction         │
│  - Checks breakpoints                                         │
│  - Reads Commands between frames                              │
│  - Publishes Events on the bus                                │
└───────┬────────────────────────────────────────────┬───────────┘
        │ futures::channel::mpsc<Command> (input)    │ broadcast<Event>
        ▲                                            ▼
┌───────┴──────────────┐                  ┌──────────────────────┐
│  Tokio runtime       │                  │  Event bus           │
│  (main thread)       │                  │  (tokio broadcast)   │
│  - luna-mcp-server   │◄─── Event ──────►│  fan-out broadcast   │
│  - async handlers    │                  └──────┬───────────────┘
│  - parse JSON-RPC    │                         │
└──────────────────────┘                         │
                                                 ▼
                                ┌────────────────────────────────┐
                                │     "GUI" thread (optional)    │
                                │  - winit/egui/wgpu             │
                                │  - 60 fps framebuffer render   │
                                │  - spectator overlays          │
                                │  - keyboard/gamepad inputs →   │
                                │    Command to the core         │
                                └────────────────────────────────┘
```

### 10.2 WASM target (Luna Studio Web — V2)

```
┌────────────────────────────────────────────────────────────────┐
│              Single-thread tasks (Web Worker or main)         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Emulation clocked by requestAnimationFrame()             │   │
│  │  - CPU master-clock catch-up scheduler (§6.6)            │   │
│  │  - bus.io_cycle() catches PPU/HDMA up mid-instruction    │   │
│  └────────────────┬────────────────────────────────────────┘   │
│                   │ Rc<RefCell<EmuState>>                       │
│                   ▼                                             │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Microtask queue: luna-mcp-client + GUI eframe           │   │
│  │  - WebSocket to a remote native Luna (NO embedded MCP   │   │
│  │    server — rmcp incompatible with WASM)                │   │
│  │  - egui/wgpu via eframe (WebGPU or WebGL2)              │   │
│  └─────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────┘
```

### 10.3 Strict discipline

- The core accesses *no* async or GUI resource directly. All interaction
  with the outside world goes through the channels (`Command` in,
  `Event` broadcast out).
- **No `crossbeam-channel` in the core** — it panics under WASM. Use
  `futures::channel::mpsc` everywhere (cross-target compatible).
- **`!Send` everywhere** in `luna-core` and `luna-mcp-core` — single-thread
  for WASM compat. Native parallelism goes through explicit threads in the
  native `luna-mcp-server` only.
- **No `borrow_mut()` across an `await`** on the WASM side — risk of a
  `RefCell already borrowed` panic.

Structural advantages:

- The core stays testable without tokio, winit, or WebSocket.
- MCP latency does not affect emulation timing.
- The core can be frozen (paused) without disturbing the MCP server or the
  GUI.
- **GUI and MCP are symmetric**: both are consumers of the same bus, which
  makes the `spectate` mode trivial (= enable both).
- The human can take over in spectator mode by sending `Command::Input`
  from the GUI exactly as the MCP agent would — the command's origin is
  traced for the "Agent activity" panel.

---

## 11. Determinism & reproducibility

**Default guarantees**

- Same ROM + same input sequence + same initial RNG seed → exactly the same
  sequence of frames.
- Save states encode the *complete* machine state (RAM, VRAM, OAM, CGRAM,
  APU RAM, registers, scheduler queue, cycle counter).

**Replay**

`.lreplay` file format (TOML + binary):

```toml
[meta]
rom_sha256 = "abc123..."
luna_version = "0.3.1"
created_at = "2026-05-23T11:00:00Z"

[inputs]
# (frame, port, buttons)
1     = [0, "Start"]
60    = [0, "B"]
120   = [0, "B|Right"]
# ...
```

A replay can be played back with:
```bash
$ luna replay session.lreplay --verify
```

The `--verify` flag re-computes the hash of the framebuffers and compares it
to a reference manifest — useful in CI.

**Time travel**: a circular buffer of N save states taken every second
(configurable) lets the agent do `rewind(seconds: 5)`. Cost: ~200 KB × N in
RAM (negligible up to several minutes).

---

## 12. Testing strategy

### Unit tests

- One crate = one test module.
- Each 65C816 opcode tested on known cases (flags, E-mode edge cases, BCD,
  etc.).
- Each PPU register tested on read/write behaviors.

### Integration tests

- **Open-source homebrew hardware-test ROMs** in `tests/roms/`:
  - CPU / PPU / DMA / HDMA / ADC result-screen ROMs
  - APU audio-test ROMs
  - advanced-PPU test ROMs
- Each ROM displays "PASS" or "FAIL" via text/screen. We capture frame N
  and look for the expected pattern.

### Visual tests (golden)

- For each reference game (~20 games), a frame at a precise point (after a
  deterministic input sequence) is stored as a PNG in `tests/golden/`.
- In CI, we replay the sequence and compare pixel-by-pixel (zero tolerance
  in cycle-accurate mode).

### Performance tests

- `cargo bench` (criterion) on the hot paths: CPU step, PPU scanline, APU
  sample generation.
- A perf regression is flagged if > 5% over two consecutive commits.

### MCP tests

- A mock MCP client that replays scripted scenarios and checks the
  responses (schemas + expected values).

---

## 13. Build, distribution, license

**Build**

```bash
# development
cargo build

# optimized release
cargo build --release

# minimal build (no GUI, no niche coprocessors)
cargo build --release --no-default-features --features "core,mcp,sa1,superfx,dsp1"
```

**Distribution**

- **Binaries**: Linux x86-64/aarch64, macOS Intel/ARM, Windows x86-64.
- **Crates.io**: all the `luna-*` crates published independently.
- **GitHub Releases**: tagged + signed checksums.
- **Docker**: image `ghcr.io/<org>/luna:latest` for CI integration.

**License**

Recommendation: **MPL-2.0** (Mozilla Public License 2.0). Rationale:

- More permissive than the GPL (compatible with commercial use).
- File-level copyleft: modifications to Luna's code must be shared, but
  integration into a larger project (e.g. a proprietary dev tool) remains
  possible.
- Compatible with potential adoption by the libretro / Anthropic community.

To discuss: GPL-3.0 (more protective) or Apache-2.0 (more permissive).

---

## 14. Roadmap & phasing

> **Status (2026-06).** Phases 0–4 are **done**: luna boots and plays commercial
> titles across the 65C816 + SPC700/DSP + PPU and all three priority
> coprocessors (SA-1, Super FX, DSP-1), and the introspection API + MCP server +
> standalone GUI ship. Phase 5 (advanced debug / spectator) is **in progress**;
> Phase 6 (1.0 polish) is ahead. The week estimates below are the original plan,
> kept for reference.

### Phase 0 — Pattern validation & skeleton (3 weeks) — ✅ done

**Research & validation** (1 week — a prerequisite to any production code):

- Studying the master-clock catch-up scheduling model:
  - the `Snes::tick` direct model
  - the rational APU catch-up
  - the per-region `access_master_cycles` computation
  - DMA/HDMA timing
  - the `start_cycle`/`end_cycle` cycle-stepping pattern
  - the workspace-organization model
- Validate the per-instruction `65816` test-suite format end-to-end against
  the CPU core.

**Code skeleton** (2 weeks):

- Cargo workspace with the ~15 crates (see §4), all compiling empty.
- GitHub Actions CI: `cargo check` + `cargo check --target
  wasm32-unknown-unknown` (fails if a cross-target crate breaks).
- `luna-async`: runtime facade (spawn/sleep/channels) with native (tokio) +
  web (wasm-bindgen-futures) implementations.
- `luna-bus`: basic memory map + LoROM mapper + `Bus` trait with
  `io_cycle()`.
- `luna-cpu-65c816`: complete instruction decoder (without fine timing
  yet). Jump-table `[fn(&mut Cpu, &mut Bus); 256]`.
- `luna-cli`: loads a ROM, runs 1 frame, dumps the CPU state.
- Tests: first pass of a few per-instruction CPU tests.

### Phase 1 — First render (4 weeks) — ✅ done

- `luna-ppu`: modes 0 and 1, scanline-based, basic sprites.
- `luna-dma`: DMA (without HDMA).
- `luna-core::Snes::step()` complete (see §6.6) — CPU + DMA + PPU catch-up
  via `bus.io_cycle()`.
- 1000+ per-instruction tests pass (target: 100% of the 65C816).
- A CPU result-screen test ROM displays "PASS".

### Phase 2 — Audio + simple games (4 weeks) — ✅ done

- `luna-apu`: SPC700 + basic DSP.
- Working HDMA.
- **Super Mario World** playable end-to-end (without major visual bugs).

### Phase 3 — API, MCP, standalone GUI (4 weeks) — ✅ done

- `luna-api`: Control + Debug + Semantic.
- `luna-mcp`: stdio server with ~15 base tools.
- `luna-gui` v0: **standalone** mode (human plays with keyboard/gamepad).
- Demo: Claude Code loads a ROM, takes a screenshot, reads the RAM.
- Implementation of the token-economy principles from the start:
  resources, detail levels, hash-then-fetch.

### Phase 4 — Priority coprocessors (6 weeks) — ✅ done

- SA-1, Super FX (GSU), DSP-1 (uPD7725 core in `luna-cpu-upd96050`).
- **Star Fox** / **Doom** (Super FX), **Super Mario RPG** / **Kirby** (SA-1),
  **Super Mario Kart** / **Pilotwings** (DSP-1 Mode 7) playable.
- DSP-1 needs a user-supplied `dsp1b.rom` firmware — see
  [`docs/firmware.md`](docs/firmware.md).

### Phase 5 — Advanced debug & spectator mode (5 weeks) — 🚧 in progress

- ✅ GUI debugger panels: CPU/SPC700 state, memory hex,
  65C816/SPC700 disassembly, I/O registers (incl. DSP-1), palette, tilemap,
  sprites.
- ⏳ Conditional breakpoints, trace logging, time travel.
- ⏳ Enriched Semantic API (decoded palette, window state); MCP resources
  (`luna://docs/...`).
- ⏳ `luna-gui` spectator overlays — agent-activity timeline, queried
  sprite/region highlighting, live token-budget panel.

### Phase 6 — Polish & 1.0 (4 weeks) — ⏳ planned

- Golden visual tests over 20 games.
- User documentation.
- Stabilization of `luna-api` (frozen SemVer, `cargo-public-api` in CI).
- Public AI demos:
  1. Claude plays Super Mario World autonomously.
  2. Claude debugs a crash on a ROM hack.
  3. Claude develops a "hello world" homebrew by assembling + testing in
     the loop.

**Estimated total**: ~6 months for V1.0.

### Post-1.0 — Opening up the ecosystem

Optional phases depending on traction & community feedback:

- **Phase 7 — Additional transports** (~4 wk): `luna-rest`, `luna-ws`,
  OpenAPI + TS type generation. Unlocks third-party web frontends.
- **Phase 8 — Luna Studio Web** (~8 wk): `luna-wasm` + homebrew IDE SPA.
  The "killer app" goal for the SNES community.
- **Phase 9 — Luna Studio Desktop** (~6 wk): evolution of `luna-gui` into a
  full IDE with integrated assembler, plugin system.
- **Phase 10 — Bindings & integrations** (~6 wk): Python/C FFI, VSCode
  extension, libretro core.
- **Phase 11 — Cloud & multi-tenancy** (~6 wk): auth, session manager,
  observability, Kubernetes deployment.

---

## 15. Risks & open questions

### Technical risks

| Risk                                            | Mitigation                                                                          |
|-------------------------------------------------|-------------------------------------------------------------------------------------|
| Cycle-accurate performance too slow             | Static-dispatch zero-alloc pattern (§6.6), criterion profiling, SIMD PPU            |
| CPU↔APU sync hard to stabilize                  | Rational u64 arithmetic (no float, §6.6), APU audio-test ROMs                       |
| MCP schemas that change (evolving spec)         | Pin to a stable version, abstract behind `luna-mcp-core`                            |
| Under-documented coprocessors (Super FX)        | Cross-reference the hardware behaviour against multiple sources                     |
| **Token cost explosion in AI usage**            | `economy/balanced/generous` profiles, hash-then-fetch, MCP resources, budget tracker (§8.5) |
| Spectator GUI slowing down the core             | Separate GUI thread, framebuffer shared via `arc-swap` or triple-buffer            |
| **Hostile borrow checker** (CPU + bus + PPU mut simultaneously) | `SnesBus<'a>` pattern created on every step, separate borrows. No `Rc<RefCell>` in the hot loop |
| **`tokio::time` panics under WASM**             | `luna-async` facade mandatory from V1 (§4.1) — ban direct `tokio::*` in the core    |
| **`crossbeam-channel` panics under WASM**       | Use `futures::channel::mpsc` everywhere, never crossbeam in the core               |
| **`rmcp` does not run under WASM**              | V2 Luna Studio Web = WebSocket client to a remote native Luna (see §9.2)            |
| **Missed mid-instruction effects** (Mario Kart, F-Zero) | `bus.io_cycle()` pattern on every CPU access (§5, §6.6). Test against the per-instruction CPU suite |
| **NMI/IRQ timing 1-cycle off**                  | Latch the IRQ/NMI state at instruction start, serve it before the next fetch |

### Open questions (to be settled in Phase 0)

1. **Final license**: MPL-2.0 (proposed) vs Apache-2.0 (more permissive).
   Validation after reviewing the desired commercial constraints.
2. **Third-party APU crate integration**: survey existing permissively
   licensed SPC700/DSP crates in Phase 0. If a suitable one exists, plan
   the integration in Phase 2 (saves ~1 month). Otherwise, APU from scratch.
3. **libretro core compatibility**: deferred to Phase 10. To confirm that
   the libretro constraints (sync API, threading) are compatible with our
   `!Send` core.
4. **WASM target from V1?**: recommended — the `luna-async` facade must be
   in place from the start to avoid costly backtracking. WASM compilation
   can stay "compile + basic tests" in V1, without a full GUI.
5. **Game-map format**: TOML, JSON, or a custom format? How to share in the
   community (GitHub registry, marketplace)?
6. **`luna-api` stabilization strategy**: at what point do we freeze the
   public API? Target: Phase 6.
7. **Multi-tenancy in V1.1 or V2**: a single emulation core per binary
   (simple) or several parallel sessions (unlocks "cloud sandbox")?
8. **`!Send` everywhere vs cfg-gate**: does the simplicity of `!Send`
   everywhere outweigh the lost native parallelism? Research recommendation:
   `!Send` everywhere (see eframe, the majority of cross-target Rust
   emulators).

### Product questions

- **Dual license model** (open source + commercial) if companies want to
  integrate Luna?
- **Marketplace of community-annotated game maps**?
- **Public benchmarks**: a suite of challenges ("beat Super Mario World
  level 1") to compare LLM performance?

---

## 16. Glossary

- **65C816**: the SNES's 16-bit CPU, derived from the 6502.
- **APU** (Audio Processing Unit): the SNES's sound subsystem, made up of
  the SPC700 and the DSP.
- **CGRAM**: 512 bytes of palette memory (256 colors × 16 bits).
- **Coprocessor**: an additional chip in a SNES cartridge (SA-1, Super FX,
  DSP-1, etc.).
- **Cycle-accurate**: emulation where every clock cycle is simulated, not
  just the final results of an instruction.
- **DMA** (Direct Memory Access): fast memory transfer without the CPU.
- **DSP**: Digital Signal Processor (here, either the APU audio DSP or a
  DSP-N coprocessor).
- **HDMA**: DMA synchronized to PPU scanlines.
- **HLE** (High-Level Emulation): simplified emulation of behaviors (vs
  cycle-accurate).
- **MCP** (Model Context Protocol): a standardized protocol for an LLM to
  communicate with external tools.
- **MMIO** (Memory-Mapped I/O): registers exposed as memory addresses.
- **OAM** (Object Attribute Memory): the memory that describes the SNES's
  128 sprites (512 bytes + 32 bytes of table 2).
- **PPU** (Picture Processing Unit): the SNES's video subsystem.
- **Scanline**: a horizontal line of pixels rendered by the PPU.
- **SPC700**: the 8-bit CPU dedicated to audio in the SNES.
- **Tilemap**: a grid of tiles that composes a background.
- **VRAM**: 64 KB of video memory (tiles, tilemaps).
- **WRAM** (Work RAM): the main CPU's 128 KB of work RAM.
