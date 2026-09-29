# One author: the maintainer's `~/.gitconfig`. No `Co-authored-by`, no tool attribution (auto-loaded)

Every commit, merge and tag in this repository is authored **and committed**
by the identity in the maintainer's `~/.gitconfig` (`git config --global
user.name` / `user.email`), and by nothing else. The commit message is about
the *change*, never about who or what produced it.

## The rules

- **Identity: always `~/.gitconfig`.** Never pass `--author`, `-c user.name`,
  `-c user.email`, `GIT_AUTHOR_*` / `GIT_COMMITTER_*`, or a repo-local
  `user.*` override. If `git config --global user.email` is not the
  maintainer's, stop and ask.
- **No trailers of any kind that attribute the work**: no `Co-authored-by:`,
  no `Generated with …`, no `Claude-Session:` or other session / tool link —
  in commit messages, tag messages, PR descriptions, merge bodies, release
  notes or issue comments. Subject + body only, `type(scope): description`
  as in `CLAUDE.md`.
- **Never let GitHub commit on our behalf.** `gh pr merge` (any mode) and the
  web merge / "Update branch" / web-editor buttons create commits whose
  committer is `GitHub <noreply@github.com>`. Do not use them.
  - **Releases:** `main` is always an ancestor of `develop` (it is recreated
    from `main` after each release), so a release is a **fast-forward**:
    wait for `develop`'s CI to be green on the exact commit, then
    `git push origin develop:main`. Branch protection accepts it because
    that SHA already carries the required checks. Tag with `git tag -a`
    locally (the tagger is `~/.gitconfig` too). A PR may still be opened for
    the review trail; GitHub closes it as merged when `main` receives its
    commits.
  - If a fast-forward is impossible, stop and ask — do not fall back to a
    GitHub merge.
- **Before any push, check**:
  `git log origin/main..HEAD --format='%an <%ae> | %cn <%ce>' | sort -u`
  must print exactly one line, the `~/.gitconfig` identity for both.

## Why

The public history had to be rewritten twice to undo exactly this: first
GitHub-squash `Co-authored-by` trailers (v0.0.1) and a bot's commits
(2026-08), then, on 2026-09-29, four stray identities (early placeholder
e-mails), 208 commits committed by `GitHub <noreply@github.com>` through
web merges, and nine `Claude-Session:` trailers. Each rewrite costs a
force-push, retagging every release and invalidating every SHA quoted
elsewhere (partner reports cite them). The maintainer's decision
(2026-09-29): one identity, from `~/.gitconfig`, forever — part of the
project's DNA.
