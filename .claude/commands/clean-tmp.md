---
description: Delete what luna's tests, smoke runs and diagnostics left in /tmp
allowed-tools: Bash(tools/clean-tmp.sh*)
---

Clean luna's leftovers from the temp directory (see
`.claude/rules/rebuild-discipline.md`, "After the tests"):

```bash
tools/clean-tmp.sh            # dry run first: what would go, with sizes
tools/clean-tmp.sh --apply    # then delete
```

It removes the `luna-*` / `luna_*` entries and `mesen_*` traces of `/tmp`,
and the scratchpads of Claude Code sessions opened in this repository —
only entries owned by the current user. Other projects' files in `/tmp`
are never touched. Do not run it while a test suite is still running.

Report the number of entries and the space freed.
