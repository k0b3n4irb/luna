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
