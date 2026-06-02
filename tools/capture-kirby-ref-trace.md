# Capturing a reference boot trace for the Kirby Super Star crash

Why: luna's S-CPU boots Kirby, does `JMP $000E`, but WRAM `$00:000E` is
`$00` (never populated) → executes `BRK` → jumps to the game's crash-trap
vector (`$00:FFE6 = $5FFF`) → runs away → corrupts the stack → black
screen. The crash mechanism is fully understood (see the
`project_smrpg_sa1_deadlock` memory and `docs/sa1_status.md`); the open
question is **what populates WRAM `$0000-$001F` on real hardware, and
where luna's boot diverges and skips it.**

A known-good emulator answers both. Capture there, then diff against luna.

---

## Option A — Mesen2 (recommended, scripted)

Mesen2: <https://github.com/SourMesen/Mesen2>. It has a Lua script window
and catches WRAM writes from any path (CPU store, DMA, block move).

1. Load **Kirby Super Star (USA)** and let it reach the title (confirms a
   good dump + that this emulator boots it).
2. **Debug → Script Window → Open** → select
   `tools/kirby-boot-ref-trace.lua` → **Run**.
3. **Power-cycle** the console (not soft reset) so capture starts at reset.
4. Let it run ~3–4 seconds, then **Stop** the script.
5. Two files are written next to Mesen2's working directory:
   - `kirby_wram_stub_writes.log` — **the key answer**: every write to
     WRAM `$0000-$001F`, with the PC and value. This is what populates the
     `$000E` stub luna is missing.
   - `kirby_pc_trace.log` — the S-CPU PC stream from reset to `JMP $000E`,
     for the divergence diff.
6. Send both back (the first is usually tiny).

If the script errors on a field name, run this once in the script window
to list the real CPU state fields and adjust `cpuStr()` in the `.lua`:
```lua
for k,_ in pairs(emu.getState().cpu) do emu.log(k) end
```

### Fallback: Mesen2 GUI, no Lua
- **Breakpoint** (Debug → Breakpoints): *Write*, memory type **SNES Work
  RAM**, address `$0000`–`$001F`. Run from power-on; when it breaks, note
  the **PC** and value in the disassembly. That PC is the stub writer.
- **Trace Logger** (Debug → Trace Logger): enable, log to file from reset,
  ~20k rows. Use that as `kirby_pc_trace.log`.

---

## Option B — bsnes-plus (alternative)

bsnes-plus debugger (<https://github.com/devinacker/bsnes-plus>):
1. Tools → Debugger.
2. Breakpoints → add: **Write**, source **CPU bus**, address `7e000e`
   (WRAM mirror; or `00000e`). Optionally a second on `7e0000`.
3. Reset. When it breaks, record the **PC** and the value being written.
4. Tracer tab → enable CPU trace to a file from reset for the PC stream.

---

## Capture luna's matching trace (for the diff)

```bash
# PC stream from reset (same window the Lua script logs up to JMP $000E).
./target/release/luna state -n 20000 \
  --cpu-trace /tmp/luna_kirby_pc.csv --cpu-trace-from 0 --cpu-trace-max 20000 \
  "tests/roms/Kirby Super Star (USA).sfc"

# WRAM region snapshot at a pre-crash point (confirms $000E stays $00).
./target/release/luna state -n 900000 --peek 00:0000:32 \
  "tests/roms/Kirby Super Star (USA).sfc"
```

Reminder: luna's `--mem-trace` **silently drops events once it hits
`--mem-trace-max`** — never conclude "addr X is never written" from a
trace that filled. Use `--peek` (ground truth) or a tight
`--mem-trace-from` window.

---

## What the diff tells us

Line up the reference `kirby_pc_trace.log` against luna's
`/tmp/luna_kirby_pc.csv` (both are PC-first). The **first PC where they
diverge** is the branch luna takes differently — and the path the
reference takes from there is the code that writes WRAM `$0000+`. That
branch's condition (a register/flag/memory value just before it) is the
real bug to fix in luna. `kirby_wram_stub_writes.log` independently names
the writer PC, so the two should corroborate.
