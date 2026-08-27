#!/usr/bin/env python3
"""Verify that lumen-dylib's own crate graph agrees with the one the release
ships.

`public/lumen-dylib` (package `lumen-dylib`) is deliberately its own Cargo
workspace, excluded from the root one (see the comment at the top of its
Cargo.toml). A runtime module loads against whichever copy of the engine a
release ships, so a module author's build of it, or this repo's own isolated
resolve of it, has to land on the exact same package versions and resolved
features as the engine that ships does. `-C prefer-dynamic` shares
monomorphized generic code across that dylib boundary by mangled symbol
name, so a version or feature gap between the two builds does not show up as
a compile error. It shows up as an undefined symbol the first time something
dlopens the mismatched pair, and a release build tends to hide it while a
debug build tends to expose it (see the build-toolchain.yml paragraph
below for why).

What "the release ships" means concretely: build-toolchain.yml compiles
`lumenc`, `lumen` (the `dynamic-engine` feature), and every bundled runtime
module in one cargo invocation, so cargo's own feature unification makes
them agree by construction. `public/lumen-dylib`, built on its own, has no
such guarantee, and nothing unifies its resolve with anything else. This
script is what checks whether its resolve happens to agree anyway.

How it computes each side, and why `cargo metadata` alone will not do:
feature activation from `cargo metadata` reflects the whole workspace's
default members unified together, not one build's package selection.
Passing `--features` at the workspace root silently pulls in unrelated
members; verified by hand while writing this script (`cargo metadata
--manifest-path crates/lumenc/Cargo.toml --features dynamic-engine` showed
`lumenc`'s own `dlopen-run` feature active, though nothing in lumenc's tree
requests it, because `crates/launcher` depends on `lumenc` with that
feature and metadata unions every member's request together). `cargo tree
-p <pkg>` resolves features the same way `cargo build -p <pkg>` would,
scoped to exactly the packages named, so that is the tool this script
drives instead, once per shipped target (see TARGETS below):

  1. "release": `cargo tree -p lumen --features dynamic-engine`. `lumen` is
     the root crate that becomes `liblumen`, the shared library every app
     and every dlopened module ultimately links against; feeding it
     `dynamic-engine` reproduces the feature graph the real build resolves
     for the packages `lumen-dylib` also depends on.
  2. lumen-dylib's own declared dependency roots (its first-party path
     dependencies, read from its own manifest rather than hardcoded here,
     so adding or dropping one there changes what this script checks with
     no edit needed) are re-queried on their own, each explicitly
     requesting the exact features step 1 resolved for it. This reproduces
     the same graph restricted to what those roots reach, which excludes
     `lumenc`'s own tooling-only dependencies (`--features package/bundle`
     pulls in `zip`, `lumenc-plugin`'s version resolver, and the like).
     Those compile into `liblumen` itself but never cross into the engine
     dylib lumen-dylib is, so a module never calls into them and a version
     gap there is not the failure mode this check exists for.
  3. "engine": `cargo tree --manifest-path public/lumen-dylib/Cargo.toml`,
     resolved every run against a lockfile seeded from root's own (see
     below), never a committed one of its own.

Both queries pass `-e normal,no-proc-macro`: proc-macro dependencies compile
for the host and run at compile time only, never linking into either dylib,
so they can never be the source of an undefined-symbol mismatch and are
excluded rather than compared.

The release-side query passes `--locked` against the root workspace's
committed `Cargo.lock`, the pin of record for what a release ships, and
trusts it rather than silently re-resolving against whatever the registry
has today. `public/lumen-dylib` carries no committed `Cargo.lock` of its
own: a tracked second lockfile duplicating the root one needs hand-updating
every time the root's dependency set moves, and with `candela-lang`/
`candela-vm` tracked by branch rather than by commit (see below) it can go
stale without anyone touching this repository at all; a committed copy
lost that race against `main` more than once in the same week this check
was added, so it is not tracked at all now.

The engine side is not locked, but it is not resolved from nothing either.
`generate_engine_lock()` seeds `public/lumen-dylib/Cargo.lock` with a copy
of the root workspace's own `Cargo.lock` before anything reads it; a plain
file copy, not a `cargo` invocation. A resolve with no lock to anchor from
lands on the newest version satisfying every requirement everywhere, and
comparing that against root's committed, human-updated `Cargo.lock` (which
only moves when someone runs `cargo update`, and by no means always picks
the newest available) reports registry churn far more often than genuine
drift; an earlier version of this check did exactly that; chasing it green
meant continually bumping root's own `Cargo.lock` for packages no one had
touched, which is a dependency-freshness policy #202 never asked for.
Seeding first uses cargo's own default behavior instead: given an existing
lockfile, it keeps every entry that still satisfies the current manifests
and resolves fresh only what the seed does not cover, so the engine-side
query in step 3 ends up pinned to root's versions everywhere lumen-dylib's
own dependency tree can use them, and picks its own version only for a
package root's lock does not carry, or one whose seeded version no longer
satisfies a requirement here. What survives that is a real difference:
something lumen-dylib's own manifest tree cannot resolve to what the
release pins, which is the drift #202 describes.
`remove_engine_lock()` deletes whatever `generate_engine_lock()` seeded
once every target has been compared, and also runs first, before seeding,
so a leftover file from a previous local run can never bias this one;
nothing this script does leaves a file behind for git to notice, and root's
own `Cargo.lock` is read, never written.

`lumen-script-candela` depends on `candela-lang` and `candela-vm` from the
`candela` repository's `main` branch rather than a fixed commit, so a
`candela` push can move what the engine side resolves at any time,
independent of anything landing in this repository. Because that side
always resolves fresh, a `candela` bump shows up here as an ordinary
version or feature difference between the two sides, reported the same way
as any other drift this script catches, not as a special case or a
resolver error to work around.

For every package name common to both sides, this compares the resolved
version set and, for versions in common, the resolved feature set (`default`
excluded: it is a meta-feature name that never itself gates code, and
comparing the named features it forwards is what matters). A package present
on only one side is not reported, but the two directions are not
symmetrically safe and both are worth spelling out:

  - Present on the release side, absent from lumen-dylib's own graph: safe.
    `lumen-dylib` never appears in the release-side query (it is queried
    separately, see step 3 above), and a module's whole ABI surface is
    bounded by what lumen-dylib itself depends on, so a package the release
    build reaches that lumen-dylib does not can never be something a module
    calls into.
  - Present in lumen-dylib's own graph, absent from the release side: this
    is the dangerous direction, the undefined-symbol case, and this script
    does not defend against it with comparison logic. It is defused by a
    dependency edge that already exists for an unrelated reason: `lumen`
    (root)'s own manifest declares `lumen-dylib` as a path dependency behind
    `dynamic-engine`, so computing the release-side graph in step 1 also
    resolves lumen-dylib itself, in the same root `Cargo.lock`. Cargo.lock
    pins every package a manifest could need under any feature combination,
    not only the one currently active, so if lumen-dylib's manifest grows a
    genuinely new dependency, the root Cargo.lock has to carry it too before
    `--locked` will resolve `lumen` with `dynamic-engine` at all; the
    release-side query in step 1 fails outright with a lock-file error
    instead of quietly succeeding without the new package. This script's
    coverage of that direction depends on that one dependency edge existing.
    The deconstructable-runtime-module direction under discussion could
    reshape how `lumen` reaches `lumen-dylib`; if that edge stops being a
    plain path dependency of the root crate, this script needs a real check
    for this direction, not a docstring note about one.

Run it the same way CI does:

    python3 tools/verify-engine-crate-graph.py

Exit codes: 0 the graphs agree, 1 they do not (message names every target,
package, and side that disagrees), 2 a `cargo tree` or `cargo update`
invocation itself failed.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
ENGINE_MANIFEST = "public/lumen-dylib/Cargo.toml"

# Every target build-toolchain.yml links a dynamic engine for. Windows is not
# here: public/lumen-dylib/Cargo.toml explains why no linkable engine exists
# there (the import-library format cannot describe the engine's export
# count), so lumen-dylib is never built for it and there is no graph to
# compare. `cargo tree --target <triple>` resolves the dependency graph for
# a triple from the target spec data cargo and rustc carry built in; it does
# not need that target's standard library installed, since resolution never
# compiles anything. Verified by running it against aarch64-apple-darwin on
# this Linux runner, which carries neither that target's std nor an aarch64
# toolchain (see the commit message for the exact command and its output).
TARGETS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
]

LINE_RE = re.compile(r"^(?P<name>\S+) v(?P<version>\S+)(?: \(.*\))?\|(?P<feats>.*)$")


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)


def cargo_metadata_no_deps(manifest_path: str | None = None) -> dict:
    cmd = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
    if manifest_path:
        cmd += ["--manifest-path", manifest_path]
    out = run(cmd)
    if out.returncode != 0:
        print(
            f"verify-engine-crate-graph: cargo metadata failed:\n{out.stderr}",
            file=sys.stderr,
        )
        sys.exit(2)
    return json.loads(out.stdout)


def engine_root_names() -> list[str]:
    """lumen-dylib's own first-party (path) dependencies, read from its
    manifest so this list tracks the file rather than duplicating it."""
    meta = cargo_metadata_no_deps(ENGINE_MANIFEST)
    (pkg,) = meta["packages"]
    return sorted(
        dep["name"]
        for dep in pkg["dependencies"]
        if dep.get("kind") is None and dep.get("path") is not None
    )


def proc_macro_names(names: list[str]) -> set[str]:
    """Which of `names` are proc-macro crates in the root workspace. Passing
    one of these as a `cargo tree -p` root bypasses the `no-proc-macro` edge
    filter (a root is never filtered as an edge), which would make it show
    up asymmetrically against the engine-side query where it is reached only
    as a filtered-out edge. They carry no feature set worth tracking anyway
    (proc macros run at compile time and link into neither dylib), so they
    are dropped from the root list rather than compared."""
    meta = cargo_metadata_no_deps()
    result = set()
    for pkg in meta["packages"]:
        if pkg["name"] in names and any(
            "proc-macro" in target["kind"] for target in pkg["targets"]
        ):
            result.add(pkg["name"])
    return result


def cargo_tree(
    args: list[str], target: str, locked: bool = True
) -> dict[tuple[str, str], frozenset[str]]:
    cmd = [
        "cargo",
        "tree",
        "-e",
        "normal,no-proc-macro",
        "--target",
        target,
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
        "--no-dedupe",
        *(["--locked"] if locked else []),
        *args,
    ]
    out = run(cmd)
    if out.returncode != 0:
        print(
            f"verify-engine-crate-graph: cargo tree failed:\n{cmd}\n{out.stderr}",
            file=sys.stderr,
        )
        sys.exit(2)
    resolved: dict[tuple[str, str], frozenset[str]] = {}
    for line in out.stdout.splitlines():
        m = LINE_RE.match(line.strip())
        if not m:
            continue
        feats = frozenset(
            f for f in m.group("feats").split(",") if f and f != "default"
        )
        resolved[(m.group("name"), m.group("version"))] = feats
    return resolved


def group_by_name(
    resolved: dict[tuple[str, str], frozenset[str]],
) -> dict[str, dict[str, frozenset[str]]]:
    grouped: dict[str, dict[str, frozenset[str]]] = {}
    for (name, version), feats in resolved.items():
        grouped.setdefault(name, {})[version] = feats
    return grouped


def release_graph(target: str) -> dict[str, dict[str, frozenset[str]]]:
    # Step 1: the full graph `lumen` resolves with the engine linked in, for
    # this target. This is what ships; nothing here is hardcoded, so a
    # feature added to lumenc's or lumen's own defaults tomorrow is picked up
    # the next time this runs.
    full = group_by_name(
        cargo_tree(["-p", "lumen", "--features", "dynamic-engine"], target)
    )

    roots = engine_root_names()
    macros = proc_macro_names(roots)
    roots = [r for r in roots if r not in macros]

    # Step 2: re-request each root's own resolved features explicitly, and
    # query just those roots. See the module docstring for why this, rather
    # than the step-1 graph directly, is what gets compared.
    feature_args = []
    for root in roots:
        versions = full.get(root, {})
        if not versions:
            continue
        # A root is a single package in one Cargo.lock resolve; there is
        # exactly one version to unpack here.
        ((_version, feats),) = versions.items()
        feature_args += [f"{root}/{f}" for f in sorted(feats)]

    args = []
    for root in roots:
        args += ["-p", root]
    args += ["--features", ",".join(feature_args)]
    return group_by_name(cargo_tree(args, target))


def engine_graph(target: str) -> dict[str, dict[str, frozenset[str]]]:
    # Unlocked, deliberately: `generate_engine_lock()` seeds this lockfile
    # from root's committed one before this runs, so the seed is a starting
    # point, not a pin. cargo's ordinary resolve keeps every entry the seed
    # already satisfies for lumen-dylib's own manifest tree and this
    # target, and only picks a version of its own for whatever the seed
    # does not cover (a package root does not depend on at all, or one
    # whose seeded version no longer satisfies a requirement here). Passing
    # `--locked` would refuse to do that second part, which is the one part
    # of this query that has to happen.
    return group_by_name(
        cargo_tree(["--manifest-path", ENGINE_MANIFEST], target, locked=False)
    )


def engine_lock_path() -> Path:
    return REPO / ENGINE_MANIFEST.replace("Cargo.toml", "Cargo.lock")


def remove_engine_lock() -> None:
    # A leftover Cargo.lock from a previous local run must never bias this
    # one, and nothing this script does should leave a file behind for git
    # to notice, so this runs both before seeding a fresh one and again
    # after the run finishes.
    engine_lock_path().unlink(missing_ok=True)


def generate_engine_lock() -> None:
    # Seed lumen-dylib's lockfile with the root workspace's own committed
    # one before anything resolves it. A resolve with nothing to anchor
    # from lands on the newest version satisfying every requirement, which
    # is not what root's long-lived, human-reviewed `Cargo.lock` is (it
    # only moves when someone runs `cargo update`), so comparing an
    # anchorless resolve against it mostly reports registry churn rather
    # than genuine drift, and chasing that green would have turned this
    # check into "keep root's lockfile perpetually current," a dependency
    # policy #202 never asked for. Seeding first means cargo's own
    # preference for keeping an existing lock's choices does the anchoring:
    # every package the two sides share stays pinned to root's version if
    # lumen-dylib's manifest tree can still use it, and only a package the
    # seed does not cover gets a fresh pick. What is left after that is a
    # real difference: something lumen-dylib's own dependency tree cannot
    # satisfy with what the release pins, which is the drift #202
    # describes. This is a plain file copy, not a `cargo` invocation; the
    # `cargo tree` calls in `engine_graph()` are what resolve it.
    root_lock = REPO / "Cargo.lock"
    engine_lock_path().write_bytes(root_lock.read_bytes())


def format_features(feats: frozenset[str]) -> str:
    return ", ".join(sorted(feats)) if feats else "(none)"


def compare(
    target: str,
    release: dict[str, dict[str, frozenset[str]]],
    engine: dict[str, dict[str, frozenset[str]]],
) -> list[str]:
    common = sorted(set(release) & set(engine))
    problems: list[str] = []

    for name in common:
        release_versions = release[name]
        engine_versions = engine[name]

        if set(release_versions) != set(engine_versions):
            problems.append(
                f"[{target}] {name}: the release graph resolves "
                f"{sorted(release_versions)}, lumen-dylib's own graph resolves "
                f"{sorted(engine_versions)}.\n"
                f"    Fix: the engine side resolves from a lockfile seeded with "
                f"root's own (see the module docstring), so this means "
                f"lumen-dylib's own manifest tree genuinely cannot use the "
                f"version root pins, not that either lockfile is merely stale. "
                f"Check whether a Cargo.toml in crates/ or {ENGINE_MANIFEST} now "
                f"requires a range the other side cannot satisfy, and reconcile "
                f"the requirement, not just the pin."
            )
            continue

        for version in sorted(release_versions):
            rf, ef = release_versions[version], engine_versions[version]
            if rf == ef:
                continue
            missing_in_engine = rf - ef
            extra_in_engine = ef - rf
            detail = []
            if missing_in_engine:
                detail.append(
                    f"the release build enables {format_features(missing_in_engine)} "
                    f"that lumen-dylib's own graph does not"
                )
            if extra_in_engine:
                detail.append(
                    "lumen-dylib's own graph enables "
                    f"{format_features(extra_in_engine)} that the release "
                    "build does not"
                )
            problems.append(
                f"[{target}] {name}@{version}: {'; '.join(detail)}.\n"
                f"    release features: {format_features(rf)}\n"
                f"    engine features:  {format_features(ef)}\n"
                f"    Fix: {ENGINE_MANIFEST} pins this package's feature "
                f"set by hand (see the comment at its top); update the pinned "
                f"list to match what lumenc's and lumen's own default features "
                f"currently forward to {name}."
            )

    return problems


def main() -> int:
    problems: list[str] = []
    checked = 0
    remove_engine_lock()
    try:
        generate_engine_lock()
        for target in TARGETS:
            release = release_graph(target)
            engine = engine_graph(target)
            checked += len(set(release) & set(engine))
            problems += compare(target, release, engine)
    finally:
        remove_engine_lock()

    if problems:
        print(
            "verify-engine-crate-graph: lumen-dylib's crate graph does not match "
            "the one the release ships.\n",
            file=sys.stderr,
        )
        for problem in problems:
            print(f"- {problem}\n", file=sys.stderr)
        print(
            f"{len(problems)} package(s) disagree across {len(TARGETS)} target(s). "
            "A module built against lumen-dylib in this state can dlopen against "
            "the shipped engine and fail with an undefined symbol instead of a "
            "load error, on whichever platform first builds it with a feature "
            "set that differs.",
            file=sys.stderr,
        )
        return 1

    print(
        "verify-engine-crate-graph: lumen-dylib's crate graph matches the one "
        f"the release ships ({checked} shared package check(s) across "
        f"{len(TARGETS)} target(s))."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
