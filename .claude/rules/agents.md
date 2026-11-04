# The agent house — each task to the agent whose model suffices (auto-loaded)

The main session routes: it understands the request, cuts it up,
delegates, checks, and keeps the decision. Sonnet by default, Opus for a
fidelity diagnosis; one lot per conversation (resume note, commit, push,
end the turn).

| The task | The agent | Model |
|---|---|---|
| commit, push of `develop`, single-author check, `/tmp` cleanup | `steward` | Haiku |
| a test suite or check and its verdict, CI runs, a long log | `watcher` | Haiku |
| CHANGELOG, the guide, a gap row or the scorecard, a rule | `scribe` | Sonnet |
| a fix or small feature whose cause or shape is known | `craftsman` | Sonnet |
| a review of fresh code before commit | `rust-code-improver` | as its file sets |
| an accuracy or timing bug, a faithful port, a bisection, a coprocessor, HDMA/DMA, the plan of a lot | `architect` | Sonnet; Opus only after two failed plans or a genuinely hard call |
| a wide read (a reference subsystem, a long trace, many files) | `Explore` | light |

- Start light, step up when needed; an agent that stops hands back its
  diagnosis and the task goes one step up, never silently down.
- Every agent is bound by the project's rules (faithful port,
  reference-first, API-first, rebuild/lint, one author, public showcase).
- An agent's "all green" is re-run by the session before it is believed.
