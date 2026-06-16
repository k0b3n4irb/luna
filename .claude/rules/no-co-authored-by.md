# No `Co-authored-by` / tool-attribution trailers (auto-loaded)

NEVER add `Co-authored-by:` trailers — or any tool/assistant attribution
(`Generated with …`, `Co-authored-by: Claude …`, etc.) — to commit
messages, PR descriptions, PR merge bodies, tag messages, or issue
comments in this repo. Authorship is the human committer's; the commit
message is about the *change*, not who/what produced it.

## What this means in practice

- **`git commit`**: write the subject + body only. No trailer block of
  `Co-authored-by:` lines. (Keep the real `type(scope): description`
  convention from `CLAUDE.md`.)
- **`gh pr merge --squash`**: GitHub auto-appends a `Co-authored-by:`
  trailer for each squashed commit's author when it generates the squash
  message. To prevent that, pass an explicit clean body:
  `gh pr merge <n> --squash --subject "<subj>" --body "<body>"` (no
  trailer), or merge with a body file that omits trailers. Do **not**
  accept the auto-generated message that carries `Co-authored-by:`.
- **PR / release / issue text**: never sign with a tool attribution
  footer.

## Why

The maintainer's released history (e.g. `v0.0.1`) had to be rewritten to
strip 9 GitHub-squash-injected `Co-authored-by: k0b3n4irb …` trailers.
That history rewrite (force-push + retag) is exactly the cost this rule
exists to avoid. Keep the log clean from the first commit.
