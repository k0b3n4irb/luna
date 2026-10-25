#!/usr/bin/env bash
# Fetch the Tom Harte 65C816 ProcessorTests dataset into tests/tom-harte/.
#
# Usage: tools/fetch-tom-harte.sh
#
# The dataset (~600 MB uncompressed) is *not* committed. It lives at
# https://github.com/SingleStepTests/65816. We do a shallow clone to
# keep the download under control.

set -euo pipefail

# The dataset is pinned: upstream corrects its cases now and then, and a
# floating HEAD changed the gate under CI without a commit here (2026-10-16
# for the 65C816). Moving the pin is a commit, and it changes the CI cache
# key, which hashes this file. Delete the local copy to fetch a new pin.
COMMIT="307c63f5673c8c6b861c52f71ddb89212a227480" # 2026-10-16
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
echo "Cloning $REPO into $DEST (shallow, at ${COMMIT:0:7})…"
git init -q "$DEST"
git -C "$DEST" fetch -q --depth 1 "$REPO" "$COMMIT"
git -C "$DEST" checkout -q FETCH_HEAD

if [ ! -d "$DEST/v1" ]; then
    echo "ERROR: expected $DEST/v1 after clone — repo layout may have changed." >&2
    exit 1
fi

echo "Done. $(ls "$DEST/v1" | wc -l) JSON files installed."
echo "Run: cargo test -p luna-cpu-65c816 --test tom_harte -- --ignored --nocapture"
