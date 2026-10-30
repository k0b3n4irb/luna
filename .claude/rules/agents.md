# The agent house — each task to the agent whose model suffices (auto-loaded)

The main session runs `opusplan` (`.claude/settings.json`): Opus while it
plans, Sonnet while it carries out. It **routes**: it understands the
request, cuts it up, delegates, checks what comes back, and keeps the
decision and the synthesis. Doing everything itself on the strongest model
costs most where it buys least — in the context re-read at every answer of
a long conversation.

| The task | The agent | Model |
|---|---|---|
| commit, push of `develop`, single-author check, `/tmp` cleanup | `steward` | Haiku |
| a test suite or a check and its verdict, CI runs, a long log | `watcher` | Haiku |
| CHANGELOG, the guide, a gap row or the scorecard, a rule | `scribe` | Sonnet |
| a fix or a small feature whose cause or shape is known (CLI, MCP, API, tooling) | `craftsman` | Sonnet |
| a review of fresh code before commit | `rust-code-improver` | as its file sets |
| an accuracy or timing bug, a faithful port, a bisection, a coprocessor, HDMA/DMA, the plan of a lot | `architect` | Opus |
| a wide read (a reference subsystem, a long trace, many files) | `Explore` (built in) | light |

## Routing rules

- **Start light, step up when needed.** An agent that stops — the cause is
  different, the change grows, it touches emulation timing — hands back its
  diagnosis, and the task goes one step up. Never the other way, silently.
- **Keep the conversation short.** Wide reads go to sub-agents and only
  their conclusion comes back; a finished lot is closed, and the next starts
  from a fresh context (the CHANGELOG, the docs and the memory carry what
  must last).
- **The project's rules bind every agent**: the faithful-port method,
  reference-first, API-first, the rebuild and lint discipline, one author,
  the public showcase (nothing private in a tracked file). What a person
  must see or hear is still judged by the owner
  (`audible-fixes-test-first.md`).
- **Quality is proven by the usual checks** (the pre-commit sequence, the
  golden and CPU suites, the doc-drift check), whoever wrote the code. An
  agent that does worse than the main session would have is reported, with
  the case, and its tier changes.

## One lot per conversation; the architect defaults to Sonnet (2026-10-30)

- A finished lot ends the conversation: write the resume note, commit, push, and end the turn. Do not chain the next lot in the
  same conversation (the context re-read on every turn is the first cost).
- The `architect` agent runs on Sonnet by default. Ask for Opus explicitly (`model: opus` in the Agent call) only after two failed
  plans or for a genuinely hard call; never to execute.
