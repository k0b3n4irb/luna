#!/usr/bin/env bash
# Fetch the SNES hardware-test ROM corpus (Peter Lemon's suite) used by
# the golden display tests in crates/luna-core/tests/snes_test_roms.rs.
#
# Following the twvd/siena convention, the corpus lives OUTSIDE this repo
# (it's large) and is checked out *at the same directory level* — i.e.
# alongside the luna checkout as ../luna_tests. We sparse-checkout only
# the test-relevant subdirectories, not the whole multi-GB repo.
#
# Usage: tools/fetch-snes-test-roms.sh
#
# Override the destination with LUNA_SNES_TEST_DIR (the test reads the
# same variable).

set -euo pipefail

REPO="https://github.com/PeterLemon/SNES.git"
# Subdirs the harness pulls ROMs from. Add more here (e.g. PPU, SPC700)
# as tests for those categories are written.
SPARSE_PATHS=(CPUTest)

DEST="${LUNA_SNES_TEST_DIR:-$(cd "$(dirname "$0")/.." && pwd)/../luna_tests}"

if [ -d "$DEST/CPUTest" ]; then
    echo "Corpus already present at $DEST"
    echo "Delete the directory and rerun to refresh."
    exit 0
fi

echo "Sparse-cloning $REPO into $DEST"
echo "  paths: ${SPARSE_PATHS[*]}"
git clone --filter=blob:none --no-checkout --depth 1 "$REPO" "$DEST"
git -C "$DEST" sparse-checkout init --cone
git -C "$DEST" sparse-checkout set "${SPARSE_PATHS[@]}"
git -C "$DEST" checkout

echo "Done. Corpus at $DEST"
echo "Run: cargo test -p luna-core --test snes_test_roms"
