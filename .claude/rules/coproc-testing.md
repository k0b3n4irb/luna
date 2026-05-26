# Coprocessor / DMA / PPU Test Discipline (auto-loaded)

Anything touching these paths has tight cross-coupling and silent
regressions elsewhere are common:

- `crates/luna-bus/src/sa1.rs` (the bus-side SA-1 mapper)
- `crates/luna-core/src/coproc/sa1.rs` (the chip-side SA-1 state)
- `crates/luna-core/src/dma/` (DMA + HDMA controllers)
- `crates/luna-ppu/`

A change in any of these requires a full `cargo test --workspace --lib`
sweep, not just the per-crate test set. Run after the rebuild discipline
sequence has passed.

## ROM smoke test

When DMA / PPU / SA-1 logic changes, also screenshot Super Mario RPG
via the CLI as a quick visual regression check:

```
/smoke-test
```

Or run the underlying command directly:

```
./target/release/luna state -n 30000000 --screenshot /tmp/smrpg.png \
  "tests/roms/Super Mario RPG - Legend of the Seven Stars (USA).sfc"
```

A working build should reach the sky-coloured title scene (frame
~2000+, NMI service rate ≥ 80%).

For PPU compositor / color-math changes the equivalent SMW Yoshi's
House intro repro is also useful — see `/smoke-test` for the
scripted-input variant.
