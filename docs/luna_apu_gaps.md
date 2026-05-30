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

## 🔴 1. ENDX (`$7C`) wrongly cleared on `$F3` read

ares `DSP::read` (`dsp/memory.cpp:1-3`) has **no side effects** — ENDX
is cleared only by a *write* to `$7C` (`memory.cpp:34-37`) or by KON
re-keying a voice (`voice.cpp:146`). luna (`lib.rs:477-482`) clears
`registers[0x7C]` whenever the SPC reads `$F3` with the index at `$7C`:

```rust
if idx == 0x7C { self.dsp.registers[0x7C] = 0; }
```

A driver that reads ENDX more than once (or in two code paths) loses
the end-of-sample bits after the first read, so one-shot samples and
sample-end synchronisation can be missed. **The headline fix** — and
safe to remove, since `dsp.rs` already maintains ENDX correctly
(write-clear at `dsp.rs:384-388`, KON-clear, per-sample `_end`
reflection at `dsp.rs:769-776`).

---

## ✅ 2. `$F1` bits 4/5 — clear the input mailbox ports — DONE

ares `smp/io.cpp:113-123`: a `$F1` write with bit 4 set zeroes the
CPU→SMP ports 0/1 (what the SPC reads at `$F4/$F5`); bit 5 zeroes ports
2/3. Implemented in the `$F1` handler (the bus view's `to_spc_ports`
was made writable). Test `control_bits_4_5_clear_input_ports`.

## 🟠 3. `$F0` TEST register not modelled

ares `smp/io.cpp:81-94`: `$F0` carries `timersDisable` (bit 0),
`ramWritable` (1), `ramDisable` (2), `timersEnable` (3), and the
external/internal wait-state dividers (bits 4-7), and writes are gated
on the P flag. luna drops `$F0` writes entirely (`lib.rs:512`), so timer
enable/disable gating and wait-state timing are not honoured.

## 🟠 4. IPL ROM baked into ARAM — `$F1` bit 7 toggle is a no-op

ares maps the 64-byte IPL ROM as a separate overlay over
`$FFC0-$FFFF`, gated by `$F1` bit 7 (`io.iplromEnable`). luna copies the
IPL bytes *into* ARAM at reset (`lib.rs:256-258`), so (a) toggling bit 7
off doesn't expose the underlying RAM, and (b) that RAM is permanently
shadowed by the IPL image. Games that reclaim `$FFC0-$FFFF` as RAM
after boot read IPL bytes instead.

---

## 🟡 Precision / cleanup

| # | Issue | ares ref | luna |
|---|---|---|---|
| ~~5~~ | ~~`$F8/$F9` AUXIO read returns 0~~ — **DONE**: read returns the stored value (`auxio_f8_f9_read_back_written_value`) | `smp/io.cpp:49-53` | ✅ |
| 6 | Wait-state cycle dividers `{2,4,10,20}` (and the 8/16→10/20 glitch) not modelled; fixed master:SPC ratio used instead | `smp/timing.cpp:9-20` | `lib.rs` step() converts at a fixed ratio + per-opcode cost |
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
- **SPC700 core** validated against the Tom Harte SPC700 test suite
  (`crates/luna-cpu-spc700/tests/tom_harte.rs`).

## Suggested order

1. ~~#1 ENDX read-clear~~ — **done** (`cd3d934`).
2. ~~#7 dead scaffolding~~, ~~#2 `$F1` port-clear~~, ~~#5 `$F8/$F9`~~ —
   **done**.
3. **#3 `$F0` TEST**, **#4 IPL overlay** — larger (touch timing / the
   memory map); remaining 🟠.
4. 🟡 #6 wait-state timing — approximation, lowest priority.
