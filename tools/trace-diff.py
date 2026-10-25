#!/usr/bin/env python3
"""trace-diff.py — compare two trace logs in Mesen2's default format, row by
row (luna's `--trace-log` against tools/mesen-trace-log.py's).

Rows are paired while the same instruction runs on both sides (same PC and
registers). Each differing pair is classed by what differs: the disassembly,
the effective address, the memory value, or the beam (the V / H columns).
When the two runs part (a loop run a different number of times), the next
8 rows that agree on both sides take the pairing up again.

    tools/trace-diff.py luna.txt mesen.txt [--examples N]

A beam difference with identical registers is a timing difference between
the two emulations, not a trace difference.
"""
import argparse
import collections
import re

REGS = re.compile(r"A:\w{4} X:\w{4} Y:\w{4} S:\w{4} D:\w{4} DB:\w{2} P:\w{8}")


def split(row):
    """PC, the text before the registers, the registers, the rest."""
    m = REGS.search(row)
    if not m:
        return row[:6], row[8:].rstrip(), "", ""
    return row[:6], row[8 : m.start()].rstrip(), m.group(0), row[m.end() :]


def key(row):
    pc, _, regs, _ = split(row)
    return pc + regs


def kind_of(a, b):
    _, ta, _, ra = split(a)
    _, tb, _, rb = split(b)
    kinds = []
    if ta != tb:
        dis = lambda t: t.split(" [")[0].split(" = ")[0]
        if dis(ta) != dis(tb):
            kinds.append("disassembly")
        elif ta.split("]")[0] != tb.split("]")[0]:
            kinds.append("effective address")
        else:
            kinds.append("value")
    if ra != rb:
        kinds.append("beam")
    return "+".join(kinds) or "spacing"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("luna")
    ap.add_argument("mesen")
    ap.add_argument("--examples", type=int, default=3)
    args = ap.parse_args()
    a = open(args.luna).read().splitlines()
    b = open(args.mesen).read().splitlines()
    kinds = collections.Counter()
    examples = collections.defaultdict(list)
    i = j = compared = parted = 0
    while i < len(a) and j < len(b):
        if key(a[i]) == key(b[j]):
            compared += 1
            if a[i] != b[j]:
                k = kind_of(a[i], b[j])
                kinds[k] += 1
                if len(examples[k]) < args.examples:
                    examples[k].append((i, a[i], b[j]))
            i += 1
            j += 1
            continue
        parted += 1
        found = False
        for span in range(1, 4000):
            for di in range(span + 1):
                dj = span - di
                if i + di + 8 < len(a) and j + dj + 8 < len(b) and all(
                    key(a[i + di + k]) == key(b[j + dj + k]) for k in range(8)
                ):
                    i, j, found = i + di, j + dj, True
                    break
            if found:
                break
        if not found:
            break
    print(f"{compared} rows compared, {parted} partings, stopped at luna {i} / mesen {j}")
    print(f"identical: {compared - sum(kinds.values())}")
    for k, c in kinds.most_common():
        print(f"{k}: {c}")
        for row, x, y in examples[k]:
            print(f"  luna  #{row}: {x}")
            print(f"  mesen     : {y}")


if __name__ == "__main__":
    main()
