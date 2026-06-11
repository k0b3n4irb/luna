# ness & jgenesis — Rust SNES emulator dissection (backend reference + frontend backlog)

> **RETRACTED 2026-06-11.** This doc's original §0/§6 conclusion — that the Doom
> border flicker is luna's lumped DMA / a "~3.3× slow loop" needing the
> state-injection oracle or a cycle-based rearchitecture — is WRONG. The flicker
> was root-caused as a PPU register bug: reading `$213F` (STAT78) did not reset
> the OPHCT/OPVCT byte-read flip-flop (ares `io.cpp:167-169`), so V-counter reads
> were 50% wrong → Doom's raster IRQ handler took a no-ack branch, re-firing the
> H/V IRQ ~200×/frame and pinning the S-CPU at I=1. A surgical PPU fix solved it;
> no DMA/scheduler rearchitecture was needed. See `docs/accuracy_scorecard.md`
> and the `project_doom_flicker_opvct_latch` memory.

Two Rust SNES emulators dissected backend + frontend as a reference comparison
for luna's architecture: **ness** (kelpsyberry, github) and **jgenesis**
(jsgroth, github). Rust → directly portable (no ares cothread problem). All
claims are `file:line` in the respective trees (cloned to /tmp during the study).

What remains valuable here, independent of the (retracted) flicker theory:

- §1 as a **neutral** reference for how ness/jgenesis structure CPU / scheduler /
  DMA / H-V IRQ — useful background, not a flicker diagnosis.
- §2 the **frontend backlog** (DRC audio resampling, triple-buffer / present
  handshake framebuffer) — real, still-relevant work, now partly implemented.
- §3 luna's genuine advantages, §4 game-specific quirks to port carefully.

## 1. Backend — how the references structure CPU / scheduler / DMA

Neutral comparison. (Do **not** read a flicker cause into this section — the Doom
flicker was a PPU OPVCT-latch bug, not any of these structural differences.)

### 1a. CPU timing model
- **ness**: whole instructions run back-to-back but **capped at
  `next_event_time()`** (`cpu/interpreter.rs:40,44,62`); time advances inside
  every access (`interpreter/common.rs:9-19`) and every idle. Event-queue driven.
- **jgenesis**: genuinely **cycle-based** — bus trait has `read/write/idle`
  (`cpu/wdc65816-emu/src/traits.rs:4-18`); `tick()` does ONE cycle
  (`core.rs:206-227`); the outer `Emulator::tick` advances PPU/APU/coproc/IRQ by
  that one access's mclk after every cycle (`api.rs:312-386`).
- **luna**: whole-instruction `run_one` + per-bus-access hooks advancing
  APU/PPU/coproc. The ness agent calls luna "similar in spirit" — both charge
  cycles per access. A full cycle-CPU is a later, separate accuracy project
  (writes-lead-reads, sub-instruction interrupt cases), not a flicker lever.

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

### 1c. H/V-timer IRQ delivery
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
- **luna**: polls the coincidence per-bus-access, delivers a level.

The references step GP-DMA per byte (ness `cpu/dma.rs:336-356`; jgenesis
`memory/dma.rs:339-388`, IRQ evaluated between bytes via `api.rs:343-386`), where
luna lumps the whole transfer. That is a real architectural difference worth
noting for general accuracy — but it was **not** the Doom flicker (the framebuffer
DMAs sit at lines 200-262, not over the raster IRQ lines 23/199).

## 2. Frontend — the backlog (independent of the flicker, still relevant)

### 2a. A/V sync — Dynamic Rate Control
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
- **luna**: audio-as-clock; DRC is the model to follow (luna already has the
  resampler + audio clock; needs the queue-depth→ratio loop + an API method).

### 2b. Frame presentation — retire the Mutex framebuffer
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
