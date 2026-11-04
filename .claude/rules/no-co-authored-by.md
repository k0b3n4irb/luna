# One author, no trailers, GitHub never commits (auto-loaded)

Every commit, merge and tag is by the `~/.gitconfig` identity, with no
`Co-authored-by`, `Generated with…` or session trailer, in commits, tags,
PRs, merge bodies or release notes. Never `--author`, `-c user.*`,
`GIT_AUTHOR_*` / `GIT_COMMITTER_*`. Subject: `type(scope): description`.

- **Never let GitHub commit for us**: no `gh pr merge`, no web merge or
  "Update branch" (committer `GitHub <noreply@github.com>`). A release is
  a fast-forward: `main` is an ancestor of `develop`; with `develop`'s CI
  green on that exact commit, `git push origin develop:main`, then
  `git tag -a` locally. Impossible fast-forward: stop and ask.
- **Before a push**: `git log origin/main..HEAD --format='%an <%ae> | %cn <%ce>' | sort -u`
  prints exactly one line.
- Why: the public history was rewritten twice (squash trailers, a bot, stray
  identities, 208 web-merge commits, `Claude-Session:` trailers). Each
  rewrite costs a force-push and invalidates every SHA quoted elsewhere.
- Commit with `git commit -F -` and a quoted heredoc (`<<'EOF'`): an
  unquoted backtick in `-m` executed a command once.
