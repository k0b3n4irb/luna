---
description: Delete GitHub releases beyond the five highest and the one OpenSNES pins, and Actions runs older than a month
allowed-tools: Bash(tools/housekeeping.sh*), Bash(gh *)
---

Run the GitHub housekeeping for luna (see `.claude/rules/housekeeping.md`):

```bash
tools/housekeeping.sh            # dry run first
tools/housekeeping.sh --apply    # then delete
```

The script keeps the release OpenSNES pins on its `main` by itself (it
appears in "releases kept" as "pinned by …"). If it reports that it
cannot read the pin, it deletes no release: find out why before naming
the pin by hand with `PINNED=`.

Report:

1. The releases kept, and the releases deleted.
2. How many workflow runs were deleted, and the cutoff date.
3. Any failure, with its message.
