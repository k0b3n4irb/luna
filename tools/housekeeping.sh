#!/usr/bin/env bash
# housekeeping.sh — keep the GitHub side of the repository small:
#   * Releases: only the KEEP_RELEASES highest versions keep their page and
#     binaries. Git tags are never deleted, so an older version is rebuilt
#     from its tag (git checkout vX.Y.Z && cargo build --release -p luna-cli).
#   * Actions: workflow runs created more than RUNS_MAX_AGE ago are deleted
#     (logs and run artifacts go with them; release assets are untouched).
#
# WHY: users who do not build from source should land on a recent version,
# and a year of CI runs is noise nobody reads. See
# .claude/rules/housekeeping.md.
#
# USAGE:
#   tools/housekeeping.sh            # dry run: prints what would be deleted
#   tools/housekeeping.sh --apply    # deletes
#   KEEP_RELEASES=5 RUNS_MAX_AGE="1 month ago" tools/housekeeping.sh
#
# Needs `gh` authenticated on the repository (GH_TOKEN, or `gh auth login`).
# Deleting a release is not undoable in practice: its binaries would have to
# be rebuilt and would not carry the same checksums.
set -u

REPO="${REPO:-k0b3n4irb/luna}"
KEEP_RELEASES="${KEEP_RELEASES:-5}"
RUNS_MAX_AGE="${RUNS_MAX_AGE:-1 month ago}"

apply=0
case "${1:-}" in
  "") ;;
  --apply) apply=1 ;;
  *) echo "usage: $0 [--apply]" >&2; exit 2 ;;
esac

cutoff="$(date -u -d "$RUNS_MAX_AGE" +%F)" || exit 2
failed=0

# --- Releases: highest KEEP_RELEASES by version number, not by date ---------
all="$(gh release list --repo "$REPO" --limit 1000 --exclude-drafts \
  --exclude-pre-releases --json tagName --jq '.[].tagName')" || exit 1
drop="$(printf '%s\n' "$all" | sort -V | head -n "-$KEEP_RELEASES")"
kept="$(printf '%s\n' "$all" | sort -V | tail -n "$KEEP_RELEASES" | tr '\n' ' ')"

echo "releases kept: $kept"
dropped=0
for tag in $drop; do
  if [ "$apply" -eq 0 ]; then
    echo "would delete release $tag"
  elif gh release delete "$tag" --repo "$REPO" --yes >/dev/null; then
    dropped=$((dropped + 1))
  else
    failed=$((failed + 1))
  fi
done

# --- Actions: runs created before the cutoff --------------------------------
runs_query="repos/$REPO/actions/runs?per_page=100&created=<$cutoff"
old="$(gh api "$runs_query" --jq '.total_count')" || exit 1
echo "workflow runs created before $cutoff: $old"
removed=0
if [ "$apply" -eq 1 ]; then
  # The API pages by 100; re-query until nothing is left or a pass frees nothing.
  while :; do
    ids="$(gh api "$runs_query" --jq '.workflow_runs[].id')" || exit 1
    [ -z "$ids" ] && break
    before="$removed"
    for id in $ids; do
      if gh api -X DELETE "repos/$REPO/actions/runs/$id" >/dev/null; then
        removed=$((removed + 1))
      else
        failed=$((failed + 1))
      fi
    done
    [ "$removed" -eq "$before" ] && break
  done
fi

if [ "$apply" -eq 0 ]; then
  echo "dry run: nothing deleted (re-run with --apply)"
else
  echo "deleted: $dropped release(s), $removed run(s); failed: $failed"
fi
[ "$failed" -eq 0 ]
