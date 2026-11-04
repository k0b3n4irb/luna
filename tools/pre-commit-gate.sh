#!/bin/sh
# Blocking local gate: the two CI checks that are cheap (about 15 s warm),
# run before a commit that stages Rust or Cargo files. The rebuild hook of
# .claude/settings.json is asynchronous and only sees Edit/Write tool calls,
# so it cannot stop a commit; this can.
# Install once:  ln -sf ../../tools/pre-commit-gate.sh .git/hooks/pre-commit
git diff --cached --name-only | grep -Eq '\.rs$|(^|/)Cargo\.(toml|lock)$' || exit 0
cargo fmt --all --check || { echo "pre-commit: cargo fmt --all --check failed" >&2; exit 1; }
cargo clippy --workspace --all-targets --all-features -- -D warnings \
  || { echo "pre-commit: clippy (the CI form) failed" >&2; exit 1; }
