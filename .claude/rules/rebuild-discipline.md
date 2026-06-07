# Rebuild and Lint Discipline (auto-loaded)

After *every* code change in this repo, before declaring a task done or
committing, run the full workspace rebuild — **debug + release, all
crates including `luna-gui` and the binaries** — so a stale binary is
never the reason a feature appears broken at runtime.

## Enforced automatically (hook)

A `PostToolUse` hook in `.claude/settings.json` runs the **full rebuild
(debug + release, all targets) followed by clippy `--all-features`** after
*every* `*.rs` edit, asynchronously, and re-wakes on failure. This makes
"rebuild after each change" mechanical, not a thing to remember — a stale
binary can never silently survive an edit. Do not remove or weaken it.

## Canonical rebuild command

```
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets
```

Available as the `/rebuild` slash command.

This catches:

- conditional `#[cfg]` paths that only compile in release.
- `luna-gui` regressions (it pulls many transitive crates; easy to
  miss with per-crate builds).
- example / test / benchmark targets that don't get hit by `cargo test`.

Run `cargo test --workspace --lib` separately when relevant — the
rebuild above does not run tests, only compiles.

When the work is purely refactoring a single crate's internals, you
may still run a per-crate build first to iterate fast, but the
workspace rebuild **must** pass before you commit.

## Lint discipline

CI runs the following — they must pass clean before commit:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Together with the rebuild + tests, this is the canonical pre-commit
sequence:

```
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets -- -D warnings
```
