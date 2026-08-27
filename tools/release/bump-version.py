#!/usr/bin/env python3
"""Set the version this source tree calls itself.

Usage:

    tools/release/bump-version.py 0.0.4

The workspace `Cargo.toml` is where the version is decided, and several other
files copy it: every internal dependency asks for that exact version, a package
outside the workspace spells it out because it cannot inherit one, `Cargo.lock`
records it per crate, and the Python SDK keeps it in `lumen/_version.py`. A copy
that lags behind fails somewhere other than where it was edited, so this moves
all of them together and prints what it touched.

It leaves alone every version that names a release. The package-manager
manifests in this directory and the toolchain pins in CI point at builds that
exist, and `update-package-manifests.sh` moves those. The version here is what
the tree will be next, which is a different thing.

`.github/workflows/release.yml` runs this after a release publishes and commits
the result to `main`, which is how `main` reaches the next patch version. Run it
by hand to go somewhere other than the next patch, or to repair a copy that
drifted.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parents[2]

VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
SECTION = re.compile(r"^\s*\[(.+)\]\s*$")
# The lookbehind is what keeps `rust-version` out of this.
KEY_VERSION = re.compile(r'(?<![-\w.])(version\s*=\s*)"([^"]*)"')
NAME_VALUE = re.compile(r'^\s*name\s*=\s*"([^"]*)"\s*$')
PY_VERSION = re.compile(r'^(__version__\s*=\s*)"([^"]*)"(.*)$', re.M)

# Directories with no manifest of ours in them. `target` holds vendored sources
# of other people's crates, which carry their own versions.
SKIP_DIRS = {".git", "target", "node_modules"}

# Files that write the version out in full, each for its own reason, and which
# therefore have to move every time. The rules below find them without being
# told to, so this is a tripwire rather than a work list: a bump that leaves one
# of them alone means a rule stopped matching, and stopping here is how that
# surfaces instead of becoming a version skew a merge queue finds later. Add a
# file here when a new one starts carrying the version.
REQUIRED = {
    # The version itself, which every workspace member inherits.
    "Cargo.toml": "the workspace version",
    # Outside the workspace, so it cannot inherit and spells the version out.
    "public/lumen-dylib/Cargo.toml": "the linkable engine's version",
    # The Python SDK ships at the version of the ABI it binds, and
    # sdk/python/tests/test_version.py fails when the two disagree.
    "sdk/python/lumen/_version.py": "the Python SDK's version",
    # Every crate this repository builds is recorded here by version.
    "Cargo.lock": "the resolved versions",
}


class Edit(NamedTuple):
    """One file's rewritten text, held back until every file is accounted for."""

    path: Path
    text: str
    count: int


def fail(message: str) -> None:
    print(f"bump-version.py: {message}", file=sys.stderr)
    raise SystemExit(1)


def manifests() -> list[Path]:
    found = []
    for path in ROOT.rglob("Cargo.toml"):
        if SKIP_DIRS.isdisjoint(part for part in path.relative_to(ROOT).parts):
            found.append(path)
    return sorted(found)


def workspace_version(text: str) -> str | None:
    """The version under `[workspace.package]`, which every member inherits."""
    section = text.split("\n[workspace.package]\n", 1)
    if len(section) != 2:
        return None
    match = re.search(r'^version\s*=\s*"([^"]+)"', section[1], re.M)
    return match.group(1) if match else None


def entries(lines: list[str]):
    """Walk a manifest as (section, logical line, line numbers).

    A dependency written as an inline table can wrap across lines, so the
    braces are counted rather than assuming one entry is one line.
    """
    section = ""
    index = 0
    while index < len(lines):
        line = lines[index]
        header = SECTION.match(line)
        if header:
            section = header.group(1)
            index += 1
            continue
        span = [index]
        depth = line.count("{") - line.count("}")
        while depth > 0 and span[-1] + 1 < len(lines):
            span.append(span[-1] + 1)
            depth += lines[span[-1]].count("{") - lines[span[-1]].count("}")
        yield section, "\n".join(lines[i] for i in span), span
        index = span[-1] + 1


def is_dependency_table(section: str) -> bool:
    """`[dependencies]`, and its dev, build, target and workspace spellings."""
    return section.split(".")[-1].endswith("dependencies")


def carries_tree_version(section: str, entry: str, builds_here: bool) -> bool:
    """Whether a `version` in this entry would mean "the version of this tree".

    `[workspace.package] version` is the one every member inherits, and a
    `[package] version` written out in full belongs to a crate that cannot
    inherit one; the second only counts for a crate this repository builds, so
    a tool that happens to live here under a version of its own
    (`tools/zed-lumen`) keeps it. A `version` beside a `path` in a dependency
    table is one crate here asking for another: cargo takes the path when
    building from this checkout and the version when the crate is published, so
    the two have to agree.
    """
    if section == "workspace.package":
        return True
    if section == "package":
        return builds_here
    if not is_dependency_table(section):
        return False
    # A dependency with no `path` is somebody else's crate.
    return bool(re.search(r"(?<![-\w.])path\s*=", entry))


def rewrite_manifest(path: Path, local: set[str], old: str, new: str) -> Edit:
    """Move every version in one manifest that means "this tree".

    Only a value equal to the version the tree carries today is touched, which
    is what keeps an unrelated dependency that happens to sit near a version
    key out of it.
    """
    lines = path.read_text(encoding="utf-8").split("\n")
    changed = 0
    builds_here = package_name(path) in local

    for section, entry, span in entries(lines):
        if not carries_tree_version(section, entry, builds_here):
            continue

        def swap(match: re.Match[str]) -> str:
            nonlocal changed
            if match.group(2) != old:
                return match.group(0)
            changed += 1
            return f'{match.group(1)}"{new}"'

        replaced = KEY_VERSION.sub(swap, entry)
        if replaced != entry:
            for offset, line in enumerate(replaced.split("\n")):
                lines[span[offset]] = line

    return Edit(path, "\n".join(lines), changed)


def package_name(path: Path) -> str | None:
    """The `[package] name` a manifest declares, if it declares one."""
    section = ""
    for line in path.read_text(encoding="utf-8").split("\n"):
        header = SECTION.match(line)
        if header:
            section = header.group(1)
            continue
        if section != "package":
            continue
        match = NAME_VALUE.match(line)
        if match:
            return match.group(1)
    return None


def lock_blocks(lines: list[str]):
    """Walk `Cargo.lock` as (name, is_local, line numbers of the block).

    A `[[package]]` block with no `source` was resolved from a path in this
    checkout, which makes the lockfile the answer to which crates this
    repository builds. A crate that merely sits in the tree without being part
    of that graph, such as an editor extension, is not in here.
    """
    starts = [i for i, line in enumerate(lines) if line == "[[package]]"]
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(lines)
        block = lines[start:end]
        names = (NAME_VALUE.match(line) for line in block)
        name = next((match.group(1) for match in names if match), None)
        if name is None:
            continue
        local = not any(line.startswith("source = ") for line in block)
        yield name, local, range(start, end)


def rewrite_lock(path: Path, old: str, new: str) -> Edit:
    """Move the lockfile's record of every crate this repository builds.

    The `dependencies` lists name crates without versions while the name is
    unambiguous, which it is here, so nothing else in the file moves with them.
    """
    lines = path.read_text(encoding="utf-8").split("\n")
    changed = 0
    for _, local, span in lock_blocks(lines):
        if not local:
            continue
        for index in span:
            if lines[index] == f'version = "{old}"':
                lines[index] = f'version = "{new}"'
                changed += 1
    return Edit(path, "\n".join(lines), changed)


def rewrite_python_sdk(path: Path, old: str, new: str) -> Edit:
    """The Python SDK binds one ABI of one runtime and ships at its version."""
    text = path.read_text(encoding="utf-8")
    changed = 0

    def swap(match: re.Match[str]) -> str:
        nonlocal changed
        if match.group(2) != old:
            return match.group(0)
        changed += 1
        return f'{match.group(1)}"{new}"{match.group(3)}'

    replaced = PY_VERSION.sub(swap, text)
    return Edit(path, replaced, changed)


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: bump-version.py <version>", file=sys.stderr)
        return 2
    new = argv[1].lstrip("v")
    if not VERSION.match(new):
        fail(f"'{argv[1]}' is not a version number")

    root_manifest = ROOT / "Cargo.toml"
    old = workspace_version(root_manifest.read_text(encoding="utf-8"))
    if old is None:
        fail("no [workspace.package] version in Cargo.toml")
    if old == new:
        print(f"the workspace is already at {new}")
        return 0

    lock = ROOT / "Cargo.lock"
    python_sdk = ROOT / "sdk/python/lumen/_version.py"
    local = {
        name
        for name, is_local, _ in lock_blocks(lock.read_text(encoding="utf-8").split("\n"))
        if is_local
    }

    edits = [rewrite_manifest(path, local, old, new) for path in manifests()]
    edits.append(rewrite_lock(lock, old, new))
    edits.append(rewrite_python_sdk(python_sdk, old, new))

    total = 0
    moved = set()
    for edit in edits:
        total += edit.count
        if edit.count:
            moved.add(str(edit.path.relative_to(ROOT)))

    missed = sorted(set(REQUIRED) - moved)
    if missed:
        for name in missed:
            print(f"  {name} still carries {old}, and holds {REQUIRED[name]}", file=sys.stderr)
        fail("a file that always moves did not, so nothing was written")

    for edit in edits:
        if edit.count:
            edit.path.write_text(edit.text, encoding="utf-8")
            print(f"  {edit.path.relative_to(ROOT)}: {edit.count}")

    if workspace_version(root_manifest.read_text(encoding="utf-8")) != new:
        fail("the workspace version did not move")

    print(f"{old} -> {new}, {total} lines")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
