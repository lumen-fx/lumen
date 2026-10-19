#!/usr/bin/env python3
"""Publish the Lumen crates to crates.io in dependency order.

`cargo publish --workspace` orders a workspace on its own, but it publishes
the whole thing in one shot: it cannot skip a version that is already on the
registry, and it has no answer for the publish rate limit, which is the thing
that decides how long a first release takes. crates.io lets an account create
a burst of new crates and then meters the rest, so a first publish is a slow
sequence of uploads with waits between them, and any step of it can fail on a
network blip. This script exists for that shape: it walks the dependency
order, skips what is already published, waits when the registry says to, and
can be re-run to pick up where it stopped.

It publishes one of two sets, chosen on the command line:

  - The Rust SDK (`--sdk`): `lumenui` (sdk/lumenui) and the `lumenui-<name>`
    module crates `tools/release/lumenui-engine.py wrappers` generates, which
    the script writes into a temporary directory outside the checkout for the
    run. None of them is a workspace member and none names an engine crate:
    `lumenui`'s build script downloads the prebuilt engine set from the
    release tagged with its version, so these publish after that release
    exists, and the script checks that it does.
  - The workspace crates reachable from the roots (default `lumenc`,
    `lumen-launcher`), the `cargo install lumenc` path. These depend on
    crates that are `publish = false`, so this set refuses to start today.

What it does, in order:

  1. Reads the set's manifests and builds its dependency graph, using normal
     and build dependencies only. Dev-dependencies are excluded on purpose:
     cargo strips them from the published manifest, so a dev-only edge
     (lumenc -> lumen-devtools, for one) is not a publish-order edge and would
     otherwise turn the graph cyclic.
  2. For the workspace set, reduces that graph to the crates reachable from
     the roots and refuses a `publish = false` one among them. Then
     topo-sorts: a crate must exist on crates.io before anything that depends
     on it can be verified, so the order is the whole point.
  3. Preflights every crate: the version must be the workspace version, the
     name must not belong to somebody else, and no dependency may be a git
     dependency without a version (cargo rejects those on publish). For the
     SDK set, the release `v<version>` must exist and its `sha256sums.txt`
     must list the engine sets; a target whose set is missing is reported,
     and does not stop the publish.
  4. Publishes each crate with `cargo publish`, waits for the new version to
     become visible, and applies the rate-limit interval before the next one.

Usage:

    tools/release/publish-crates.py --sdk --plan     # the Rust SDK set
    tools/release/publish-crates.py --sdk --dry-run
    tools/release/publish-crates.py --sdk --execute
    tools/release/publish-crates.py --plan           # the workspace set
    tools/release/publish-crates.py --dry-run
    tools/release/publish-crates.py --execute

`--plan` and `--dry-run` touch nothing on the registry. `--execute` is the
only mode that uploads, and it needs CARGO_REGISTRY_TOKEN in the environment.

A dry run of the SDK set before its release exists needs an engine set to
build against: point LUMEN_ENGINE_ARTIFACTS at one (`lumenui-engine.py build`
and `collect` make it), and the release check is skipped. A module crate's
dry run builds against this checkout's `lumenui` while that version is not on
the registry yet.

Exit codes: 0 success (or nothing left to do), 1 a publish failed, 2 the
preflight refused to start.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CRATES_IO_API = "https://crates.io/api/v1/crates"
USER_AGENT = "lumen-release (https://github.com/lumen-fx/lumen)"

# The workspace set's entry points. Everything else in that set is there
# because one of these needs it: `lumenc` is the CLI people `cargo install`,
# and `lumen-launcher` is the stub `lumenc package` turns into an app
# executable, which a source install has no other way to obtain. The Rust SDK
# is the other set (`--sdk`): its crates depend on no crate here, so they are
# not part of this order.
DEFAULT_ROOTS = ("lumenc", "lumen-launcher")

# The Rust SDK crates in the tree: the build-script crate, which `lumenui`'s
# own build script and every app's depend on, and `lumenui`. The module
# crates beside them are generated.
SDK_BUILD_MANIFEST = REPO / "sdk" / "lumenui-build" / "Cargo.toml"
SDK_MANIFEST = REPO / "sdk" / "lumenui" / "Cargo.toml"
DEFAULT_GH_REPO = "lumen-fx/lumen"

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
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        raise SystemExit("cargo metadata failed:\n" + out.stderr.strip())
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


# ---------------------------------------------------------------------- sdk


def load_engine_tool():
    """tools/release/lumenui-engine.py as a module: its target table is the
    one the SDK's build script and the release share."""
    path = REPO / "tools" / "release" / "lumenui-engine.py"
    spec = importlib.util.spec_from_file_location("lumenui_engine_tool", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def generate_wrappers(out: Path) -> list[Path]:
    """Write the module crates into `out`; their manifests, one per crate."""
    proc = subprocess.run(
        [sys.executable, str(REPO / "tools" / "release" / "lumenui-engine.py"),
         "wrappers", "--out", str(out)],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise Preflight("lumenui-engine.py wrappers failed:\n"
                        + (proc.stdout + proc.stderr).strip())
    return sorted(out.glob("lumenui-*/Cargo.toml"))


def manifest_package(manifest: Path) -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps",
         "--manifest-path", str(manifest)],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        raise Preflight(f"cargo metadata failed for {manifest}:\n" + out.stderr.strip())
    (pkg,) = json.loads(out.stdout)["packages"]
    return pkg


def sdk_packages(wrapper_dir: Path) -> dict[str, dict]:
    """`lumenui-build`, `lumenui` and the module crates, the last written into
    `wrapper_dir` for the run."""
    packages = {}
    for manifest in [SDK_BUILD_MANIFEST, SDK_MANIFEST, *generate_wrappers(wrapper_dir)]:
        pkg = manifest_package(manifest)
        packages[pkg["name"]] = pkg
    return packages


def release_assets(version: str) -> set[str] | None:
    """The asset names the release's sha256sums.txt lists, or None when the
    release (or that file) is not published."""
    repo = os.environ.get("LUMEN_GH_REPO") or DEFAULT_GH_REPO
    url = f"https://github.com/{repo}/releases/download/v{version}/sha256sums.txt"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    text = ""
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                text = resp.read().decode("utf-8", "replace")
            break
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return None
            if err.code < 500:
                raise
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        if attempt == 3:
            raise RuntimeError(f"no answer from {url}")
        time.sleep(2 ** attempt)
    names = set()
    for line in text.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2:
            names.add(parts[1].strip().lstrip("*"))
    return names


def engine_set_problems(version: str, targets: list[str]) -> tuple[list[str], list[str]]:
    """(problems, warnings) about the release the SDK's build script fetches.

    The crates are no use without the release: `lumenui` downloads its engine
    set from `v<version>`, so a release that is not there refuses the run. A
    release missing some targets' sets still publishes, the way a release with
    a failed build leg still ships its other archives: the crates are the same
    for every target, and re-running that leg and the release upload fills the
    gap for a version already on crates.io.
    """
    assets = release_assets(version)
    if assets is None:
        return [
            f"the release v{version} has no sha256sums.txt, so `lumenui` {version} would "
            f"have no engine to download; publish after the release is up, or set "
            f"LUMEN_ENGINE_ARTIFACTS for a dry run"
        ], []
    missing = [
        f"lumenui-engine-{target}{suffix}"
        for target in targets
        for suffix in (".json", ".tar.zst")
        if f"lumenui-engine-{target}{suffix}" not in assets
    ]
    if len(missing) == 2 * len(targets):
        return [f"the release v{version} carries no lumenui-engine set at all"], []
    return [], [f"the release v{version} lists no {name}" for name in missing]


# ------------------------------------------------------------------ publish


def run_cargo_publish(
    pkg: dict,
    sdk: bool,
    dry_run: bool,
    allow_dirty: bool,
    patches: dict[str, Path],
) -> tuple[int, str]:
    if sdk:
        # Each SDK crate is a workspace of its own with no committed lockfile,
        # so there is no lockfile for `--locked` to hold it to.
        cmd = ["cargo", "publish", "--manifest-path", pkg["manifest_path"]]
    else:
        cmd = ["cargo", "publish", "-p", pkg["name"], "--locked"]
    if dry_run:
        cmd.append("--dry-run")
    if allow_dirty:
        cmd.append("--allow-dirty")
    for name, path in sorted(patches.items()):
        cmd += ["--config", f"patch.crates-io.{name}.path={json.dumps(path.as_posix())}"]
    print(f"    $ {' '.join(cmd)}", flush=True)
    proc = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    return proc.returncode, proc.stdout + proc.stderr


def rate_limited(output: str) -> bool:
    lowered = output.lower()
    return "429" in lowered or "too many requests" in lowered or "rate limit" in lowered


UNRESOLVED = re.compile(r"failed to select a version for the requirement `([A-Za-z0-9_-]+) ")


def waiting_on_a_sibling(output: str, pending: set[str]) -> str | None:
    """The name of a crate in this same run that a dry run needs published.

    A dry run resolves dependencies against the registry, so verifying a crate
    whose Lumen dependencies are not published yet cannot succeed however
    correct the crate is. Before the first release that is most of the list,
    and it is a fact about the order rather than a fault in the crate.
    """
    for match in UNRESOLVED.finditer(output):
        if match.group(1) in pending:
            return match.group(1)
    return None


def sibling_patches(pkg: dict, packages: dict[str, dict], pending: set[str]) -> dict[str, Path]:
    """For an SDK dry run: the crates of this set `pkg` depends on that are not
    on the registry at this version, pointed at their directories here, so the
    crate verifies against the code that is about to publish with it."""
    patches = {}
    for dep in pkg["dependencies"]:
        name = dep["name"]
        if dep["kind"] != "dev" and name in pending and name != pkg["name"]:
            patches[name] = Path(packages[name]["manifest_path"]).parent
    return patches


# --------------------------------------------------------------------- main


def workspace_version() -> str:
    text = (REPO / "Cargo.toml").read_text()
    section = text.split("\n[workspace.package]\n", 1)
    m = re.search(r'^version\s*=\s*"([^"]+)"', section[-1], re.M) if len(section) == 2 else None
    if not m:
        raise SystemExit("no [workspace.package] version in Cargo.toml")
    return m.group(1)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--plan", action="store_true",
                      help="print the order and each crate's registry state")
    mode.add_argument("--dry-run", action="store_true",
                      help="package and verify every crate, upload nothing")
    mode.add_argument("--execute", action="store_true", help="publish for real")
    parser.add_argument("--sdk", action="store_true",
                        help="publish the Rust SDK (lumenui and its generated module "
                             "crates) instead of the workspace set")
    parser.add_argument("--root", action="append", default=[], metavar="CRATE",
                        help="workspace entry point to publish, repeatable (default: "
                             + ", ".join(DEFAULT_ROOTS) + ")")
    parser.add_argument("--allow-dirty", action="store_true",
                        help="pass --allow-dirty to cargo publish")
    parser.add_argument("--no-wait", action="store_true",
                        help="skip the rate-limit waits, for a run of one or two crates")
    parser.add_argument("--stop-after", type=int, default=0, metavar="N",
                        help="publish at most N crates this run")
    args = parser.parse_args()
    if args.sdk and args.root:
        parser.error("--root names workspace crates; --sdk publishes the SDK set")

    version = workspace_version()
    if not args.sdk:
        return publish(args, version, None)
    # Outside the checkout: the workspace above would claim the crates, and an
    # untracked file in the tree reads as a dirty working tree to cargo.
    wrapper_dir = Path(tempfile.mkdtemp(prefix="lumenui-sdk-"))
    try:
        return publish(args, version, wrapper_dir)
    finally:
        shutil.rmtree(wrapper_dir, ignore_errors=True)


def publish(args: argparse.Namespace, version: str, wrapper_dir: Path | None) -> int:
    sdk = wrapper_dir is not None
    problems: list[str] = []
    warnings: list[str] = []
    try:
        if wrapper_dir is not None:
            packages = sdk_packages(wrapper_dir)
            roots = sorted(packages)
            label = "the Rust SDK"
        else:
            packages = workspace_packages(cargo_metadata())
            roots = args.root or list(DEFAULT_ROOTS)
            label = ", ".join(roots)
        order = publish_order(packages, roots)
    except Preflight as err:
        print(f"preflight: {err}", file=sys.stderr)
        return 2

    print(f"workspace version {version}, {plural(len(order), 'crate')} from {label}\n")

    for name in order:
        if packages[name]["version"] != version:
            problems.append(
                f"{name} is at {packages[name]['version']} and the workspace at {version}; "
                f"tools/release/bump-version.py {version} moves them together"
            )
    for mismatch in dependency_version_mismatches(packages, version):
        problems.append(f"dependency version mismatch: {mismatch}")
    for name in order:
        for dep in git_dependencies_without_version(packages[name]):
            problems.append(
                f"{name} depends on `{dep}` from git with no version; it needs a "
                f"published release and a `version` (and `package = ...` if the "
                f"registry name differs) before {name} can be published"
            )

    if sdk:
        artifacts = os.environ.get("LUMEN_ENGINE_ARTIFACTS")
        if artifacts and not args.execute:
            print(f"engine set: {artifacts} (LUMEN_ENGINE_ARTIFACTS); the release is not checked\n")
        else:
            targets = sorted(load_engine_tool().TARGET_NAMES.values())
            found, missing = engine_set_problems(version, targets)
            problems += found
            warnings += missing

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

    for warning in warnings:
        # An annotation on the run when this runs in GitHub Actions.
        prefix = "::warning::" if os.environ.get("GITHUB_ACTIONS") else "warning: "
        print(f"{prefix}{warning}")

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
    deferred: list[str] = []
    pending = set(todo)
    for name in todo:
        if args.stop_after and published >= args.stop_after:
            print(f"stopping after {plural(published, 'crate')} as asked; re-run to continue")
            break
        state = states[name][0]
        pkg = packages[name]
        patches = sibling_patches(pkg, packages, pending) if sdk and args.dry_run else {}
        print(f"\n>> {name} {version} ({state})")
        code, output = run_cargo_publish(pkg, sdk, args.dry_run, args.allow_dirty, patches)
        if code != 0 and rate_limited(output) and not args.dry_run:
            print("    rate limited, waiting before one retry")
            time.sleep(NEW_CRATE_INTERVAL)
            code, output = run_cargo_publish(pkg, sdk, False, args.allow_dirty, {})
        if code != 0 and args.dry_run:
            blocker = waiting_on_a_sibling(output, pending)
            if blocker:
                print(f"    not verifiable until {blocker} {version} is published")
                deferred.append(name)
                continue
        if code != 0:
            print(output.strip())
            print(f"\nfailed on {name}; fix it and re-run, the crates before it are done",
                  file=sys.stderr)
            return 1

        if args.dry_run:
            print(f"    verified {name} {version}")
            continue

        published += 1
        pending.discard(name)
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

    if deferred:
        print(f"\n{plural(len(deferred), 'crate')} could not be verified before the crates "
              f"below them are published: {', '.join(deferred)}")
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
