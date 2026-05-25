# luna APU — prioritized correctness gap list

Cross-referenced against `docs/apu_dsp_reference.md`. Each gap cites luna's current state + the reference site where the canonical behavior lives.

**Priority key**:
- **P0** — symptom-explaining; user-audible "wrong pitch + distortion + voice drops + wrong melody" comes from these.
- **P1** — distortion / drift over time, audible but secondary.
- **P2** — silent corruption / timing-sensitive cases, affects fewer drivers.
- **P3** — long tail (specific opcodes, edge cases, FIR clamp staging).

---

## P0 — fix these first; they explain the symptoms

### A1. Flat 4-cycles-per-opcode → per-opcode cycle table

**luna**: `crates/luna-cpu-spc700/src/opcodes.rs:20` charges 4 SPC cycles for every instruction. `crates/luna-apu/src/lib.rs:982` propagates the same 4 cycles to BOTH `tick_voices` (DSP sample tick) AND `tick_timers` (T0/T1/T2 = song tempo).

**Canonical** (`docs/apu_dsp_reference.md` §2): 256-entry cycle table, 2..8 cycles per opcode. ares `processor/spc700/instruction.cpp` and Mesen2 `Spc.Instructions.cpp:114-172` (`IncCycleCount`) have the same table.

**Symptom**: tempo + pitch desync together. Music driver thinks 4 cycles passed per instruction, real driver math expects 5-7. Net: **tempo runs ~30% wrong AND every voice's BRR-advance timing is wrong by the same factor**.

**Fix**: import the 256-entry cycle table from ares or Mesen2 verbatim, dispatch by opcode in `Cpu::step`. ~100-line patch + a Tom Harte test sweep for SPC700 (if such test data exists).

---

### A2. Pitch accumulator threshold `0x1000` → `0x8000` (8× too fast)

**luna**: `crates/luna-apu/src/lib.rs:486` — `while acc >= 0x1000 { advance BRR }` with `pitch & 0x3FFF` (14-bit) added per sample.

**Canonical** (`docs/apu_dsp_reference.md` §4): 16-bit accumulator, BRR-advance bit is **bit 15 (= 0x8000)**. Ares stores 0..0x7FFF in `gaussianOffset`, advances when overflow into bit 15. Mesen2 `DspVoice.cpp:198` equivalent: threshold `0x4000` with explicit halving.

**Symptom**: BRR samples advance 8× too fast → notes play **3 octaves above** intended pitch. Aliasing past Nyquist produces distortion that sounds like "fuzz" on every voice.

**Fix**: change threshold to `0x8000`, ensure pitch accumulator is u16 (currently extended to u32 in luna's code — that's fine), confirm gaussian phase index is computed from the correct bits.

---

### A3. ADSR rate-table indexing broken (unit mismatch + missing tables)

**luna**: `crates/luna-apu/src/lib.rs:880-940` — `voice_age % period < SPC_CYCLES_PER_SAMPLE` (compares SPC cycles to sample counts; wrong unit), hardcoded `-8/sample` Release that ignores rate table, attack indexing `(adsr1 & 0x0F) | 0x10` (wrong mapping).

**Canonical** (`docs/apu_dsp_reference.md` §6): 32-entry rate table + 32-entry offset table + global counter wrapping at `0x77FF`. An envelope rate "fires" when `(global_counter + OFFSET[rate]) % RATE[rate] == 0`. Attack uses `2*Ar+1` indexing. Release uses `0x1F` (rate 31).

**Symptom**: envelopes drop too fast (voice drops) OR sustain too long (notes hang). Combined with #A2 pitch bug, voices that should sustain through a measure cut off after a beat.

**Fix**: import the two 32-entry tables, replace `voice_age` accounting with global counter, fix attack mapping, replace `-8/sample` with proper rate-table Release path. ~150-line patch.

---

### A4. KON 5-sample delay missing

**luna**: KON write begins playback immediately (`lib.rs:576-588`).

**Canonical** (`docs/apu_dsp_reference.md` §4): on KON edge, `keyOnDelay = 5`. For samples 1-4 the voice is forced to silence (interpolation pos = `0x4000`, pitch=0). At sample 5 the BRR start address is loaded from the directory and real playback begins. Envelope starts Attack at sample 5.

**Symptom**: voices "click" on note onset (no smooth start), and the first sample read might be stale BRR data → noise burst at every key-on → "wrong sample" effect.

**Fix**: add `key_on_delay: [u8; 8]` to voice state, decrement per sample, gate BRR-advance + envelope on `delay == 0`. ~30-line patch.

---

## P1 — audible distortion, fix after P0

### A5. BRR decoder missing `>> 1` half-shifts + `(i16)(s << 1)` truncate

**luna**: `crates/luna-apu/src/lib.rs:760-796` — straight `nibble << range` for raw, straight `p1` / `p2` from history without halving, `clamp(-32768, 32767) as i16` (16-bit clamp).

**Canonical** (`docs/apu_dsp_reference.md` §5):
- After `raw = nibble << range`, do `raw >>= 1` (the famous BRR half-shift).
- `p1 = buffer[offset-1] >> 1` and `p2 = buffer[offset-2] >> 1` when reading history.
- After filter mixing, clamp to **15 bits** (-16384..+16383), NOT 16-bit.
- Final step: `s = (i16)(s << 1)` (truncating wrap, NOT saturating).
- Store the doubled value into the history buffer.

**Symptom**: predictor filter F2/F3 accumulate divergence over time. Long sustained notes get progressively distorted. Filter-1 transitions snap-pop.

**Fix**: 4-line addition to the BRR decoder + change buffer storage semantics. Add unit tests against known BRR test vectors if available.

---

### A6. ENDX double-buffer not modeled

**luna**: `ENDX` (`$7C`) is updated immediately when a voice hits end-of-block-with-end-flag (`lib.rs:373`).

**Canonical** (`docs/apu_dsp_reference.md` §9): ENDX is written at DSP cycle 29-30 of the macro pipeline. Music drivers polling ENDX during sample N see the previous sample's bits, not the current one. Some drivers depend on this 1-sample latency for voice-recycling logic.

**Symptom**: SMW (and other complex drivers) may see voices "end" too early, leading to spurious KOFFs on still-playing voices → voice drops mid-note.

**Fix**: add a shadow `endx_pending: u8`, copy to public ENDX once per sample at the right pipeline cycle. ~10-line patch.

---

### A7. Echo FIR clamp staging + scale

**luna**: `lib.rs:600+` `process_echo` — needs full audit against the canonical 3-stage clamp.

**Canonical** (`docs/apu_dsp_reference.md` §8):
- 8-tap FIR accumulator with **`>> 6`** per tap (NOT `>> 7`).
- History stored as `echo_in >> 1` (halved on write).
- Taps 0-5 accumulate without clamp.
- Tap 6 truncates via `int16` cast (allowed to wrap).
- Tap 7 clamps to `i16` saturating.
- Final result masked `& ~0x01` (bit-0 clear).

**Symptom**: echo feedback diverges over time → growing distortion that gets louder as the echo loop iterates. Particularly bad on games with deep reverb (Zelda 3, Chrono Trigger).

**Fix**: rewrite `process_echo` against the spec. ~50-line replacement.

---

## P2 — silent corruption / timing-sensitive

### A8. DSP voice pipeline not interleaved across 32 cycles

**luna**: `lib.rs:412-636` `tick_one_sample` processes voices 0..7 serially per sample.

**Canonical** (`docs/apu_dsp_reference.md` §3): each voice's `voice1..voice9` stages run at staggered cycles within the 32-cycle macro. Voice 0's pipeline literally spans into the next sample. PMON, ENDX, KON-latch, BRR-header-read all happen at specific cycles.

**Symptom**: PMON-modulated voices read the wrong source-voice output sample (chord glitching). KON latch timing differs by ~5 cycles from real hardware.

**Fix**: large rewrite — restructure the per-sample tick into a 32-cycle inner loop. ~500-line refactor. NOT in the P0 scope; defer until P0 + P1 land and we re-test.

---

### A9. Direct GAIN modes (4 variants) — verify each

**luna**: GAIN handling at `lib.rs:870-940` — coverage unclear, may only implement direct value.

**Canonical** (`docs/apu_dsp_reference.md` §6): when ADSR1 bit 7 = 0, GAIN register selects between:
- bits 7:5 = `000`: direct value (`GAIN & 0x7F` is envelope, no time evolution)
- bits 7:5 = `100`: linear decrease (env -= 32 each rate-table firing)
- bits 7:5 = `101`: exp decrease (env -= ((env-1) >> 8) + 1)
- bits 7:5 = `110`: linear increase (env += 32, clamp at 0x7FF)
- bits 7:5 = `111`: bent-line increase (faster slope below 0x600)

**Fix**: implement the 4 modes. ~40-line patch. Lower priority because most opensnes test ROMs use ADSR not GAIN.

---

### A10. Noise LFSR semantics

**luna**: `lib.rs:428-453` — verify against the canonical 15-bit Galois LFSR with seed `0x4000` and rate from `$6C FLG[4:0]`.

**Fix**: spot-check against ares' `dsp/noise.cpp` (or wherever).

---

## P3 — long tail, polish

### A11. SPC700 flag-handling edge cases
DAA/DAS half-carry behavior, ADC/SBC corner cases, DIV result quirks. Tom Harte's SPC700 test set is the canonical validator. Cross-check each against luna's `Cpu::step` once P0 lands.

### A12. Sample-directory mid-loop changes
Music driver writing to `$5D DIR` mid-playback should not affect already-loaded voices until they hit a loop. Verify luna handles this.

### A13. BRR `range >= 13` edge case
ares uses `(s >> 3) << 11` (preserve sign, drop magnitude). luna uses `-2048` / `0` which is close but not identical. Affects malformed BRR data; rare in well-formed samples.

---

## Recommended landing order

For maximum return on test-fixing-effort, land in this order:

1. **A1 (cycle table)** — fixes 80% of "wrong tempo" and gives downstream fixes a stable baseline.
2. **A2 (pitch threshold)** — instantly drops pitch to the right octave.
3. **A4 (KON delay)** — eliminates note-onset click.
4. **A3 (ADSR rate table)** — sustains/releases sound right.
5. **A5 (BRR shifts)** — predictor stability, less long-term distortion.
6. **A6 (ENDX double-buffer)** — fixes voice-drop bug.
7. **A7 (echo FIR)** — clean reverb.
8. P2/P3 as test ROMs surface them.

After A1+A2+A4 alone, the opensnes snesmod_music example should sound **mostly correct** (right notes, right tempo, audible). A3+A5 finish the job for clean sustains and no distortion drift.

## Implementation strategy proposal

A1, A2, A4 are 3 small independent patches (cycle table import + threshold change + key-on delay field). I recommend landing them as **3 separate commits** so each is bisectable and we can A/B-test the audio after each step.

A3 (ADSR) is bigger — 150 lines + new global counter state. Land as its own commit.

A5 (BRR shifts) is 4 lines but semantically tricky — land with unit tests.

A6 (ENDX) and A7 (echo) are isolated, land separately.

A8 (32-cycle pipeline interleaving) is the biggest, ~500 lines. Defer entirely until P0+P1 are validated audibly.
