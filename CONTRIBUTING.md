# Contributing to luna

Thanks for your interest! luna is young and moves fast; this page gets you
from clone to green tests. The deeper engineering mandates (faithful-port
method, API-first layering, HDMA pillar rules) live in
[`.claude/rules/`](.claude/rules/) — they are written for AI-agent sessions
but apply to every contribution, human or otherwise.

## Build

- Rust toolchain: pinned by [`rust-toolchain.toml`](rust-toolchain.toml)
  (edition 2024) — `rustup` picks it up automatically.
- Linux build dependencies: `libasound2-dev` (cpal → ALSA) and `libudev-dev`
  (gilrs → gamepad hotplug). Windows (WASAPI)
  and macOS (CoreAudio) need nothing extra.

```bash
git clone https://github.com/k0b3n4irb/luna && cd luna
cargo run --release -p luna-gui -- "path/to/game.sfc"   # GUI
cargo run --release -p luna-cli -- --help               # headless CLI
```

## Test setup

`cargo test --workspace` is green **out of the box** — tests that need
external data skip cleanly when it is absent. To run the full suites:

- **Golden ROM suite** (homebrew hardware tests, CI-gated):
  `tools/fetch-snes-test-roms.sh` sparse-clones the open-source corpus into
  the sibling directory `../luna_tests`. Then
  `cargo test -p luna-core --test snes_test_roms --release`.
- **Tom Harte CPU suites** (exhaustive per-instruction, `#[ignore]` by
  default): `tools/fetch-tom-harte.sh` and `tools/fetch-tom-harte-spc700.sh`
  fetch the datasets; run with `LUNA_TOM_HARTE_REQUIRE=1 cargo test ...
  --ignored`.
- **Commercial-game goldens / HDMA corpus**: need copyrighted ROMs in
  `tests/roms/` (gitignored — dump your own cartridges). Absent ROMs skip;
  they are a developer-local safety net, never a CI requirement.

## Before you commit

The canonical pre-commit sequence:

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib --bins \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

CI runs the `fmt` and `clippy` lines exactly as written. For the rest it
runs `cargo check --workspace --all-targets` instead of the two builds, the
wider `cargo test --workspace --all-features` (integration tests included),
and the golden ROM suite in release mode. The local sequence builds for
real so that a stale binary is never what you test by hand.

When the change touches the documentation (`book/`, `docs/`, `README.md`,
this file, `CHANGELOG.md`, `CLAUDE.md`, `.claude/`) or the surface it
describes (a `luna` subcommand or option, an MCP tool), also run

```bash
mdbook build book && tools/check-doc-drift.py
```

CI runs both (job `docs`). The script compares every documented claim with
its subject: relative links and anchors resolve, every quoted
`docs/…`/`tools/…`/`crates/…` path exists, the workspace version is the
CHANGELOG head and every cited `vX.Y.Z` is a tag, every subcommand and long
option of the built `luna` and every tool the MCP server lists has its place
in the guide, and an option the CHANGELOG adds is named in the guide. It
needs a built `luna` (`LUNA_BIN`, else `target/release/luna`, else
`target/debug/luna`); `--list` names the checks, and a subset runs as
`tools/check-doc-drift.py links version`.

## Conventions

- **Commits**: `type(scope): description` — e.g. `fix(ppu): ...`,
  `feat(cli): ...`, `docs: ...`. No `Co-authored-by`/tool-attribution
  trailers.
- **Branches/PRs**: branch from `develop` and open the pull request
  against `develop`. A pull request is the proposal and the review trail;
  it is never merged with GitHub's buttons or `gh pr merge` (no squash, no
  merge commit, no "Update branch"), because those create commits whose
  committer is GitHub. The maintainer lands the change on `develop` from a
  local clone, as ordinary commits under the repository's single identity.
  `main` only moves by fast-forward to a released `develop` commit, so
  nothing is ever committed on `main` directly.
- **Accuracy work**: read the matching reference implementation (ares +
  Mesen2) *first* — see
  [`.claude/rules/reference-first.md`](.claude/rules/reference-first.md) —
  and update the row in [`docs/accuracy_scorecard.md`](docs/accuracy_scorecard.md)
  in the same PR.
- **Anything a human can see or hear** (rendering, audio, GUI behaviour)
  gets validated in the GUI before merge, not just by unit tests.

## Dependencies

Dependency updates are **manual**, made by the maintainer as ordinary
commits. `cargo deny` (in CI and weekly) reports advisories, license and
source problems; acting on them is a maintainer decision. Automated
dependency bots (Dependabot, Renovate, or any app that opens pull requests
on its own) are **not allowed**: CI fails on their config files and on pull
requests opened by a bot.

## Fuzzing

Two surfaces take untrusted input: the ROM parser (with the mapper shims
behind it) and the save-state loader. Both are fuzzed (`fuzz/`, four
targets — `cartridge_parse`, `cartridge_forced`, `cartridge_to_system`,
`load_state` — weekly in CI, and on any push to `develop` or pull request
that touches the fuzzed code or `fuzz/`). Before changing `luna-cartridge`, the mapper shims or the
save-state format, a quick local run is cheap:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run cartridge_parse -- -max_total_time=120
cargo +nightly fuzz run load_state -- -max_total_time=120
```

See [`fuzz/README.md`](fuzz/README.md) for the targets, the contract they
assert, and how to replay a crash reproducer.

## Versioning & releases

### Versioning

The version tracks luna's **user-facing contract**, not the Rust API:

- **What the contract covers:** the `luna` CLI (subcommands, flags, their
  output formats), the MCP tool catalogue (names, parameters, result
  fields), `luna test` manifests, the `fbhash` values a manifest can pin,
  and the release asset names (`luna_vX.Y.Z_<os>_<arch>.zip`).
- **`major`** — a change that breaks that contract for users *in general*:
  removing or renaming a flag or MCP tool, changing a manifest key's
  meaning, changing every `fbhash`.
- **`minor`** — new features and accuracy work. Accuracy work routinely
  moves what a given ROM does at a given frame (that is the point of it),
  and a minor may correct a CLI behaviour that contradicted its own
  documentation or a sibling subcommand; such corrections are marked
  **BREAKING** in the CHANGELOG.
- **`patch`** — a hotfix on a released binary (e.g. v1.10.1).
- **Not covered:** the **save-state format** is only guaranteed to reload
  in the version that wrote it (a changed format bumps
  `SAVE_STATE_VERSION` and old states are refused with a clear error, never
  mis-loaded); the **Rust crate APIs** carry no stability promise — the
  crates are not published (`publish = false`). MCP changes are kept
  additive (new optional parameters, new tools, new result fields) within
  a major.
- **Release flow**: a release is a fast-forward of `main` to a `develop`
  commit, never a GitHub merge.
  1. On `develop`: bump `version` in the workspace `Cargo.toml` (which
     updates `Cargo.lock`), regenerate `fuzz/Cargo.lock`
     (`cargo update --manifest-path fuzz/Cargo.toml --workspace` — `fuzz/` is its own
     workspace, so its lock file does not follow the main one), and
     finalize the version's `CHANGELOG.md` section.
  2. Optional dry run: tag that commit `vX.Y.Z-rc1` and push the tag.
     `release.yml` builds it like a real release and publishes it as a
     GitHub *pre-release* carrying the notes of `X.Y.Z`. Check the four
     archives, then delete the pre-release and the tag (locally and on
     the remote); an RC tag is never kept.
  3. Wait for CI to be green on the exact `develop` commit, then
     `git push origin develop:main`. `main` is always an ancestor of
     `develop`, so this is a fast-forward; if it is not, stop — do not
     fall back to a merge.
  4. `git tag -a vX.Y.Z` on that commit, locally, and push the tag.
     `release.yml` builds and attaches the four platform zips
     (`luna_vX.Y.Z_<os>_<arch>.zip`), titles the page `luna vX.Y.Z` and fills it with that
     version's `CHANGELOG.md` section — `tools/release-notes.py vX.Y.Z`
     prints it locally.

  `develop` and `main` then point at the same commit: there is nothing to
  reconcile. The book names the assets with a `vX.Y.Z` placeholder, so no
  doc edit is needed per release.
- **Housekeeping**: only the five highest versions keep a GitHub release
  with binaries; older releases are deleted after each release, and
  Actions runs older than a month are deleted too
  (`tools/housekeeping.sh`, dry run by default). Tags are never deleted:
  an older version is rebuilt from its tag.
- **Before tagging, run the full suite locally *with* `tests/roms/`
  populated**:

  ```bash
  LUNA_SNES_TEST_DIR=<corpus> LUNA_SNES_TEST_REQUIRE=1 LUNA_GAME_TEST_REQUIRE=1 \
  LUNA_MOUSE_ROM=<opensnes>/examples/input/mouse/mouse.sfc \
  LUNA_SUPERSCOPE_ROM=<opensnes>/examples/input/superscope/superscope.sfc \
    cargo test --workspace --all-targets
  ```

  `<corpus>` is the directory `tools/fetch-snes-test-roms.sh` filled
  (`../luna_tests` by default). `LUNA_SNES_TEST_REQUIRE=1` turns a missing
  corpus, or a missing ROM of the homebrew golden suite, into a failure
  instead of a skip. `LUNA_GAME_TEST_REQUIRE=1` does the same for every
  test that needs a file the repository cannot ship: the commercial-game
  goldens, the smoke and reset tests (`tests/roms/`), the mouse and Super
  Scope tests (the two OpenSNES example ROMs named above). The
  differentials against a Mesen2 capture (GSU, DSP-1 port) are `#[ignore]`d
  manual harnesses and are not part of this run. Without them a missing or renamed ROM skips and the run
  still reads green. The commercial smoke and game goldens SKIP on CI
  (copyrighted ROMs are never committed), so a stale golden passes CI
  silently and only a local run catches it. That is exactly how the
  v1.12.0 prep caught three `tests/golden/smoke/` PNGs left un-anchored
  by the line-origin change.

## License

MPL-2.0. By contributing you agree your work is released under the same
license.
