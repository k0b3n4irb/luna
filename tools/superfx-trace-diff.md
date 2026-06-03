# Finding a SuperFX rendering glitch by trace diff

luna's GSU engine is functionally correct (it boots Star Fox, renders the
3D title logo and the Arwing model on the Controls screen, and executes
coherent code). The remaining glitches show up in **heavier gameplay
scenes** — i.e. timing / GSU↔SNES sync under load, or a specific-opcode
edge case. The way to pin the exact divergence is to diff luna's GSU
instruction stream against a known-accurate reference for the **same
scene**.

## 1. Capture luna's GSU trace at the buggy scene

`--superfx-trace` writes a per-opcode CSV (`seq,pc,opcode,sfr,r0..r15`).
Drive into the scene with `--input` (`frame:hexmask` pulses; Start =
`0x1000` on Star Fox) and cap the ring buffer big enough to hold it:

```
./target/release/luna state -n 60000000 \
  --input "600:0x1000,610:0,1200:0x1000,1210:0,2000:0x1000,2010:0" \
  --superfx-trace /tmp/luna_gsu.csv --superfx-trace-max 4000000 \
  --screenshot /tmp/scene.png \
  "tests/roms/Star Fox (USA) (Rev 2).sfc"
```

The ring buffer keeps the **most recent** N opcodes (drops the oldest half
when full), so size `--superfx-trace-max` to cover the window of interest.

## 2. Capture a reference trace of the same scene

Pick whichever reference you can script a GSU/GSU-1 instruction log from:

- **bsnes-plus** — `Tools → Debugger → SMP/GSU`; enable the GSU trace
  log, drive to the same scene, save the log.
- **twvd/siena** (`github.com/twvd/siena`, Rust, no warranty) — it has a
  GSU core (`src/cpu_gsu`) and PeterLemon GSU tests; add a per-`step()`
  log of `(pc, opcode, R[0..16], sfr)` and run the same ROM/scene.
- **ares** — `ares/sfc/coprocessor/superfx`; its debugger can trace the
  GSU.

Normalise both to the same columns (PC + opcode + the 16 registers). The
**PC + register stream** is the diffable signal; reaching an identical
GSU entry point (e.g. the per-object render routine) is enough to line
them up — you don't need cycle-identical timing to find a *logic*
divergence.

## 3. Diff and find the first divergence

```
# align on a common PC, then diff the register columns forward
diff <(cut -d, -f2-3 /tmp/luna_gsu.csv) <(cut -d, -f2-3 /tmp/ref_gsu.csv) | head
```

The **first instruction where the registers diverge** is the bug:
- Registers diverge after a specific opcode → that opcode's
  implementation is wrong (cross-check it in `crates/luna-bus/src/superfx.rs`
  against `docs/superfx_reference.md` §2).
- PCs stay identical but a **plot lands wrong** → the plot pipeline /
  tile-address math (§4) or the RAM size/`ram_mask` (board size).
- luna STOPs (`opcode == $00`) at a different point than the reference →
  a control-flow / handshake timing divergence (the harder, sync class).

## What we already know (rules out the gross failures)

- The engine executes coherent code (INC/STW/LOOP RAM-fill loops, FMULT
  for 3D projection, PLOT) — verified by `--superfx-trace`.
- Plot / multiplies / fetch-with-branch-delay-slot all match ares + siena
  by code review.
- The Arwing renders cleanly on the Controls screen.

So focus the diff on a **busy gameplay frame**, not the menus.
