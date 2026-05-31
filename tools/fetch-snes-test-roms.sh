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
# Subdirs the harness pulls ROMs from.
SPARSE_PATHS=(CPUTest PPU SPC700)

DEST="${LUNA_SNES_TEST_DIR:-$(cd "$(dirname "$0")/.." && pwd)/../luna_tests}"

if [ -d "$DEST/.git" ]; then
    # Already cloned — just (re)apply the sparse set so newly-added paths
    # (e.g. PPU) get pulled into an existing checkout.
    echo "Corpus present at $DEST; ensuring paths: ${SPARSE_PATHS[*]}"
    git -C "$DEST" sparse-checkout set "${SPARSE_PATHS[@]}"
    git -C "$DEST" checkout
    echo "Done."
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
