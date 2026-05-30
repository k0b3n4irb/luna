#!/usr/bin/env bash
# Fetch the Tom Harte SPC700 ProcessorTests dataset into
# tests/tom-harte-spc700/.
#
# Usage: tools/fetch-tom-harte-spc700.sh
#
# The dataset is *not* committed. It lives at
# https://github.com/SingleStepTests/spc700. We do a shallow clone to
# keep the download under control. Mirrors tools/fetch-tom-harte.sh
# (the 65C816 equivalent).

set -euo pipefail

REPO="https://github.com/SingleStepTests/spc700.git"
DEST_PARENT="$(cd "$(dirname "$0")/.." && pwd)/tests"
DEST="$DEST_PARENT/tom-harte-spc700"

if [ -d "$DEST/v1" ]; then
    echo "Dataset already present at $DEST/v1"
    echo "Delete the directory and rerun to refresh."
    exit 0
fi

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
echo "Run: cargo test -p luna-cpu-spc700 --test tom_harte -- --ignored --nocapture"
