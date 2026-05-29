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

The clippy default set (`all` = correctness + suspicious + complexity
+ style + perf, already enabled workspace-wide) catches a long tail of
real hazards: out-of-bounds indexing, `needless_range_loop` in the
per-pixel renderer, redundant clones in hot loops, ranges that can
never match. luna's code is dense bit-level address math, and every
warning we let pile up is one more place a genuine defect hides unseen.

What clippy will NOT catch is *semantic* error: the 2026-05-25 OAM
peek-mask bug (`& 0x21F` where the 544-byte OAM needs `% 0x220`,
commit `0ae7249`) is perfectly valid code no clippy lint covers. That
is exactly the argument for a zero-warning workspace — we can't afford
lint noise burying the findings that ARE machine-detectable. Don't
accumulate that debt again.

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
