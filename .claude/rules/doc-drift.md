# Documentation drift — the docs are checked against the tree (auto-loaded)

The guide (`book/src/`), `docs/`, `README.md`, `CONTRIBUTING.md`,
`CLAUDE.md` and `.claude/` are claims about the luna in the tree, and
`tools/check-doc-drift.py` verifies them mechanically. CI runs it (job
`docs` in `ci.yml`) after building `luna-cli` and the guide.

## What it checks

| Check | Claim | Subject |
|---|---|---|
| `links` | every relative link and `#anchor` in the guide, `docs/`, `README.md`, `CONTRIBUTING.md` | the file, and its headings (mdBook ids) |
| `orphans` | every page under `book/src/` | `SUMMARY.md` |
| `paths` | every backticked (or extension-bearing) `docs/…`, `tools/…`, `crates/…`, `book/…`, `tests/…`, `fuzz/…`, `.claude/…`, `.github/…` path in `CLAUDE.md`, `.claude/`, `CONTRIBUTING.md`, the guide | the tree (and no dependency-bot config exists) |
| `version` | the workspace `version`, every `vX.Y.Z` cited in the guide, `README.md`, `CONTRIBUTING.md` | the `CHANGELOG.md` head, `git tag` |
| `cli` | every subcommand has its `### \`luna <cmd>\`` section in the CLI reference; every long option is named in the guide | `luna --help`, `luna <cmd> --help` |
| `mcp` | every MCP tool is named in the guide's catalogue section | the tool list `luna mcp` serves over stdio |
| `index` | every `--option`, `luna <cmd>` and `snake_case` tool name in the task index (`book/src/task-index.md`); every subcommand has a task there | `luna <cmd> --help`, the MCP tool list |
| `changelog` | every `--option` added under `[Unreleased]` or the head version | the guide |

Fenced code blocks are examples, not claims: paths and links inside them
are not checked. Globs (`docs/luna_*_gaps.md`), templates (`<rom>`) and
`tests/roms/` (gitignored) are skipped. A path that must *not* exist
(`.github/dependabot.yml`) fails the check when it appears.

## When to run it

- Before committing anything under `book/`, `docs/`, `.claude/`, or
  `CLAUDE.md` / `CONTRIBUTING.md` / `README.md` / `CHANGELOG.md`.
- After adding or renaming a `luna` subcommand or option, or an MCP tool:
  the guide section comes in the same commit. A new option lands with a
  what/why and an example in `book/src/using/`, not just a CHANGELOG line.

```bash
cargo build --release -p luna-cli      # the cli/mcp checks ask the binary
mdbook build book && tools/check-doc-drift.py
```

## When it goes red

Fix the claim or the subject, never the check: a stale path is renamed
to the real one (or the sentence loses the path, keeps its meaning), a
missing option gets its row and example, a vanished heading gets its
link updated. Adding a skip to the script needs a reason in the commit
message. The script is mutation-tested by hand when a check is added
(plant the drift, see red, remove it, see green).

## Why

OpenSNES's documentation (2026-10-05 review) stays true because
`check_doc_drift.py` runs in their CI: version macros, example counts,
quoted paths, quoted prototypes, every build knob documented. Their luna
reference page is generated from our `--help` and checked against the pin.
Our guide had none of that. The first run of this script found a stale
OpenSNES path in two rules, a skills folder under `.claude/` that never existed,
an old asset name in the install page, and `luna test` absent from the
CLI reference. (Its first version also flagged eighteen anchors in the
architecture page: mdBook maps every space to a dash, the script
collapsed them. The anchors were right; the script was fixed, and checked
against a real `mdbook build`.)
