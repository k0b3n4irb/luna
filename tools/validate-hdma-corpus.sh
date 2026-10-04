#!/usr/bin/env bash
# validate-hdma-corpus.sh — visual regression sweep for HDMA / PPU / DMA
# changes across a corpus of commercial titles that exercise per-line HDMA
# (gradients, raster splits, mid-frame status-bar / window splits, Mode 7).
#
# WHY: the HDMA controller is shared by every game, so a change there
# (e.g. the mid-frame-enable fix — Yoshi's Island text band, see
# docs/archive/yoshis_island_text_barcode_investigation.md) must be eyeballed
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
# EXIT STATUS: non-zero when a ROM is present but its screenshot was not
# produced (luna crashed, timed out, or refused the ROM). The pictures
# themselves are still judged by eye: a zero exit means "every shot exists",
# not "every shot is right".
#
# Per .claude/rules/coproc-testing.md: a black/forced-blank shot is NOT a
# bug — commercial titles fade through black and wait at the title for
# Start. We inject Start ($1000) to reach gameplay, and take two shots per
# title (intro, gameplay) so at least one lands on visible content.
#
# The capture points are FRAMES (`--until-frame`), like the golden harness
# (crates/luna-core/tests/snes_test_roms.rs, `run_game_to_frame`): a game
# advances once per frame, so frame N is the same scene however cycle-exact
# the CPU timing is, where an instruction count slides through the game
# every time that timing improves. The numbers are the frames the former
# `-n` budgets reached on 2026-10-04.
set -u

BIN="${BIN:-./target/release/luna}"
ROMS="${ROMS:-tests/roms}"
OUT="${OUT:-/tmp/luna-hdma-val}"
mkdir -p "$OUT"

# Start pulse train ($1000), walking past title/menus toward gameplay.
STARTS="1200:0x1000,1210:0,1800:0x1000,1810:0,2600:0x1000,2610:0,3400:0x1000,3410:0,4200:0x1000,4210:0"

# "ROM filename | slug | intro frame | gameplay frame" — gameplay uses Start.
ENTRIES=(
  "Contra III - The Alien Wars (USA).sfc|contra3|2736|5732"
  "Tales of Phantasia (Japan).sfc|tales|1919|3889"
  "Super Metroid (Japan, USA) (En,Ja).sfc|metroid|1176|2986"
  "Final Fantasy III (USA) (Rev 1).sfc|ff6|4685|4685"
  "F-Zero (USA).sfc|fzero|598|1465"
  "Axelay (USA).sfc|axelay|880|3522"
  "Super Castlevania IV (USA).sfc|scv4|1362|4133"
  "Gradius III (USA).sfc|gradius3|1319|3956"
  "Super Mario World 2 - Yoshi's Island (U) (V1.1).smc|yi|2353|2542"
)

[ -x "$BIN" ] || { echo "build first: cargo build --release -p luna-cli"; exit 1; }

shots=0
expected=()
for e in "${ENTRIES[@]}"; do
  IFS='|' read -r rom slug intro play <<< "$e"
  if [ ! -f "$ROMS/$rom" ]; then
    printf "  skip  %-10s (absent)\n" "$slug"
    continue
  fi
  # Remove the previous run's shots first: a stale file must not stand in
  # for one this run failed to write.
  rm -f "$OUT/${slug}_intro.png" "$OUT/${slug}_play.png"
  timeout 420 "$BIN" state --until-frame "$intro" --screenshot "$OUT/${slug}_intro.png" "$ROMS/$rom" >/dev/null 2>&1 &
  timeout 420 "$BIN" state --until-frame "$play" --input "$STARTS" --screenshot "$OUT/${slug}_play.png" "$ROMS/$rom" >/dev/null 2>&1 &
  expected+=("${slug}_intro.png" "${slug}_play.png")
  shots=$((shots + 2))
done
wait

echo "wrote screenshots to $OUT/ (review them — sizes below; tiny = blank/fade):"
missing=0
for f in ${expected[@]+"${expected[@]}"}; do
  if [ -s "$OUT/$f" ]; then
    printf "  %-22s %8d bytes\n" "$f" "$(stat -c%s "$OUT/$f")"
  else
    printf "  %-22s MISSING — the ROM is present but luna wrote no screenshot\n" "$f"
    missing=$((missing + 1))
  fi
done
echo "($shots shots attempted, $missing missing)"
[ "$missing" -eq 0 ] || exit 1
