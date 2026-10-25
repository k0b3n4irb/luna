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

# The dataset is pinned: upstream corrects its cases now and then, and a
# floating HEAD changed the gate under CI without a commit here (2026-10-16
# for the 65C816). Moving the pin is a commit, and it changes the CI cache
# key, which hashes this file. Delete the local copy to fetch a new pin.
COMMIT="67d15f492b2740964abd4efd1229e0ec9c342228" # 2024-05-10
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
echo "Cloning $REPO into $DEST (shallow, at ${COMMIT:0:7})…"
git init -q "$DEST"
git -C "$DEST" fetch -q --depth 1 "$REPO" "$COMMIT"
git -C "$DEST" checkout -q FETCH_HEAD

if [ ! -d "$DEST/v1" ]; then
    echo "ERROR: expected $DEST/v1 after clone — repo layout may have changed." >&2
    exit 1
fi

echo "Done. $(ls "$DEST/v1" | wc -l) JSON files installed."
echo "Run: cargo test -p luna-cpu-spc700 --test tom_harte -- --ignored --nocapture"
