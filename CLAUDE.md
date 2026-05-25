# luna — agent rules

## Reference-first implementation

Before writing or rewriting **any** SNES subsystem feature — bus
dispatch, CPU opcode timing, PPU register, DMA mode, DSP envelope,
SA-1 coprocessor logic, joypad scan, etc. — consult the
corresponding implementation in both reference emulators **and
understand it fully** before touching luna code.

The two canonical references:

* **ares** — `https://raw.githubusercontent.com/ares-emulator/ares/master/ares/sfc/...`
  Gold standard for hardware accuracy.
* **Mesen2** — `https://raw.githubusercontent.com/SourMesen/Mesen2/master/Core/SNES/...`
  Independent second source; cross-check when ares is unclear.

Workflow per feature:

1. **Fetch** the relevant files from both repos (`curl -s`) into
   `/tmp/` for diffing. For a directory listing use the GH API:
   `gh api repos/ares-emulator/ares/contents/ares/sfc/<subsys> --jq '.[].name'`.
2. **Read** the actual source — register decoders, state machines,
   bit layouts. Quote line numbers when summarising.
3. **Both references must agree** on the semantic before luna
   adopts it. When they diverge, document the discrepancy and
   pick the one with more clarity (usually ares' verified
   behaviour).
4. **Write up a short spec** to `/tmp/<feature>_reference.md`:
   register table, state-transition diagram, edge cases. This
   is the diff target.
5. **Inventory** what luna currently does (`Explore` agent works
   well for this) into `/tmp/luna_<feature>_inventory.md`.
6. **Then** implement against the spec — never from memory or
   from `fullsnes.htm` paraphrases alone. The bit layouts and
   timing quirks differ between secondary docs and what real
   hardware does; ares + Mesen2 are the empirical truth.

Patches that skip this step have caused real regressions in the
luna history (the SA-1 CCNT bit-5 vs bit-7 inversion, the CC1/CC2
cdsel inversion, the echo FIR half-scale precision bug). Always
read the source first.

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

## Controller bindings (GUI)

Player 1 keyboard layout, wired in `luna-gui/src/app.rs`:

| Keyboard | SNES button | JOY1 bit |
|---|---|---:|
| `Z`        | B      | 15 |
| `A`        | Y      | 14 |
| Right `Shift` | Select | 13 |
| `Enter`    | Start  | 12 |
| `↑` `↓` `←` `→` | D-pad | 11..8 |
| `X`        | A      | 7 |
| `S`        | X      | 6 |
| `Q`        | L      | 5 |
| `W`        | R      | 4 |

The SNES auto-read latch fires once per VBlank (line 225 NTSC, 240 PAL)
when `NMITIMEN.0` is set; the same pulse also re-arms the manual-mode
$4016/$4017 shift register (per ares' `controllerPort.latch()`). Real
hardware physically locks out conflicting D-pad directions (Up + Down,
Left + Right) — luna drops both opposing bits when the auto-read
latches.

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
