#!/usr/bin/env bash
# validate-hdma-corpus.sh — visual regression sweep for HDMA / PPU / DMA
# changes across a corpus of commercial titles that exercise per-line HDMA
# (gradients, raster splits, mid-frame status-bar / window splits, Mode 7).
#
# WHY: the HDMA controller is shared by every game, so a change there
# (e.g. the mid-frame-enable fix — Yoshi's Island text band, see
# docs/yoshis_island_text_barcode_investigation.md) must be eyeballed
# across a broad set, not just the title that motivated it. The titles
# below were chosen because each leans on HDMA differently; Contra III's
# top status bar and Tales of Phantasia's bottom window are *direct* tests
# of the mid-frame-split class.
#
# USAGE:
#   cargo build --release -p luna-cli
#   tools/validate-hdma-corpus.sh            # -> /tmp/luna-hdma-val/*.png
#   OUT=/some/dir tools/validate-hdma-corpus.sh
#
# Then open the PNGs and confirm each scene renders (no banding, no missing
# layer, no garbled split). ROMs are copyrighted and gitignored — dump your
# own into tests/roms/; titles that are absent are skipped, not failed.
#
# Per .claude/rules/coproc-testing.md: a black/forced-blank shot is NOT a
# bug — commercial titles fade through black and wait at the title for
# Start. We inject Start ($1000) to reach gameplay, and sample several
# instruction counts so at least one lands on visible content.
set -u

BIN="${BIN:-./target/release/luna}"
ROMS="${ROMS:-tests/roms}"
OUT="${OUT:-/tmp/luna-hdma-val}"
mkdir -p "$OUT"

# Start pulse train ($1000), walking past title/menus toward gameplay.
STARTS="1200:0x1000,1210:0,1800:0x1000,1810:0,2600:0x1000,2610:0,3400:0x1000,3410:0,4200:0x1000,4210:0"

# "ROM filename | slug | intro -n | gameplay -n" — gameplay uses Start.
ENTRIES=(
  "Contra III - The Alien Wars (USA).sfc|contra3|30000000|60000000"
  "Tales of Phantasia (Japan).sfc|tales|30000000|60000000"
  "Super Metroid (Japan, USA) (En,Ja).sfc|metroid|18000000|45000000"
  "Final Fantasy III (USA) (Rev 1).sfc|ff6|65000000|65000000"
  "F-Zero (USA).sfc|fzero|8000000|20000000"
  "Axelay (USA).sfc|axelay|10000000|40000000"
  "Super Castlevania IV (USA).sfc|scv4|15000000|45000000"
  "Gradius III (USA).sfc|gradius3|15000000|45000000"
  "Super Mario World 2 - Yoshi's Island (U) (V1.1).smc|yi|28000000|28000000"
)

[ -x "$BIN" ] || { echo "build first: cargo build --release -p luna-cli"; exit 1; }

shots=0
for e in "${ENTRIES[@]}"; do
  IFS='|' read -r rom slug intro play <<< "$e"
  if [ ! -f "$ROMS/$rom" ]; then
    printf "  skip  %-10s (absent)\n" "$slug"
    continue
  fi
  timeout 420 "$BIN" state -n "$intro" --screenshot "$OUT/${slug}_intro.png" "$ROMS/$rom" >/dev/null 2>&1 &
  timeout 420 "$BIN" state -n "$play" --input "$STARTS" --screenshot "$OUT/${slug}_play.png" "$ROMS/$rom" >/dev/null 2>&1 &
  shots=$((shots + 2))
done
wait

echo "wrote screenshots to $OUT/ (review them — sizes below; tiny = blank/fade):"
for f in "$OUT"/*.png; do [ -f "$f" ] && printf "  %-22s %8d bytes\n" "$(basename "$f")" "$(stat -c%s "$f")"; done
echo "($shots shots attempted)"
