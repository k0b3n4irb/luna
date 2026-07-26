## What & why

<!-- One paragraph: the change and the reason. Link the issue if any. -->

## Checklist

- [ ] Pre-commit sequence is green locally
      (`cargo build --workspace --all-targets && cargo build --release --workspace --all-targets && cargo test --workspace --lib && cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`)
- [ ] Commit subjects follow `type(scope): description`
- [ ] **Accuracy change** → the matching row/item in `docs/accuracy_scorecard.md`
      (and the relevant gap/audit doc) is updated in this PR
- [ ] **New CLI flag / MCP tool / GUI control** → documented with an example
      in `book/src/using/`
- [ ] **Visible / audible behaviour change** → validated in the GUI
      (`.claude/rules/audible-fixes-test-first.md`), not just by unit tests
- [ ] **DMA / PPU / SA-1 / coproc paths touched** → full
      `cargo test --workspace --lib` sweep + ROM smoke test run
