#!/usr/bin/env python3
"""Doc-drift sentinel: the guide, the reference docs and the rules must
describe the luna that is in the tree.

Each check compares a claim the documentation makes against the thing the
claim is about, and fails when they disagree:

  links      every relative link in the guide (`book/src`), `docs/`,
             `README.md` and `CONTRIBUTING.md` resolves to a file, and an
             in-page `#anchor` to a heading of that file.
  orphans    every page under `book/src` is reachable from `SUMMARY.md`.
  paths      every `docs/…`, `tools/…`, `crates/…`, `book/…`, `.claude/…`
             or `.github/…` path quoted in `CLAUDE.md`, `.claude/`,
             `CONTRIBUTING.md` or the guide exists in the tree.
  version    the workspace version in `Cargo.toml` is the head version of
             `CHANGELOG.md`, and every `vX.Y.Z` the guide, `README.md` or
             `CONTRIBUTING.md` cites is a git tag.
  cli        every subcommand of `luna --help` has its `### \\`luna <cmd>\\``
             section in the CLI reference, and every long option of every
             subcommand is named somewhere in the guide.
  mcp        every tool the MCP server lists (`tools/list` over stdio) is
             named in the guide's MCP catalogue section.
  changelog  every `--option` the CHANGELOG adds under `[Unreleased]` or the
             head version is named in the guide (a new option lands with
             its example, not just a CHANGELOG line).

`cli` and `mcp` need a `luna` binary: `LUNA_BIN`, else `target/release/luna`,
else `target/debug/luna`. Without one they fail (CI builds the binary first);
`--allow-missing-binary` turns that into a skip for a quick local pass.

Usage:
    tools/check-doc-drift.py                 # every check
    tools/check-doc-drift.py links version   # a subset
    tools/check-doc-drift.py --list          # the check names
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOOK = ROOT / "book" / "src"
GUIDE_CLI_PAGE = BOOK / "using" / "cli-api-mcp.md"

# ---------------------------------------------------------------------------
# helpers


def md_files(*dirs: Path) -> list[Path]:
    out: list[Path] = []
    for d in dirs:
        if d.is_file():
            out.append(d)
        else:
            out.extend(sorted(d.rglob("*.md")))
    return out


def read(p: Path) -> str:
    return p.read_text(encoding="utf-8")


def strip_code_blocks(text: str) -> str:
    """Drop fenced code blocks: a path or link inside one is an example."""
    return re.sub(r"```.*?```", "", text, flags=re.S)


def rel(p: Path) -> str:
    return str(p.relative_to(ROOT))


def mdbook_anchor(heading: str) -> str:
    """mdBook's heading id: lowercase, drop what is not alnum/space/-/_,
    spaces to dashes."""
    text = re.sub(r"`", "", heading).strip()
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)  # link text only
    # mdBook's normalize_id: per character, so two spaces give two dashes
    return "".join(
        ch.lower() if (ch.isalnum() or ch in "_-") else "-" if ch.isspace() else ""
        for ch in text
    )


def headings_of(p: Path) -> set[str]:
    anchors: set[str] = set()
    for line in strip_code_blocks(read(p)).splitlines():
        m = re.match(r"^#{1,6}\s+(.*?)\s*(?:\{#([^}]+)\})?\s*$", line)
        if m:
            anchors.add(m.group(2) or mdbook_anchor(m.group(1)))
    # explicit <a id="…"> / <a name="…"> anchors
    anchors.update(re.findall(r'<a\s+(?:id|name)="([^"]+)"', read(p)))
    return anchors


LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")


def luna_binary(allow_missing: bool) -> Path | None:
    cands = [os.environ.get("LUNA_BIN")] if os.environ.get("LUNA_BIN") else []
    cands += [ROOT / "target" / "release" / "luna", ROOT / "target" / "debug" / "luna"]
    for c in cands:
        if c and Path(c).is_file():
            return Path(c)
    if allow_missing:
        return None
    raise SystemExit(
        "no luna binary: set LUNA_BIN or build one (cargo build -p luna-cli); "
        "--allow-missing-binary skips the cli/mcp checks"
    )


# ---------------------------------------------------------------------------
# checks — each returns a list of failure strings


def check_links() -> list[str]:
    fails: list[str] = []
    sources = md_files(BOOK, ROOT / "docs", ROOT / "README.md", ROOT / "CONTRIBUTING.md")
    for src in sources:
        text = strip_code_blocks(read(src))
        for target in LINK_RE.findall(text):
            if re.match(r"^(https?:|mailto:|data:)", target):
                continue
            if src.is_relative_to(BOOK) and target.startswith("api/"):
                continue  # rustdoc, assembled under /api by docs.yml
            path_part, _, anchor = target.partition("#")
            path_part = path_part.split("?")[0]
            if path_part:
                dest = (src.parent / path_part).resolve()
                if not dest.exists():
                    fails.append(f"{rel(src)}: link target missing: {target}")
                    continue
            else:
                dest = src
            if anchor and dest.suffix == ".md" and dest.is_file():
                if anchor not in headings_of(dest):
                    fails.append(f"{rel(src)}: no heading for anchor: {target}")
    return fails


def check_orphans() -> list[str]:
    summary = read(BOOK / "SUMMARY.md")
    linked = {
        (BOOK / t.split("#")[0]).resolve()
        for t in LINK_RE.findall(summary)
        if not t.startswith("http")
    }
    fails = []
    for page in md_files(BOOK):
        if page.name == "SUMMARY.md":
            continue
        if page.resolve() not in linked:
            fails.append(f"{rel(page)}: not in book/src/SUMMARY.md")
    return fails


# A quoted path: in backticks, or bare with a file extension. Bare prose
# ("MCP tools/resources", "60 tools/s") is not a path claim.
PATH_RE = re.compile(
    r"(?P<tick>`)?(?<![\w/.-])(?P<path>(?:docs|tools|crates|book|tests|fuzz|\.claude|\.github)/[\w./-]*[\w/])(?P<tail>[*<{]?)"
)
PATH_EXT = (".md", ".rs", ".sh", ".py", ".toml", ".yml", ".yaml", ".lua", ".json", ".css")

# Paths the rules say must NOT exist (.claude/rules/no-dependency-bots.md).
FORBIDDEN_PATHS = (".github/dependabot.yml", ".github/dependabot.yaml", "renovate.json")


def check_paths() -> list[str]:
    fails: list[str] = []
    for forbidden in FORBIDDEN_PATHS:
        if (ROOT / forbidden).exists():
            fails.append(f"{forbidden} exists (dependency bots are banned)")
    sources = md_files(
        ROOT / "CLAUDE.md",
        ROOT / "CONTRIBUTING.md",
        ROOT / ".claude" / "rules",
        ROOT / ".claude" / "commands",
        BOOK,
    )
    seen: set[tuple[str, str]] = set()
    for src in sources:
        for m in PATH_RE.finditer(strip_code_blocks(read(src))):
            quoted = m.group("path").rstrip("./")
            if m.group("tail") or quoted.startswith("tests/roms/") or quoted in FORBIDDEN_PATHS:
                continue  # a glob, a template, a gitignored ROM, a banned file
            if not m.group("tick") and not quoted.endswith(PATH_EXT):
                continue
            key = (rel(src), quoted)
            if key in seen:
                continue
            seen.add(key)
            if not (ROOT / quoted).exists():
                fails.append(f"{rel(src)}: path does not exist: {quoted}")
    return fails


VERSION_RE = re.compile(r"\bv(\d+\.\d+\.\d+)\b")


def check_version() -> list[str]:
    fails: list[str] = []
    cargo = read(ROOT / "Cargo.toml")
    m = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M)
    ws_version = m.group(1) if m else None
    head = re.search(r"^## \[(\d+\.\d+\.\d+)\]", read(ROOT / "CHANGELOG.md"), re.M)
    head_version = head.group(1) if head else None
    if ws_version != head_version:
        fails.append(
            f"Cargo.toml version {ws_version!r} != CHANGELOG.md head version {head_version!r}"
        )
    tags = set(
        subprocess.run(
            ["git", "tag", "-l", "v*"], cwd=ROOT, capture_output=True, text=True, check=True
        ).stdout.split()
    )
    if not tags:
        # A shallow clone without tags would flag every citation; say so
        # instead (CI checks out with fetch-tags: true).
        return fails + ["git sees no v* tag (shallow checkout?): fetch the tags first"]
    for src in md_files(BOOK, ROOT / "README.md", ROOT / "CONTRIBUTING.md"):
        for v in sorted(set(VERSION_RE.findall(read(src)))):
            if f"v{v}" not in tags:
                fails.append(f"{rel(src)}: cites v{v}, which is not a git tag")
    return fails


def _help(binary: Path, *args: str) -> str:
    return subprocess.run(
        [str(binary), *args, "--help"], capture_output=True, text=True, check=True
    ).stdout


def check_cli(binary: Path | None) -> list[str]:
    if binary is None:
        return []
    fails: list[str] = []
    top = _help(binary)
    cmds_block = top.split("Commands:", 1)[1].split("Options:", 1)[0]
    commands = [
        line.split()[0]
        for line in cmds_block.splitlines()
        if re.match(r"^  \S", line) and line.split()[0] != "help"
    ]
    guide = "\n".join(read(p) for p in md_files(BOOK))
    cli_page = read(GUIDE_CLI_PAGE)
    for cmd in commands:
        if not re.search(rf"^### `luna {re.escape(cmd)}`", cli_page, re.M):
            fails.append(f"{rel(GUIDE_CLI_PAGE)}: no `### `luna {cmd}`` section")
        options = sorted(set(re.findall(r"^\s+(?:-\w, )?(--[a-z0-9][a-z0-9-]*)", _help(binary, cmd), re.M)))
        for opt in options:
            if opt in ("--help", "--version"):
                continue
            if not re.search(rf"(?<![\w-]){re.escape(opt)}(?![\w-])", guide):
                fails.append(f"luna {cmd} {opt}: not named anywhere in book/src")
    return fails


def mcp_tool_names(binary: Path) -> list[str]:
    p = subprocess.Popen(
        [str(binary), "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    assert p.stdin and p.stdout

    def send(obj: dict) -> None:
        p.stdin.write(json.dumps(obj) + "\n")
        p.stdin.flush()

    send(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "check-doc-drift", "version": "0"},
            },
        }
    )
    p.stdout.readline()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
    reply = json.loads(p.stdout.readline())
    p.stdin.close()
    p.wait(timeout=10)
    return sorted(t["name"] for t in reply["result"]["tools"])


def check_mcp(binary: Path | None) -> list[str]:
    if binary is None:
        return []
    page = read(GUIDE_CLI_PAGE)
    m = re.search(r"^## 4\..*?(?=^## 5\.)", page, re.M | re.S)
    section = m.group(0) if m else page
    fails = []
    for name in mcp_tool_names(binary):
        if f"`{name}`" not in section:
            fails.append(f"{rel(GUIDE_CLI_PAGE)}: MCP tool `{name}` has no row in §4")
    return fails


def check_changelog() -> list[str]:
    text = read(ROOT / "CHANGELOG.md")
    # [Unreleased] + the head version section
    m = re.search(r"^## \[Unreleased\].*?(?=^## \[\d)(?:.*?)(?=^## \[\d)", text, re.M | re.S)
    recent = m.group(0) if m else text
    added = "\n".join(
        block for block in re.split(r"^### ", recent, flags=re.M) if block.startswith("Added")
    )
    guide = "\n".join(read(p) for p in md_files(BOOK))
    fails = []
    for opt in sorted(set(re.findall(r"`(--[a-z0-9][a-z0-9-]*)", added))):
        if not re.search(rf"(?<![\w-]){re.escape(opt)}(?![\w-])", guide):
            fails.append(f"CHANGELOG.md adds {opt}, which the guide never names")
    return fails


# ---------------------------------------------------------------------------

CHECKS = ["links", "orphans", "paths", "version", "cli", "mcp", "changelog"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("checks", nargs="*", choices=CHECKS + [[]], help="subset to run (default: all)")
    ap.add_argument("--list", action="store_true", help="print the check names")
    ap.add_argument("--allow-missing-binary", action="store_true", help="skip cli/mcp without a luna binary")
    args = ap.parse_args()
    if args.list:
        print("\n".join(CHECKS))
        return 0
    selected = args.checks or CHECKS
    binary = luna_binary(args.allow_missing_binary) if {"cli", "mcp"} & set(selected) else None
    if binary is None and {"cli", "mcp"} & set(selected):
        print("note: no luna binary, cli/mcp skipped")

    total = 0
    for name in selected:
        fails = {
            "links": check_links,
            "orphans": check_orphans,
            "paths": check_paths,
            "version": check_version,
            "cli": lambda: check_cli(binary),
            "mcp": lambda: check_mcp(binary),
            "changelog": check_changelog,
        }[name]()
        status = "OK" if not fails else f"{len(fails)} finding(s)"
        print(f"[{name}] {status}")
        for f in fails:
            print(f"  - {f}")
        total += len(fails)
    print(f"check-doc-drift: {'OK' if total == 0 else f'{total} finding(s)'}")
    return 0 if total == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
