# GitHub housekeeping — five releases, one month of Actions runs (auto-loaded)

The GitHub side of the repository is kept small, as part of the routine:

- **Releases:** only the **five highest versions** (by version number, not
  by publication date) keep their page and binaries. Older releases are
  deleted. **Git tags are never deleted**: an old version is rebuilt from
  its tag.
- **Actions:** workflow runs **older than one month** are deleted.

Maintainer's decision (2026-10-03): a user who wants an old version can
build it; our role is to move users who do not build onto recent versions.

## How

```
tools/housekeeping.sh            # dry run
tools/housekeeping.sh --apply    # deletes
```

`gh` must be authenticated (the fine-grained token is enough).

## When

- **After every release**, once the new tag's assets are verified.
- **During ordinary work**, whenever the dry run shows something to delete
  (runs age out every day). Run the dry run first and read it.

This standing instruction covers the script's defaults only. Any other
deletion on GitHub (a tag, a branch, a kept release, a different
threshold) still needs an explicit yes.

## Before a release leaves the list

OpenSNES downloads a pinned luna release
(`tools/luna-test/luna.version` in their repository). When the version
their `main` pins is about to become the sixth, tell them **before**
deleting it, in a partner note.

## What stays forbidden

- `gh release delete --cleanup-tag` on a published version.
- A scheduled GitHub workflow doing this on its own: what *acts* on the
  repository needs an explicit yes (see `no-dependency-bots.md`).
