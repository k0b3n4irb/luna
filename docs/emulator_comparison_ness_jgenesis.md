# Challenging luna's fundamentals — ness + jgenesis dissection (2026-06-10)

Two Rust SNES emulators dissected backend + frontend to challenge luna's
architecture: **ness** (kelpsyberry, github) and **jgenesis** (jsgroth, github).
Rust → directly portable (no ares cothread problem). All claims are `file:line`
in the respective trees (cloned to /tmp during the study).

## 0. THE convergent verdict — the Doom flicker is luna's LUMPED DMA

**Both references, independently, fix the straddling-IRQ flicker at the DMA /
scheduler layer — and BOTH say a cycle-based CPU is NOT the requirement.**

- **ness** steps GP-DMA per byte (`+= 8` each) inside `while cur_time <
  next_event_time() { transfer 1 unit }` (`core/src/cpu/dma.rs:336-356`). The
  H/V IRQ is a **pre-scheduled event** with an exact computed dot
  (`ppu/counters.rs:185-217`); the DMA loop yields at that event's timestamp, so
  the coincidence is honored *inside* the DMA.
- **jgenesis** steps GP-DMA one byte per `Emulator::tick`, returning
  `DmaStatus::InProgress { master_cycles_elapsed: 8 }` (`backend/snes-core/src/
  memory/dma.rs:339-388`); the outer loop advances the PPU H/V counters + the
  H/V-IRQ evaluator **between every DMA byte** (`api.rs:343-386`). IRQ latches on
  the rising edge `irq.pending |= !line && new_line` (`memory.rs:941`) and holds
  until the CPU services it after DMA releases the bus.

luna **lumps** the whole DMA (charges its time at once, no per-byte interleave),
so the H/V-IRQ evaluator runs **once after** the transfer — a coincidence buried
inside the DMA (or pushed past the frame boundary by the DMA's duration) is
silently skipped. That is precisely the "~50% of frames get neither raster IRQ →
border flicker" symptom, now confirmed by two independent references.

> jgenesis agent, verbatim: *"the straddling-IRQ flicker is fixed at the DMA/
> scheduler layer … A cycle-based CPU helps the residual sub-instruction cases,
> but the straddling-IRQ flicker is fixed at the DMA/scheduler layer."*

**So Phase 5 (DMA cycle-stepping) is THE fix, and it is BOUNDED — not the
1-2-week cycle-by-cycle 65c816 rewrite I had feared.**

## 1. Backend — the fundamentals challenged

### 1a. CPU timing model (verdict: luna's is *defensible*, not the bug)
- **ness**: whole instructions run back-to-back but **capped at
  `next_event_time()`** (`cpu/interpreter.rs:40,44,62`); time advances inside
  every access (`interpreter/common.rs:9-19`) and every idle. Event-queue driven.
- **jgenesis**: genuinely **cycle-based** — bus trait has `read/write/idle`
  (`cpu/wdc65816-emu/src/traits.rs:4-18`); `tick()` does ONE cycle
  (`core.rs:206-227`); the outer `Emulator::tick` advances PPU/APU/coproc/IRQ by
  that one access's mclk after every cycle (`api.rs:312-386`).
- **luna**: whole-instruction `run_one` + per-bus-access hooks advancing
  APU/PPU/coproc. The ness agent calls luna "similar in spirit" — both charge
  cycles per access. **luna's CPU model is NOT the flicker cause.** The
  difference that matters is (i) luna doesn't *bound* CPU execution at the next
  IRQ event, and (ii) lumped DMA. A full cycle-CPU is a later, separate accuracy
  project, not required for the flicker.

### 1b. Scheduler / interleave
- **ness** = global **event queue** on one master clock (`schedule.rs:25-114`);
  CPU runs to the next event, then events drain (`emu.rs:59-79`). The H/V IRQ is
  a first-class scheduled event ⇒ a **hard barrier** that can never land "between
  frames." APU catches up on demand ($2140-7F access). **No coprocessor in tree**
  (plain LoROM/HiROM only) — ness gives luna *nothing* on SA-1/GSU scheduling.
- **jgenesis** = **cycle-driven**: each `Emulator::tick` advances all components
  by the current access's variable slice (6/8/12 mclk); fixed component order
  DMA→CPU→APU→coproc→PPU→IRQ-eval (`api.rs:284-403`). Coprocessor ticked per
  slice via `memory.tick(master_cycles)` (`api.rs:350`) — **a real reference for
  coprocessor interleave** (unlike ness).
- **luna** = CPU-driven, lumped DMA, IRQ polled per-access (not a barrier).

### 1c. H/V-timer IRQ delivery (the flicker mechanism)
- **ness**: compute the **exact next-coincidence timestamp** on every
  HTIME/VTIME/$4200/scanline change and schedule it (`counters.rs:185-217`);
  sticky/level request held until $4211 read; gated by I-flag at instruction
  boundary. Register writes are timestamped at `last_poll_time`, not mid-instr
  `cur_time` (`access.rs:124-169`) — a subtle anti-look-ahead choice.
- **jgenesis**: re-evaluate the IRQ **level every cycle/slice** with a **windowed
  check** — does htime fall in `(last_ppu_htime, ppu_htime]`, with scanline
  wrap? (`memory.rs:887-934`). Rising-edge latch `pending |= !line && new_line`
  (`memory.rs:941`); poll on the instruction's **final cycle**
  (`instructions.rs:39-42`); $4211 read clears pending (`memory.rs:416-424`).
- **luna**: polls the coincidence per-bus-access, delivers a level — but with
  lumped DMA the poll can skip across the coincidence dot entirely.

## 2. Frontend — the fundamentals challenged

### 2a. A/V sync — luna's BIGGEST gap: no Dynamic Rate Control
- **jgenesis**: audio-as-clock (park/unpark, like luna) **+ DRC**: every 20
  frames, nudge the audio resample ratio by ≤±0.5% based on queue depth
  (`common/jgenesis-common/src/audio.rs:64-91`), fed back via
  `update_audio_output_frequency` (`frontend.rs:380`, applied
  `runner.rs:368-369`). This **locks 60.0988 Hz emulation to the real audio
  device clock** with no pitch artifact, no frame drop/dup. Three modes:
  audio+DRC (default), frame-time limiter, pure vsync.
- **ness**: audio-as-clock too, but a **spin_loop busy-wait inside the DSP
  callback** (`audio.rs:70-81`) + an independent fixed-timer accumulator cap
  (`emu.rs:183-199`). No DRC.
- **luna**: audio-as-clock only, **no DRC, no fixed-timer fallback** → two
  free-running 60 Hz clocks drift/beat. Borrow jgenesis's DRC (luna already has
  the resampler + audio clock; needs the queue-depth→ratio loop + an API method).

### 2b. Frame presentation — luna's Mutex framebuffer is the weakest link
- **ness**: lock-free **triple buffer** (`triple_buffer.rs:9-46`) — producer
  never blocks, consumer skips re-upload when no fresh frame (`ui.rs:576`). No
  tearing, no torn/half-written frames, zero lock contention.
- **jgenesis**: **present handshake** — runner sends a frame pointer over
  `sync_channel(1)` and blocks until the main thread presents
  (`mainloop/render.rs:81-98`). Bounds emu-ahead to ≤1 frame, every produced
  frame is presented (no silent drop/dup), zero-copy.
- **luna**: one RGBA framebuffer behind a **Mutex**; UI grabs "latest" at vsync.
  Latent stall (UI holding the lock during a slow upload back-pressures the emu)
  + silent drop/dup between two free 60 Hz clocks. Adopt the triple buffer (or
  sync_channel(1) handshake).

### 2c. Input timing
- Both refs sample input **at the emulated joypad read** (the cycle the game
  reads $4016/$4017): jgenesis `InputPoller::poll` (`frontend.rs:307`), ness
  delta-based at emu-loop top. luna latches an absolute per-frame snapshot.
  jgenesis's poll-at-read is finer; luna's **absolute-state-through-luna-api** is
  more robust to lost edges than ness's deltas — keep luna's model, but consider
  sampling at the auto-read point.

## 3. What luna does BETTER (be honest)
- **Single `luna-api` observe+control contract** shared by CLI/GUI/MCP. jgenesis
  bolts the debugger on separately (`DebuggerRunnerProcess`); ness has no such
  unified surface. This is a genuine luna advantage — DRC + triple-buffer can be
  added *to luna-api* without losing it.
- **5 Hz DC blocker + 6-point Hermite** — ness has cubic but **no DC blocker**.

## 4. The devil in the details (game-specific quirks to port carefully)
- jgenesis: **writes lead/lag reads by a cycle** — `bus.write` records
  `pending_write`, applied AFTER components advance (`bus.rs:214-221`,
  `api.rs:388-393`; fixes Rendering Ranger R2). luna's lumped model can't express
  this.
- jgenesis: **latched interrupts across DMA** reproduce the 1-cycle post-DMA
  interrupt-recognition delay (`api.rs:319-326`; Wild Guns). DMA starts on an
  8-mclk boundary and ends re-aligned to a whole CPU cycle via a CPU-clone trick
  (`dma.rs:391-421`).
- ness: IRQ scheduled vs `last_poll_time`; HTIME `(<<2)+14` offset fudge; **$4211
  4-cycle hold window NOT implemented** (both emulators flag this gap); "can't
  trigger on the last dot of a field" (`counters.rs:227-228`).
- Both: interrupt **poll on the instruction's last cycle**; NMI true-edge, IRQ
  level; open bus is a real modeled value (jgenesis `cpu_open_bus()`).

## 5. The plan this produces

- **Phase 5 (DMA cycle-stepping) = THE bounded flicker fix.** Port the per-unit
  DMA state machine (jgenesis `dma.rs` / ness `dma.rs:336`): DMA yields after
  each transfer unit; run the PPU H/V-counter advance + the H/V-IRQ **windowed
  level eval** (jgenesis `memory.rs:887-942`: rising-edge latch held until
  serviced) on each yielded slice. This decouples IRQ latching (at the true dot,
  even mid-DMA) from IRQ servicing (after DMA releases the bus). Oracle: Doom →
  4 IRQ/frame regular like Mesen (the bisection oracle), `gsu_*` byte-exact, DKC,
  no GSU/SA-1 regression.
- **Frontend backlog (independent, high felt-value):** (1) DRC; (2) lock-free
  triple buffer or sync_channel(1) present handshake to retire the Mutex
  framebuffer; both added to `luna-api`.
- **Not required for the flicker:** the full cycle-by-cycle 65c816 rewrite. It's
  a later accuracy project (writes-lead-reads, sub-instruction interrupt cases),
  not the lever.

## 6. Phase 5 DE-RISKED AND REFUTED for luna's Doom flicker (2026-06-10)

Before the Phase 5 refactor, instrumented luna's Doom: logged every GP-DMA
($420B) with start scanline + duration, correlated with the INIDISP raster
writes. Result **refutes the lumped-DMA-causes-the-flicker hypothesis for this
specific case**:
- **Only 2 of 1556 GP-DMAs span scanline 23 or 199** (the raster IRQ lines).
  The big framebuffer DMAs are at lines **200-262** (bottom + vblank), NOT over
  the IRQ lines. So per-byte DMA stepping would NOT change the IRQ timing — the
  DMA doesn't overlap the IRQ. **The de-risk saved a multi-day refactor.**

But it surfaced the real, **confound-free** signal (a rate over 2000 frames, so
the boot offset doesn't bias it):
- **luna: ~0.6 GP-DMA triggers/frame and 0.74 INIDISP writes/frame.**
- **Mesen: 2.01 GP-DMA/frame and 2.00 INIDISP/frame, every frame.**

So **Doom's entire main loop runs ~3.3× less often on luna** (the GP-DMA +
raster pair is one loop iteration). This is BOTH the flicker (border re-blanked
0.6×/frame not 2×) AND the user's "Doom is a bit slower than Mesen" — one root.

Hypotheses now REFUTED for the flicker: (1) H/V-IRQ level model, (2) GSU bus
arbitration, (3) DRAM refresh (marginal), (4) lumped DMA / Phase 5 (DMAs don't
span the IRQ lines), (5) GSU clock speed (a 3× spike made it *worse*, but that
test is confounded by the demo being GSU-driven → different scene). The
Mesen-instr/frame measurement (to decide CPU-rate vs Doom-waiting) could not be
captured (Mesen Lua exec callback config kept failing).

**Verdict: the flicker is a deep, multi-factor emulated-timing deficit (Doom's
loop runs 3.3× slow), NOT isolable by surgical per-subsystem tests because every
luna-vs-Mesen comparison is confounded by the irreducible boot-frame offset
(docs/cooperative_scheduler_reference.md §4b).** The only confound-free oracle is
**full-system state injection** (inject a Mesen savestate — CPU+WRAM+PPU+GSU+APU
— into luna, run both forward, bisect the first divergence in the loop). That, or
commit to the **cycle-based rearchitecture** (§0/§1 — jgenesis's model) as the
fundamental fix. Surgical patching is exhausted.
