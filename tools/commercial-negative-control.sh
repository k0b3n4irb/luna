#!/usr/bin/env bash
# commercial-negative-control.sh — run luna's own oracles over the local
# corpus of commercial ROMs, where any alarm is almost surely luna's.
#
# WHY: those ROMs were written in assembly by professionals and shipped on
# real consoles. An oracle that fires on one of them is a false positive of
# the oracle (or a luna bug), not a defect of the ROM — the negative
# control OpenSNES leans on to tell a luna false positive from a real SDK
# defect (asked 2026-10-21). Two oracles run:
#   1. `luna bench` — liveness (crash, hang, dead APU) and the "suspect"
#      heuristic: every title must come out ✅ ok.
#   2. `[asserts.dma] unsafe_writes = 0` — the DMA oracle of the OpenSNES
#      suite, one generated manifest per ROM, through `luna test`.
# `profile --budget` is deliberately absent: commercial games really do
# lag (slowdown with many sprites), so an over-budget frame accuses no one.
#
# USAGE:
#   cargo build --release -p luna-cli
#   tools/commercial-negative-control.sh            # tests/roms, 600 frames
#   ROMS=<dir> FRAMES=<n> tools/commercial-negative-control.sh
#
# Run before every release (CONTRIBUTING.md, "Versioning & releases").
# Exit 0 = both oracles clean. The ROMs are not committed (tests/roms/ is
# gitignored); the script fails if the corpus is empty.
set -euo pipefail

LUNA=${LUNA:-target/release/luna}
ROMS=${ROMS:-tests/roms}
FRAMES=${FRAMES:-600}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

shopt -s nullglob nocaseglob
roms=("$ROMS"/*.sfc "$ROMS"/*.smc)
if [ ${#roms[@]} -eq 0 ]; then
  echo "error: no .sfc/.smc ROMs in $ROMS" >&2
  exit 2
fi

echo "== luna bench: ${#roms[@]} ROMs, $FRAMES frames"
"$LUNA" bench "$ROMS" -f "$FRAMES" --jobs 0 --out "$WORK/bench" | tail -1
summary=$(grep -m1 '^\*\*Summary:\*\*' "$WORK/bench/report.md")
ok=$(sed -E 's/^\*\*Summary:\*\* ([0-9]+) .*/\1/' <<<"$summary")
bench_ok=1
if [ "$ok" != "${#roms[@]}" ]; then
  bench_ok=0
  echo "FAIL: not every title is ✅ ok — $summary"
  grep -E '❌|⚠️|🔑' "$WORK/bench/report.md" | grep '^|' || true
fi

echo "== [asserts.dma] unsafe_writes = 0 at $FRAMES frames"
i=0
for rom in "${roms[@]}"; do
  i=$((i + 1))
  abs=$(cd "$(dirname "$rom")" && pwd)/$(basename "$rom")
  # TOML basic string: escape backslashes and quotes in the path.
  esc=${abs//\\/\\\\}
  esc=${esc//\"/\\\"}
  printf 'rom = "%s"\nframes = %s\n\n[asserts.dma]\nunsafe_writes = 0\n' \
    "$esc" "$FRAMES" > "$WORK/$(printf '%03d' "$i").toml"
done
dma_ok=1
"$LUNA" test --jobs 0 "$WORK"/*.toml | grep -v '^PASS ' || dma_ok=0

if [ "$bench_ok" = 1 ] && [ "$dma_ok" = 1 ]; then
  echo "negative control: clean"
else
  echo "negative control: FAILED — an alarm on a commercial ROM is luna's to explain"
  exit 1
fi
