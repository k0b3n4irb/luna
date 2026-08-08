# Developing homebrew with luna — `luna test`

Homebrew developers have had no serious CI story: the classic loop is
"build, open an emulator, eyeball it". `luna test` (issue #181) turns
that into a declarative suite a pipeline can run in seconds — one TOML
manifest per test, executed in-process against the same `luna-api`
surface the GUI and MCP use, with the CLI's exit-code contract:

| Exit | Meaning |
|---|---|
| `0` | Every manifest's asserts passed. |
| `1` | At least one assert failed. |
| `2` | Manifest / usage error (bad TOML, missing ROM, no manifests found). |

```
luna test [PATHS...] [--update] [--only SUBSTR] [--report json]
```

`PATHS` are manifest files, or directories scanned recursively for
`*.toml` (default: `./tests`).

## The manifest

```toml
# tests/boot.toml — "the game boots and reports ready"
rom = "../build/game.sfc"      # relative to this manifest
sym = "../build/game.sym"      # optional (a beside-ROM .sym auto-loads)
force_mapper = "lorom"         # optional — headerless/WIP images
frames = 600                   # run bound: `frames` or `steps` (exactly one)
input = "300:0x1000,310:0"     # optional joypad script, or "@inputs/boot.txt"
screenshot = "artifacts/boot.png"  # optional artifact, written after the run

[asserts]
wdm_empty = true               # SNES_ASSERT never fired (WDM channel silent)
nocash_contains = "BOOT OK"    # the $21FC TTY printed this
fbhash = "7429bf441a1c7d6c"    # displayed-frame hash — see below

[asserts.values]               # loaded symbol (or "BANK:OFFSET") = expected
r_game_state = 0x02            # ≤ 0xFF checks one byte…
r_score = 0x2EE0               # …larger values check a little-endian u16
```

What each assert means:

- **`wdm_empty`** — the SDK's `SNES_ASSERT` macro executes `WDM $00`;
  an empty log after the run is the "no assertions fired" green light.
- **`nocash_contains`** — the `$21FC` Nocash TTY is the ROM's printf
  channel (`SNES_NOCASH("...")`); assert on any marker text it prints.
- **`fbhash`** — the 64-bit displayed-frame hash (the same
  cross-arch-stable value `luna state --print-fbhash` emits — *not*
  the golden suite's SHA-256). After an **intended** render change, run
  `luna test --update` to regenerate every manifest's `fbhash` in
  place; formatting and comments are preserved.
- **`[asserts.values]`** — read WRAM through the loaded symbol table
  (or a literal `"7E:0100"` hex pair) and compare.

Input scripts use exactly the `--input` grammar (`frame:mask`, `#`
comments, `@file`), so a recording exported from the GUI or captured
over MCP (`take_input_capture`) replays verbatim. Checkpoints spend
from the same budget as the run bound (issue #126 semantics).

## A GitHub Actions recipe

Copy this into a homebrew repo — it builds the ROM, fetches a pinned
luna release binary (no Rust toolchain), and runs the suite:

```yaml
name: test
on: [push, pull_request]
jobs:
  luna-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build the ROM
        run: make        # your wla-dx build
      - name: Install luna
        run: |
          curl -sL -o luna.tar.gz \
            https://github.com/k0b3n4irb/luna/releases/latest/download/luna-linux-amd64.tar.gz
          tar xzf luna.tar.gz && sudo install luna /usr/local/bin/
      - name: Run the test suite
        run: luna test tests --report json
      - name: Upload screenshots
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: luna-artifacts
          path: tests/artifacts/
```

`--report json` appends a machine-readable summary (per-test pass/fail,
failure details, measured `fbhash`) to stdout for dashboards or PR
comments.

## Tips

- Keep one manifest per behaviour ("boots", "menu reachable", "level 1
  completable") — `--only level1` runs a subset while iterating.
- A **black screenshot is not a failed test**: commercial-style intros
  sit in forced blank waiting for Start. Drive them with `input` (the
  same lesson as the smoke-test corpus).
- For deeper debugging of a failing test, replay the same ROM + input
  under `luna state` with traces, or over MCP with the interactive
  tools — every assert here reads the same `luna-api` state they do.
