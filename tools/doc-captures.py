#!/usr/bin/env python3
"""Regenerate the captures the guide and the README show.

Every picture under `book/src/assets/captures/` is a frame luna rendered
from a ROM of the pinned PeterLemon corpus (`tools/fetch-snes-test-roms.sh`).
None is drawn or retouched by hand: this script is the only writer of that
folder, and `--check` proves the committed files are what the luna in the
tree renders today.

  screen   `luna run --until-frame N --screenshot`: the composited frame.
  asset    `luna assets-dump -n STEPS`: one of the PNGs it writes (a
           tilemap, the palette), as a debugger would show it.

Usage:
    tools/doc-captures.py            # rewrite the folder
    tools/doc-captures.py --check    # exit 1 when a committed file differs
    tools/doc-captures.py --list     # the table: name, kind, ROM

The corpus is `LUNA_SNES_TEST_DIR`, else `../luna_tests` (the golden suite's
convention). The binary is `LUNA_BIN`, else `target/release/luna`, else
`target/debug/luna`. A missing corpus or binary is exit 2, never a pass.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "book" / "src" / "assets" / "captures"

# name, kind, ROM (relative to the corpus), frame (screen) or steps (asset),
# and for an asset the file of the dump to keep.
CAPTURES: list[tuple[str, str, str, int, str | None]] = [
    ("mode7-hdma", "screen", "PPU/HDMA/Mode7HDMA/Mode7HDMA.sfc", 90, None),
    ("hdma-wave", "screen", "PPU/HDMA/WaveHDMA/WaveHDMA.sfc", 90, None),
    ("bg-rings", "screen", "PPU/Rings/Rings.sfc", 90, None),
    ("hdma-gradient", "screen", "PPU/HDMA/RedSpaceHDMA/RedSpaceHDMA.sfc", 90, None),
    ("cgram-hicolor", "screen", "PPU/HDMA/HiColor64PerTileRow/HiColor64PerTileRow.sfc", 90, None),
    ("mode7-tilemap", "asset", "PPU/HDMA/Mode7HDMA/Mode7HDMA.sfc", 3_000_000, "bg1_tilemap_mode7.png"),
    ("mode7-palette", "asset", "PPU/HDMA/Mode7HDMA/Mode7HDMA.sfc", 3_000_000, "palette.png"),
]


def fail(msg: str) -> "NoReturn":  # noqa: F821
    print(f"doc-captures: {msg}", file=sys.stderr)
    sys.exit(2)


def luna_binary() -> Path:
    cands = [os.environ["LUNA_BIN"]] if os.environ.get("LUNA_BIN") else []
    cands += [ROOT / "target" / "release" / "luna", ROOT / "target" / "debug" / "luna"]
    for c in cands:
        if Path(c).is_file():
            return Path(c)
    fail("no luna binary (set LUNA_BIN, or `cargo build --release -p luna-cli`)")


def corpus_dir() -> Path:
    d = Path(os.environ.get("LUNA_SNES_TEST_DIR") or ROOT.parent / "luna_tests")
    if not d.is_dir():
        fail(f"no corpus at {d} (run tools/fetch-snes-test-roms.sh, or set LUNA_SNES_TEST_DIR)")
    return d


def run(cmd: list[str]) -> None:
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        fail(f"{' '.join(cmd)}\n{r.stderr.strip()}")


def render(binary: Path, corpus: Path, dest: Path) -> None:
    """Write every capture of the table into `dest`."""
    dest.mkdir(parents=True, exist_ok=True)
    for name, kind, rom, amount, keep in CAPTURES:
        rom_path = corpus / rom
        if not rom_path.is_file():
            fail(f"{rom}: not in the corpus at {corpus}")
        target = dest / f"{name}.png"
        # The corpus ROMs carry no valid header checksum: the mapper is forced,
        # as the golden suite does.
        if kind == "screen":
            run([str(binary), "run", "--force-mapper", "lorom", "--until-frame", str(amount),
                 "--screenshot", str(target), str(rom_path)])
        else:
            with tempfile.TemporaryDirectory() as tmp:
                run([str(binary), "assets-dump", "--force-mapper", "lorom", "-n", str(amount),
                     "--out", tmp, str(rom_path)])
                assert keep is not None
                target.write_bytes((Path(tmp) / keep).read_bytes())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="compare the committed captures with a fresh render")
    ap.add_argument("--list", action="store_true", help="print the capture table")
    args = ap.parse_args()
    if args.list:
        for name, kind, rom, amount, keep in CAPTURES:
            unit = "frame" if kind == "screen" else "steps"
            print(f"{name}.png\t{kind}\t{rom}\t{unit} {amount}" + (f"\t{keep}" if keep else ""))
        return 0

    binary, corpus = luna_binary(), corpus_dir()
    if not args.check:
        render(binary, corpus, OUT)
        for stale in sorted(set(OUT.glob("*")) - {OUT / f"{c[0]}.png" for c in CAPTURES}):
            stale.unlink()
        print(f"doc-captures: wrote {len(CAPTURES)} capture(s) to {OUT.relative_to(ROOT)}")
        return 0

    findings: list[str] = []
    with tempfile.TemporaryDirectory() as tmp:
        render(binary, corpus, Path(tmp))
        for name, *_ in CAPTURES:
            committed = OUT / f"{name}.png"
            if not committed.is_file():
                findings.append(f"{name}.png: in the table, not in the folder")
            elif committed.read_bytes() != (Path(tmp) / f"{name}.png").read_bytes():
                findings.append(f"{name}.png: luna no longer renders the committed file")
    known = {f"{c[0]}.png" for c in CAPTURES}
    for extra in sorted(p.name for p in OUT.glob("*") if p.name not in known):
        findings.append(f"{extra}: in the folder, not in the table (hand-made?)")
    for f in findings:
        print(f"  - {f}")
    print(f"doc-captures: {'OK' if not findings else f'{len(findings)} finding(s)'}")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
