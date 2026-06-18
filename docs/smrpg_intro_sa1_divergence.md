# SMRPG intro deadlock — cross-emulator WRAM differential (OPEN)

**Status: OPEN.** A real, pre-existing luna bug (not caused by any recent
change). Super Mario RPG's no-input intro plays the early scenes, then —
after the "Mario leaves the house / jumps" scene — the screen goes
**forced-blank (black) with audio still playing** and never recovers.
Mesen2 plays further intro scenes from the same point. Reproduces headless
in luna (so it is debuggable without the GUI).

## Symptom (luna CLI)

At ~PPU frame 2137 the S-CPU enters a spin: `INIDISP=$8F` (forced blank),
`nmis_serviced` frozen at 1599 while `frame_count` keeps advancing and
`inidisp_write_count` climbs — i.e. the CPU is alive but stuck in a
wait-loop (WRAM `$7F:F7AC↔F7AF`, `Y=$305C` → polling SA-1 I-RAM), with NMI
disabled (`NMITIMEN=$01`). The SA-1 is **also** running (arithmetic
`$2250-$2254`, I-RAM result writes) — both CPUs do real work, but the
S-CPU's exit condition is never met → it's a **data divergence in the
SA-1↔S-CPU handshake**, not a simple freeze.

(NB: injecting Start early takes the New-Game path, which avoids this intro
deadlock — that is why the name-entry screen renders. The earlier
`project_smrpg_sa1_deadlock` "SMRPG works, it's the title-wait" note was
incomplete: the no-input intro genuinely deadlocks.)

## Differential result (THE method)

Tooling (committed):
- `luna wram-trace` → per-frame FNV-1a hashes of 32×4 KiB WRAM pages.
- `tools/mesen-wram-hash.lua` → byte-exact Mesen2 reference (run headless:
  `Mesen --testRunner --snes.ramPowerOnState=AllZeros <lua> <rom>`).
  **`--snes.ramPowerOnState=AllZeros` is mandatory** — luna zeros WRAM at
  power-on; Mesen2 defaults to Random, which otherwise makes every
  not-yet-written page mismatch.
- `tools/diff-wram-hashes.py` → auto-aligns the boot-frame offset
  (page-level scoring) and reports the first divergent frame + pages.
- `tools/mesen-wram-dump.lua` → raw 128 KiB WRAM dump at chosen frames,
  to pair with `luna wram-trace --dump-frame N` for byte-level diffing.

Findings (offset 0, no input, USA ROM):
- **Frames 1–21:** only WRAM page 1 (`$7E1000`) differs — a *transient*
  scratch difference that **re-converges: frame 23 is byte-identical**.
- **Frame 24 = first real divergence** (19 bytes, then it cascades and
  never recovers). Differing bytes (luna vs Mesen):
  - `$7E:0070=00/03`, `$7E:0072=00/06` — produced by an `MVN $7E,$7E`
    block-propagate at PC `$C3:0310`; luna's MVN **source region
    (`$7E:0066+`) is all `00`**, Mesen's holds real values → the MVN is the
    symptom; the seed is set wrong/earlier.
  - `$7E:1D00=00/24`, `$7E:1DA8=00/24`, `$7E:1FE9-$7E:1FF8` (pointer-ish,
    incl. bank `$7E`) — **never written by luna** in this window → a code
    path Mesen executes but luna skips.
- No clean 1-frame timing slip at the onset (neighbour frames don't align
  better), so it's a genuine data divergence, not a cadence offset.

## Next dichotomy step

Frame 23 WRAM is byte-identical in both, so the divergence is produced
entirely within the frame-23→24 game step. The root is the **first
differing input** read during that step — almost certainly an SA-1 result
(register `$2300`/`$2306-9` or I-RAM `$30xx`) that luna returns differently,
causing the S-CPU to branch differently (skip the `$7E:1D00` writer, seed
the MVN with zeros). Pin it with an SA-1 register-read / instruction-trace
differential vs Mesen2 in the narrow frame-23→24 window, then read the ares
SA-1 reference at exactly that op and translate faithfully.
