# Scope — Doom "slow / laggy" performance (2026-06-09)

Governed by `.claude/rules/api-first.md` and `faithful-port-and-dichotomy.md`.
This scope is measurement-first; the headline overturns the obvious assumption.

## Headline: it is NOT the GSU engine. Do not optimise it.

Measured headless emulation speed (`luna state -n N`, no GUI, full emulation
incl. APU):

| title | in-game emulated fps | × real-time |
|---|---|---|
| Doom (heaviest GSU) | **369 fps** | **6.15×** |
| Star Fox | 438 fps | 7.3× |
| Super Mario World | 452 fps | 7.5× |

Even with the GSU rendering 3D every frame, Doom emulates at **6× the 60 fps it
needs**. And `Emulator::render_frame_rgba(false)` — the GUI's per-frame draw — is
a plain framebuffer→RGBA copy (`snes.ppu.framebuffer()` iterate + alpha; ~224 KB,
~hundreds of µs), NOT a re-render. **So the GSU engine and the renderer are both
far from the bottleneck.** Optimising the GSU `run_one`/`step_coproc` hot path
would be wasted effort for the GUI lag.

## Where the lag actually is: the GUI runtime layer

`luna state` (the 6× measurement) renders only ONCE at the end. The GUI does
emulation + per-frame render + audio + egui present, on the audio-as-clock
pacing in `crates/luna-gui/src/emu_thread.rs`. The lag is in THAT layer. Ranked
candidates:

1. **Audio-as-clock pacing** (most likely). The emu thread drains audio into the
   cpal SPSC ring, `park_timeout(50ms)` when the ring is full (cpal unparks it on
   consumption). If the ring size / unpark cadence is off, or Doom's APU produces
   samples at a slightly different rate, the publish cadence becomes bursty →
   micro-stutter perceived as lag. Doom-specific because its APU/DSP load differs
   from Star Fox's (which the user reports smooth).
2. **Per-frame frame-time spikes.** 6× is the AVERAGE; a heavy-GSU frame (big
   render / DMA) could spike toward the 16.6 ms budget and drop. Needs a per-frame
   wall-time distribution, not just the average.
3. **egui / eframe present + vsync**, and the `framebuffer_rgba` mutex contention
   between the emu thread and the render thread.
4. **Audio underrun** (cf. [[project_pitchmod_spc700_crash]] — an SPC700 runaway
   freezes audio; a milder Doom stall would starve the ring → pacing hitch).

## The diagnostic only the user can run (no display here)

`emu_thread.rs` already prints to stderr every second:
`luna-emu: <N> batches/s, <M> samples/s, ring_full×<K> (last batch: <S> steps)`.
**Run Doom in the GUI and report that line.** It pins the bottleneck:
- `samples/s` ≈ 32040 → emu is keeping real-time (audio-paced); lag is render/
  vsync/present side.
- `samples/s` < 32040 → emu thread is being starved/throttled below real-time
  (despite the 6× core — points at pacing/park logic or a per-frame stall).
- high `ring_full×K` → emu OUTpacing audio (healthy); low/zero with low samples/s
  → ring starving (underrun) → stutter.

## Staged plan (oracle = GUI fps + the stderr stats + user feel)
1. **Measure (user):** capture the emu-thread stderr for Doom (and Star Fox as
   the smooth control). Decides which candidate.
2. **If pacing:** tune the ring size / replace the `park_timeout(50ms)` safety
   with a tighter condvar/cpal-unpark, or decouple frame publication from the
   audio drain so video cadence is vsync-steady even if audio bursts.
3. **If frame spikes:** profile per-frame wall-time (instrument a debug build, or
   `perf record` the GUI), find the spike (likely a periodic big DMA/GSU render),
   smooth or pipeline it.
4. **If present/vsync:** check eframe repaint requests + texture upload path;
   ensure the emu thread isn't blocked on the render thread's mutex.

## Effort / risk
Low-to-medium. This is GUI-runtime tuning, not an emulation rewrite — much more
tractable (and more *felt*) than the GSU cycle-timing frontier. The one blocker
is the diagnostic: it needs a GUI run (display), which the headless tooling here
can't do. Get the stderr stats first; the fix follows directly.

## Reference
- `crates/luna-gui/src/emu_thread.rs` — the audio-as-clock pacing loop.
- `crates/luna-gui/src/audio.rs` — cpal output + 6-point cubic Hermite resampler
  + 5 Hz DC blocker.
- `crates/luna-api/src/lib.rs::render_frame_rgba` — the cheap framebuffer copy.
