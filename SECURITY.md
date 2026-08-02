# Security Policy

## Supported versions

Only the latest release (see the [releases page]) receives fixes. Luna
ships prebuilt binaries; older tags are not patched retroactively.

[releases page]: https://github.com/k0b3n4irb/luna/releases

## Reporting a vulnerability

Luna parses untrusted input (ROM files, save states, firmware images),
so memory-safety or parsing issues are in scope even though the
workspace is `unsafe_code = "deny"`.

- **Preferred:** open a private report via GitHub's
  [private vulnerability reporting](https://github.com/k0b3n4irb/luna/security/advisories/new).
- **Alternative:** email `kobenairb@gmail.com` with `[luna security]`
  in the subject.

Please include the ROM / input that triggers the issue (or a synthetic
reproducer — copyrighted ROMs cannot be redistributed) and the exact
`luna` version or commit.

You can expect an acknowledgement within a week. Please do not open a
public issue for an exploitable bug before a fix has shipped.

## Supply chain

- `Cargo.lock` is committed; release binaries are built with `--locked`.
- `cargo deny` (advisories + licenses + sources) runs in CI on every
  change to the dependency graph and weekly (`deny.yml`).
- Dependency updates are **manual and deliberate**: no bot opens pull
  requests against this repository. `cargo deny` is the gate that
  surfaces a vulnerable or unlicensed dependency; acting on it is a
  maintainer decision, not an automated one.
