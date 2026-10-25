#!/usr/bin/env bash
# Delete what luna's tests, smoke runs and diagnostics leave in the temp
# directory. Dry run by default; --apply deletes.
#
#   tools/clean-tmp.sh            # list what would go, with sizes
#   tools/clean-tmp.sh --apply    # delete it
#
# What counts as luna's: every entry of $TMPDIR (default /tmp) whose name
# starts with `luna-` or `luna_` (the integration tests, the smoke runs,
# `luna wram-trace`, the reference fetches all use that prefix), the
# `mesen_*` traces the differential harnesses read, and the scratchpads of
# Claude Code sessions opened in this repository. Only entries owned by the
# current user are touched; other projects sharing the directory are not.
# Run it after a test session, not during one: a test running in parallel
# would lose its fixtures.
set -euo pipefail

APPLY=0
case "${1:-}" in
  --apply) APPLY=1 ;;
  "") ;;
  *) echo "usage: $0 [--apply]" >&2; exit 2 ;;
esac

TMP="${TMPDIR:-/tmp}"
TMP="${TMP%/}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Claude Code names a project's scratch folder after its path, `/` → `-`.
SCRATCH="$TMP/claude-$(id -u)/${REPO//\//-}"

targets=()
shopt -s nullglob
for p in "$TMP"/luna-* "$TMP"/luna_* "$TMP"/mesen_* "$SCRATCH"/*/scratchpad/*; do
  [[ -O "$p" ]] && targets+=("$p")
done

if ((${#targets[@]} == 0)); then
  echo "clean-tmp: nothing to delete under $TMP"
  exit 0
fi

du -sch "${targets[@]}" 2>/dev/null | sort -h | tail -n 30
echo "clean-tmp: ${#targets[@]} entries"

if ((APPLY)); then
  rm -rf -- "${targets[@]}"
  echo "clean-tmp: deleted"
else
  echo "dry run: nothing deleted (re-run with --apply)"
fi
