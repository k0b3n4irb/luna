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

## DIAGNOSIS (2026-06-09, from the user's GUI stderr) — it's the video present

Doom GUI stats: `samples/s ≈ 32770` rock-steady, `ring_full×24` constant, `batches/s`
swinging 680↔2654. Reading:
- **Emulation is CLEARED.** Steady 32770 samples/s = real-time SNES audio; the emu
  keeps up. `ring_full×24` = the emu OUTpaces audio and is paced down (healthy 6×
  headroom). The `batches/s` 4× swing is NOT a slowdown — it's the instruction
  mix: at constant cycles/sec, cheap spin-loops (e.g. a vblank/GSU wait) execute
  far more instr/sec than heavy code. A red herring.
- **The lag is the video present, and it is NOT frame-synced.** `main.rs`:
  `ControlFlow::Poll` (659) busy-spins; `about_to_wait` (486) calls
  `request_redraw()` every iteration (the "~16 ms" comment is wrong); `pixels`
  presents vsync-limited (Fifo default). So the render presents at the display's
  60 Hz reading `framebuffer_rgba`, while the emu publishes frames at its own
  audio-paced 60 Hz. **Two unsynchronised 60 Hz clocks → phase drift → a frame
  shown twice / skipped every few seconds → judder.** Same path for all games,
  but vastly more visible in Doom's continuous 3D panning than Star Fox's
  discrete motion — exactly the reported "Doom laggy, Star Fox good".
- Secondary: `ControlFlow::Poll` busy-spins the main thread (wastes a core; can
  contend with the emu thread).

## Fix plan (ranked, oracle = smooth Doom panning in GUI)
1. **Frame-sync the present to the emu's frame production** (highest value). The
   emu thread signals "new frame ready" (e.g. an `AtomicU64` frame counter +
   `window.request_redraw()` from the publish site, or a small triple-buffer);
   the render presents a NEW emu frame each vsync, no double/skip. Switch
   `ControlFlow::Poll`→`Wait` and drive redraws from the new-frame signal +
   window events — kills the busy-spin AND the judder.
2. **Dynamic rate control** (if 1 leaves residual drift): nudge the audio
   resample ratio slightly so the emu's 60 Hz locks to the display's vsync
   (standard emulator A/V-sync technique). The resampler already exists
   (`audio.rs`, 6-point Hermite).
3. **Triple-buffer the framebuffer** to drop the `framebuffer_rgba` Mutex from the
   present path (avoid emu↔render lock contention / tearing).

## Effort / risk
Low-to-medium, HIGH felt impact. This is GUI-runtime tuning (present sync), not an
emulation rewrite — far more tractable and more *felt* than the GSU cycle-timing
frontier, and it improves EVERY game's smoothness. Fix (1) alone likely resolves
the reported lag. Perception-affecting → GUI-validate per `audible-fixes-test-first`.

## Reference
- `crates/luna-gui/src/emu_thread.rs` — the audio-as-clock pacing loop.
- `crates/luna-gui/src/audio.rs` — cpal output + 6-point cubic Hermite resampler
  + 5 Hz DC blocker.
- `crates/luna-api/src/lib.rs::render_frame_rgba` — the cheap framebuffer copy.

## CORRECTION (2026-06-09) — the present-cadence diagnosis was WRONG

The "two unsynchronised 60 Hz clocks → judder" hypothesis above was TESTED and
REFUTED. A present-on-new-frame change (emu bumps a `frame_seq`, GUI requests a
redraw only when it advances) was implemented and GUI-validated: **no visible
change** on Doom. It is a *no-op* for appearance because the old code already
presented the latest framebuffer at vsync — the new code just does fewer
`request_redraw` calls. So the judder is **not** in the present cadence. Reverted.

What is actually established (solid):
- luna emulates Doom at real-time (audio 32770 samples/s steady, 6× headless
  headroom) — NOT slow at the emulation/core level.
- The present path is correct (latest framebuffer @ vsync).
- Doom's top/bottom **border rows flicker** (luna-specific; Mesen renders them
  constant black) — the INIDISP forced-blank scanline JITTER from the CPU/GSU
  cycle-timing imprecision (see `av_sync`/cooperative-scheduler notes). At 60 Hz
  in continuous motion this reads as judder.
- Star Fox is smooth on the SAME present/pacing path.

NOT cleanly measured (headless): Doom's in-game 3D framerate vs Mesen — every
attempt was confounded by a static attract scene or scripted input that didn't
hold motion. The open dichotomy is therefore: is the "laggy" (a) the border
flicker (→ cycle-timing/GSU frontier, or an overscan-crop mitigation), or (b) a
genuinely lower 3D framerate than Mesen (→ luna's GSU rendering Doom slower)?
Needs either a continuous-motion capture or the user's direct observation.
The lag is content-level, NOT the GUI present layer — do not patch the present.
