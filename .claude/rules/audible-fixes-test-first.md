# Audible / visible-rendering fixes — test BEFORE commit (auto-loaded)

For any change that affects something a human can perceive at runtime
(audio output, GUI framebuffer / sprites / colors, GUI controls,
visible HUD), the workflow is **always**:

1. Write the code change.
2. **Stop**. Don't commit yet.
3. Tell the user what to test and how (`cargo run --release -p luna-gui`,
   which ROM to load, what to listen for / look for).
4. Wait for the user to come back with "OK c'est bien" or "non, c'est pire / pareil".
5. **Only then** commit.

The reason: rendering correctness (audio waveforms, pixel layout, etc.)
is fundamentally validated by ears and eyes, not by `cargo test`. Unit
tests can confirm a single byte of the WAV header is right; they can't
tell you that a music driver is producing recognisable notes. Skipping
the user-listen step bakes incorrect behaviour into the commit history
where it's harder to bisect later.

Applies to:

- Anything touching `crates/luna-apu/`, `crates/luna-cpu-spc700/`,
  or `crates/luna-gui/src/audio.rs`.
- Anything touching `crates/luna-ppu/src/renderer.rs`,
  `crates/luna-ppu/src/ppu.rs` rendering paths, or
  `crates/luna-gui/src/app.rs` framebuffer plumbing.
- GUI keybinding / control-response changes.
- Any change you'd describe as "should look / sound different now".

Does NOT apply to:

- Pure refactors with explicit "no behaviour change" intent
  (extracting a function, renaming, splitting a file). State that
  explicitly in the commit message and CI tests are sufficient.
- Documentation (`docs/`, comments, `.md` files).
- Debug-only infrastructure (tracers that don't change emulation).

If the user explicitly says "commit as you go" or "don't bother
asking, just push through" for a specific session, that overrides this
rule — but the override is per-session, not per-repo. Default back to
test-before-commit at the start of every new session.
