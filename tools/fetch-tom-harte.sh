#!/usr/bin/env bash
# Fetch the Tom Harte 65C816 ProcessorTests dataset into tests/tom-harte/.
#
# Usage: tools/fetch-tom-harte.sh
#
# The dataset (~600 MB uncompressed) is *not* committed. It lives at
# https://github.com/SingleStepTests/65816. We do a shallow clone to
# keep the download under control.

set -euo pipefail

REPO="https://github.com/SingleStepTests/65816.git"
DEST_PARENT="$(cd "$(dirname "$0")/.." && pwd)/tests"
DEST="$DEST_PARENT/tom-harte"

if [ -d "$DEST/v1" ]; then
    echo "Dataset already present at $DEST/v1"
    echo "Delete the directory and rerun to refresh."
    exit 0
fi

# tests/tom-harte/ may contain only a .gitkeep placeholder (the dir is
# tracked but empty). Clear it so `git clone` can create the directory.
if [ -d "$DEST" ] && [ -z "$(ls -A "$DEST" | grep -v '^\.gitkeep$')" ]; then
    rm -f "$DEST/.gitkeep"
    rmdir "$DEST" 2>/dev/null || true
fi

mkdir -p "$DEST_PARENT"
echo "Cloning $REPO into $DEST (shallow)…"
git clone --depth 1 "$REPO" "$DEST"

if [ ! -d "$DEST/v1" ]; then
    echo "ERROR: expected $DEST/v1 after clone — repo layout may have changed." >&2
    exit 1
fi

echo "Done. $(ls "$DEST/v1" | wc -l) JSON files installed."
echo "Run: cargo test -p luna-cpu-65c816 --test tom_harte -- --ignored --nocapture"
