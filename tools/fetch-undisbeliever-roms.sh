#!/usr/bin/env bash
# Build the hardware-glitch test ROMs of undisbeliever/snes-test-roms (Zlib)
# that the `luna strict` early-latch rules R10 / R11 are checked against
# (crates/luna-cli/tests/strict_undis.rs).
#
# The ROMs are not vendored and not downloaded as binaries: upstream ships
# sources, so this script clones them at a pinned commit, builds the pinned
# assembler (bass-untech, ISC) and assembles ONLY the ROMs the tests name,
# into a git-ignored directory. It prints each ROM's SHA-256 so a rebuild can
# be compared with the one the tests were written against.
#
# Needs: git, g++ and make (the assembler), Python 3 with Pillow (the
# resource converters of the upstream Makefile).
#
# Usage: tools/fetch-undisbeliever-roms.sh
# Override the destination with LUNA_UNDIS_TEST_DIR (the tests read the same
# variable; default: .luna_tests_undis at the root of this repository).

set -euo pipefail

REPO="https://github.com/undisbeliever/snes-test-roms.git"
# Pinned upstream commit and the submodule it records (bass-untech): an
# upstream edit must never silently change what the tests judge. Bump both
# deliberately and re-run the tests.
REV="ac6ef8006809c0ff5dabc8ff137a4623967697fe"
BASS_REPO="https://github.com/undisbeliever/bass-untech.git"
BASS_REV="9db6088a378061afc7b82f50997a5b9a1d951175"

# The ROMs the tests name (paths under src/hardware-glitch-tests/, no .asm).
ROMS=(
    inidisp-early-read-glitch/inidisp-hammer-0f8f
    inidisp-early-read-glitch/inidisp-hammer-0f
    inidisp-early-read-glitch/inidisp-hammer-0f-long
    inidisp-early-read-glitch/inidisp-hammer-long-0f2100
    inidisp-early-read-glitch/inidisp-hammer-0f00
    inidisp-early-read-glitch/inidisp-hammer-0f0f
    inidisp-early-read-glitch/inidisp-hammer-8f0f
    inidisp-early-read-glitch/inidisp-d7-glitch-test
    setini-early-read-mode7ex
    setini-early-read-obj
)

DEST="${LUNA_UNDIS_TEST_DIR:-$(cd "$(dirname "$0")/.." && pwd)/.luna_tests_undis}"
SRC="$DEST/.src"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: '$1' is required ($2)" >&2
        exit 1
    }
}
need git "to fetch the sources"
need make "to build"
need g++ "to build the bass-untech assembler"
need python3 "the upstream resource converters"
python3 -c 'import PIL' 2>/dev/null || {
    echo "error: the Python module Pillow is missing (the upstream Makefile" >&2
    echo "       converts its PNG resources with it): pip install Pillow" >&2
    exit 1
}

mkdir -p "$DEST"
if [ ! -d "$SRC/.git" ]; then
    echo "Fetching $REPO @ ${REV:0:12} into $SRC"
    rm -rf "$SRC"
    git init -q "$SRC"
    git -C "$SRC" remote add origin "$REPO"
fi
git -C "$SRC" fetch -q --depth 1 origin "$REV"
git -C "$SRC" checkout -q --detach FETCH_HEAD

if [ ! -d "$SRC/bass-untech/.git" ]; then
    echo "Fetching $BASS_REPO @ ${BASS_REV:0:12}"
    rm -rf "$SRC/bass-untech"
    git init -q "$SRC/bass-untech"
    git -C "$SRC/bass-untech" remote add origin "$BASS_REPO"
fi
git -C "$SRC/bass-untech" fetch -q --depth 1 origin "$BASS_REV"
git -C "$SRC/bass-untech" checkout -q -f --detach FETCH_HEAD

# The pinned bass-untech has a use-after-free in three statements of
# core/execute.cpp, `setDefine(p(0), {}, p(1), level)` and its `evaluate` and
# `variable` siblings: `p(1)` appends to the vector that `p(0)` references, so
# a compiler that evaluates the arguments left to right (GCC 16 does, on
# aarch64 at least) hands over a dangling name and every valueless `define`
# silently vanishes ("Rom block code does not exist"). Forcing the append
# first is order-independent and changes nothing else; the patch is applied
# to the throw-away clone only, and the build fails if the upstream text is
# not exactly the expected one.
BASS_EXEC="$SRC/bass-untech/bass/core/execute.cpp"
sed -i -E '/auto p = s\.trimLeft\("(define|evaluate|variable) ", 1L\)\.split\("=", 1L\)\.strip\(\);$/ s/$/ p(1);/' "$BASS_EXEC"
[ "$(grep -c 'strip(); p(1);$' "$BASS_EXEC")" = 3 ] || {
    echo "error: bass-untech execute.cpp is not the pinned text, the dangling-reference patch did not apply" >&2
    exit 1
}

# Refuse a tree that is not exactly the pinned one (an interrupted run).
[ "$(git -C "$SRC" rev-parse HEAD)" = "$REV" ] || { echo "error: wrong snes-test-roms revision" >&2; exit 1; }
[ "$(git -C "$SRC/bass-untech" rev-parse HEAD)" = "$BASS_REV" ] || { echo "error: wrong bass-untech revision" >&2; exit 1; }

targets=()
for r in "${ROMS[@]}"; do
    targets+=("bin/hardware-glitch-tests/$r.sfc")
done
{ make -C "$SRC" directories && make -C "$SRC" -j"$(nproc)" "${targets[@]}"; } >"$DEST/build.log" 2>&1 || {
    echo "error: the build failed, see $DEST/build.log" >&2
    tail -20 "$DEST/build.log" >&2
    exit 1
}

echo "ROMs in $DEST (SHA-256):"
for r in "${ROMS[@]}"; do
    name="$(basename "$r").sfc"
    cp "$SRC/bin/hardware-glitch-tests/$r.sfc" "$DEST/$name"
    printf '  %s  %s\n' "$(sha256sum "$DEST/$name" | cut -d' ' -f1)" "$name"
done
echo "Done. Run: LUNA_UNDIS_TEST_REQUIRE=1 cargo test -p luna-cli --test strict_undis"
