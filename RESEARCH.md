# Luna — Pre-Phase-0 Research

> Synthesis of the three research tracks conducted ahead of any code, to validate
> the architectural choices expressed in `ARCHITECTURE.md`. Research date:
> May 2026.

Three independent agents investigated in parallel:

1. **State of the art** of SNES emulators written in Rust → fork vs
   from-scratch decision.
2. **WASM compatibility** of the key dependencies → async
   cross-target strategy.
3. **Cycle-accurate patterns** in existing Rust emulators →
   scheduler architecture.

The full reports are reproduced below. The structural decisions that
follow from them are recorded in `ARCHITECTURE.md`
(§4, §5, §6.6, §9.2, §10, §14, §15).

---

## Contents

- [Structural decisions (summary)](#structural-decisions-summary)
- [Track 1 — State of the art: Rust SNES emulators](#track-1--state-of-the-art-rust-snes-emulators)
- [Track 3 — WASM compatibility of the key deps](#track-3--wasm-compatibility-of-the-key-deps)
- [Track 5 — Cycle-accurate patterns in Rust](#track-5--cycle-accurate-patterns-in-rust)

---

## Structural decisions (summary)

### Decision #1 — From-scratch under MPL-2.0 confirmed

- **jgenesis** (the only mature Rust SNES, 7 coprocessors, active) is **GPL-3.0** →
  blocks MPL-2.0 and would contaminate our MCP/API layer.
- **siena** has good architecture but **no declared license**.
- No fork that would save > 2 months.
- **But**: intensive reading of jgenesis is allowed (≠ copying) to
  draw inspiration from the architecture.
- Potential dependency: `emu-rs/snes-apu` (license to be checked in Phase 0).

### Decision #2 — Scheduler: jgenesis `CPU master-clock catch-up` pattern

- The `BinaryHeap<Event>` initially considered is rejected (heap
  overhead + Box<dyn> kills performance at 21M cycles/s).
- Adopted pattern: the CPU drives the master-clock, `bus.io_cycle()` catches
  the PPU/HDMA up on every memory access, APU sync via u64 rational
  arithmetic.
- Zero-alloc in the hot loop, mid-instruction accuracy for free,
  trivial save-states.

### Decision #3 — `luna-async` facade mandatory from V1

- `tokio::time` *panics* under WASM (`wasm32-unknown-unknown`).
- `crossbeam-channel` *panics* under WASM (parking primitive absent).
- Solution: an in-house `luna-async` crate that abstracts `spawn`/`sleep`/
  channels with `native` (tokio) and `web` (wasm-bindgen-futures +
  gloo-timers) impls.

### Decision #4 — Split `luna-mcp` into 3 crates

- `luna-mcp-core` (types, schemas) → cross-target ✅
- `luna-mcp-server` (rmcp + tokio) → **native only** ❌
- `luna-mcp-client` (WebSocket transport) → cross-target ✅
- Consequence: Luna Studio Web (WASM) cannot host an MCP
  server; it will connect via WebSocket to a remote native Luna.

### Decision #5 — Reject coroutines / genawaiter

- `#[coroutine]` is nightly only, save-states broken.
- `genawaiter` has marginal performance, LLVM struggles to inline.
- The jgenesis static-dispatch pattern does better on stable + is idiomatic.

### Decision #6 — `!Send` everywhere in the core

- Single-thread, WASM-compatible by default.
- Native parallelism goes through dedicated threads in
  `luna-mcp-server` only, not in `luna-core`.

---

## Track 1 — State of the art: Rust SNES emulators

### Projects evaluated

#### jgenesis (jsgroth) — **THE SERIOUS CANDIDATE**

- **URL**: https://github.com/jsgroth/jgenesis
- **License**: **GPL-3.0** (blocking for MPL-2.0)
- **Activity**: very active — v0.12.1 released May 2026, 2309 commits on
  master, 339 stars, 15 forks
- **Architecture**: modular multi-system (10 consoles). Cargo
  workspace with clean separation `backend/snes-core`,
  `backend/snes-coprocessors`, `cpu/` (65816, SPC700, 68000, Z80,
  6502, SH-2 cores shared between systems), `config/`, `frontend/`.
- **Accuracy**: targets "moderately accurate", not strictly
  cycle-accurate on SNES but with corrections for V-IRQ, DMA,
  Mode 7, vertical mosaic, and SA-1 wait-state timing.
- **SNES coprocessors**: **Super FX, SA-1, DSP-1, CX4, S-DD1, SPC7110,
  ST018** — the most complete set in the Rust SNES ecosystem.
- **Tests**: dedicated CPU harnesses, clean ARCHITECTURE.md, GitHub
  Actions CI, Linux/Windows/WASM builds.
- **Verdict**: technically top-tier, **blocked by the GPL-3.0 license**.

#### siena (twvd)

- **URL**: https://github.com/twvd/siena
- **License**: not stated (red flag — copyright by default)
- **Activity**: 489 commits, 19 stars, active CI, codecov tracked
- **Architecture**: cycle-accurate for 65816 and SPC700 (claimed),
  multi-threaded PPU renderer (scanline)
- **Coprocessors**: DSP-1 (LLE), SuperFX, SA-1 (partial), Super
  Gameboy. No CX4, S-DD1, SPC7110, ST018.
- **Status**: author states "not really fit for playing games" — active
  hobby project
- **Verdict**: legal risk (no license), limited coprocessors

#### rsnes (nat-rix)

- **URL**: https://github.com/nat-rix/rsnes
- **License**: MIT (compatible with MPL-2.0 ✓)
- **Activity**: abandoned since March 2022 (~4 years), 17 stars, 1 fork
- **Coprocessors**: none complete — all TODO
- **Verdict**: dead. Reusable as inspiration only.

#### ness (kelpsyberry)

- **URL**: https://github.com/kelpsyberry/ness
- **License**: not publicly visible
- **Activity**: 18 stars, 59 commits, active CI, prebuilt binaries
- **Verdict**: active but poorly documented. Author built `dust` (GBA),
  serious competence. Worth investigating further if we reject jgenesis.

#### Others

| Project | URL | License | Status |
|---|---|---|---|
| FranLMSP/snes | github.com/FranLMSP/snes | GPL-3.0 | WIP, embryonic |
| chronium/snes-emu | github.com/chronium/snes-emu | MIT | 22 commits |
| Achtuur/SNESemu | github.com/Achtuur/SNESemu | MIT | early WIP |
| mrjkey/rust-snes-emu | github.com/mrjkey/rust-snes-emu | n/a | empty shell |
| super-rustcom, pichi, rustsnes | not found | — | do not exist |

### Reusable crates

- **`w65c816`** (crates.io): the only standalone Rust 65C816 core. **Very
  incomplete** — "less than 60 instructions missing, plenty of addressing
  modes missing". Unusable as-is.
- **`snes-apu`** (emu-rs/snes-apu): a port of a C++ SNES APU to Rust.
  "Minimal maintenance", internal `unsafe`, "highly-accurate".
  **Potentially reusable** for the SPC700 + DSP audio if the license is
  compatible (to be checked in Phase 0).

### Comparison table

| Project | License | Active | Cycle-acc | Coproc | Modularity | Luna verdict |
|---|---|---|---|---|---|---|
| jgenesis | GPL-3.0 | ✅ | "moderate" | **7** | Excellent | Technically top-tier, **blocked by license** |
| siena | none | ✅ | ✅ | 4 partial | Good | Legal risk, limited coprocessors |
| ness | ? | ✅ | ? | ? | Good | To investigate |
| rsnes | MIT | ❌ 2022 | ❌ | 0 | Medium | Dead |
| FranLMSP | GPL-3.0 | WIP | ? | 0 | Medium | Too early |
| w65c816 crate | MIT/Apache | Stagnant | Partial | n/a | n/a | Unusable |
| snes-apu crate | MIT (to verify) | Minimal | "highly accurate" | n/a | OK | **Potential** as a dep |

### Recommendation: FROM-SCRATCH (with targeted borrowing)

**Why no fork:**

1. jgenesis (the only technically serious one) is GPL-3.0 → it would
   contaminate any distribution under MPL-2.0 and kill integration into
   proprietary AI agents.
2. The permissively-licensed forks (rsnes, chronium…) are dead or
   embryonic, with no coprocessors. No time savings.
3. Siena: no declared license = not legally forkable, and coprocessors
   too limited.
4. No reusable standalone Rust 65C816 core.
5. From-scratch maximizes the Luna objective (introspection API + MCP)
   designed as a day-1 design constraint.

**Verdict**: from-scratch under MPL-2.0, drawing openly on
jgenesis (reading GPL code is allowed — it is copying that
contaminates) for architectural choices, and using `snes-apu`
as a dependency if its license permits.

**Code to study in Phase 0** (reading, not copying):

- jgenesis `ARCHITECTURE.md` and the workspace structure
- jgenesis `backend/snes-core/src/api.rs` (the `Snes::tick` model)
- jgenesis `backend/snes-core/src/apu.rs` (rational catch-up)
- jgenesis `backend/snes-core/src/memory/dma.rs` (DMA/HDMA timing)
- siena's scanline-threaded PPU renderer (ideas)
- bsnes C++ (GPL-3.0, read-only) for SNES accuracy

### Sources

- [jsgroth/jgenesis](https://github.com/jsgroth/jgenesis) +
  [ARCHITECTURE.md](https://github.com/jsgroth/jgenesis/blob/master/ARCHITECTURE.md)
- [twvd/siena](https://github.com/twvd/siena)
- [nat-rix/rsnes](https://github.com/nat-rix/rsnes)
- [kelpsyberry/ness](https://github.com/kelpsyberry/ness)
- [FranLMSP/snes](https://github.com/FranLMSP/snes)
- [w65c816 crate](https://crates.io/crates/w65c816)
- [emu-rs/snes-apu](https://github.com/emu-rs/snes-apu)
- [GitHub topic: snes-emulator (Rust)](https://github.com/topics/snes-emulator?l=rust)

---

## Track 3 — WASM compatibility of the key deps

Target: `wasm32-unknown-unknown` (no WASI, no threads).

### Compatibility table

| Crate | wasm32-unknown-unknown status | Caveats / Features |
|---|---|---|
| `tokio` 1.x | Partial | `sync`, `macros`, `io-util` OK. `rt`/`rt-multi-thread`/`net`/`fs`/`process`/`signal` KO. `time` **panics** on uu |
| `tokio_with_wasm` 0.7+ | OK | Shim that reimplements `spawn`, `sleep`, `JoinHandle`… via `setTimeout`/microtasks. Marked "hacky, temporary" |
| `wasm-bindgen-futures` | OK | `spawn_local`, `JsFuture`, `future_to_promise`. No multitasking executor — a bridge to the JS microtask queue |
| `smol` / `async-std` | KO | Rely on native I/O reactors. No web support |
| `futures` / `futures-channel` (mpsc/oneshot) | OK | Async channels, single-thread friendly. **To be used instead of tokio mpsc in multi-target mode** |
| `crossbeam-channel` | **KO on uu** | `recv()` panics "unreachable" — parking primitive absent without threads |
| `flume` | Conditional | OK with non-blocking `try_recv` + `async` feature. Blocking = same problem |
| `async-channel` | OK | Multi-producer, pure async, works single-threaded |
| `wgpu` 22+ | OK | `webgpu` (recent Chromium) and `webgl` (GLES2) features. For WebGPU: `RUSTFLAGS="--cfg=web_sys_unstable_apis"` |
| `egui` / `eframe` | OK | `eframe` officially targets web via `eframe_template` (Trunk + wasm-bindgen) |
| `serde` + `serde_json` | OK | `no_std` possible with `alloc` |
| `rmcp` 0.13+ | Partial | Compiles on `wasm32-wasip2` and `wasm32-wasip1`. **No official `wasm32-unknown-unknown` support** for the server |
| `schemars` | OK | `std` feature can be disabled for no_std |
| `ts-rs` | OK (build-time) | Generates the `.ts` files when running tests on the native side. Should not be linked into the WASM binary |
| `utoipa` | OK | Pure derive/macro |
| `cpal` | Partial | "wasm-bindgen" backend (WebAudio): **output OK, input KO**. Blocked by autoplay policy |
| `genawaiter` | OK | Stackless coroutines on stable. Pure Rust, no syscalls. WASM-compatible but **rejected** (cf. track 5) |
| `gloo-file`, `gloo-events`, `gloo-timers` | OK | Recommended for DOM/file/timers from Rust |

### Cross-target async strategy (`luna-async` facade)

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

// Universal channels (cross-target)
pub use futures::channel::{mpsc, oneshot};
```

**Rules**:

- Ban `tokio::*` in `luna-core` and `luna-mcp-core`: go through
  `luna_async::*`.
- For `Send` bounds: most cross-target emulators choose
  `!Send` everywhere to keep things simple. `wasm-bindgen-futures` imposes `!Send`,
  so either we make everything `!Send`, or we cfg-gate the bound.

### Cross-target 60Hz loop

| Aspect | Native | WASM |
|---|---|---|
| Pacer | dedicated thread + `spin_sleep` / `Instant` | `requestAnimationFrame` |
| State sharing | `Arc<Mutex<EmuState>>` or channel | `Rc<RefCell<EmuState>>` (single-thread) |
| MCP / async | tokio tasks on another thread | `spawn_local` tasks on the same thread |

Canonical WASM pattern: `Rc<RefCell<Option<Closure>>>` that reschedules itself
([official wasm-bindgen example](https://rustwasm.github.io/docs/wasm-bindgen/examples/request-animation-frame.html)).
Between each frame, the microtask queue drains the MCP futures — so the
MCP handler and the emulation loop share an `Rc<RefCell<EmuState>>`
without contention, **provided you never `borrow_mut()` across an
`await`**.

### Must replace (WASM-blocking)

| Blocker | Replacement |
|---|---|
| `crossbeam-channel` | `futures::channel::mpsc` / `async-channel` |
| `std::thread::spawn` | `wasm_bindgen_futures::spawn_local`. CPU-heavy work via Web Worker (`gloo-worker`, `wasm_thread`) |
| `tokio::time` | `gloo-timers::future::TimeoutFuture` on the web side (`luna_async::sleep` facade) |
| `tokio::net` / `axum` (MCP transport) | On the web side: `postMessage` or WebSocket (`gloo-net`/`ws_stream_wasm`). **Do not** run the rmcp server in the browser |
| `std::fs` (ROM loading) | `<input type="file">` + drag&drop → `gloo-file::File::read_as_bytes` → `Vec<u8>` injected |
| `cpal` audio input | OK for output only. For input: `web-sys::MediaStream` directly |
| `rmcp` HTTP/SSE server in the browser | Impossible. **V2 web = WebSocket client to a remote native Luna** |

### Recommended crate layout

```
luna/
├── luna-core/          ✅ wasm-safe, no I/O, no tokio
├── luna-async/         ✅ wasm-safe facade (spawn/sleep/channels)
├── luna-frontend/      Frontend trait + native & web impl
├── luna-ui/            ✅ wasm-safe: egui widgets, debugger, watcher
├── luna-mcp-core/      ✅ wasm-safe: Tool/Resource types, serde, schemars
├── luna-mcp-server/    ❌ native-only: rmcp + tokio + axum (SSE/HTTP)
├── luna-mcp-client/    ✅ cross-target: WebSocket transport for Studio Web
└── luna-app-native/    native binary
└── luna-app-web/       cdylib wasm (eframe + wasm-bindgen)
```

### Truly blocking points to arbitrate

1. **MCP in V2 web**: do not embed the rmcp server in the WASM bundle.
   Either a remote native server + WS client, or wait for
   `wasm32-wasip2` + the Component Model (maturity mid-2026).
2. **`tokio::time` panics under WASM**: if anything in `luna-core`
   or `luna-mcp-core` calls `tokio::time`, the build compiles but
   crashes at runtime. The `luna-async` facade is **mandatory** from V1.
3. **Threads for low-latency audio**: single-thread WASM = no
   portable AudioWorklet. Accept 50-100 ms of latency in V2 or
   bridge via JS AudioWorklet + SAB ring buffer (complex, COOP/COEP).
4. **`Send` bounds**: decide early. Recommendation: `!Send` everywhere.

### Sources

- [tokio Issue #5418 — time panic on uu](https://github.com/tokio-rs/tokio/issues/5418)
- [tokio_with_wasm](https://github.com/cunarist/tokio-with-wasm)
- [wasm-bindgen-futures](https://crates.io/crates/wasm-bindgen-futures)
- [wgpu Web wiki](https://github.com/gfx-rs/wgpu/wiki/Running-on-the-Web-with-WebGPU-and-WebGL)
- [crossbeam Issue #756 — panic on wasm](https://github.com/crossbeam-rs/crossbeam/issues/756)
- [CPAL WASM wiki](https://github.com/RustAudio/cpal/wiki/Setting-up-a-new-CPAL-WASM-project)
- [rmcp WASM considerations](https://paiml.github.io/rust-mcp-sdk/course/part3-deployment/ch09-01-wasm-considerations.html)
- [mcp-wasm PoC](https://github.com/beekmarks/mcp-wasm)
- [wasm-bindgen rAF example](https://rustwasm.github.io/docs/wasm-bindgen/examples/request-animation-frame.html)
- [eframe_template](https://github.com/emilk/eframe_template)
- [bokuweb/rustynes](https://github.com/bokuweb/rustynes), [takahirox/nes-rust](https://github.com/takahirox/nes-rust) — NES in WASM
- [bmoxb/rustyboy](https://github.com/bmoxb/rustyboy) — cross-target close to Luna

---

## Track 5 — Cycle-accurate patterns in Rust

### Survey of the patterns

#### TetaNES (lukexor) — "CPU-driven master clock catch-up"

The CPU owns the `master_clock`. On each CPU half-cycle
(`start_cycle`/`end_cycle`), it calls
`bus.ppu.clock_to(master_clock - PPU_OFFSET)` which catches the PPU up
to the current instant.

```rust
// tetanes-core/src/common.rs:155
pub trait Clock { fn clock(&mut self) {} }
```

DMA handled as a special CPU state: `handle_dma()` emits dummy
`start_cycle`/`end_cycle` calls that advance the PPU normally.

#### jgenesis (jsgroth) — "Per-CPU-cycle dispatch" **(the model for Luna)**

The central `tick()`:

```rust
// backend/snes-core/src/api.rs:284-395
let (master_cycles_elapsed, pending_write) = if self.memory_refresh_pending { ... }
else { match self.dma_unit.tick(...) {
        DmaStatus::None => { self.main_cpu.tick(&mut bus); ... (bus.access_master_cycles, ...) }
        DmaStatus::InProgress { master_cycles_elapsed } => (master_cycles_elapsed, None)
}};
self.apu.tick(master_cycles_elapsed);
self.memory.tick(master_cycles_elapsed);
self.ppu.tick(master_cycles_elapsed);
```

APU sync via drift-free rational-arithmetic catch-up:

```rust
// backend/snes-core/src/apu.rs:274-298
self.master_cycles_product += main_master_cycles * apu_master_clock_frequency;
while self.master_cycles_product >= 24 * self.main_master_clock_frequency {
    self.master_cycles_product -= 24 * self.main_master_clock_frequency;
    self.clock(); // 1 APU master tick
}
```

#### gameroy (Rodrigodd) — "Lazy update + event prediction"

Each component publishes `next_interrupt: u64` (absolute master cycles).
The `GameBoy` only advances components on demand. Very good performance,
but **each component must know how to predict** its next event.
To be used **as a complement** to optimize WAI/STP.

#### rboy (mvdnes) — "Instruction-step + propagate"

The most naive. `cpu.do_cycle()` executes a full instruction, returns
`ticks`, then `mmu.do_cycle(ticks)` distributes the delta to GPU/timer/sound.
**Readable but not cycle-accurate at mid-instruction.**

#### DaveTCode/nes-emulator-rust — "Per-cycle state-machine"

The CPU advances one master cycle at a time via a `State` enum. Very
testable but a lot of boilerplate.

#### moa (transistorfet) — "libco-style event-queue"

A real discrete-event scheduler with a sorted `Vec<NextStep>`. Clean pattern,
but a lot of overhead (RefCell, Rc, HashMap). **Not optimized for
21 MHz × 60fps**.

#### Lochnes/bagnalla — "Rust generators/coroutines"

`#[coroutine]` (nightly). Readable but ~11ms/frame NES, save-states
impossible. **To be rejected.**

#### Generic DES crates

- `desru`: BinaryHeap + Box<dyn FnMut> → too many allocations
- `nexosim`: multi-thread actor model, designed for system simulation,
  not real-time 60fps
- `desim`: nightly

**No generic crate is suitable at 21 MHz.**

### Comparison table

| Pattern | Readability | Perf | Testability | Cycle-acc | Mid-instr | Luna verdict |
|---|---|---|---|---|---|---|
| **CPU master-clock catch-up (TetaNES, jgenesis)** | Good | Excellent (zero alloc) | Good | Yes if bus ticks on each access | Yes | **Recommended** |
| Per-cycle state-machine (DaveTCode) | Medium | Very good | Excellent | Perfect | Native | For an isolated CPU |
| Event queue (moa) | Very good | Poor | Excellent | Good | Difficult | Too slow |
| Lazy + next_event (gameroy) | Medium | Excellent | Medium | Good | Difficult | As a complement |
| Naive instruction-step (rboy) | Excellent | Excellent | Good | No | No | Insufficient for SNES |
| Generators/coroutines | Excellent | Medium | Difficult | Perfect | Native | Reject |
| DES crate (nexosim/desru) | Good | Bad | Good | Variable | Variable | Not suitable |

### Recommendation for Luna

**Main pattern: jgenesis-style CPU-driven master clock catch-up**,
with four refinements:

1. **Single time unit**: everything in master clock cycles
   (21.477 MHz NTSC = ~357,954 master cycles/frame). `u64` does not
   overflow for 27,000 years.
2. **The 65C816 exposes `tick(bus) -> u64`** which executes ONE instruction
   and returns the master-cycles delta consumed. On every memory access,
   it calls `bus.io_cycle()` which immediately catches up PPU+DMA.
3. **APU**: rational-arithmetic catch-up (3.072 MHz SPC700 /
   21.477 MHz CPU). jgenesis pattern, line 274.
4. **Event prediction** as a complement (gameroy-style): the PPU computes
   `next_event_mclk` to optimize the 65C816's WAI/STP.

**Why not a pure event-queue**: at 21M cycles/sec, BinaryHeap + Box +
dyn dispatch adds 50-100ns/event, i.e. 50% of the cycle budget. TetaNES and
jgenesis hit a comfortable 60fps because there is **no
allocation** in the hot loop.

**Why not coroutines**: nightly, broken save-state, marginal performance.

### Full Rust sketch

```rust
// === luna-core/src/scheduler.rs ===

pub type MCycles = u64;
pub const NTSC_MASTER_HZ: u64 = 21_477_272;
pub const MCYCLES_PER_FRAME_NTSC: MCycles = 357_366;

#[derive(Default)]
pub struct TickEffect {
    pub frame_complete: bool,
    pub audio_samples: smallvec::SmallVec<[(f32, f32); 8]>,
}

pub trait Bus {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, val: u8);
    fn io_cycle(&mut self, mcycles: MCycles);
    fn nmi_pending(&self) -> bool;
    fn irq_pending(&self) -> bool;
}

pub trait Component {
    fn tick(&mut self, delta: MCycles) -> TickEffect;
    fn next_event_mclk(&self) -> Option<MCycles> { None }
}

pub struct Snes {
    pub cpu: Cpu65816,
    pub ppu: Ppu,
    pub apu: Apu,
    pub dma: DmaUnit,
    pub cart: Cartridge,
    pub wram: Box<[u8; 0x20000]>,
    pub total_mclk: MCycles,
    pub frame_mclk: MCycles,
    pub memory_refresh_pending: bool,
}

impl Snes {
    #[inline]
    pub fn step(&mut self) -> TickEffect {
        let delta = if self.memory_refresh_pending {
            self.memory_refresh_pending = false;
            MEMORY_REFRESH_CYCLES
        } else if self.dma.active() {
            self.dma.tick(&mut SnesBus::new(&mut self.cart, &mut self.wram,
                                           &mut self.ppu, &mut self.cpu))
        } else {
            let mut bus = SnesBus::new(&mut self.cart, &mut self.wram,
                                       &mut self.ppu, &mut self.cpu_regs);
            self.cpu.step(&mut bus);
            bus.access_master_cycles_total
        };

        let mut effect = TickEffect::default();
        let apu_eff = self.apu.tick(delta);
        effect.audio_samples.extend(apu_eff.audio_samples);

        let ppu_eff = self.ppu.catch_up_to(self.total_mclk + delta);
        if ppu_eff.frame_complete { effect.frame_complete = true; }

        self.total_mclk += delta;
        self.frame_mclk += delta;
        if self.crosses_memory_refresh_boundary(delta) {
            self.memory_refresh_pending = true;
        }
        effect
    }

    pub fn run_to_frame(&mut self, audio_out: &mut Vec<(f32, f32)>) {
        loop {
            let eff = self.step();
            audio_out.extend(eff.audio_samples);
            if eff.frame_complete { return; }
        }
    }
}

// === Rational-arithmetic APU ===
impl Apu {
    pub fn tick(&mut self, main_mcycles: MCycles) -> TickEffect {
        self.numerator += main_mcycles * APU_HZ;
        while self.numerator >= CPU_HZ * APU_DIVIDER {
            self.numerator -= CPU_HZ * APU_DIVIDER;
            self.spc700.step(&mut self.bus);
            self.timer0.tick(); self.timer1.tick(); self.timer2.tick();
            if self.sample_divider.tick() { self.emit_sample(); }
        }
        TickEffect::default()
    }
}

// === Interrupts ===
impl Cpu65816 {
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.pending_nmi { self.service_nmi(bus); return; }
        if self.pending_irq && !self.p.contains(Flags::I) {
            self.service_irq(bus); return;
        }
        let opcode = self.fetch_op(bus);
        DISPATCH[opcode as usize](self, bus);
        self.pending_nmi |= bus.nmi_pending();
        self.pending_irq = bus.irq_pending();
    }
}
```

### Risks & mitigations

| Risk | Mitigation |
|---|---|
| **Hostile borrow-checker** (bus mut + cpu mut + ppu mut) | jgenesis pattern: `SnesBus<'a>` created on each step, separate borrows. No `Rc<RefCell>` in the hot loop |
| **Missed mid-instruction effects** (Mario Kart, transparency) | `bus.io_cycle()` on EVERY CPU memory access. Test against Tom Harte 65816 |
| **APU/CPU drift** | `u64` rational arithmetic (no float). Test: after 1h, `apu.cycle_count() ≈ apu_freq * elapsed` |
| **Complex DMA timing** | Separate DMA unit that returns `DmaStatus { None, InProgress { mcycles } }` |
| **NMI/IRQ timing 1-cycle off** (Wild Guns bug documented in jgenesis) | Latch the NMI/IRQ state at the start of the instruction, service it BEFORE the next fetch |
| **Performance < 60fps** | Profile with `criterion`; target 100M cycles/sec min on a modern ARM. Inline the DISPATCH jump-table |
| **Testability** | The `Bus` trait allows injecting a test bus (RAM-only). The `Component::tick` trait for the PPU |
| **Save states** | All concrete fields `Serialize`/`Deserialize` via serde + bincode (impossible with coroutines) |
| **Run-ahead / netplay** | Pure `step()` → clone the entire state and replay it |

### Executive summary

Adopt the **jgenesis-style** pattern (CPU master-clock catch-up +
mid-instruction `bus.io_cycle()` + rational APU), which is today the
Rust reference for a cycle-accurate SNES validated against the Tom Harte
test suites. The code is readable (1 iteration = 1 CPU/DMA step),
zero-alloc in the hot loop, and save-state compatible. It is the
functional equivalent of libco/higan **without coroutines**, in idiomatic
Rust static dispatch.

### Files to study in Phase 0

- `jgenesis/backend/snes-core/src/api.rs` (the `Snes::tick` at line 284)
- `jgenesis/backend/snes-core/src/apu.rs` (rational catch-up at line 274)
- `jgenesis/backend/snes-core/src/memory/dma.rs` (DMA/HDMA timing)
- `jgenesis/backend/snes-core/src/bus.rs` (`access_master_cycles` computation)
- `tetanes-core/src/cpu.rs` lines 280-325 (the `start_cycle`/`end_cycle` pattern)
- `gameroy/core/src/gameboy.rs` line 375 (event prediction for WAI/STP)

### Sources

- [TetaNES — common.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/common.rs),
  [cpu.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/cpu.rs#L280-L325),
  [control_deck.rs](https://github.com/lukexor/tetanes/blob/main/tetanes-core/src/control_deck.rs#L679)
- [jgenesis — api.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/api.rs#L284),
  [apu.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/apu.rs#L274),
  [memory/dma.rs](https://github.com/jsgroth/jgenesis/blob/master/backend/snes-core/src/memory/dma.rs)
- [gameroy — gameboy.rs](https://github.com/Rodrigodd/gameroy/blob/master/core/src/gameboy.rs#L375),
  [interpreter.rs](https://github.com/Rodrigodd/gameroy/blob/master/core/src/interpreter.rs#L633)
- [rboy — mmu.rs](https://github.com/mvdnes/rboy/blob/master/src/mmu.rs#L179)
- [DaveTCode/nes-emulator-rust](https://github.com/DaveTCode/nes-emulator-rust/blob/main/emulator/src/cpu/mod.rs#L1049)
- [moa — system.rs](https://github.com/transistorfet/moa/blob/main/emulator/core/src/system.rs)
- [kyle.space — NES emulator post](https://kyle.space/posts/i-made-a-nes-emulator/),
  [bagnalla/6502](https://github.com/bagnalla/6502)
- [desru](https://docs.rs/desru), [nexosim](https://github.com/asynchronics/nexosim)
- [Tom Harte ProcessorTests 65816](https://github.com/SingleStepTests/65816)
