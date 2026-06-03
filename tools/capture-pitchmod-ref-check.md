# Reference check: does PitchMod's SPC700 halt on real-accurate hardware?

luna's `spc_pitchmod` audio golden fails: the SPC700 plays the driver,
then runs away into a data region and **halts on a `$FF` (STOP) opcode at
PC `$FA42`**, freezing audio at **56620 of 96000 samples (~1.77 s of
~3 s)**. It regressed at commit `081e78d` (the Tom-Harte-correct SPC700
cycle fix): the driver runs **T0 (`$FD`) with reload = 1** and branches on
it, and the corrected cycles land a T0 read on the other side of a
128-cycle boundary (full chain in the `project_pitchmod_spc700_crash`
memory).

luna's SPC700 **cycles (Tom-Harte) and timer model (ares) both check out**,
so the open question is binary and only a known-accurate emulator settles
it:

| On Mesen2/bsnes (correct cycles) | Means |
|---|---|
| SPC700 **halts** (STOP/SLEEP), audio cuts ~1.7 s | PitchMod is a knife-edge fragile ROM → luna is correct → **ignore the golden** |
| SPC700 **never halts**, audio plays full ~3 s | luna has a real hidden bug → **REOPEN** (don't ignore) |

---

## Option A — Mesen2 (recommended, scripted)

1. Load **PitchMod.sfc** — the same ROM luna tests, at
   `<corpus>/SPC700/PitchMod/PitchMod.sfc` (the PeterLemon `SNES/SPC700`
   set). It's headerless homebrew; if Mesen2 mis-detects the mapper, force
   **LoROM**. Region is irrelevant (the SPC runs on its own 1.024 MHz
   clock), so NTSC is fine.
2. **Debug → Script Window → Open** → `tools/pitchmod-ref-check.lua` →
   **Run**.
3. **Power-cycle**, let it run **~5 seconds**, then **Stop**.
4. The script prints a **VERDICT** to the script log and writes
   `pitchmod_ref.log`:
   - `>>> SPC700 HALTED on STOP ($FF) at PC=$XXXX …` → fragile ROM, luna
     correct. (Bonus: if `PC=$FA42` it's the *exact* same halt as luna.)
   - `>>> ~3 s elapsed, SPC700 still running, NO halt …` → luna bug,
     reopen.

If a Lua field name errors, run once in the script window:
`for k,_ in pairs(emu.getState().spc) do emu.log(k) end` and adjust.

### Fallback: Mesen2 GUI, no Lua
- Open the **SPC Debugger** (Debug → Debugger, SPC tab). Run a few
  seconds. Does the SPC PC stay in the driver loops, or does it wander to
  `$FAxx` / stop? A **Break on** *unofficial/STOP* (or a breakpoint on
  exec of `$FA42`) firing = the halt reproduces.
- Or just **listen**: the pitch-modulation tone should sound continuously.
  If it cuts to silence around ~1.7 s, that matches luna's crash.

---

## Option B — bsnes-plus

bsnes-plus (`https://github.com/devinacker/bsnes-plus`): Tools → Debugger
→ SMP (SPC700) view. Run a few seconds; breakpoint on exec of `$FA42` (or
watch for the SMP PC entering `$FAxx`). If it breaks/halts and audio cuts,
the fragility reproduces.

---

## What to send back

The one line that settles it:
- **"SPC halted at $XXXX after N instrs, audio cut ~1.7 s"** → I apply
  `#[ignore]` to `spc_pitchmod` with the documented reason.
- **"played full ~3 s, SPC never halted"** → I reopen it as a real luna
  bug and keep digging (with the reference `pitchmod_ref.log` PC stream to
  diff against luna's — re-add the SPC PC tracer per the memory notes).

luna reference points to compare: halts at SPC PC **`$FA42`** (a `$FF`),
first timer divergence at SPC-instr **274093** (`$F090`, T0 read), audio
stops at **56620** samples.
