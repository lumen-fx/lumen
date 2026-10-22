#!/usr/bin/env python3
"""Build, collect and check the prebuilt engine the published `lumenui` links.

An app depends on `lumenui` (public/lumenui/rust), whose build script
downloads, for its target, the compiled crates of `lumenui-engine`
(public/lumenui/rust/engine) and points rustc at them, so the app compiles
none of the engine. The same set carries
the runtime modules listed in MODULES, compiled in the same build so they
share the engine's crates, and each reaches an app through a wrapper crate
this script generates (`lumenui-canvas` re-exports `lumen-canvas`). This
script makes that set, writes the wrapper crates, and checks that an app
really builds from them.

  build    Compile the engine for this machine's target, the way a release
           does, and record what cargo produced.
  collect  Turn that record into the set: exactly the target closure's
           `.rlib` files, the host proc-macro libraries they name, the
           native static libraries they link without carrying, and the
           `lumen-crash` helper under `bin/`, plus a manifest. Writes the unpacked set, `lumenui-engine-<target>.tar.zst`
           and `lumenui-engine-<target>.json` (the manifest, also inside the
           archive, published on its own so a build can check the compiler
           before downloading the archive).
  wrappers Write the published crate of every module in MODULES,
           `lumenui-<name>` for `lumen-<name>`, under OUT_DIR: a manifest, a
           build script, and a `src/lib.rs` that re-exports the module, all
           generated from the module's own manifest and
           `public/lumenui/rust`'s. A module directory's `sdk/examples/`
           becomes the crate's examples, and its `sdk/smoke/` (an app's `src/`
           and `lumen.toml`) becomes a smoke app in `<crate>/smoke` depending
           on `lumenui` and the crate alone. Nothing of a wrapper is
           committed: CI lints, builds and runs what this writes, and a
           publish uploads it.
  check    Read the `cargo build -v` log of an app built against the set, and
           fail if cargo compiled any crate other than the app, `lumenui`, its
           `lumenui-<module>` wrappers, and the crates their build scripts
           need.

Usage:

    tools/release/lumenui-engine.py build --target-dir DIR --messages build.jsonl
    tools/release/lumenui-engine.py collect --messages build.jsonl --out dist/
    tools/release/lumenui-engine.py wrappers --out OUT_DIR
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
  - The `lumen-crash` helper is built in a cargo invocation of its own, into
    the same target directory: in the engine's invocation its features would
    unify with the engine's, and its dependencies would show up in the
    messages as a second build of crates the set already holds. `collect`
    takes it from beside the engine's uplifted rlib, where cargo puts a
    requested binary. `lumenui_build::link_engine`, which
    `public/lumenui/rust/build.rs` runs, copies it beside the app.

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
import textwrap
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PACKAGE = "lumenui-engine"
LIB = "lumenui_engine"
# The crash helper every windowed app starts beside itself. The set carries
# it so `cargo build` of an app has one beside the app's executable.
HELPER = "lumen-crash"

# The runtime modules the Rust SDK carries, each published as the
# `lumenui-<name>` crate `wrappers` generates for `lumen-<name>`. Adding a
# module here is the whole of putting it in the SDK. Built in the one
# invocation with the engine: a separate build resolves features on its own
# and produces crates whose hashes are not the engine's, which an app cannot
# use together.
MODULES = ("lumen-canvas",)

# The rlib of each crate the build asks for by name, which cargo reports by
# the copy it uplifts out of deps/. A module's library is named after its
# package.
REQUESTED_RLIBS = {f"lib{LIB}.rlib"} | {
    f"lib{module.replace('-', '_')}.rlib" for module in MODULES
}

# The release's name for each target, the one every asset name carries. The
# same table is in public/lumenui/rust/build/src/fetch.rs.
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
        *(arg for module in MODULES for arg in ("-p", module)),
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

    helper = ["cargo", "build", "--release", "--locked", "-p", HELPER, "--target", triple]
    print("+", " ".join(helper), file=sys.stderr)
    proc = subprocess.run(helper, cwd=REPO, env=env, stdout=sys.stderr)
    if proc.returncode != 0:
        fail(f"cargo build of {HELPER} failed ({proc.returncode})")


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
                # package: the engine crate and the modules are the ones this
                # build asks for. A crate that also builds a shared library has
                # no hash in its file names at all, inside deps/ too.
                if p.parent.name != "deps" and p.name not in REQUESTED_RLIBS:
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


def helper_file(deps: list[Path], triple: str) -> Path:
    """The `lumen-crash` the build left beside the engine's uplifted rlib."""
    engine = next(p for p in deps if p.name == f"lib{LIB}.rlib")
    name = HELPER + (".exe" if "windows" in triple else "")
    path = engine.parent / name
    if not path.is_file():
        fail(f"no {name} beside {engine}; `build` makes it")
    return path


def cmd_collect(args: argparse.Namespace) -> None:
    info = rustc_info()
    triple = info["host"]
    name = TARGET_NAMES.get(triple) or fail(f"no release name for {triple}")
    msgs = read_messages(args.messages)
    deps, native = collect_files(msgs, triple)
    helper = helper_file(deps, triple)

    out = Path(args.out).resolve()
    stage = out / f"lumenui-engine-{name}"
    if stage.exists():
        shutil.rmtree(stage)
    (stage / "deps").mkdir(parents=True)
    files = []
    for group, paths in (("deps", deps), ("native", native), ("bin", [helper])):
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
    print(
        f"files:    {len(files)} ({len(deps)} in deps/, {len(native)} in native/, 1 in bin/)"
    )
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


# ----------------------------------------------------------------- wrappers


def package_metadata(*args: str) -> list[dict]:
    """`cargo metadata --no-deps` packages, of the workspace or one manifest."""
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", *args],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(out)["packages"]


def toml_str(value: str) -> str:
    """A TOML basic string. JSON's string escapes are a subset of TOML's."""
    return json.dumps(value)


def toml_list(values: list[str]) -> str:
    return "[" + ", ".join(toml_str(v) for v in values) + "]"


def doc_lines(text: str) -> list[str]:
    """Prose as `//!` lines, with HTML-looking text escaped for rustdoc."""
    text = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    return [f"//! {line}" for line in textwrap.wrap(text, 76, break_on_hyphens=False)]


def wrapper_manifest(module: dict, sdk: dict, name: str, short: str) -> str:
    lumenui = (REPO / "public" / "lumenui" / "rust").as_posix()
    description = (
        f"{module['description'].rstrip('.')}. "
        "For Rust SDK (lumenui) apps, linked from the release's prebuilt engine."
    )
    version = toml_str(sdk["version"])
    return f"""\
# Generated by tools/release/lumenui-engine.py `wrappers` from the
# `{module['name']}` module and public/lumenui/rust. Not committed; edit the
# generator.
[workspace]

[package]
name = {toml_str(name)}
version = {version}
edition = {toml_str(sdk['edition'])}
license = {toml_str(sdk['license'])}
authors = {toml_list(sdk['authors'])}
repository = {toml_str(sdk['repository'])}
homepage = {toml_str(sdk['homepage'])}
documentation = {toml_str(f"https://docs.lumenfx.dev/reference/ffi/#{short}")}
rust-version = {toml_str(sdk['rust_version'])}
description = {toml_str(description)}
keywords = {toml_list(module['keywords'][:5])}
categories = {toml_list(module['categories'])}
build = "build.rs"
include = ["build.rs", "src/**", "Cargo.toml"]

[lib]
name = {toml_str(name.replace('-', '_'))}

[dependencies]
# The SDK, whose build script points the compiler at the prebuilt set this
# crate's module comes from. Always the same version: the set is one build.
lumenui = {{ version = {version}, path = {toml_str(lumenui)} }}

[package.metadata.docs.rs]
# docs.rs builds without network access, so the crate documents itself
# without the module, as `lumenui` does.
targets = ["x86_64-unknown-linux-gnu"]
"""


WRAPPER_BUILD_RS = """\
//! Tells the crate whether the module is there to re-export.
//!
//! The module comes from the prebuilt engine set `lumenui`'s build script
//! puts on the compiler's search path, which reaches this crate because it
//! depends on `lumenui`. docs.rs builds with no network, so `lumenui`
//! fetches nothing there; this crate then documents itself without the
//! module (`cfg(lumenui_docs_only)`), the same way `lumenui` does.

fn main() {
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(lumenui_docs_only)");
    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo:rustc-cfg=lumenui_docs_only");
    }
}
"""


def wrapper_lib(module: dict, name: str, short: str, lib: str) -> str:
    about = doc_lines(
        f"The `{module['name']}` runtime module for a lumenui app, re-exported "
        "from the release's prebuilt engine set that `lumenui` downloads. The "
        "crate builds nothing of its own and needs the same pinned compiler "
        "`lumenui` does."
    )
    lines = [
        f"//! # {name}",
        "//!",
        *doc_lines(module["description"]),
        "//!",
        *about,
        "//!",
        "//! See the [Rust SDK reference]"
        f"(https://docs.lumenfx.dev/reference/ffi/#{short}).",
        "",
        "#![forbid(unsafe_code)]",
        "",
        "#[cfg(not(lumenui_docs_only))]",
        f"extern crate {lib};",
        "",
        "#[cfg(not(lumenui_docs_only))]",
        f"pub use {lib}::*;",
    ]
    return "\n".join(lines) + "\n"


def smoke_manifest(name: str) -> str:
    lumenui = (REPO / "public" / "lumenui" / "rust").as_posix()
    return f"""\
# Generated by tools/release/lumenui-engine.py `wrappers`: an app shaped the
# way a user's is, on `lumenui` and `{name}` and nothing of the engine.
[workspace]

[package]
name = {toml_str(f"{name}-smoke")}
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
lumenui = {{ path = {toml_str(lumenui)} }}
{name} = {{ path = ".." }}
"""


def toolchain_file() -> str:
    """The pinned compiler, the only one that reads the set's crates."""
    text = (REPO / "rust-toolchain.toml").read_text()
    m = re.search(r'^channel\s*=\s*"([^"]+)"', text, re.M)
    if not m:
        fail("no channel in rust-toolchain.toml")
    return f'[toolchain]\nchannel = "{m.group(1)}"\ncomponents = ["rustfmt", "clippy"]\n'


def cmd_wrappers(args: argparse.Namespace) -> None:
    out = Path(args.out).resolve()
    if under(out, REPO):
        # Inside the checkout, the workspace above would claim the crates.
        fail(f"{out} is inside the checkout; write the crates outside it")
    sdk_manifest = REPO / "public" / "lumenui" / "rust" / "Cargo.toml"
    (sdk,) = package_metadata("--manifest-path", str(sdk_manifest))
    packages = {p["name"]: p for p in package_metadata()}
    out.mkdir(parents=True, exist_ok=True)
    # rustup looks upward from a crate for the pin, so one file covers every
    # crate and smoke app written here.
    (out / "rust-toolchain.toml").write_text(toolchain_file())
    for module_name in MODULES:
        module = packages.get(module_name)
        if module is None:
            fail(f"no workspace crate named {module_name}")
        if not module_name.startswith("lumen-"):
            fail(f"{module_name} is not named lumen-<name>")
        for key in ("description", "keywords", "categories"):
            if not module.get(key):
                fail(f"{module_name} needs `{key}` in its manifest; its wrapper publishes it")
        lib = next((t["name"] for t in module["targets"] if "lib" in t["kind"]), None)
        if lib is None:
            fail(f"{module_name} has no lib target")
        short = module_name.removeprefix("lumen-")
        name = f"lumenui-{short}"

        crate = out / name
        if crate.exists():
            shutil.rmtree(crate)
        (crate / "src").mkdir(parents=True)
        (crate / "Cargo.toml").write_text(wrapper_manifest(module, sdk, name, short))
        (crate / "build.rs").write_text(WRAPPER_BUILD_RS)
        (crate / "src" / "lib.rs").write_text(wrapper_lib(module, name, short, lib))

        extras = Path(module["manifest_path"]).parent / "sdk"
        if (extras / "examples").is_dir():
            shutil.copytree(extras / "examples", crate / "examples")
        if (extras / "smoke").is_dir():
            shutil.copytree(extras / "smoke", crate / "smoke")
            (crate / "smoke" / "Cargo.toml").write_text(smoke_manifest(name))
        print(crate)


# -------------------------------------------------------------------- check


def cmd_check(args: argparse.Namespace) -> None:
    """Every crate cargo compiled must be the app, a published SDK crate
    (`lumenui` and its `lumenui-<module>` wrappers), or a crate one of their
    build scripts needs; anything else is the engine compiling."""
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
    sdk = {
        i
        for i, p in by_id.items()
        if p["name"] == "lumenui" or p["name"].startswith("lumenui-")
    }

    # The closure of the build-dependencies of the SDK crates and of the app
    # itself (its build script compiles the UI through `lumenui-build`),
    # through every edge kind: a build dependency's own dependencies compile
    # too. A crate already allowed for being an SDK crate (`lumenui-build`)
    # still has its dependencies walked.
    allowed = set(meta["workspace_members"]) | sdk
    stack = [
        d["pkg"]
        for crate in sdk | set(meta["workspace_members"])
        for d in nodes[crate]["deps"]
        if any(k["kind"] == "build" for k in d["dep_kinds"])
    ]
    walked: set[str] = set()
    while stack:
        pid = stack.pop()
        if pid in walked:
            continue
        walked.add(pid)
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
        f"ok: {len(compiled)} crates compiled, all of them the app, the SDK "
        "crates, or their build scripts' dependencies"
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
    w = sub.add_parser("wrappers")
    w.add_argument("--out", required=True)
    k = sub.add_parser("check")
    k.add_argument("--app", required=True)
    k.add_argument("--log", required=True)
    args = parser.parse_args()
    {
        "build": cmd_build,
        "collect": cmd_collect,
        "wrappers": cmd_wrappers,
        "check": cmd_check,
    }[args.cmd](args)


if __name__ == "__main__":
    main()
