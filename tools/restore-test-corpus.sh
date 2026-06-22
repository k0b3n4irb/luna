#!/usr/bin/env bash
# Restore the SNES accuracy test-ROM corpus into ../luna_tests (the sibling
# directory the golden suite `crates/luna-core/tests/snes_test_roms.rs` reads,
# or $LUNA_SNES_TEST_DIR). The ROMs are open-source PeterLemon hardware tests
# (github.com/PeterLemon/SNES) — NOT committed (large, external corpus), so this
# script reproduces them on demand. A blobless sparse clone keeps it to a few MB.
#
# Usage:  tools/restore-test-corpus.sh [DEST]
#   DEST defaults to ../luna_tests (sibling of the repo).
set -euo pipefail

REPO="https://github.com/PeterLemon/SNES.git"
DEST="${1:-$(cd "$(dirname "$0")/../.." && pwd)/luna_tests}"
# Test-ROM directories the golden suite consumes (cone sparse-checkout paths).
PATHS=(CPUTest PPU)

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Cloning PeterLemon SNES test ROMs (blobless, sparse: ${PATHS[*]}) ..."
git clone --filter=blob:none --no-checkout --depth 1 "$REPO" "$TMP/SNES" >/dev/null 2>&1
git -C "$TMP/SNES" sparse-checkout init --cone >/dev/null
git -C "$TMP/SNES" sparse-checkout set "${PATHS[@]}" >/dev/null
git -C "$TMP/SNES" checkout >/dev/null 2>&1

mkdir -p "$DEST"
for p in "${PATHS[@]}"; do
  [ -d "$TMP/SNES/$p" ] && cp -r "$TMP/SNES/$p" "$DEST/"
done

count="$(find "$DEST" -name '*.sfc' | wc -l | tr -d ' ')"
echo "Restored $count test ROMs into $DEST"
echo "Run the suite with:  cargo test --release --test snes_test_roms"
