---
description: Full workspace rebuild — debug + release, all crates and targets
allowed-tools: Bash(cargo *)
---

Run the canonical rebuild discipline command for luna:

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets
```

This is mandatory after every code change before declaring a task done.
See `.claude/rules/rebuild-discipline.md` for the rationale.

After the rebuild completes, report:

1. Whether both profiles built successfully.
2. Any warnings (clippy or otherwise) that appeared.
3. Build time for each profile.
