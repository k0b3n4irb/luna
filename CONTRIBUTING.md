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

The canonical pre-commit sequence (also enforced in CI):

```bash
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets \
  && cargo test --workspace --lib \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Conventions

- **Commits**: `type(scope): description` — e.g. `fix(ppu): ...`,
  `feat(cli): ...`, `docs: ...`. No `Co-authored-by`/tool-attribution
  trailers.
- **Branches/PRs**: branch from `develop`, PR back to `develop`
  (squash-merged). `main` only receives release merges — including
  **Dependabot's**: it is configured with `target-branch: develop`, so
  check the base before merging a bot PR. A bump landing on `main`
  directly makes the next release PR conflict on `Cargo.lock`.
- **Accuracy work**: read the matching reference implementation (ares +
  Mesen2) *first* — see
  [`.claude/rules/reference-first.md`](.claude/rules/reference-first.md) —
  and update the row in [`docs/accuracy_scorecard.md`](docs/accuracy_scorecard.md)
  in the same PR.
- **Anything a human can see or hear** (rendering, audio, GUI behaviour)
  gets validated in the GUI before merge, not just by unit tests.

## Fuzzing

The ROM parser is the one surface that takes untrusted input, so it is
fuzzed (`fuzz/`, three targets, weekly in CI). Before changing
`luna-cartridge` or the mapper shims, a quick local run is cheap:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run cartridge_parse -- -max_total_time=120
```

See [`fuzz/README.md`](fuzz/README.md) for the targets, the contract they
assert, and how to replay a crash reproducer.

## Versioning & releases

- **Application SemVer**: `minor` = new features / accuracy improvements,
  `patch` = hotfix on a released binary (e.g. v1.10.1). There is no API
  stability promise — the crates are not published (`publish = false`).
- **Release flow**: bump `version` + finalize `CHANGELOG.md` in a PR to
  `develop`; merge `develop` → `main`; tag `vX.Y.Z` on `main`
  (`release.yml` builds and attaches the 4-platform binaries +
  checksums); then reconcile `develop` with `main`. Update the pinned
  asset names in `book/src/using/install.md` as part of the bump PR.
- **Before tagging, run the full suite locally *with* `tests/roms/`
  populated** — `cargo test --workspace --all-targets`. The commercial
  smoke and game goldens SKIP on CI (copyrighted ROMs are never
  committed), so a stale golden passes CI silently and only a local run
  catches it. That is exactly how the v1.12.0 prep caught three
  `tests/golden/smoke/` PNGs left un-anchored by the line-origin
  change.

## Publishing to crates.io

The workspace defaults to `publish = false`; a crate opts in explicitly.
**Only crates with no internal dependencies are published**, so a
consumer of a core never drags the emulator in:

| Crate | Status |
|---|---|
| `luna-cpu-spc700` | published — no internal deps |
| `luna-cpu-upd96050` | published — no internal deps |
| `luna-cpu-65c816` | **held**: depends on `luna-bus`, which today also carries ~6 000 lines of Super FX / SA-1 / S-DD1 chip logic. Publishing it would export that shape to the ecosystem, and crates.io versions are forever. Unblocked by splitting the coprocessors out of `luna-bus` (see `ARCHITECTURE.md`). |
| everything else | internal (`luna-core` is also taken on crates.io by an unrelated crate) |

Release order for a published crate:

```bash
cargo publish --dry-run -p <crate>   # packages + verifies in isolation
cargo publish -p <crate>             # irreversible: a version is never reusable
```

Publish **after** the git tag, from `main`, so the published source
matches a released commit. A published crate keeps its own `README.md`
(crates.io shows it, and a core's story is not the emulator's).

## License

MPL-2.0. By contributing you agree your work is released under the same
license.
