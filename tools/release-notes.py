#!/usr/bin/env python3
"""release-notes.py — the GitHub release body for a tag, from CHANGELOG.md.

    tools/release-notes.py v1.32.0 > release_notes.md

Prints the `## [1.32.0]` section of CHANGELOG.md, followed by the download
table. A pre-release tag (v1.33.0-rc1) uses the section of its final version,
so a release-candidate dry run shows the notes the real release will carry;
before that section is written, it falls back to `## [Unreleased]`.
Exits 1 when CHANGELOG.md has no section for a release version.

GitHub renders every newline of a release body as a line break, and the
changelog is wrapped at ~72 columns: paragraphs and list items are unwrapped
here so the page reflows. Fenced code blocks and tables are left as written.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# A line that opens a block of its own and so never continues the previous one.
BLOCK_START = re.compile(r"\s*([-*+] |\d+[.)] |#|\||>|```|~~~)")

# One zip per platform, named `luna_<tag>_<os>_<arch>.zip` like OpenSNES's
# `opensnes_<tag>_<os>_<arch>.zip` (release.yml builds them).
ASSETS = [
    ("Linux", "linux_x86_64", "x86_64"),
    ("Linux", "linux_arm64", "arm64 (aarch64)"),
    ("macOS", "darwin_arm64", "arm64 (Apple Silicon)"),
    ("Windows", "windows_x86_64", "x86_64"),
]


def section(changelog: str, version: str) -> list[str]:
    """The lines under `## [version]`, up to the next `## [` heading."""
    out, found = [], False
    for line in changelog.splitlines():
        if line.startswith("## ["):
            if found:
                break
            found = line.startswith(f"## [{version}]")
            continue
        if found:
            out.append(line)
    return out


def unwrap(lines: list[str]) -> list[str]:
    """Join the wrapped lines of each paragraph and list item into one line."""
    out: list[str] = []
    in_fence = False
    for line in lines:
        fence = line.lstrip().startswith(("```", "~~~"))
        prev = out[-1] if out else ""
        joins = (
            not in_fence
            and not fence
            and line.strip()
            and prev.strip()
            and not BLOCK_START.match(line)
            and not prev.lstrip().startswith(("#", "|", "```", "~~~"))
        )
        if joins:
            out[-1] = f"{prev} {line.strip()}"
        else:
            out.append(line)
        if fence:
            in_fence = not in_fence
    return out


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <tag>", file=sys.stderr)
        return 2
    tag = sys.argv[1]
    version = tag.removeprefix("v").split("-")[0]
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    notes = section(changelog, version)
    if not notes and "-" in tag:
        notes = section(changelog, "Unreleased")
    body = "\n".join(unwrap(notes)).strip()
    if not body:
        print(f"release-notes: CHANGELOG.md has no section [{version}]", file=sys.stderr)
        return 1

    print("## What's in this release\n")
    print(body)
    print("\n---\n\n## Download\n")
    print("| Platform | File | Architecture |")
    print("|----------|------|--------------|")
    for platform, suffix, arch in ASSETS:
        print(f"| **{platform}** | `luna_{tag}_{suffix}.zip` | {arch} |")
    print("\n### Quick start\n")
    print("1. Download and extract the zip for your platform")
    print("2. `luna-gui game.sfc` to play in the debugger, `luna --help` for the CLI")
    print("3. See [Install & first run](https://k0b3n4irb.github.io/luna/using/install.html)"
          " for the runtime requirements")
    return 0


if __name__ == "__main__":
    sys.exit(main())
