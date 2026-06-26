# SNES APU (SPC700 + S-DSP) — reference spec

luna's faithful-port reference for the SNES audio subsystem (SPC700 CPU +
S-DSP), describing the hardware-accurate behaviour each component models.

---

## 1. Clocks

- **SNES master clock**: 21.477 272 MHz NTSC (21.281 370 MHz PAL).
- **SPC700 clock**: 1.024 MHz = master / ~21. Each SPC cycle = ~21 master cycles.
- **DSP sample rate**: SPC_clock / 32 = **32 000 Hz** (one output sample emitted every 32 SPC cycles).
- The DSP runs a **32-cycle macro pipeline** that emits one stereo sample.

## 2. SPC700 opcode cycle counts — NOT FLAT

The SPC700 has 256 opcodes with cycle costs ranging **2..8 cycles**. They are **NOT** uniform. Music drivers depend on accurate cycle counts because:
- T0/T1/T2 timers (= song tempo) increment based on elapsed SPC cycles.
- The DSP sample tick fires every 32 SPC cycles → pitch sounds wrong if cycle accounting is off.

The hardware uses per-opcode cycle costs, charged per-instruction in the dispatcher.

**Examples** (selected):
| Opcode | Mnemonic | Cycles |
|---|---|---|
| `$00` | NOP | 2 |
| `$8F` | MOV dp, #imm | 5 |
| `$3F` | CALL !abs | 8 |
| `$6F` | RET | 5 |
| `$01..F1` | TCALL n | 8 |
| `$03..F3` | BBS/BBC | 5 (+2 if branch taken) |
| `$CF` | MUL YA | 9 |
| `$9E` | DIV YA, X | 12 |
| `$DF` | DAA | 3 |

The full table must be wired from a canonical source.

**luna status: DONE.** A per-opcode table holds the real 2..12 costs
(not flat). The SPC700 dispatcher charges the per-opcode cost for each
instruction, and the APU feeds the returned per-instruction cost straight
into the timer and voice ticking — so timer tempo and DSP sample rate
track real cycle counts. The old "flat 4 cycles" approximation is gone.

**Branch-taken `+2` penalty: also DONE.** A branch-taken penalty of 2
cycles is added when a branch is taken (BRA / Bcc / CBNE / DBNZ / BBS /
BBC set the branch-taken flag at the branch sites). The base table lists
the *not-taken* cost; the taken idle is added on top. This was the last
open SPC700 cycle gap and it is now closed.

## 3. SPC700 ↔ DSP timing

After every opcode the DSP is synchronized: it catches up by running its
32-cycle pipeline however many times the elapsed SPC cycles cover.

DSP pipeline organization:
- 32 DSP cycles per output sample.
- The voice pipeline stages are **interleaved across the 32 cycles**.
- Voice 0's pipeline runs at cycles {0, 1, 2, 21, 24, 29, 30, ...} — it literally spans into the next sample.
- Voice 7's pipeline runs at staggered cycles in the same window.

**Consequence**: a per-sample loop that processes voices serially (`for v in 0..8 { ... }`) gets the wrong inter-voice timing for:
- **PMON** (pitch modulation): voice N reads voice N-1's `latch.output` from a specific prior cycle.
- **ENDX**: the bit-clear-then-bit-set sequence happens at cycles 29-30.
- **KON latch**: the 63-clock KON delay reads `dsp_regs[0x4C] KON` and clears via the `_keylatch` mechanism.

## 4. DSP pitch counter — the canonical interpolation pipeline

Each voice has a **16-bit pitch accumulator** (the `gaussianOffset` / interpolation-position register).

### Per DSP sample:
```
gaussianOffset += pitch       // pitch = 14-bit register, 0..0x3FFF
if gaussianOffset >= 0x8000:  // BRR-advance bit
    decode next BRR sample
    push into circular buffer
    gaussianOffset -= 0x4000   // or &= 0x3FFF + carry bit handling
```

The **BRR advance threshold is 0x8000** (= bit 15). The low 12 bits form the gaussian interpolation phase (256-entry table indexed by `(gaussianOffset >> 4) & 0xFF` essentially).

**luna status: CORRECT.** The live DSP follows the hardware-reference
formulation, where `gaussian_offset` is masked to `0x3FFF` and the
BRR-advance test is `>= 0x4000`. The voice pipeline decodes the next BRR
group when `gaussian_offset >= 0x4000`, then advances
`(gaussian_offset & 0x3FFF) + pitch`. This is one equivalent form of the
`>= 0x8000`/`-= 0x4000` formulation above (both are equivalent: the
BRR-advance bit is carried at 0x4000 over a 0x3FFF-masked accumulator).

### KON 5-sample delay

On a KON edge for voice V:
- `keyOnDelay = 5` (countdown)
- For the next 4 samples: force `interpolationPos = 0x4000` (or `gaussianOffset = 0`), force `pitch = 0` in the BRR-advance step → no BRR decode happens, voice plays interpolated silence
- At `keyOnDelay == 0`: load BRR start address from directory, begin real playback
- Envelope: stays at 0 during the delay, enters Attack at sample 5

Without this, voices "click" at note onset and the BRR address gets read with stale state → wrong sample played first.

## 5. BRR sample decoder

BRR (Bit Rate Reduction) block layout: **9 bytes** per block = 1 header + 8 data bytes (16 × 4-bit nibbles).

Header byte:
- bits 7:4 = **range** (shift amount 0..12; values 13-15 are clamped specially)
- bits 3:2 = **filter** (0..3)
- bit 1 = **loop**
- bit 0 = **end**

### Decode formula
```
nibble = signed_4bit(extract from data byte)
if range <= 12:
    raw = (nibble << range) >> 1     // ← THE HALF-SHIFT
else:
    raw = (nibble >> 3) << 11         // sign-preserve + drop magnitude
                                       // (keeps sign, drops the low 11 bits)

p1 = buffer[offset-1] >> 1   // ← FIVE !! the previous samples are read HALVED
p2 = buffer[offset-2] >> 1

match filter:
  0: s = raw
  1: s = raw + p1 + ((-p1) >> 4)
  2: s = raw + p1 * 2 + ((-p1 * 3) >> 5) - p2 + (p2 >> 4)
  3: s = raw + p1 * 2 + ((-p1 * 13) >> 6) - p2 + ((p2 * 3) >> 4)

s = clamp15(s)             // 15-bit clamp (NOT 16-bit)
s = (i16)(s << 1)          // ← FINAL SHIFT-LEFT with i16 wrap (not saturate!)
buffer[offset] = s          // store as doubled, will be halved on next read
```

**luna status: CORRECT.** The BRR decoder implements all three:
1. The post-decode half-shift: `s <<= scale; s >>= 1`, with the
   `scale > 12` clamp path `s &= !0x7FF`.
2. The history half-shift on `p2` (`>> 1`) and the filter-internal
   `p1 >> 1` (e.g. filter 1) — matching the hardware BRR decoder.
3. The final wrap-truncate: `(s << 1) as i16` after the 15-bit clamp.

The buffer holds **12 samples** (4 per BRR row × 3 most recent rows) so the 4-tap gaussian can read across 2 row boundaries.

## 6. ADSR envelope

### Rate table (32 entries)
The exact rate table:
```
const RATE_TABLE: [u16; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512,
    384,  320,  256,  192,  160, 128,  96,  80,
     64,   48,   40,   32,   24,  20,  16,  12,
     10,    8,    6,    5,    4,   3,   2,   1
];
```

### Counter offsets table (32 entries)
```
const OFFSET_TABLE: [u16; 32] = [
    1, 0, 1040, 536, 0, 1040, 536, 0,
    1040, 536, 0, 1040, 536, 0, 1040, 536,
    0, 1040, 536, 0, 1040, 536, 0, 1040,
    536, 0, 1040, 536, 0, 1040, 0, 0
];
```

### Counter mechanism
The DSP runs a global counter that **wraps at 0x77FF** (= 30719). Each DSP sample increments it by 1. An envelope rate is "active" when:
```
(global_counter + OFFSET_TABLE[rate]) % RATE_TABLE[rate] == 0
```
When active, the envelope steps according to its current phase:
- **Attack**: env += (Ar == 31) ? 1024 : 32
- **Decay**: env -= ((env - 1) >> 8) + 1
- **Sustain**: same as Decay
- **Release**: env -= 8 (fast-release / forced-release path)
- **Direct gain**: per the 4 gain modes (lin+, lin-, exp+, exp-)

**luna status: CORRECT.** The live envelope is a faithful hardware port:
- Global counter polling uses the counter-rate / counter-offset tables —
  the `(counter + OFFSET[rate]) % RATE[rate] == 0` test, exactly the
  mechanism in §6 above.
- Attack uses `2*Ar+1`, and Decay uses `2*Dr+16`.
- All four phases + the four GAIN modes are implemented with the
  rate-gated step.

## 7. Gaussian interpolation

The canonical 256-entry × 4-section table (1024 entries total) is built from `sin(pi*k*1.28/1024) * ((cos(pi*k*2/1023)-1)*0.5 + (cos(pi*k*4/1023)-1)*0.08 + 1) / k`, normalised so each 4-tap group sums to 2048.

Per sample:
```
frac = (gaussianOffset >> 4) & 0xFF   // 0..255
s = (TABLE[0x0FF - frac] * sample_3) >> 11
s += (TABLE[0x1FF - frac] * sample_2) >> 11
s += (TABLE[0x100 + frac] * sample_1) >> 11
s = (i16)s                              // ← partial-sum wrap quirk
s = saturating_add(s, (TABLE[frac] * sample_0) >> 11)
s = clamp15(s) & ~1                     // bit-0 clear
```

luna implements this in the live DSP — the 3-tap `>>11` accumulate, the
`(output as i16)` partial-sum wrap, and the final `clamp16(output) & !1`.
**Correct**, matching the hardware gaussian interpolation. ✓

## 8. Echo (8-tap FIR + delay line)

DSP registers:
- `$2D EON` — per-voice echo-input enable
- `$4D EFB` — echo feedback (signed 8-bit)
- `$5D` — used by both DIR (sample directory page) AND `EDL` (echo delay length: low nibble × 2048 bytes)
- `$6D ESA` — echo start address page
- `$7D EDL` — echo delay length (low 4 bits)
- `$0C MVOLL`, `$1C MVOLR`, `$2C EVOLL`, `$3C EVOLR` — main and echo volumes
- `$0F..$7F FIR coefficients` (8 of them, signed 8-bit)

Per sample:
```
echo_in_l = read 16-bit signed from APURAM[ESA*256 + echo_offset*4 + 0..1]
echo_in_r = read 16-bit signed from APURAM[ESA*256 + echo_offset*4 + 2..3]

// Push into 8-stage history (history stored HALVED: history[i] = echo_in >> 1)
// 8-tap FIR with the standard coefficient set, accumulator >> 6 per tap

// 3-stage clamp protocol:
//   taps 0..5 — accumulate freely
//   tap 6 — truncate via int16 cast (allowed to wrap)
//   tap 7 — clamp16
// final: result & ~0x01 (bit-0 clear)

// Feedback: write (fir_l + echo_in_l*EFB/128) back to APURAM (gated by !FLG.ECEN)
// Output: voice_sum_l + (fir_l * EVOLL) >> 7  →  apply MVOLL  →  clamp/output
```

**luna status: CORRECT.** The live echo path is a faithful hardware
port: history is stored halved (`s >> 1`), the FIR does the `>> 6`
per-tap, and the staged clamp protocol is implemented — taps 0..5
accumulate freely, tap 6 truncates via an `i16` cast, tap 7 clamps to
16 bits and clears bit 0 (`& !1`). The feedback write is gated by the
echo read-only flag.

## 9. KON / KOFF / ENDX (double-buffered)

- `$4C KON` — write 1-bit per voice to key ON. Latched at the end of the 32-cycle macro pipeline.
- `$5C KOFF` — same for key OFF.
- `$7C ENDX` — read-only mirror of "voice hit end-of-BRR-block-with-end-bit-set". Double-buffered: written at cycle 29-30, becomes visible the NEXT sample. Music drivers poll ENDX to know when a voice has completed.

**luna status: CORRECT.** The live DSP implements the full 5-step KON
delay and the ENDX timing:
- On a KON edge at the sample boundary, `keyon_delay = 5` and the mode
  enters Attack. During the countdown the envelope is forced to 0,
  `gaussian_offset` is held at `0x4000` for the interpolated-silence
  samples and 0 on the load sample, and real playback (BRR start-address
  load from the directory) begins at delay 5 → 0.
- ENDX is the per-voice end bit OR'd into the `$7C` register, with the
  cycle-29/30 staging emulated by the pipeline split (the end bit is set
  from the looped flag and cleared when `keyon_delay == 5`). This is the
  hardware ENDX double-buffer behaviour, not a synchronous shortcut.

## 10. Reset state

DSP register reset:
- `$6C FLG = 0xE0` (RESET + MUTE + ECEN flags all set)
- All other registers undefined per real hardware but most emulators zero them
- KON = 0, KOF = 0, ENDX = 0

SPC700 reset:
- PC loaded from `$FFFE-$FFFF` (in IPL ROM at boot, then in RAM after)
- A = X = Y = 0
- SP = 0xEF (or whatever IPL boot left it at)
- PSW = 0x02 (Z flag set)

## 11. Initialization sequence

The IPL ROM at `$FFC0-$FFFF` is the SPC700's boot code. On reset:
1. SPC700 starts executing at `$FFC0`.
2. Writes `$AA` to `$F4` (port 0) and `$BB` to `$F5` (port 1) — signals to main CPU "I'm ready, send me code".
3. Polls `$F4` for a non-zero value (the main CPU's protocol command).
4. When the protocol is satisfied, the SPC jumps to wherever the main CPU's upload says.

This handshake is **always** the first ~700 cycles after reset. luna's APU reaches this state cleanly (confirmed by mailbox tracer earlier: `$AA/$BB/$CC` ack at mclk 60656/60720/61824).
