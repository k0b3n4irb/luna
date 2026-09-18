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
# Pinned upstream commit: the golden hashes are baselines against THIS
# corpus, so an upstream edit must never silently change what CI tests.
# Bump deliberately (and re-run the goldens) to adopt upstream changes.
REV="350b394e86ec5d62f600b5cbf64cdce3721bb6ef"
# Subdirs the harness pulls ROMs from.
SPARSE_PATHS=(CPUTest PPU SPC700 INPUT)

DEST="${LUNA_SNES_TEST_DIR:-$(cd "$(dirname "$0")/.." && pwd)/../luna_tests}"

if [ -d "$DEST" ] && [ ! -d "$DEST/.git" ] && [ -n "$(ls -A "$DEST")" ]; then
    # A corpus copied in by hand (no .git) can't be updated or repaired —
    # and `git clone` refuses a non-empty directory. Say so instead of
    # failing obscurely.
    echo "error: $DEST exists but is not a git checkout, so it cannot be" >&2
    echo "       repaired in place. Move it aside (or delete it) and re-run." >&2
    exit 1
fi

if [ ! -d "$DEST/.git" ]; then
    echo "Sparse-cloning $REPO @ ${REV:0:12} into $DEST"
    git init -q "$DEST"
    git -C "$DEST" remote add origin "$REPO"
    git -C "$DEST" sparse-checkout init --cone
else
    echo "Corpus present at $DEST; ensuring rev ${REV:0:12}"
fi

echo "  paths: ${SPARSE_PATHS[*]}"
# (Re)apply the sparse set so newly-added paths get pulled into an
# existing checkout, then move to the pinned commit.
git -C "$DEST" sparse-checkout set "${SPARSE_PATHS[@]}"
git -C "$DEST" fetch -q --filter=blob:none --depth 1 origin "$REV"
git -C "$DEST" checkout -q --detach FETCH_HEAD

echo "Done. Corpus at $DEST"
echo "Run: cargo test -p luna-core --test snes_test_roms"
