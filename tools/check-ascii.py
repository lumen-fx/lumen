#!/usr/bin/env python3
"""Fail if any tracked source file contains a non-ASCII character.

The repo rule: source files are ASCII only. If a character is not on the
keyboard and it needs to be on screen, use an icon asset or a drawn element.
Unicode that must exist as data (test fixtures, translation catalogs) is
written as a `\\u{...}` escape so the source file itself stays ASCII.

Usage: python3 tools/check-ascii.py
Run from anywhere inside the repo; it resolves the repo root itself.
"""

import subprocess
import sys
from pathlib import Path

# Extensions that hold binary data, not source text. Non-ASCII bytes here are
# normal and irrelevant to the ASCII-source rule.
BINARY_EXTENSIONS = {
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".pdf",
    ".wav", ".ogg", ".mp3", ".mp4",
    ".woff", ".woff2", ".ttf", ".otf",
    ".zip", ".gz", ".bin", ".so", ".dylib", ".dll", ".a", ".o",
}

# Files allowed to carry non-ASCII characters, with the reason why.
EXEMPTIONS = {
    # German translation content: the file's job is to hold German text.
    "lumen/i18n/examples/hello/de-DE.ftl",
}


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True, capture_output=True, text=True,
    )
    return Path(out.stdout.strip())


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files"],
        check=True, capture_output=True, text=True, cwd=root,
    )
    return [line for line in out.stdout.splitlines() if line]


def find_offenses(path: Path) -> list[tuple[int, int, str]]:
    """Return (line, column, character) for every non-ASCII char in path."""
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return [(0, 0, "<invalid utf-8>")]

    offenses = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        for col, ch in enumerate(line, start=1):
            if ord(ch) > 127:
                offenses.append((line_no, col, ch))
    return offenses


def main() -> int:
    root = repo_root()
    failures: dict[str, list[tuple[int, int, str]]] = {}

    for rel_path in tracked_files(root):
        if rel_path in EXEMPTIONS:
            continue
        if Path(rel_path).suffix.lower() in BINARY_EXTENSIONS:
            continue

        full_path = root / rel_path
        if not full_path.is_file():
            continue  # submodules, symlinked gitlinks, etc.

        offenses = find_offenses(full_path)
        if offenses:
            failures[rel_path] = offenses

    if not failures:
        print("check-ascii: all tracked source files are ASCII.")
        return 0

    for rel_path, offenses in sorted(failures.items()):
        print(f"{rel_path}:")
        for line_no, col, ch in offenses:
            if line_no == 0:
                print("  file is not valid UTF-8")
                continue
            print(f"  line {line_no}, col {col}: {ch!r} (U+{ord(ch):04X})")

    print(
        f"\ncheck-ascii: {len(failures)} file(s) contain non-ASCII characters.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
