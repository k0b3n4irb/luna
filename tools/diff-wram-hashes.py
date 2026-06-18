#!/usr/bin/env python3
"""diff-wram-hashes.py — find the first divergent frame between luna's and
Mesen2's per-frame WRAM page hashes (the `wram-trace` differential).

Both files are lines of "<frame> <h0> ... <h31>". The absolute frame numbers
may differ by a constant boot offset between emulators, so we auto-detect the
offset that best aligns the two hash *sequences*, then report the first frame
(in luna's numbering) where any page hash differs, and WHICH pages diverged.

Usage:
  tools/diff-wram-hashes.py /tmp/luna_wram.txt /tmp/mesen_wram.txt
"""
import sys


def load(path):
    rows = []
    with open(path) as f:
        for line in f:
            t = line.split()
            if len(t) >= 2:
                rows.append((int(t[0]), t[1:]))
    return rows


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    luna = load(sys.argv[1])
    mesen = load(sys.argv[2])
    lh = [h for _, h in luna]
    mh = [h for _, h in mesen]

    # Auto-detect the constant offset `off` (mesen index = luna index + off)
    # that maximises matching *pages* over the overlap. Page-level (not
    # whole-line) scoring is robust to a single persistent divergent page —
    # which would otherwise drag a whole-line score to ~0 and mis-pick.
    best_off, best_score = 0, -1
    for off in range(-90, 91):
        score = 0
        for i in range(len(lh)):
            j = i + off
            if 0 <= j < len(mh):
                a, b = lh[i], mh[j]
                score += sum(1 for p in range(min(len(a), len(b))) if a[p] == b[p])
        if score > best_score:
            best_off, best_score = off, score
    print(f"best frame offset (mesen = luna + {best_off}); "
          f"matched {best_score} frames")

    first = None
    for i in range(len(lh)):
        j = i + best_off
        if not (0 <= j < len(mh)):
            continue
        if lh[i] != mh[j]:
            first = i
            break

    if first is None:
        print("NO DIVERGENCE found over the overlapping range — "
              "luna matches Mesen2 for every compared frame.")
        return

    luna_frame = luna[first][0]
    diff_pages = [p for p in range(len(lh[first]))
                  if lh[first][p] != mh[first + best_off][p]]
    print(f"\nFIRST DIVERGENCE at luna frame {luna_frame} "
          f"(mesen frame {mesen[first + best_off][0]})")
    print(f"  diverging WRAM pages: {diff_pages}")
    print(f"  (page P covers WRAM bytes 0x{4096*diff_pages[0]:05x}.."
          f"0x{4096*diff_pages[0]+0xfff:05x} = "
          f"${0x7e0000 + 4096*diff_pages[0]:06x} for page {diff_pages[0]})")
    # Show a few frames of context around the divergence.
    print("\n  context (luna_frame: diverging-page-indices):")
    for i in range(max(0, first - 2), min(len(lh), first + 4)):
        j = i + best_off
        if not (0 <= j < len(mh)):
            continue
        dp = [p for p in range(len(lh[i])) if lh[i][p] != mh[j][p]]
        flag = " <-- first" if i == first else ""
        print(f"    luna {luna[i][0]:>5}: {dp}{flag}")


if __name__ == "__main__":
    main()
