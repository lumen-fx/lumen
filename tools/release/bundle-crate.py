#!/usr/bin/env python3
"""Write a crate of this workspace out as one self-contained package.

The crates Lumen publishes to crates.io (`lumenc`, the plugin SDKs) are built
on engine crates that are not published. This turns one of them into a single
package that carries the source of every Lumen crate it uses, so it publishes
on its own: each internal crate it reaches becomes a module of the package,
and the crates.io crates those reach become the package's own dependencies.

What it does to get there:

  1. Resolves the crate's dependency graph the way a default build of it sees
     it, on every target (`cargo tree --target all -e normal`), which says
     which internal crates it uses, which features each is built with, and
     which crates.io crates each one depends on.
  2. Copies the crate's own `src/` to the package, and every internal crate it
     reaches under `src/vendor/<name>/`, declared from the package root as
     `pub mod <lib name>` with a `#[path]`.
  3. Rewrites paths so each module still finds what it named: `crate::` in an
     internal crate becomes `crate::<its module>::`, `$crate::` likewise, and
     a path through another internal crate (`lumen_core::x`) becomes
     `crate::lumen_core::x`. A `#[macro_export]` macro lands at the package
     root, so the module it came from re-exports it under its old path.
  4. Freezes features: `feature = "x"` in a `cfg` becomes `all()` or `any()`
     by whether the resolved build enabled it. The package has no features of
     its own; it is the crate's default build.
  5. Handles the Lumen proc-macro crates, which cannot be modules of a
     library. A derive whose output does not depend on the target is run once
     against each item that uses it, and its output replaces the attribute
     (`expand_internal_derives`). Any other proc-macro crate travels under
     `macros/`, and the package's build script compiles it for the host
     (`bundle_proc_crates`).
  6. Merges build scripts: the package's `build.rs` runs the build script of
     every carried crate, each as a module, and its build-dependencies become
     the package's. A build script that needs another Lumen crate cannot be
     carried; one whose output only the crate's tests read is left out.
  7. Copies the files a carried crate reads from outside its `src/`, and the
     workspace's Cargo.lock, so `cargo install --locked` builds the versions
     this tree was tested with.
  8. Writes a manifest naming the crates.io dependencies, each at the
     requirement its users declared and the features the resolved build
     enabled.

Usage:

    tools/release/bundle-crate.py lumenc --out DIR

`DIR` must not exist; the package is written there, ready for `cargo package`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Directories of a crate that hold nothing its library compiles.
SKIP_DIRS = {"tests", "benches", "examples", "fixture", "fixtures", "target", "fuzz"}

# Crate-level inner attributes a module cannot carry.
CRATE_ONLY_ATTR = re.compile(
    r"^\s*#!\[(?:no_std|recursion_limit|crate_type|crate_name|feature\(|"
    r"cfg_attr\(\s*docsrs\s*,\s*feature|doc\(\s*html_)[^\n]*\]\s*$",
    re.M,
)


@dataclass
class Node:
    name: str
    version: str
    path: Path | None  # set for a crate of this workspace
    lib: str
    proc_macro: bool
    features: set[str] = field(default_factory=set)
    children: set[tuple[str, str]] = field(default_factory=set)


def cargo(*args: str, cwd: Path = REPO, env: dict | None = None) -> str:
    out = subprocess.run(
        ["cargo", *args], cwd=cwd, capture_output=True, text=True, env=env
    )
    if out.returncode != 0:
        raise SystemExit(f"cargo {' '.join(args)} failed:\n{out.stderr.strip()}")
    return out.stdout


TREE_LINE = re.compile(r"^(\d+)(\S+) v(\S+)( \(proc-macro\))?(?: \(([^)]*)\))?( \(proc-macro\))?\|([^|]*)\|(\S*)")


def resolve(root: str) -> dict[tuple[str, str], Node]:
    """The graph a default build of `root` compiles, on every target."""
    text = cargo(
        "tree", "-p", root, "--target", "all", "-e", "normal",
        "--prefix", "depth", "-f", "{p}|{f}|{lib}",
    )
    nodes: dict[tuple[str, str], Node] = {}
    stack: list[tuple[str, str]] = []
    for line in text.splitlines():
        m = TREE_LINE.match(line)
        if not m:
            continue
        depth = int(m.group(1))
        name, version = m.group(2), m.group(3)
        proc_macro = bool(m.group(4) or m.group(6))
        source = m.group(5) or ""
        path = Path(source) if source.startswith("/") else None
        key = (name, version)
        node = nodes.get(key)
        if node is None:
            node = nodes[key] = Node(name, version, path, m.group(8), proc_macro)
        node.features |= {f for f in m.group(7).split(",") if f}
        del stack[depth:]
        if stack:
            nodes[stack[-1]].children.add(key)
        stack.append(key)
    return nodes


def metadata() -> dict[str, dict]:
    meta = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
    return {p["name"]: p for p in meta["packages"]}


# ------------------------------------------------------------------ rewriting


def balanced(text: str, start: int) -> int:
    """Index just past the parenthesis group opening at `start`."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        elif c == '"':
            i = text.index('"', i + 1)
        i += 1
    return len(text)


CFG_START = re.compile(r"\bcfg(?:_attr)?\s*!?\s*\(")
FEATURE = re.compile(r'\bfeature\s*=\s*"([^"]+)"')


def freeze_features(text: str, enabled: set[str]) -> str:
    out = []
    pos = 0
    for m in CFG_START.finditer(text):
        if m.start() < pos:
            continue
        end = balanced(text, m.end() - 1)
        group = text[m.end() - 1 : end]
        group = FEATURE.sub(lambda f: "all()" if f.group(1) in enabled else "any()", group)
        out.append(text[pos : m.end() - 1])
        out.append(group)
        pos = end
    out.append(text[pos:])
    return "".join(out)


def rewrite(text: str, module: str | None, internal: set[str], root: str = "crate") -> str:
    """Re-root one source file's paths inside the package.

    `module` is the internal crate's module name, or None for the package's
    own sources, whose `crate::` already means the package root. `root` is
    the path the carried crates hang off: `crate` inside the library, its
    name in a binary.
    """
    if module is not None:
        text = re.sub(r"\$crate::", f"$crate::{module}::", text)
        text = re.sub(r"(?<![\w$])crate::", f"crate::{module}::", text)
    if internal:
        names = "|".join(sorted(map(re.escape, internal), key=len, reverse=True))
        # `::lumen_core::x`, `lumen_core::x` and `use lumen_core;` all name the
        # crate, now a module at the root.
        text = re.sub(rf"(?<![\w$:])::({names})\b", rf"{root}::\1", text)
        text = re.sub(rf"(?<![\w$:]){'(' + names + ')'}::", rf"{root}::\1::", text)
        text = re.sub(rf"(\buse\s+)({names})\s*;", rf"\1{root}::\2;", text)
        text = re.sub(rf"(\buse\s+)({names})(\s+as\s)", rf"\1{root}::\2\3", text)
    return text


MACRO_EXPORT = re.compile(r"#\[macro_export[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*macro_rules!\s*(\w+)")


def exported_macros(src: Path) -> list[str]:
    names = []
    for path in sorted(src.rglob("*.rs")):
        names += MACRO_EXPORT.findall(path.read_text(encoding="utf-8"))
    return names


# ------------------------------------------------------------ proc-macro uses


def derive_names(proc_dir: Path) -> list[str]:
    names = []
    for path in sorted((proc_dir / "src").rglob("*.rs")):
        names += re.findall(
            r"#\[proc_macro_derive\(\s*(\w+)", path.read_text(encoding="utf-8")
        )
    return names


ITEM_START = re.compile(r"\b(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+(\w+)")


def item_span(text: str, at: int) -> tuple[int, int, str]:
    """(start, end, name) of the struct or enum whose attributes start at `at`."""
    m = ITEM_START.search(text, at)
    if not m:
        raise SystemExit("bundle-crate: a derive with no struct or enum after it")
    i = m.end()
    while text[i] not in "{;(":
        i += 1
    if text[i] == ";":
        return at, i + 1, m.group(1)
    if text[i] == "(":
        i = balanced(text, i)
        while text[i] != ";":
            i += 1
        return at, i + 1, m.group(1)
    depth = 0
    while True:
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return at, i + 1, m.group(1)
        i += 1


def expand_internal_derives(
    pkg_dir: Path, proc_crates: dict[str, Path], target_dir: Path, modules: set[str]
) -> None:
    """Replace each use of a derive from an internal proc-macro crate with what
    the derive generates.

    A proc-macro crate cannot be a module, and publishing it on its own is the
    thing this package exists to avoid, so its uses are expanded here, once:
    the item is copied into a scratch crate that depends on the proc-macro
    crate, rustc prints that crate expanded, and everything after the item is
    the derive's output. The item keeps its other derives and loses this one
    and the helper attributes that belonged to it.
    """
    for proc_name, proc_dir in proc_crates.items():
        derives = derive_names(proc_dir)
        lib = proc_name.replace("-", "_")
        helpers = set()
        for path in (proc_dir / "src").rglob("*.rs"):
            for group in re.findall(r"attributes\(([^)]*)\)", path.read_text(encoding="utf-8")):
                helpers |= {h.strip() for h in group.split(",") if h.strip()}
        use_line = re.compile(rf"^\s*use\s+(?:crate::)?{lib}::(\w+);\s*\n", re.M)
        for path in sorted(pkg_dir.joinpath("src").rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            if lib not in text:
                continue
            text = use_line.sub("", text)
            for derive in derives:
                pattern = re.compile(rf"^[ \t]*#\[derive\(([^)]*\b{derive}\b[^)]*)\)\]", re.M)
                while True:
                    m = pattern.search(text)
                    if not m:
                        break
                    attrs_start = text.rfind("\n", 0, m.start()) + 1
                    # Back up over the doc comments and attributes above it.
                    lines = text[:attrs_start].split("\n")
                    while len(lines) > 1 and re.match(r"^\s*(///|#\[)", lines[-2]):
                        lines.pop(-2)
                    start = len("\n".join(lines[:-1])) + (1 if len(lines) > 1 else 0)
                    _, end, name = item_span(text, m.start())
                    item = text[start:end]
                    generated = rewrite(
                        run_derive(proc_name, proc_dir, derive, item, target_dir),
                        module_of(pkg_dir, path),
                        modules,
                    )
                    others = [d.strip() for d in m.group(1).split(",") if d.strip() and d.strip() != derive]
                    kept = item.replace(m.group(0), f"#[derive({', '.join(others)})]" if others else "")
                    for helper in helpers:
                        kept = strip_attr(kept, helper)
                    text = text[:start] + kept + "\n" + generated + text[end:]
            path.write_text(text, encoding="utf-8")


def module_of(pkg_dir: Path, path: Path) -> str | None:
    """The internal crate module a file of the package belongs to, or None for
    the package's own sources."""
    parts = path.relative_to(pkg_dir / "src").parts
    return parts[1] if len(parts) > 2 and parts[0] == "vendor" else None


def strip_attr(text: str, name: str) -> str:
    out = []
    pos = 0
    for m in re.finditer(rf"#\[\s*{name}\s*\(", text):
        if m.start() < pos:
            continue
        end = balanced(text, m.end() - 1)
        while text[end] != "]":
            end += 1
        out.append(text[pos : m.start()])
        pos = end + 1
        while pos < len(text) and text[pos] in " \t":
            pos += 1
        if pos < len(text) and text[pos] == "\n":
            pos += 1
    out.append(text[pos:])
    return "".join(out)


def run_derive(proc_name: str, proc_dir: Path, derive: str, item: str, target_dir: Path) -> str:
    """The items `derive` generates for `item`, as source."""
    lib = proc_name.replace("-", "_")
    # Keep only this derive and its helpers: the scratch crate expands, it
    # does not type-check, so the item's other derives would only need their
    # crates resolved for nothing.
    bare = re.sub(r"#\[derive\([^)]*\)\]", f"#[derive({lib}::{derive})]", item)
    bare = re.sub(r"^\s*///.*\n", "", bare, flags=re.M)
    with tempfile.TemporaryDirectory(prefix="bundle-derive-", dir=target_dir) as tmp:
        scratch = Path(tmp)
        deps = [f'{proc_name} = {{ path = "{proc_dir.as_posix()}" }}']
        # The expansion names crates the scratch crate has to be able to
        # resolve. The proc-macro crate's own tests expand against exactly
        # those, so its dev-dependencies are the list.
        meta = metadata()
        for dep in meta[proc_name]["dependencies"]:
            if dep["kind"] != "dev" or dep["name"] == proc_name:
                continue
            key = dep["rename"] or dep["name"]
            if dep["name"] in meta and dep.get("path"):
                spec = f'path = "{Path(dep["path"]).as_posix()}"'
            else:
                spec = f"version = {toml_str(dep['req'])}"
                if dep["name"] != key:
                    spec += f", package = {toml_str(dep['name'])}"
            if dep["features"]:
                spec += ", features = [" + ", ".join(map(toml_str, dep["features"])) + "]"
            deps.append(f"{key} = {{ {spec} }}")
        (scratch / "Cargo.toml").write_text(
            "[workspace]\n[package]\nname = \"bundle-derive\"\nversion = \"0.0.0\"\n"
            "edition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[dependencies]\n" + "\n".join(deps) + "\n"
        )
        (scratch / "lib.rs").write_text(item_marker(bare))
        env = dict(os.environ, RUSTC_BOOTSTRAP="1", CARGO_TARGET_DIR=str(target_dir / "derive"))
        lock = REPO / "Cargo.lock"
        shutil.copy(lock, scratch / "Cargo.lock")
        expanded = cargo(
            "rustc", "--lib", "--quiet", "--", "-Zunpretty=expanded", cwd=scratch, env=env
        )
    # The derive's output sits between the item and the marker after it.
    marker = expanded.rfind("\n", 0, expanded.find("BUNDLE_DERIVE_END"))
    name = ITEM_START.search(item).group(1)
    at = re.search(rf"\b(?:struct|enum)\s+{name}\b", expanded).start()
    _, end, _ = item_span(expanded, at)
    generated = expanded[end:marker]
    if not generated.strip():
        raise SystemExit(f"bundle-crate: #[derive({derive})] generated nothing to keep")
    return generated + "\n"


def item_marker(item: str) -> str:
    return item + "\nconst BUNDLE_DERIVE_END: () = ();\n"


# ---------------------------------------------------------------- manifest


def toml_str(value: str) -> str:
    return json.dumps(value)


def dependency_table(
    nodes: dict[tuple[str, str], Node], bundled: list[Node], meta: dict[str, dict]
) -> tuple[dict[str, str], dict[str, dict[str, str]]]:
    """([dependencies] lines, per-target [dependencies] lines), keyed by the
    name the code uses."""
    plain: dict[str, dict] = {}
    targeted: dict[str, dict[str, dict]] = {}
    for node in bundled:
        declared = meta[node.name]["dependencies"]
        for child_key in sorted(node.children):
            child = nodes[child_key]
            if child.path is not None:
                continue
            specs = [d for d in declared if d["name"] == child.name and d["kind"] is None]
            if not specs:
                raise SystemExit(f"bundle-crate: {node.name} -> {child.name} is not a declared dependency")
            for spec in specs:
                key = spec["rename"] or child.name
                entry = {
                    "package": child.name,
                    # A git dependency with no version is published as the
                    # registry release of what it resolved to.
                    "req": spec["req"] if spec["req"] != "*" else f"^{child.version}",
                    "version": child.version,
                    "features": set(child.features),
                }
                table = plain if spec["target"] is None else targeted.setdefault(spec["target"], {})
                prev = table.get(key)
                if prev:
                    if prev["package"] != child.name:
                        raise SystemExit(f"bundle-crate: `{key}` names both {prev['package']} and {child.name}")
                    if prev["version"] != child.version:
                        raise SystemExit(
                            f"bundle-crate: {child.name} resolves to {prev['version']} and {child.version}; "
                            f"one package cannot depend on both under the name `{key}`"
                        )
                    prev["features"] |= entry["features"]
                else:
                    table[key] = entry

    def line(key: str, entry: dict) -> str:
        parts = [f"version = {toml_str(entry['req'])}"]
        if entry["package"] != key:
            parts.append(f"package = {toml_str(entry['package'])}")
        features = sorted(entry["features"] - {"default"})
        parts.append("default-features = " + ("true" if "default" in entry["features"] else "false"))
        if features:
            parts.append("features = [" + ", ".join(map(toml_str, features)) + "]")
        return f"{key} = {{ {', '.join(parts)} }}"

    plain_lines = {k: line(k, e) for k, e in sorted(plain.items())}
    target_lines = {
        t: {k: line(k, e) for k, e in sorted(entries.items()) if k not in plain}
        for t, entries in sorted(targeted.items())
    }
    return plain_lines, {t: v for t, v in target_lines.items() if v}


# --------------------------------------------------------------------- main


def copy_tree(src: Path, dst: Path) -> None:
    """Copy what a library compiles from: its `src/`, without directories
    that are packages of their own (`core/` holds its sibling crates)."""

    def ignore(directory: str, names: list[str]) -> set[str]:
        return {
            n
            for n in names
            if n in SKIP_DIRS or (Path(directory, n) / "Cargo.toml").is_file()
        }

    shutil.copytree(src, dst, ignore=ignore)


OUTSIDE_SRC = re.compile(r'include_(?:str|bytes)!\(\s*"(\.\./[^"]*)"|#\[path\s*=\s*"(\.\./[^"]*)"')


def carry_outside_src(crate_dir: Path, dest_dir: Path, crate: str, package: Path) -> None:
    """Copy the files a crate's library reads from outside `src/` (through
    `include_str!`, `include_bytes!` or `#[path]`).

    A file inside the crate goes to the same place under `dest_dir`, which
    mirrors the crate's directory. A file elsewhere in this repository goes
    under `extern/` at the package root, keeping its repository path, and the
    reference is rewritten to reach it there.
    """
    src = crate_dir / "src"
    for path in src.rglob("*.rs"):
        copied = dest_dir / path.relative_to(crate_dir)
        text = copied.read_text(encoding="utf-8")
        original = path.read_text(encoding="utf-8")
        for m in OUTSIDE_SRC.finditer(original):
            line_start = original.rfind("\n", 0, m.start()) + 1
            if original[line_start : m.start()].lstrip().startswith("//"):
                continue
            literal = m.group(1) or m.group(2)
            target = (path.parent / literal).resolve()
            if target.is_relative_to(crate_dir.resolve()):
                place = dest_dir / target.relative_to(crate_dir.resolve())
            elif target.is_relative_to(REPO):
                place = package / "extern" / target.relative_to(REPO)
                text = text.replace(f'"{literal}"', f'"{os.path.relpath(place, copied.parent)}"')
            else:
                raise SystemExit(f"bundle-crate: {crate} reads {target}, outside this repository")
            place.parent.mkdir(parents=True, exist_ok=True)
            if target.is_dir():
                shutil.copytree(target, place, dirs_exist_ok=True)
            else:
                shutil.copy(target, place)
        copied.write_text(text, encoding="utf-8")


# ------------------------------------------------------- proc-macro crates


NESTED_MANIFEST = "Cargo.toml.bundled"


def bundle_proc_crates(
    out: Path, procs: dict[str, Node], meta: dict[str, dict], modules: set[str]
) -> list[str]:
    """Carry proc-macro crates as sources the package's build script compiles.

    A proc-macro crate cannot be a module, and its expansion cannot always be
    taken once ahead of time: a function-like macro that emits `cfg_attr`
    expands differently per target. So the crate travels whole, under
    `macros/`, with its manifest renamed so `cargo package` keeps it inside
    this package. The build script builds it for the host and points the
    compiler at the result; the package root names it with `extern crate`.
    Paths the macro emits into an internal crate (`::lumen_capability::x`)
    become `crate::lumen_capability::x`, which resolves in the package the
    macro expands in. Returns the library names, for those `extern crate`s.
    """
    names = []
    members = []
    for name, node in sorted(procs.items()):
        pkg = meta[name]
        dest = out / "macros" / name
        copy_tree(node.path / "src", dest / "src")
        for path in dest.rglob("*.rs"):
            text = path.read_text(encoding="utf-8")
            text = freeze_features(text, node.features)
            path.write_text(rewrite(text, None, modules), encoding="utf-8")
        deps = []
        for dep in pkg["dependencies"]:
            if dep["kind"] is not None:
                continue
            if dep["name"] in meta:
                raise SystemExit(f"bundle-crate: proc-macro {name} depends on {dep['name']}")
            spec = [f"version = {toml_str(dep['req'])}"]
            if dep["rename"]:
                spec.append(f"package = {toml_str(dep['name'])}")
            if not dep["uses_default_features"]:
                spec.append("default-features = false")
            if dep["features"]:
                spec.append("features = [" + ", ".join(map(toml_str, dep["features"])) + "]")
            deps.append(f"{dep['rename'] or dep['name']} = {{ {', '.join(spec)} }}")
        (dest / NESTED_MANIFEST).write_text(
            f"[package]\nname = {toml_str(name)}\nversion = {toml_str(pkg['version'])}\n"
            f"edition = {toml_str(pkg['edition'])}\n\n[lib]\nproc-macro = true\n"
            f"path = \"src/lib.rs\"\n\n[dependencies]\n" + "\n".join(deps) + "\n"
        )
        members.append(name)
        names.append(node.lib)
    (out / "macros" / NESTED_MANIFEST).write_text(
        "[workspace]\nresolver = \"2\"\nmembers = ["
        + ", ".join(map(toml_str, members))
        + "]\n\n[profile.release]\ndebug = false\n"
    )
    return names


MACROS_BUILD = r"""
/// Builds the proc-macro crates under `macros/` for the host and points the
/// compiler at them (tools/release/bundle-crate.py). Offline: cargo fetched
/// every crate they need already, as this package's build-dependencies.
pub fn main() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create the macro build directory");
        for entry in std::fs::read_dir(from).expect("read the bundled macros") {
            let entry = entry.expect("read the bundled macros");
            let path = entry.path();
            let name = entry.file_name();
            let dest = if name == "Cargo.toml.bundled" {
                to.join("Cargo.toml")
            } else {
                to.join(&name)
            };
            if path.is_dir() {
                copy(&path, &dest);
            } else {
                std::fs::copy(&path, &dest).expect("copy a bundled macro source");
            }
        }
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=macros");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let work = out.join("macros");
    copy(&manifest_dir.join("macros"), &work);
    let target = out.join("macros-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--release", "--offline", "--manifest-path"])
        .arg(work.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET");
    let status = command.status().expect("run cargo for the bundled macros");
    assert!(status.success(), "building the bundled proc-macro crates failed");
    // The uplifted libraries, which keep one stable name whichever layout
    // cargo uses for its intermediate files.
    println!(
        "cargo:rustc-link-search=all={}",
        target.join("release").display()
    );
}
"""


# ------------------------------------------------------------ build scripts


def build_script(pkg: dict) -> Path | None:
    for target in pkg["targets"]:
        if "custom-build" in target["kind"]:
            return Path(target["src_path"])
    return None


def test_only_out_dir(src: Path) -> bool:
    """Whether every use of `OUT_DIR` in a crate's library sits under
    `#[cfg(test)]`, so its build script serves only its tests."""
    for path in src.rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        for m in re.finditer(r'env!\(\s*"OUT_DIR"', text):
            if "#[cfg(test)]" not in text[: m.start()]:
                return False
    return True


# --------------------------------------------------------------------- main


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("crate")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    out: Path = args.out.resolve()
    if out.exists():
        raise SystemExit(f"bundle-crate: {out} exists")
    meta = metadata()
    root_pkg = meta.get(args.crate)
    if root_pkg is None:
        raise SystemExit(f"bundle-crate: no workspace crate {args.crate}")
    nodes = resolve(args.crate)
    root = next(n for n in nodes.values() if n.name == args.crate and n.path is not None)
    lib_target = next((t for t in root_pkg["targets"] if "lib" in t["kind"]), None)
    if lib_target is None:
        raise SystemExit("bundle-crate: the crate needs a library target")
    root_lib_name = lib_target["name"]
    bins = [t for t in root_pkg["targets"] if "bin" in t["kind"]]

    # Every internal crate the default build reaches, with the features it
    # is built with across all the places it is reached from.
    internal: dict[str, Node] = {}
    for node in nodes.values():
        if node.path is None or node.name == args.crate:
            continue
        if node.name in internal:
            internal[node.name].features |= node.features
            internal[node.name].children |= node.children
        else:
            internal[node.name] = node
    procs = {n.name: n for n in internal.values() if n.proc_macro}
    libs = {n.name: n for n in internal.values() if not n.proc_macro}
    modules = {n.lib for n in libs.values()}

    # Derives whose expansion does not depend on the target are expanded once
    # here; every other proc-macro crate is carried and built at install.
    expandable = {}
    for name, node in procs.items():
        source = "".join(p.read_text(encoding="utf-8") for p in (node.path / "src").rglob("*.rs"))
        only_derives = "#[proc_macro_derive" in source and not re.search(
            r"#\[proc_macro\]|#\[proc_macro_attribute\]", source
        )
        quoted_cfg = re.search(r"quote!\s*\{[^}]*#\[cfg", source, re.S)
        if only_derives and not quoted_cfg:
            expandable[name] = node.path
    carried = {n: procs[n] for n in procs if n not in expandable}

    out.mkdir(parents=True)
    root_dir = Path(root_pkg["manifest_path"]).parent
    copy_tree(root_dir / "src", out / "src")
    carry_outside_src(root_dir, out, root_pkg["name"], out)
    bin_paths = {Path(t["src_path"]).resolve().relative_to(root_dir.resolve() / "src") for t in bins}
    for path in (out / "src").rglob("*.rs"):
        text = freeze_features(path.read_text(encoding="utf-8"), root.features)
        if path.relative_to(out / "src") in bin_paths:
            # A binary reaches the library by its name, and the carried crates
            # through it.
            text = rewrite(text, None, modules, root_lib_name)
        else:
            text = rewrite(text, None, modules)
        path.write_text(text, encoding="utf-8")

    declarations = []
    scripts: list[tuple[str, Path]] = []
    build_deps: list[tuple[Node, dict]] = []
    for name, node in sorted(libs.items()):
        dest = out / "src" / "vendor" / node.lib
        copy_tree(node.path / "src", dest / "src")
        carry_outside_src(node.path, dest, name, out)
        for path in dest.rglob("*.rs"):
            text = path.read_text(encoding="utf-8")
            text = freeze_features(text, node.features)
            text = rewrite(text, node.lib, modules)
            path.write_text(text, encoding="utf-8")
        lib_rs = dest / "src" / "lib.rs"
        text = CRATE_ONLY_ATTR.sub("", lib_rs.read_text(encoding="utf-8"))
        macros = sorted(set(exported_macros(dest / "src")))
        if macros:
            text += "\n// Exported at the package root; reachable here under the crate's old path.\n"
            text += "".join(f"#[doc(hidden)]\npub use crate::{m};\n" for m in macros)
        lib_rs.write_text(text, encoding="utf-8")
        declarations.append(node.lib)

        script = build_script(meta[name])
        if script is not None:
            internal_build = [
                d["name"] for d in meta[name]["dependencies"]
                if d["kind"] == "build" and d["name"] in meta
            ]
            if internal_build:
                if not test_only_out_dir(node.path / "src"):
                    raise SystemExit(
                        f"bundle-crate: {name}'s build script needs {', '.join(internal_build)}, "
                        f"which a build script of this package cannot carry"
                    )
                print(f"  leaving out {name}'s build script: it builds test fixtures only")
            else:
                scripts.append((node.lib, script))
                build_deps += [(node, d) for d in meta[name]["dependencies"] if d["kind"] == "build"]
    root_script = build_script(root_pkg)
    if root_script is not None:
        scripts.append((root_lib_name, root_script))
        build_deps += [(root, d) for d in root_pkg["dependencies"] if d["kind"] == "build"]

    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target")) / "bundle-crate"
    target_dir.mkdir(parents=True, exist_ok=True)
    expand_internal_derives(out, expandable, target_dir, modules)
    carried_libs = bundle_proc_crates(out, carried, meta, modules) if carried else []

    root_lib = out / "src" / Path(lib_target["src_path"]).resolve().relative_to(root_dir.resolve() / "src")
    text = root_lib.read_text(encoding="utf-8")
    # A crate that re-exported a dependency whole now has it as a module,
    # documented as part of its API. The rest stay reachable, for the paths
    # macros expand to and for naming a type a public field carries, but out
    # of the documentation.
    reexported = set()
    for module in modules:
        line = re.compile(rf"^\s*pub\s+use\s+crate::{module}\s*;\s*\n", re.M)
        if line.search(text):
            reexported.add(module)
            text = line.sub("", text)
    text += "\n// The Lumen crates this one is built on, carried in the package (tools/release/bundle-crate.py).\n"
    for lib in carried_libs:
        text += f"extern crate {lib};\n"
    for module in declarations:
        hidden = "" if module in reexported else "#[doc(hidden)]\n"
        text += f"{hidden}#[path = \"vendor/{module}/src/lib.rs\"]\npub mod {module};\n"
    # A crate-level lint the root sets (`#![forbid(unsafe_code)]`) would now
    # reach every carried crate below it, which each set their own.
    text = re.sub(r"^#!\[(?:forbid|deny)\([^\]]*\)\]\s*\n", "", text, flags=re.M)
    root_lib.write_text(text, encoding="utf-8")

    # The versions this tree was tested with, for `cargo install --locked` and
    # for the verify step of `cargo publish`; cargo drops what the package
    # does not use.
    shutil.copy(REPO / "Cargo.lock", out / "Cargo.lock")

    # One build script runs every carried crate's, each as a module.
    if scripts or carried_libs:
        build_dir = out / "build"
        build_dir.mkdir()
        body = ["// Generated by tools/release/bundle-crate.py.", ""]
        calls = []
        if carried_libs:
            (build_dir / "macros.rs").write_text(MACROS_BUILD.lstrip())
            body.append('#[path = "build/macros.rs"]\nmod macros;')
            calls.append("macros::main();")
        for lib, script in scripts:
            dest = build_dir / f"{lib}.rs"
            source = script.read_text(encoding="utf-8")
            source = re.sub(r"^fn main\(\)", "pub fn main()", source, flags=re.M)
            # Paths a build script names relative to its own file.
            for m in re.finditer(r'#\[path\s*=\s*"([^"]+)"\]', source):
                rel = m.group(1)
                src_file = (script.parent / rel).resolve()
                copy_to = build_dir / lib / rel
                copy_to.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(src_file, copy_to)
                source = source.replace(m.group(0), f'#[path = "{lib}/{rel}"]')
            dest.write_text(source, encoding="utf-8")
            body.append(f'#[path = "build/{lib}.rs"]\nmod {lib}_build;')
            calls.append(f"{lib}_build::main();")
        body.append("\nfn main() {\n    " + "\n    ".join(calls) + "\n}\n")
        (out / "build.rs").write_text("\n".join(body))

    plain, targeted = dependency_table(nodes, [root, *libs.values()], meta)
    build_lines = {}
    for node, dep in build_deps:
        if dep["name"] in meta:
            continue
        key = dep["rename"] or dep["name"]
        spec = [f"version = {toml_str(dep['req'])}"]
        if dep["rename"]:
            spec.append(f"package = {toml_str(dep['name'])}")
        if not dep["uses_default_features"]:
            spec.append("default-features = false")
        if dep["features"]:
            spec.append("features = [" + ", ".join(map(toml_str, dep["features"])) + "]")
        build_lines[key] = f"{key} = {{ {', '.join(spec)} }}"
    # The carried proc-macro crates' own dependencies, so cargo fetches them
    # before the build script builds those crates offline.
    for name in carried:
        for dep in meta[name]["dependencies"]:
            if dep["kind"] is None and dep["name"] not in build_lines:
                build_lines[dep["name"]] = f"{dep['name']} = {toml_str(dep['req'])}"

    version = root_pkg["version"]
    lines = [
        "# Generated by tools/release/bundle-crate.py: this crate together with the",
        "# source of the Lumen crates it uses, as one package.",
        "[package]",
        f"name = {toml_str(root_pkg['name'])}",
        f"version = {toml_str(version)}",
        f"edition = {toml_str(root_pkg['edition'])}",
        f"license = {toml_str(root_pkg['license'])}",
        f"authors = [{', '.join(map(toml_str, root_pkg['authors']))}]",
        f"repository = {toml_str(root_pkg['repository'])}",
        f"description = {toml_str(root_pkg['description'])}",
    ]
    # The compiler this tree is built and tested with, which is the oldest the
    # package is known to build with.
    pin = re.search(r'channel\s*=\s*"(\d+\.\d+)', (REPO / "rust-toolchain.toml").read_text())
    if pin:
        lines.append(f"rust-version = {toml_str(pin.group(1))}")
    for key in ("homepage", "documentation"):
        if root_pkg.get(key):
            lines.append(f"{key} = {toml_str(root_pkg[key])}")
    if root_pkg.get("keywords"):
        lines.append("keywords = [" + ", ".join(map(toml_str, root_pkg["keywords"])) + "]")
    if root_pkg.get("categories"):
        lines.append("categories = [" + ", ".join(map(toml_str, root_pkg["categories"])) + "]")
    if (out / "build.rs").exists():
        lines.append('build = "build.rs"')
    lines += ["", "[lib]", f"name = {toml_str(root_lib_name)}",
              f"path = {toml_str('src/' + root_lib.relative_to(out / 'src').as_posix())}", ""]
    for t in bins:
        rel = Path(t["src_path"]).resolve().relative_to(root_dir.resolve())
        lines += ["[[bin]]", f"name = {toml_str(t['name'])}", f"path = {toml_str(rel.as_posix())}", ""]
    lines += ["[dependencies]", *plain.values(), ""]
    for target, entries in targeted.items():
        lines += [f"[target.{toml_str(target)}.dependencies]", *entries.values(), ""]
    if build_lines:
        lines += ["[build-dependencies]", *build_lines.values(), ""]
    (out / "Cargo.toml").write_text("\n".join(lines))
    print(f"bundled {root_pkg['name']} {version} with {len(libs)} Lumen crates into {out}")
    for name in sorted(libs):
        print(f"  {name}")
    if expandable:
        print("expanded the derives of " + ", ".join(sorted(expandable)))
    if carried:
        print("carried for the build script to compile: " + ", ".join(sorted(carried)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
