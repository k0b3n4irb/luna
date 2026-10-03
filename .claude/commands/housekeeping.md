---
description: Delete GitHub releases beyond the five highest and Actions runs older than a month
allowed-tools: Bash(tools/housekeeping.sh*), Bash(gh *)
---

Run the GitHub housekeeping for luna (see `.claude/rules/housekeeping.md`):

```bash
tools/housekeeping.sh            # dry run first
tools/housekeeping.sh --apply    # then delete
```

Before `--apply`, check that the release OpenSNES pins on its `main`
(`tools/luna-test/luna.version`, in the OpenSNES repository) is not in the "would delete" list; if it
is, stop and write them a partner note first.

Report:

1. The releases kept, and the releases deleted.
2. How many workflow runs were deleted, and the cutoff date.
3. Any failure, with its message.
