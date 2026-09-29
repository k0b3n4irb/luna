# No dependency bots — permanently (auto-loaded)

NEVER add, enable, suggest or merge anything from an automated dependency
bot in this repository: Dependabot, Renovate, or any app that opens pull
requests, pushes branches or bumps versions on its own.

## What this means in practice

- **No config file**: no `.github/dependabot.yml`, no `renovate.json` or
  its variants. CI fails if one appears (`ci.yml`, job `fmt`, step
  "No dependency bots").
- **No repository setting**: the bot's alerts, security updates and
  automated fixes stay **disabled** on GitHub. Do not enable them, and do
  not call the API that does.
- **No bot pull request**: CI fails on a PR whose author is a bot. Never
  merge one, even with the check bypassed.
- **Dependency updates are manual.** `cargo deny` (CI + weekly) *surfaces*
  advisories; acting on one is the maintainer's call, made as an ordinary
  commit (`fix(deps): …` / `chore(deps): …`) by the maintainer's identity.
- **Do not re-propose it.** "Keep dependencies current automatically" is
  not a gap to fill. If an advisory needs action, say so and wait.

## Why

A bot arrived once without being asked for (bundled into the 2026-07
supply-chain lot), opened pull requests under its own identity, and
landed in the repository's contributors. Removing it took a history
rewrite, a force-push and a GitHub support request. The maintainer has
banned it for good (2026-09-29). This extends the general rule: what
*observes* (CI, `cargo deny`) may be added in a lot; what *acts* on the
repository (opens PRs, merges, pushes) needs an explicit yes, and for
dependency bots the answer is already no.
