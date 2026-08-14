#!/usr/bin/env python3
"""Publish the Lumen crates to crates.io in dependency order.

`cargo publish --workspace` orders a workspace on its own, but it publishes
the whole thing in one shot: it cannot skip a version that is already on the
registry, and it has no answer for the publish rate limit, which is the thing
that decides how long a first release takes. crates.io lets an account create
a burst of new crates and then meters the rest, so a first publish of this
workspace is a slow sequence of uploads with waits between them, and any step
of it can fail on a network blip. This script exists for that shape: it walks
the same dependency order, skips what is already published, waits when the
registry says to, and can be re-run to pick up where it stopped.

What it does, in order:

  1. Reads `cargo metadata` and builds the workspace dependency graph, using
     normal and build dependencies only. Dev-dependencies are excluded on
     purpose: cargo strips them from the published manifest, so a dev-only
     edge (lumenc -> lumen-devtools, for one) is not a publish-order edge and
     would otherwise turn the graph cyclic.
  2. Reduces that graph to the crates reachable from the roots (`lumenui`,
     `lumenc`, `lumen-launcher`) that are not `publish = false`, and topo-sorts
     it. A crate must exist on crates.io before anything that depends on it can
     be verified, so the order is the whole point.
  3. Preflights every crate: the version must already be absent from the
     registry, the name must not belong to somebody else, and no dependency may
     be a git dependency without a version (cargo rejects those on publish).
  4. Publishes each crate with `cargo publish -p <name>`, waits for the new
     version to become visible, and applies the rate-limit interval before the
     next one.

Usage:

    tools/release/publish-crates.py --plan          # print order and status
    tools/release/publish-crates.py --dry-run       # cargo publish --dry-run
    tools/release/publish-crates.py --execute       # the real thing

`--plan` and `--dry-run` touch nothing on the registry. `--execute` is the
only mode that uploads, and it needs CARGO_REGISTRY_TOKEN in the environment.

Exit codes: 0 success (or nothing left to do), 1 a publish failed, 2 the
preflight refused to start.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CRATES_IO_API = "https://crates.io/api/v1/crates"
USER_AGENT = "lumen-release (https://github.com/lumen-fx/lumen)"

# The published entry points. Everything else in the list is here because one
# of these needs it: `lumenui` is the Rust SDK, `lumenc` is the CLI people
# `cargo install`, and `lumen-launcher` is the stub `lumenc package` turns into
# an app executable, which a source install has no other way to obtain.
DEFAULT_ROOTS = ("lumenui", "lumenc", "lumen-launcher")

# crates.io meters publishing with a leaky bucket: an account may create a
# burst of new crates and then earns one back every ten minutes, and may
# publish a burst of new versions of crates it already owns and then earns one
# back every minute. Both intervals here carry a small margin over the
# published rate, since the bucket refills on the server's clock, not ours.
NEW_CRATE_BURST = 5
NEW_CRATE_INTERVAL = 630.0
NEW_VERSION_INTERVAL = 70.0


class Preflight(Exception):
    """A condition that makes the run pointless to start."""


def plural(count: int, noun: str) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


# ----------------------------------------------------------------- metadata


def cargo_metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=REPO,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(out.stdout)


def workspace_packages(meta: dict) -> dict[str, dict]:
    members = set(meta["workspace_members"])
    return {p["name"]: p for p in meta["packages"] if p["id"] in members}


def publishable(pkg: dict) -> bool:
    """False for `publish = false`. cargo reports that as an empty registry
    list, and `null` for a crate with no restriction."""
    return pkg.get("publish") != []


def graph(packages: dict[str, dict]) -> dict[str, set[str]]:
    """name -> workspace crates it needs at build time.

    `kind` is null for a normal dependency in cargo metadata, "build" for a
    build-dependency, "dev" for a dev-dependency. Only the first two decide
    publish order.
    """
    edges: dict[str, set[str]] = {}
    for name, pkg in packages.items():
        deps = set()
        for dep in pkg["dependencies"]:
            if dep["name"] in packages and dep["kind"] in (None, "build"):
                deps.add(dep["name"])
        edges[name] = deps
    return edges


def publish_order(packages: dict[str, dict], roots: list[str]) -> list[str]:
    edges = graph(packages)
    for root in roots:
        if root not in packages:
            raise Preflight(f"no workspace crate named {root}")

    reachable: set[str] = set()
    todo = list(roots)
    while todo:
        name = todo.pop()
        if name in reachable:
            continue
        reachable.add(name)
        todo.extend(edges[name])

    skipped = sorted(n for n in reachable if not publishable(packages[n]))
    if skipped:
        raise Preflight(
            "these crates are `publish = false` but something published "
            "depends on them: " + ", ".join(skipped)
        )

    order: list[str] = []
    state: dict[str, int] = {}

    def visit(name: str, trail: tuple[str, ...]) -> None:
        if state.get(name) == 2:
            return
        if state.get(name) == 1:
            raise Preflight("dependency cycle: " + " -> ".join(trail + (name,)))
        state[name] = 1
        for dep in sorted(edges[name]):
            if dep in reachable:
                visit(dep, trail + (name,))
        state[name] = 2
        order.append(name)

    for name in sorted(reachable):
        visit(name, ())
    return order


# ----------------------------------------------------------------- registry


def registry_get(path: str, attempts: int = 4) -> dict | None:
    """One crates.io API read, or None for a name that does not exist.

    Retried: this runs dozens of times in a row, and crates.io asks callers to
    keep the rate down, so a reset connection is a normal thing to meet rather
    than a reason to abandon a half-finished publish run.
    """
    req = urllib.request.Request(
        f"{CRATES_IO_API}/{path}", headers={"User-Agent": USER_AGENT}
    )
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return None
            if err.code < 500 and err.code != 429:
                raise
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        if attempt == attempts - 1:
            raise RuntimeError(f"crates.io did not answer for {path}")
        time.sleep(2 ** attempt)
    return None


def registry_state(name: str, version: str) -> tuple[str, str]:
    """(state, detail) where state is one of:

    absent    - the name is free, publishing creates the crate
    published - this exact version is on the registry already
    owned     - the crate exists, points at this repository, needs the version
    foreign   - the crate exists and belongs to a different project
    """
    data = registry_get(name)
    if data is None:
        return "absent", "new crate"
    versions = {v["num"] for v in data.get("versions", [])}
    repo = (data["crate"].get("repository") or "").rstrip("/")
    ours = "https://github.com/lumen-fx/lumen"
    if version in versions:
        return "published", f"{name} {version} already on crates.io"
    if repo.lower() != ours.lower():
        return "foreign", f"crates.io/crates/{name} belongs to {repo or 'another project'}"
    return "owned", f"latest is {data['crate'].get('max_version')}"


def wait_for_version(name: str, version: str, timeout: float = 600.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        data = registry_get(name)
        if data and version in {v["num"] for v in data.get("versions", [])}:
            return True
        time.sleep(10)
    return False


# ---------------------------------------------------------------- preflight


def git_dependencies_without_version(pkg: dict) -> list[str]:
    """Dependencies cargo will refuse to publish: a git source and no version.

    A published crate cannot carry a git dependency. Adding `version` makes
    cargo use the git checkout locally and the registry release when
    published, which is the form the candela host needs before it can go out.
    """
    bad = []
    for dep in pkg["dependencies"]:
        if dep["kind"] == "dev":
            continue
        if dep.get("source", "") and dep["source"].startswith("git+"):
            if dep["req"] == "*":
                bad.append(dep["name"])
    return bad


def dependency_version_mismatches(packages: dict[str, dict], version: str) -> list[str]:
    """Internal path dependencies whose `version` is not the workspace one.

    Every workspace crate shares one version, so a path dependency must ask for
    that version or the published crate resolves to a release that predates the
    change it was published for.
    """
    wrong = []
    for name, pkg in packages.items():
        for dep in pkg["dependencies"]:
            if dep["name"] not in packages or dep["kind"] == "dev":
                continue
            if dep["req"] in ("*", ""):
                wrong.append(f"{name} -> {dep['name']} (no version)")
            elif dep["req"].lstrip("^=") != version:
                wrong.append(f"{name} -> {dep['name']} ({dep['req']})")
    return wrong


# ------------------------------------------------------------------ publish


def run_cargo_publish(name: str, dry_run: bool, allow_dirty: bool) -> tuple[int, str]:
    cmd = ["cargo", "publish", "-p", name, "--locked"]
    if dry_run:
        cmd.append("--dry-run")
    if allow_dirty:
        cmd.append("--allow-dirty")
    print(f"    $ {' '.join(cmd)}", flush=True)
    proc = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    return proc.returncode, proc.stdout + proc.stderr


def rate_limited(output: str) -> bool:
    lowered = output.lower()
    return "429" in lowered or "too many requests" in lowered or "rate limit" in lowered


# --------------------------------------------------------------------- main


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--plan", action="store_true",
                      help="print the order and each crate's registry state")
    mode.add_argument("--dry-run", action="store_true",
                      help="package and verify every crate, upload nothing")
    mode.add_argument("--execute", action="store_true", help="publish for real")
    parser.add_argument("--root", action="append", default=[], metavar="CRATE",
                        help="entry point to publish, repeatable (default: "
                             + ", ".join(DEFAULT_ROOTS) + ")")
    parser.add_argument("--allow-dirty", action="store_true",
                        help="pass --allow-dirty to cargo publish")
    parser.add_argument("--no-wait", action="store_true",
                        help="skip the rate-limit waits, for a run of one or two crates")
    parser.add_argument("--stop-after", type=int, default=0, metavar="N",
                        help="publish at most N crates this run")
    args = parser.parse_args()

    roots = args.root or list(DEFAULT_ROOTS)
    meta = cargo_metadata()
    packages = workspace_packages(meta)
    version = packages[roots[0]]["version"]

    try:
        order = publish_order(packages, roots)
    except Preflight as err:
        print(f"preflight: {err}", file=sys.stderr)
        return 2

    print(f"workspace version {version}, {plural(len(order), 'crate')} "
          f"from {', '.join(roots)}\n")

    problems: list[str] = []
    for mismatch in dependency_version_mismatches(packages, version):
        problems.append(f"dependency version mismatch: {mismatch}")
    for name in order:
        for dep in git_dependencies_without_version(packages[name]):
            problems.append(
                f"{name} depends on `{dep}` from git with no version; it needs a "
                f"published release and a `version` (and `package = ...` if the "
                f"registry name differs) before {name} can be published"
            )

    states: dict[str, tuple[str, str]] = {}
    for name in order:
        # crates.io asks API callers to stay around one request a second.
        time.sleep(1.0)
        state, detail = registry_state(name, version)
        states[name] = (state, detail)
        if state == "foreign":
            problems.append(f"{name}: {detail}")

    width = max(len(n) for n in order)
    for i, name in enumerate(order, 1):
        state, detail = states[name]
        print(f"  {i:2d}. {name:<{width}}  {state:<9} {detail}")

    if problems:
        print("\nblocked:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 2

    todo = [n for n in order if states[n][0] != "published"]
    if not todo:
        print(f"\nnothing to do: every crate is on crates.io at {version}")
        return 0
    print(f"\n{plural(len(todo), 'crate')} to publish")

    if args.plan:
        return 0

    if args.execute and not os.environ.get("CARGO_REGISTRY_TOKEN"):
        print("preflight: CARGO_REGISTRY_TOKEN is not set", file=sys.stderr)
        return 2

    new_crates = 0
    published = 0
    for name in todo:
        if args.stop_after and published >= args.stop_after:
            print(f"stopping after {plural(published, 'crate')} as asked; re-run to continue")
            break
        state = states[name][0]
        print(f"\n>> {name} {version} ({state})")
        code, output = run_cargo_publish(name, dry_run=args.dry_run, allow_dirty=args.allow_dirty)
        if code != 0 and rate_limited(output) and not args.dry_run:
            print("    rate limited, waiting before one retry")
            time.sleep(NEW_CRATE_INTERVAL)
            code, output = run_cargo_publish(name, dry_run=False, allow_dirty=args.allow_dirty)
        if code != 0:
            print(output.strip())
            print(f"\nfailed on {name}; fix it and re-run, the crates before it are done",
                  file=sys.stderr)
            return 1

        if args.dry_run:
            print(f"    verified {name} {version}")
            continue

        published += 1
        if not wait_for_version(name, version):
            print(f"\n{name} {version} did not appear on crates.io in time; "
                  f"re-run once it does", file=sys.stderr)
            return 1
        print(f"    published {name} {version}")

        if name == todo[-1] or args.no_wait:
            continue
        if state == "absent":
            new_crates += 1
            if new_crates >= NEW_CRATE_BURST:
                print(f"    waiting {NEW_CRATE_INTERVAL:.0f}s for the new-crate rate limit")
                time.sleep(NEW_CRATE_INTERVAL)
        else:
            print(f"    waiting {NEW_VERSION_INTERVAL:.0f}s for the new-version rate limit")
            time.sleep(NEW_VERSION_INTERVAL)

    print("\ndone")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Preflight as err:
        print(f"preflight: {err}", file=sys.stderr)
        sys.exit(2)
    except KeyboardInterrupt:
        sys.exit(130)
