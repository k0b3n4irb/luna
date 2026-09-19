# Fuzzing luna's untrusted-input surface

luna takes outside bytes through two doors. A **ROM file**: the parser
scores candidate headers across offsets, turns header bytes into allocation
sizes, and strips SMC / DSP-1 tails — all driven by arbitrary bytes — and
whatever it accepts then sizes RAM and address masks inside the mapper
shims. And a **save-state** (a GUI slot file, `--load-state`, base64 over
MCP), which is decoded straight *into* a running machine. Both chains are
fuzzed here. (Not yet fuzzed: `.sym` symbol files, `luna test` TOML
manifests, `--input` scripts — all parsed by the CLI from files the user
wrote.)

## Targets

| Target | Covers |
|---|---|
| `cartridge_parse` | `Cartridge::from_bytes` — auto-detect, header scoring, SMC/firmware stripping |
| `cartridge_forced` | `Cartridge::from_bytes_forced` for all 8 `MapperKind`s (first input byte picks one) — the `--force-mapper` / GUI "load as…" path, which **skips checksum validation** and is therefore the weaker door |
| `cartridge_to_system` | parse → `Snes::try_from_cartridge` → `reset` → 256 steps: the accepted-but-malformed cart reaching the mapper shims (a header naming an unemulated chip is a clean refusal) |
| `load_state` | `Emulator::load_state` on a live machine, then 64 steps + peeks. The first input byte picks the layer: raw container, or a genuine container carrying the input as its **mapper** blob or its **core** blob — so the fuzzer gets past the version / ROM-hash gate |

**Contract under test:** any input either parses or returns an error
(`CartError` / `ApiError::SaveState`). It must never panic (out-of-bounds,
capacity overflow), never allocate unboundedly, and — for `load_state` — a
refused state must leave the machine running.

## Running

```bash
cargo install cargo-fuzz            # once
cargo +nightly fuzz run cartridge_parse                 # until Ctrl-C
cargo +nightly fuzz run cartridge_parse -- -max_total_time=300
cargo +nightly fuzz cmin cartridge_parse                # minimize the corpus
```

A crash writes a reproducer under `fuzz/artifacts/<target>/`; replay it
with `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`.

## Corpus

`corpus/` holds a **small committed seed set** (~200 KB per target,
minimized with `cmin`) so a fresh clone and CI start from real coverage
rather than from zero. Your local corpus will grow well past this — that
is expected and gitignored beyond the seeds; re-run `cmin` before
committing new seeds.

## CI

`.github/workflows/fuzz.yml` runs each target weekly (Mondays, after the
Tom Harte suites) and on any PR touching `luna-cartridge` or `fuzz/`,
with a short per-target budget — a regression net, not a discovery
campaign. Crash reproducers are uploaded as build artifacts.

## Status

First campaign, 2026-08-01 (local, cargo-fuzz 0.13.2):
**~67 million executions across the three targets, zero crashes** —
26.8M `cartridge_parse`, 39.7M `cartridge_forced`, 0.43M
`cartridge_to_system` (the slow one: it boots and steps a system per
input). This confirms the hardening the 2026-07-26 audit read in the
source (clamped size exponents, `rom_mirror` on every mapper index,
`checked_sub` on the extended header) holds under adversarial input.
