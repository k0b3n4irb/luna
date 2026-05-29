# Rust lint discipline — full clippy sweep before every commit (auto-loaded)

luna is a Rust workspace. The clippy gate **already in
`rebuild-discipline.md`** is the bare minimum (`cargo clippy --workspace
--all-targets -- -D warnings`). This rule tightens it: every code
change in this repo MUST keep the **entire workspace lint-clean** —
no warnings, no skipped lints, no localised `#[allow]` shortcuts.

## What "lint-clean" means here

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`--all-features` is what catches lints in conditional code paths
(test-utils, debug-only counters, etc.). The standard gate omits it
because it's not what CI runs by default; this rule says: **run it
locally before committing anyway**.

When a clippy finding shows up:

1. **Fix the code, not the lint.** Refactor, rename, simplify — make
   the offending construct genuinely unnecessary.
2. If the lint is *empirically wrong* for our case (e.g.
   `too_many_arguments` on a deliberate trait-object boundary),
   prefer **module-scoped** suppression at the impl boundary (
   `#![allow(clippy::too_many_arguments)]` at the top of the file)
   over scattered `#[allow]` annotations on individual items. State
   the rationale in a one-line comment above the attribute.
3. Never suppress with `#[allow(warnings)]`, `#[allow(clippy::all)]`,
   or `#[allow(dead_code)]` to "make it pass". Those are tells that
   the underlying issue wasn't understood. The reviewer should be
   able to reason about each suppression individually.
4. `cargo fix --clippy` and `cargo clippy --fix` are fine for the
   purely mechanical findings (unused imports, redundant pattern
   binders, etc.). For everything else, hand-edit so the diff stays
   reviewable.

## Why this matters here

The clippy default set already catches a long tail of correctness
hazards (off-by-one in slice math, unhandled `Result`s smuggled past
`unwrap`, redundant clones inside hot loops). luna's SNES emulation
is full of bit-level address math where a single warning we ignored
can become a deadlock investigation two weeks later — see the
2026-05-25 OAM peek-mask bug (commit `0ae7249`) which would have been
flagged by `clippy::nonsensical_const_eval` had we kept the workspace
clean. Don't accumulate that debt again.

## Pre-commit sequence (extends `rebuild-discipline.md`)

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

This is now THE canonical sequence. The `--all-features` clippy step
supersedes the one in `rebuild-discipline.md`; that older line stays
in place for CI compatibility, but locally the `--all-features` form
is what we run.

If any step fails, fix it — don't `--no-verify` past it. The hooks /
lint gates exist because every previous time we skipped them, we
spent more time later untangling the consequence than it would have
cost to fix forward.
