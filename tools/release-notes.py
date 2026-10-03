#!/usr/bin/env python3
"""release-notes.py — the GitHub release body for a tag, from CHANGELOG.md.

    tools/release-notes.py v1.32.0 > release_notes.md

Prints the `## [1.32.0]` section of CHANGELOG.md, followed by the download
table. A pre-release tag (v1.33.0-rc1) uses the section of its final version,
so a release-candidate dry run shows the notes the real release will carry.
Exits 1 when CHANGELOG.md has no section for the version.

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

ASSETS = [
    ("Linux", "x86_64", "luna-linux-x86_64.tar.gz"),
    ("Linux", "aarch64", "luna-linux-aarch64.tar.gz"),
    ("Windows", "x86_64", "luna-windows-x86_64.zip"),
    ("macOS", "Apple Silicon (arm64)", "luna-macos-aarch64.tar.gz"),
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
    notes = section((ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), version)
    body = "\n".join(unwrap(notes)).strip()
    if not body:
        print(f"release-notes: CHANGELOG.md has no section [{version}]", file=sys.stderr)
        return 1

    print("## What's in this release\n")
    print(body)
    print("\n---\n\n## Download\n")
    print("| Platform | Architecture | File |")
    print("|---|---|---|")
    for platform, arch, name in ASSETS:
        print(f"| **{platform}** | {arch} | `{name}` |")
    print(
        "\nEach archive holds `luna` (the headless CLI) and `luna-gui`, and has a"
        " `.sha256` beside it. The same files also exist under a versioned name"
        f" (`luna-v{tag.removeprefix('v')}-<os>-<arch>`)."
        " See [Install & first run](https://k0b3n4irb.github.io/luna/using/install.html)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
