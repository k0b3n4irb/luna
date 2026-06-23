#!/usr/bin/env python3
"""Compare NMI/IRQ delivery timing between luna and Mesen2 (cycle-accuracy P3).

The self-contained delivery-timing differential (docs/roadmap_to_A.md):
  1. Mesen reference (headless):
       IRQ_STOP_FRAME=300 ~/bin/Mesen --testRunner tools/mesen-irq-trace.lua \
         "<rom>"                         # writes /tmp/mesen_irq.csv
  2. luna's NMI vector fetches, frame-aligned:
       luna state "<rom>" --until-frame 300 -n 999999999 \
         --mem-trace /tmp/luna_vec.csv --mem-trace-addr FFEA:FFEA
  3. this diff:
       tools/irq-trace-diff.py /tmp/luna_vec.csv /tmp/mesen_irq.csv

Both clocks are absolute-since-reset with DIFFERENT origins, so we compare
the ORIGIN-INDEPENDENT signal: the inter-NMI delta cadence (master clocks
between consecutive NMI vector fetches). Reads of $FFEA outside V-blank
(scanline < 220) are ROM/data fetches of the vector table, not deliveries —
filtered out.

Result on Doom (2026-06-23): luna and Mesen agree — same ~47 deliveries over
300 frames, same ~357366-clock inter-NMI cadence + jitter. luna's
instruction-atomic interrupt model is cycle-correct at the observable level;
no per-access-poll fix is warranted (the residual is below this measurement's
floor). Reuse this to validate any future interrupt-timing change vs Mesen.
"""
import sys
from collections import Counter

CLOCKS_PER_LINE = 1364
LINES = 262  # NTSC


def nmi_deliveries(path, clkcol, addrcol):
    clks = []
    for ln in open(path).read().splitlines()[1:]:
        f = ln.split(",")
        if "FFEA" not in f[addrcol]:
            continue
        clk = int(f[clkcol])
        if (clk // CLOCKS_PER_LINE) % LINES >= 220:  # V-blank → real delivery
            clks.append(clk)
    return sorted(clks)


def deltas(c):
    return [c[i + 1] - c[i] for i in range(len(c) - 1)]


def main():
    luna_csv = sys.argv[1] if len(sys.argv) > 1 else "/tmp/luna_vec.csv"
    mesen_csv = sys.argv[2] if len(sys.argv) > 2 else "/tmp/mesen_irq.csv"
    luna = nmi_deliveries(luna_csv, 0, 3)   # mclk,frame,pc,addr,kind,...
    mesen = nmi_deliveries(mesen_csv, 0, 1)  # master_clock,addr,kind,value
    print(f"real NMI deliveries: luna={len(luna)} mesen={len(mesen)}")
    print("luna  inter-NMI delta histogram:", sorted(Counter(deltas(luna)).items()))
    print("mesen inter-NMI delta histogram:", sorted(Counter(deltas(mesen)).items()))


if __name__ == "__main__":
    main()
