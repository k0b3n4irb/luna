# luna — agent rules

## Rebuild discipline

After *every* code change in this repo, before declaring a task done or
committing, run the full workspace rebuild — **debug + release, all crates
including `luna-gui` and the binaries** — so a stale binary is never the
reason a feature appears broken at runtime.

The canonical command is:

```
cargo build --workspace --all-targets \
  && cargo build --release --workspace --all-targets
```

This catches:
- conditional `#[cfg]` paths that only compile in release.
- `luna-gui` regressions (it pulls many transitive crates; easy to miss with
  per-crate builds).
- example/test/benchmark targets that don't get hit by `cargo test`.

Run `cargo test --workspace --lib` separately when relevant — the rebuild
above does not run tests, only compiles.

When the work is purely refactoring a single crate's internals, you may
still run a per-crate build first to iterate fast, but the workspace
rebuild **must** pass before you commit.

## Linting

CI runs `cargo fmt --all --check` and `cargo clippy --workspace --all-targets
-- -D warnings`. Run both before commit.

## Tests on coprocessor / DMA / PPU edits

Anything touching `luna-bus/src/sa1.rs`, `luna-coproc/src/sa1.rs`,
`luna-dma`, or `luna-ppu` needs a full `cargo test --workspace --lib`
sweep — these crates have tight cross-coupling and silent regressions
elsewhere are common.

## ROM smoke tests

When DMA / PPU / SA-1 logic changes, screenshot Super Mario RPG via the
CLI as a quick visual regression check:

```
cargo build --release -p luna-cli
./target/release/luna state -n 30000000 --screenshot /tmp/smrpg.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

A working build should reach the sky-coloured title scene (frame ~2000+,
NMI service rate ≥80%).
