---
paths:
  - "crates/**"
  - "Cargo.toml"
  - "Cargo.lock"
---
# Rebuild and lint discipline (loaded when working on code)

A stale binary or a lint left behind must never be why something looks
broken, or why CI is red.

## The pre-commit sequence

```
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib --bins \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`/rebuild` runs the two builds. `--bins` matters: `luna-cli` and `luna-gui`
are binary crates. CI runs `cargo check` and the wider `cargo test
--workspace --all-features` (the golden corpus, a debug build locally).
fmt and clippy are the exact CI commands. Never verify through a pipe
(`| tail` zeroes the exit code). After a push, follow the CI run to green.

## Two helpers, neither is a gate

- The `PostToolUse` hook of `.claude/settings.json` rebuilds (debug +
  release) and runs clippy after a `*.rs` edit, under the
  `heavy-jobs.lock` flock, with `-j2`: a lock place bounds how many heavy
  jobs run, not their memory, and an incremental build without `-j` peaked
  at 4 108 MiB (111 s) against 1 919 MiB (56 s) with it (2026-11-10). It is asynchronous (it only re-wakes on
  failure) and sees only Edit/Write/MultiEdit: an edit through the shell
  (sed, python, git checkout) bypasses it, and it never blocks a commit.
  After a shell edit, `cargo build -p luna-gui` explicitly.
- **The blocking gate** (2026-11-04, after a clippy lint kept CI red 16 h):
  `tools/pre-commit-gate.sh` runs fmt and the CI-form clippy (about 15 s
  warm) and refuses the commit. Install once:
  `ln -sf ../../tools/pre-commit-gate.sh .git/hooks/pre-commit`.

## Lint: the whole workspace stays clean

1. Fix the code, not the lint.
2. A lint wrong for our case gets a module-scoped `#![allow(...)]` at the
   impl boundary with a one-line rationale, not scattered item-level
   allows.
3. Never `#[allow(warnings)]`, `#[allow(clippy::all)]` or
   `#[allow(dead_code)]` to make it pass.
4. `cargo clippy --fix` only for mechanical findings.
5. `--all-features` is what reaches the conditional code (`test-utils`,
   `atomic-oracle`).

Clippy cannot see semantic errors (the `& 0x21F` OAM mask where `% 0x220`
was needed): zero warnings only keeps the machine-detectable ones from
burying them.

## After the tests: clean `/tmp`

Tests, smoke runs and reference fetches leave hundreds of entries in
`/tmp`. At the end of a test session, `tools/clean-tmp.sh` (dry run) then
`--apply` (`/clean-tmp`) deletes luna's own (`luna-*`, `luna_*`,
`mesen_*`, this repository's scratchpads). Write by hand only under a
`luna-` prefix or in the scratchpad. Never while a suite is running.
