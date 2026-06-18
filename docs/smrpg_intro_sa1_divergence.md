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

## REFRAMED (2026-06-18): not SA-1 — it's the Akao CPU↔SPC700 handshake

After fixing the peek bug (below) and re-running with reliable tools, the
register-read differential over the frame-23→24 step (luna vs Mesen2) shows:

- `$2300` CFR (SA-1 status): both read `00` — **match**. SA-1 exonerated.
- SA-1 I-RAM: byte-identical at frames 23 and 24. SA-1 exonerated.
- The dominant differing input is **`$2140` (APU port 0)** — read ~12k times
  in a tight spin at bank **`$C4`** (`$C4:0605/07DC/07AA` = the **Akao sound
  driver**), with luna's vs Mesen's value distributions differing. The S-CPU
  writes `$2140` an incrementing `$40..$7F` sequence — the Akao data-upload
  handshake to the SPC700.

The 19-byte frame-24 WRAM divergence (`$7E:0070=03`, `$7E:0072=06`,
`$7E:1D00=24`, `$7E:1DA8=24`, `$7E:1FE9-F8`) is written by game code
(`$C3:xxxx`) down a branch that runs in Mesen but **not** luna — gated,
transitively, on the Akao handshake state. SMRPG uses Square's **Akao**
driver — the exact family the cycle-accuracy plan names as the motivating
bug ("Chrono Trigger audio deadlock — Akao's timing-coupled CPU↔SPC
handshake deadlocks under luna"). So this is an **APU/SPC700 cycle-timing**
issue, not an SA-1 one.

### APU/SPC700 handshake differential (done): SPC700 writes port 3 = 0, not 2

Captured the full `$2140-$2143` exchange from boot in both emulators
(luna mem-trace; Mesen Lua read/write callbacks) and compared the collapsed
value sequences. **They match exactly for 25,359 collapsed accesses**, then
diverge: Mesen reads `$2143 = 02` (its SPC700 wrote port 3) and proceeds;
**luna keeps reading `$2143 = 00` forever** → the S-CPU spins → the freeze.

So the SPC700 echo VALUES are correct through the whole Akao upload; the
divergence is the post-upload handshake. luna's SPC700 is running (PC cycles
`$0301`→`$0307`), `to_cpu_ports=[0,0,0,0]`. Disassembling luna's ARAM:

```
$0301  FA 59 F6   MOV $F6,$59    ; port2 ($2142) <- dp $59
$0304  FA 69 F7   MOV $F7,$69    ; port3 ($2143) <- dp $69   <-- handshake byte
$0307  6F         RET
```

luna executes this and writes `$F7 = [dp $69]`, but **`dp $69 = 0` in luna
vs `02` in Mesen** — the Akao driver's internal state variable diverges. So
the root is an **SPC700 execution/timing divergence inside the Akao driver**
(the CT/Akao family; cf. `project_pitchmod_spc700_crash` SPC700-timer lead),
NOT the SA-1 and NOT a wrong SPC700 port echo.

### SPC700 instruction-trace differential (done): timer 2 is the root

Built `--spc-trace` (commit `d6eae1d`; per-opcode `seq,pc,a,x,y,sp,psw`,
mirrors `--sa1-trace`) and a Mesen2 SPC700 PC trace (Lua exec callback,
`cpuType=spc`, `memType=spcMemory` — note: `emu.getState()` inside a memory
callback aborts it, so log PC-only). Both from IPL boot (`$FFC0`).

Collapsing spin-loops (period ≤8) and diffing: the two SPC700 streams match
for ~55,500 collapsed instructions, then **branch differently at SPC PC
`$022E`**:

```
$0225 2E FD 1B  CBNE $FD,+   ; A vs timer0 output
$0228 2E FE 4C  CBNE $FE,+   ; A vs timer1 output
$022B 2E F4 0D  CBNE $F4,+   ; A vs CPU port0
$022E 2E FF 6F  CBNE $FF,+$6F; A vs TIMER 2 output -> branch $02A0
```

A = `$00` at every `$022E` (9,279×). Mesen: `A != [$FF]` → branches to
`$02A0` (proceeds, eventually sets dp $69 = 2 and signals port 3). luna:
`A == [$FF]` → falls through to `$0231`→`$0301` (`MOV $F7,$69` with
dp $69 = 0). So **luna's SPC700 timer-2 output (`$FF`) reads 0 where Mesen's
has ticked non-zero.** luna's timer 2 (the fast 16-cycle / 64 kHz timer)
isn't accumulating relative to the driver's polls — the Akao tempo/sync
timer. This derails the whole handshake → the freeze.

**Root cause: luna's SPC700 timer-2 timing.** Same family as the documented
CT audio deadlock and the `project_pitchmod_spc700_crash` SPC700-timer lead.

### Diving into the fix (measured): the timer is *functionally* correct

Probed T2 at the `$FF` reads (env-gated `eprintln`, since removed). At the
poll loop: **T2 is enabled, target=256 (reload `$FC`=0), `timer_internal`
climbs at the correct rate (one tick per 16 SPC cycles), and it DOES tick**
— early reads show `out=1`/`out=4`. So it is **not** "T2 broken / disabled /
wrong rate". With target=256 the output is non-zero only briefly each ~4096
cycles, so the failure is a **sub-instruction timing/phase difference**: the
two emulators execute the same ~55,500 SPC instructions (collapsed), and at
instruction 55,537 luna's T2 output happens to read 0 where Mesen's has just
ticked → luna takes the `$0231` branch once → cascade to the freeze.

This points squarely at SPC700 **cycle-timing phase**, consistent with
`project_pitchmod_spc700_crash`: the per-opcode cycle COUNTS are
Tom-Harte-validated, but the timer is advanced at the wrong intra-instruction
phase. Suspect: luna's `Apu::run_one_spc` ticks the bus-access cycles
in-place (`clock_cycle`) but the instruction's **idle cycles in bulk at the
end** (`tick_timers(unclocked)`), whereas ares interleaves the timer every
cycle (`smp/timing.cpp` `wait()`→`stepTimers`). Over a tight poll loop this
phase can drift enough to flip the T2 read at the critical instant.

### Confirming differential (done): it's the CPU↔SPC scheduling grammar

T2 output is a pure function of cumulative SPC cycles (luna ticks T2 on
`subdiv % 16` boundaries identically via `clock_cycle` and `tick_timers`), so
the divergence ⇒ **cumulative SPC cycle drift**. Checked every targeted
candidate; all RULED OUT:

- **SPC clock ratio matches ares exactly**: ares `apuFrequency = 32040×768 =
  24_606_720`, SMP = `/12`, opcode-cycle rate = `/24 = 1_025_280` =
  luna's `SPC_CLOCK_HZ`. So no systematic clock drift.
- **Timer 2 functionally correct** (enabled, 16-cycle rate, ticks).
- **Per-opcode SPC700 cycle counts Tom-Harte-validated.**
- **`$2140` write ordering correct** (`write_inner` runs `io_cycle` →
  `Apu::step` before `cpu_write_port`, so the SPC is advanced to the write's
  cycle first).

The raw IPL-upload trace shows the actual mechanism: at the `FFCF/FFD2`
mailbox-wait spin **luna exits after ~5 iterations, Mesen after 10+** — luna's
SPC sees the CPU's `$2140` write after too few cycles. luna runs the SPC in
**batches** (per S-CPU bus access, with the SPIKE timestamped-mailbox
*approximation* in `Apu::cpu_write_port`/`run_one_spc`); ares **cooperatively
interleaves the CPU and SMP every cycle** (`Thread::synchronize`). The
batching/SPIKE granularity makes mailbox spins resolve at the wrong cycle,
accumulating drift that flips the Akao T2 poll at instr 55,537.

**Faithful fix = port ares' cooperative cycle-interleaved CPU↔SPC scheduling**
(the same class as the Super FX cooperative-scheduler port in
`.claude/rules/faithful-port-and-dichotomy.md`). The faithful-port rule
**forbids** the tempting shortcut here — a speculative SPIKE-lag/"magic
timing" tweak — and the Super FX cautionary tale shows approximating the
cooperative model wastes days. This is a staged, multi-session Phase-2
architectural change with audio-regression risk across every game; it must be
done by dichotomy with the `--spc-trace` differential as the oracle and
ear-validated. NOT a quick patch. The SA-1 path below is **superseded**.

## ⚠️ CORRECTION (peek bug): the SA-1 is NOT the cause

The SA-1 analysis below was **invalidated by a luna debug-tooling bug**:
`Snes::dbg_peek_bytes` returned a hardcoded `0` for the whole `$2000-$5FFF`
band, which includes the SA-1 **I-RAM (`$3000-$37FF`)**. So every I-RAM
peek/dump read `0`, making luna's I-RAM look all-zero and "diverging" from
Mesen when it was not. Fixed: route `$3000-$37FF` through the mapper in the
peek (side-effect-free). With the fix:

- luna's I-RAM at `$3008` = `5C 8F 80 C0  5C AB 80 C0` — the NMI/IRQ JML
  trampolines ARE correctly installed (the S-CPU write *did* store;
  `iram_writable_for` returned true, SIWP=`$FF`).
- **luna's full SA-1 I-RAM is byte-identical to Mesen at frames 23 AND 24.**

So the SA-1 boot spin at `$C0:816F` is **normal** early-boot behavior in both
emulators, not a deadlock, and the SA-1 is exonerated for the frame-24
divergence. The real divergence is **WRAM only** (the `wram_page_hashes`
oracle reads `snes.wram` directly and was always reliable): frame 23
byte-identical, frame 24 = 19 WRAM bytes, cause still open and NOT
SA-1-I-RAM-related. Candidates for the differing input in the frame-23→24
step: SA-1 arithmetic result regs (`$2306-9`), CFR (`$2300`), a PPU/CPU
register, or sub-frame timing — to be pinned next with the now-reliable
tools (the WRAM differential + correct I-RAM/register reads).

---

## (superseded) earlier SA-1 reading — kept for the record

Extended the differential to the **SA-1 I-RAM** (the WRAM trace only hashed
WRAM): at frame 23 (WRAM byte-identical) the **I-RAM already diverges** —
luna `00` where Mesen has `$30001=01`, `$30004/8/c=5c`, `$30008-f` =
`5C 8F 80 C0` / `5C AB 80 C0`, `$307fe/f = a6 81`. So the **SA-1 is the
root**, upstream of the WRAM divergence.

luna's SA-1 instruction trace (`--sa1-trace`) to frame 23: **1.38M instrs
but only 137 distinct PCs, 691,199 iterations each at `$C0:816F` and
`$C0:8171`** — a tight spin:

```
$C0:816F  A5 00     LDA $00      ; read I-RAM[$0]  (S-CPU view $00:3000)
$C0:8171  F0 FC     BEQ $816F    ; loop while zero
```

The SA-1 sits in its **boot handshake**, waiting for I-RAM[$0] to go
non-zero, and **never escapes the boot region** (`$C0:80xx/81xx`) — it never
writes `$3004/8/c` (zero occurrences across a 40M-instruction run), so the
table the S-CPU later copies into WRAM stays zero → the frame-24 WRAM
divergence, then the eventual frame-2137 forced-blank spin.

**The decisive clue — SA-1 vectors:** the S-CPU programs CNV (NMI) = `$0008`
and CIV (IRQ) = `$000C` (I-RAM). In Mesen those I-RAM slots hold JML
trampolines (`$0008: JML $C0808F`, `$000C: JML $C080AB`); **in luna they are
zero** — the SA-1 NMI/IRQ handlers are never installed. So an **SA-1
interrupt that fires in Mesen (driving the boot / releasing the spin) is not
being delivered in luna**, OR the handshake write that installs the handlers
+ rings the I-RAM[$0] doorbell never happens.

This is consistent with the deadlock at frame 2137 (the SA-1 *does* run
later in luna, but the early boot handshake left it mis-synchronised).

### Next step

Get Mesen's SA-1 instruction trace working (Lua exec callback,
`cpuType=sa1`, `memType=sa1Memory`) to see **how Mesen's SA-1 exits the
`$C0:816F` spin** — does PC jump to the IRQ vector target `$C080AB` (IRQ
fired), the NMI target `$C0808F` (NMI fired), or fall through (I-RAM[$0]
written by the S-CPU)? That selects the fix:
- IRQ/NMI vector → luna's **SA-1 interrupt delivery** (timer / H-V / CCNT
  source) is the bug; compare to ares `coprocessor/sa1`.
- fall-through → the **S-CPU→SA-1 I-RAM[$0] doorbell** path (a bus-view /
  CFR-SFR handshake issue, cf. the Kirby `$2180` DMA-view bug).
