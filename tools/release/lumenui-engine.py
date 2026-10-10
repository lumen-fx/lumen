#!/usr/bin/env python3
"""Build, collect and check the prebuilt engine the published `lumenui` links.

An app depends on `lumenui` (sdk/lumenui), whose build script downloads, for
its target, the compiled crates of `lumenui-engine` (sdk/rust) and points
rustc at them, so the app compiles none of the engine. This script makes that
set and checks that an app really builds from it.

  build    Compile the engine for this machine's target, the way a release
           does, and record what cargo produced.
  collect  Turn that record into the set: exactly the target closure's
           `.rlib` files, the host proc-macro libraries they name, and the
           native static libraries they link without carrying, plus a
           manifest. Writes the unpacked set, `lumenui-engine-<target>.tar.zst`
           and `lumenui-engine-<target>.json` (the manifest, also inside the
           archive, published on its own so a build can check the compiler
           before downloading the archive).
  check    Read the `cargo build -v` log of an app built against the set, and
           fail if cargo compiled any crate other than the app, `lumenui`, and
           the crates `lumenui`'s build script needs.

Usage:

    tools/release/lumenui-engine.py build --target-dir DIR --messages build.jsonl
    tools/release/lumenui-engine.py collect --messages build.jsonl --out dist/
    tools/release/lumenui-engine.py check --app APP_DIR --log build.log

Why the rules in `collect` are the ones they are:

  - The build passes `--target` even for this machine's own triple. cargo then
    puts the target's crates under `<target-dir>/<triple>/` and everything that
    runs at build time (build scripts, proc macros, and their dependencies)
    under `<target-dir>/release/`, so the two cannot be mixed up.
  - Only the files cargo names in its `compiler-artifact` messages are taken,
    from `deps/`, never a directory listing: a reused target directory holds
    stale builds of the same crates, and two candidates for one crate are an
    error (E0464) in the app's build. The one file from outside `deps/` is the
    engine crate's own rlib, the copy cargo makes of a requested package.
  - `.rmeta` files are left out: every `.rlib` carries the same metadata, and
    rustc reads it from there.
  - A proc macro runs inside the compiler, so it is a library for the build
    machine, and an app's derive needs it at compile time.
  - A native library linked with `-bundle` is not inside the rlib that names
    it, so the final link of the app needs the file itself. So does a library
    a crate ships in its own source and only puts on the search path (the
    `windows` crates' import libraries), which no rlib carries either.
  - The engine is compiled without LTO, with bitcode embedded beside the
    machine code. An rlib built for LTO holds bitcode alone, which an app
    built without LTO (every `cargo run`) cannot link; one built without
    bitcode refuses an app that turns LTO on.
  - `--remap-path-prefix` keeps this machine's checkout and cargo paths out of
    the set: they would otherwise end up in every app's panic messages.

Exit codes: 0 success, 1 a step failed, 2 bad usage.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PACKAGE = "lumenui-engine"
LIB = "lumenui_engine"

# The release's name for each target, the one every asset name carries. The
# same table is in sdk/lumenui/build.rs.
TARGET_NAMES = {
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
    "x86_64-apple-darwin": "macos-x86_64",
    "aarch64-apple-darwin": "macos-aarch64",
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "aarch64-pc-windows-msvc": "windows-aarch64",
}

PROC_MACRO_SUFFIXES = (".so", ".dylib", ".dll")


def fail(message: str) -> None:
    print(f"lumenui-engine.py: {message}", file=sys.stderr)
    raise SystemExit(1)


def rustc_info() -> dict[str, str]:
    out = subprocess.run(
        ["rustc", "-vV"], cwd=REPO, capture_output=True, text=True, check=True
    ).stdout
    info = {}
    for line in out.splitlines():
        key, _, value = line.partition(":")
        if value:
            info[key.strip()] = value.strip()
    info["version_line"] = out.splitlines()[0].strip()
    return info


def workspace_version() -> str:
    text = (REPO / "Cargo.toml").read_text()
    section = text.split("[workspace.package]", 1)[1]
    m = re.search(r'^version\s*=\s*"([^"]+)"', section, re.M)
    if not m:
        fail("no [workspace.package] version in Cargo.toml")
    return m.group(1)


# -------------------------------------------------------------------- build


def cmd_build(args: argparse.Namespace) -> None:
    triple = rustc_info()["host"]
    if triple not in TARGET_NAMES:
        fail(f"no release name for {triple}")
    target_dir = Path(args.target_dir).resolve()
    remap = [
        f"--remap-path-prefix={REPO}=/lumen",
        f"--remap-path-prefix={cargo_home()}=/cargo",
    ]
    env = dict(os.environ)
    # RUSTFLAGS reaches only the target's crates when --target is given, which
    # is the split this wants: the proc macros run here and nowhere else.
    flags = env.get("RUSTFLAGS", "").split() + remap + ["-C", "embed-bitcode=yes"]
    env["RUSTFLAGS"] = " ".join(flags)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    cmd = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "-p",
        PACKAGE,
        "--target",
        triple,
        "--config",
        'profile.release.lto="off"',
        "--message-format=json-render-diagnostics",
    ]
    print("+", " ".join(cmd), file=sys.stderr)
    with open(args.messages, "w") as out:
        proc = subprocess.run(cmd, cwd=REPO, env=env, stdout=out)
    if proc.returncode != 0:
        fail(f"cargo build failed ({proc.returncode})")


# ------------------------------------------------------------------ collect


def read_messages(path: str) -> list[dict]:
    msgs = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line.startswith("{"):
                msgs.append(json.loads(line))
    return msgs


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def native_lib_names(name: str) -> list[str]:
    return [f"lib{name}.a", f"{name}.lib"]


def cargo_home() -> Path:
    return Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()


def under(path: Path, root: Path) -> bool:
    a = os.path.normcase(str(path.resolve()))
    b = os.path.normcase(str(root))
    return a == b or a.startswith(b.rstrip(os.sep) + os.sep)


def collect_files(msgs: list[dict], triple: str) -> tuple[list[Path], list[Path]]:
    """(deps, native): the files the set is made of, deduplicated."""
    deps: dict[str, Path] = {}
    native: dict[str, Path] = {}
    marker = f"{os.sep}{triple}{os.sep}"
    registry = cargo_home()
    for m in msgs:
        reason = m.get("reason")
        if reason == "compiler-artifact":
            kinds = m["target"]["kind"]
            if "custom-build" in kinds:
                continue
            for name in m["filenames"]:
                p = Path(name)
                if "proc-macro" in kinds:
                    if p.suffix in PROC_MACRO_SUFFIXES:
                        deps[p.name] = p
                    continue
                # A crate for the target, never one for the build machine.
                if marker not in str(p) or p.suffix != ".rlib":
                    continue
                # Outside deps/ is a copy cargo uplifted for a requested
                # package; the engine crate's is the only one this build asks
                # for. A crate that also builds a shared library has no hash
                # in its file names at all, inside deps/ too.
                if p.parent.name != "deps" and p.name != f"lib{LIB}.rlib":
                    continue
                deps[p.name] = p
        elif reason == "build-script-executed":
            dirs = []
            for entry in m.get("linked_paths", []):
                _, _, path = entry.rpartition("=") if "=" in entry else ("", "", entry)
                dirs.append(Path(path))
            # A crate that ships libraries in its own source (the `windows`
            # crates' import libraries) adds that directory to the search path
            # and leaves the naming to a `#[link]` in another crate, so the
            # file is in no rlib and the app's link has to find it. Only for
            # the target's build scripts, and only inside cargo's registry:
            # a system directory is the app machine's own.
            if marker in m.get("out_dir", ""):
                for d in dirs:
                    if d.is_dir() and under(d, registry):
                        for f in sorted(d.iterdir()):
                            if f.is_file() and f.suffix in (".lib", ".a"):
                                native[f.name] = f
            for lib in m.get("linked_libs", []):
                kind, _, name = lib.partition("=")
                if not name or "-bundle" not in kind:
                    continue
                for d in dirs:
                    found = next(
                        (d / f for f in native_lib_names(name) if (d / f).is_file()), None
                    )
                    if found:
                        native[found.name] = found
                        break
                else:
                    fail(f"{m['package_id']} links {lib} and no file for it was found")
    if f"lib{LIB}.rlib" not in deps:
        fail(f"the build produced no lib{LIB}.rlib; was it built for {triple}?")
    return sorted(deps.values()), sorted(native.values())


def cmd_collect(args: argparse.Namespace) -> None:
    info = rustc_info()
    triple = info["host"]
    name = TARGET_NAMES.get(triple) or fail(f"no release name for {triple}")
    msgs = read_messages(args.messages)
    deps, native = collect_files(msgs, triple)

    out = Path(args.out).resolve()
    stage = out / f"lumenui-engine-{name}"
    if stage.exists():
        shutil.rmtree(stage)
    (stage / "deps").mkdir(parents=True)
    files = []
    for group, paths in (("deps", deps), ("native", native)):
        if paths:
            (stage / group).mkdir(exist_ok=True)
        for p in paths:
            dest = stage / group / p.name
            shutil.copy2(p, dest)
            files.append(
                {
                    "path": f"{group}/{p.name}",
                    "size": dest.stat().st_size,
                    "sha256": sha256(dest),
                }
            )

    manifest = {
        "schema": 1,
        "crate": LIB,
        "lumen_version": workspace_version(),
        "target": name,
        "triple": triple,
        "rustc": {
            "release": info["release"],
            "commit_hash": info["commit-hash"],
            "version": info["version_line"],
        },
        "files": files,
    }
    text = json.dumps(manifest, indent=2) + "\n"
    (stage / "manifest.json").write_text(text)
    (out / f"lumenui-engine-{name}.json").write_text(text)

    archive = out / f"lumenui-engine-{name}.tar.zst"
    write_archive(stage, archive, args.level)

    raw = sum(f["size"] for f in files)
    print(f"set:      {stage}")
    print(f"files:    {len(files)} ({len(deps)} in deps/, {len(native)} in native/)")
    print(f"raw:      {raw / 2**20:.1f} MiB")
    print(f"archive:  {archive} ({archive.stat().st_size / 2**20:.1f} MiB)")


def write_archive(stage: Path, archive: Path, level: int) -> None:
    """A tar of the set's contents, compressed by the `zstd` command."""
    zstd = shutil.which("zstd") or fail("zstd is not on PATH")
    with open(archive, "wb") as sink:
        proc = subprocess.Popen(
            [zstd, f"-{level}", "-T0", "-q", "-c"], stdin=subprocess.PIPE, stdout=sink
        )
        assert proc.stdin is not None
        with tarfile.open(fileobj=proc.stdin, mode="w|", format=tarfile.PAX_FORMAT) as tar:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    info = tar.gettarinfo(path, arcname=path.relative_to(stage).as_posix())
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    with open(path, "rb") as f:
                        tar.addfile(info, f)
        proc.stdin.close()
        if proc.wait() != 0:
            fail("zstd failed")


# -------------------------------------------------------------------- check


def cmd_check(args: argparse.Namespace) -> None:
    """Every crate cargo compiled must be the app, `lumenui`, or a crate
    `lumenui`'s build script needs; anything else is the engine compiling."""
    app = Path(args.app).resolve()
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=app,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    lumenui = next((i for i, p in by_id.items() if p["name"] == "lumenui"), None)
    if lumenui is None:
        fail(f"{app} does not depend on lumenui")

    # The closure of lumenui's build-dependencies, through every edge kind
    # (a build dependency's own build dependencies compile too).
    allowed = set(meta["workspace_members"]) | {lumenui}
    stack = [
        d["pkg"]
        for d in nodes[lumenui]["deps"]
        if any(k["kind"] == "build" for k in d["dep_kinds"])
    ]
    while stack:
        pid = stack.pop()
        if pid in allowed:
            continue
        allowed.add(pid)
        stack.extend(d["pkg"] for d in nodes[pid]["deps"])
    allowed_names = {by_id[i]["name"] for i in allowed}

    # cargo colours its status words when the terminal asks for it (CI sets
    # CARGO_TERM_COLOR=always), so the escapes come off before matching.
    log = re.sub(r"\x1b\[[0-9;]*m", "", Path(args.log).read_text(errors="replace"))
    compiled = re.findall(r"^\s*Compiling (\S+) v\S+", log, re.M)
    if not compiled:
        fail(f"{args.log} shows no `Compiling` line; is it a fresh `cargo build -v` log?")
    engine = sorted({c for c in compiled if c not in allowed_names})
    if engine:
        fail(
            "the app's build compiled crates that should have come prebuilt: "
            + ", ".join(engine)
        )
    print(
        f"ok: {len(compiled)} crates compiled, all of them the app, lumenui, "
        "or lumenui's build script's dependencies"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--target-dir", required=True)
    b.add_argument("--messages", required=True)
    c = sub.add_parser("collect")
    c.add_argument("--messages", required=True)
    c.add_argument("--out", required=True)
    c.add_argument("--level", type=int, default=19)
    k = sub.add_parser("check")
    k.add_argument("--app", required=True)
    k.add_argument("--log", required=True)
    args = parser.parse_args()
    {"build": cmd_build, "collect": cmd_collect, "check": cmd_check}[args.cmd](args)


if __name__ == "__main__":
    main()
