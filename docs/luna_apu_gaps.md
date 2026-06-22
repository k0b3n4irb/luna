# luna APU / audio subsystem — correctness gaps vs ares

Reference-first audit of luna's APU against ares (`ares/sfc/smp/*`,
`ares/sfc/dsp/*`). Companion to `luna_bg_gaps.md` / `luna_obj_gaps.md`.

Scope: the **SMP glue** (`crates/luna-apu/src/lib.rs` — `$00F0-$00FF`
control regs, timers, the `$2140-43`↔`$F4-F7` mailbox), the **S-DSP**
(`crates/luna-apu/src/dsp.rs`), and the SPC700 core
(`crates/luna-cpu-spc700`).

> **History:** this file previously listed gaps A1-A13 (flat opcode
> cycles, in-line pitch/ADSR/BRR/echo bugs). All of those were resolved
> by the migration to the cycle-accurate `dsp.rs` ares port and the
> per-opcode `cycles.rs` table — they referenced code that no longer
> exists. This is a fresh audit of the **current** code. (Re-authored
> 2026-05-30.)

The DSP is now a deliberate 1-for-1 ares transliteration, so the audit
weights the hand-written SMP glue, where the real findings are.

## Severity legend

- 🔴 real bug, can audibly misbehave
- 🟠 feature missing
- 🟡 precision / cleanup

---

## ✅ 1. ENDX (`$7C`) wrongly cleared on `$F3` read — DONE

ares `DSP::read` (`dsp/memory.cpp:1-3`) has **no side effects** — ENDX
is cleared only by a *write* to `$7C` (`memory.cpp:34-37`) or by KON
re-keying a voice (`voice.cpp:146`). luna used to clear `registers[0x7C]`
whenever the SPC read `$F3` with the index at `$7C`, which lost the
end-of-sample bits on a second read.

**Fixed**: the side-effecting clear is gone — DSP reads no longer mutate
ENDX (`lib.rs` ~889 comment "ENDX (`$7C`) is cleared only by a write to
`$7C` or by KON"), and `dsp.rs` maintains it (write-clear, KON-clear,
per-sample `_end` reflection). Regression tests: "ENDX must survive
repeated reads" and "write to `$7C` clears ENDX" (`lib.rs` ~1126/1130).

---

## ✅ 2. `$F1` bits 4/5 — clear the input mailbox ports — DONE

ares `smp/io.cpp:113-123`: a `$F1` write with bit 4 set zeroes the
CPU→SMP ports 0/1 (what the SPC reads at `$F4/$F5`); bit 5 zeroes ports
2/3. Implemented in the `$F1` handler (the bus view's `to_spc_ports`
was made writable). Test `control_bits_4_5_clear_input_ports`.

## 🟢 3. `$F0` TEST register — timer gating DONE (wait-state bits → #6)

ares `smp/io.cpp:81-94`: `$F0` carries `timersDisable` (bit 0),
`ramWritable` (1), `ramDisable` (2), `timersEnable` (3), and the
external/internal wait-state dividers (bits 4-7), gated on the P flag.

**Done**: the `$F0` value is stored (`test`, reset `0x0A` = the ares
power-on `timersEnable`+`ramWritable` state) and the **timer gating** is
modelled — `tick_timers` freezes when `timersEnable` is clear or
`timersDisable` is set (ares `timing.cpp:45-49`), with the clock divider
still running so phase resumes on re-enable. Test
`test_register_gates_timer_advance`.

**Remaining** (folds into #6): the wait-state dividers (bits 4-7) and
`ramWritable`/`ramDisable` (bits 1-2) are stored but not acted on; the
P-flag write gate is omitted (pathological for `$F0`).

## ✅ 4. IPL ROM overlay (not baked into ARAM) — DONE

ares maps the 64-byte IPL ROM as a separate overlay over
`$FFC0-$FFFF`, gated by `$F1` bit 7 (`io.iplromEnable`). luna used to
copy the IPL bytes *into* ARAM at reset. Now ARAM is physical RAM only
and the IPL ROM is a read overlay (`aram_with_ipl`) applied on the SPC
bus path; the DSP reads physical ARAM directly (it bypasses the
overlay, matching hardware — and fixing a latent bug where a sample at
`$FFC0` would have read IPL bytes). Clearing bit 7 now exposes the
underlying RAM. Tests `ipl_rom_overlay_toggles_with_f1_bit7`,
`new_resets_spc_into_ipl_rom` (+ boot handshake unchanged).

---

## 🟡 Precision / cleanup

| # | Issue | ares ref | luna |
|---|---|---|---|
| ~~5~~ | ~~`$F8/$F9` AUXIO read returns 0~~ — **DONE**: read returns the stored value (`auxio_f8_f9_read_back_written_value`) | `smp/io.cpp:49-53` | ✅ |
| ~~6~~ | ~~Wait-state cycle dividers `{2,4,10,20}` (and the 8/16→10/20 glitch) not modelled~~ — **DONE**: `clock_cycle` picks the per-access wait-state (`$F0` external bits 4-5 / internal bits 6-7; internal for idle/`$00Fx`/IPLROM) and advances `spc_pos_2x` by `cycleWaitStates {2,4,10,20}`, the timers by `{2,4,8,16}` (the 8/16→10/20 clock glitch the timers escape), and the DSP sample by the clock divider. ws=0 byte-identical (24 APU tests + SPC differential + 58 goldens); `wait_states_divide_the_spc_clock` proves ws=1≈½ / ws=3≈⅒. | `smp/timing.cpp:9-20` | ✅ |
| ~~7~~ | ~~Dead legacy DSP scaffolding in `lib.rs`~~ — **DONE**: removed the duplicate gaussian table + unused `ADSR_RATE_PERIODS` / `COUNTER_OFFSET` / `COUNTER_RELOAD` / `VOICE_END_SPC_CYCLES` / `AdsrPhase`; refreshed the stale module docs | — | ✅ |

---

## ✅ Verified correct (do not regress)

- **DSP port (`dsp.rs`)** is a faithful ares transliteration:
  - Gaussian table is an **exact** match to `dsp/gaussian.cpp` (formula,
    2048-normalisation, mirrored indexing) and the 4-tap interpolation.
  - ENDX write-clear + KON-clear + per-sample `_end` reflection.
  - Struct layout mirrors ares (Voice / Echo / Noise / BRR / Latch /
    MainVol / Clock); the KON 5-clock delay machine is present.
- **Per-opcode cycle table** (`cycles.rs`) replaced the old flat-4
  approximation (former gap A1).
- **Timer rates**: T0/T1 @ 8 kHz (128 SPC cycles), T2 @ 64 kHz (16
  cycles); reload 0 = 256; output clears on read (`$FD-$FF`); enable
  0→1 resets the counter — all matching ares once the stage-1 toggle is
  folded in.
- **Mailbox** direction model (`to_spc`/`to_cpu`), `$F2` DSPADDR
  read-back, `$F3` index masking (`& 0x7F` mirror region).
- **SPC700 core** — semantically audited against ares (ALU, DAA/DAS/
  DIV/MUL exact; see `luna_spc700_gaps.md`), AND covered by a
  SingleStepTests / Tom Harte harness (`crates/luna-cpu-spc700/tests/
  tom_harte.rs`) plus the PeterLemon SPC700 ALU hardware tests, validated
  by their `$2140` mailbox memory-result protocol
  (`spc700_*` in `crates/luna-core/tests/snes_test_roms.rs`). (The earlier
  "no Tom Harte test" note here was stale.)

## Suggested order

1. ~~#1 ENDX read-clear~~ — **done** (`036f65f`).
2. ~~#7 dead scaffolding~~, ~~#2 `$F1` port-clear~~, ~~#5 `$F8/$F9`~~ —
   **done**.
3. ~~#4 IPL overlay~~ — **done**.
4. ~~#3 `$F0` timer gating~~ — **done** (timersEnable/Disable).
5. 🟡 #6 wait-state timing (incl. the `$F0` wait-state + RAM bits) —
   approximation, lowest priority; a timing-model refactor for low
   real-world return (few drivers deviate from the default wait states).
