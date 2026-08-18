#!/usr/bin/env bash
# Build the browser runtime: the WebAssembly module a page loads and the
# module script that instantiates it.
#
# Two workflows want this pair. ci.yml measures it and runs the browser suite
# against it; release.yml packs it into lumen-web.tar.gz for `lumenc web` to
# find. They want the same two files built the same way, so the recipe lives
# here and both call it.
#
# The pair comes out under the names a site refers to them by, because that is
# what a toolchain ships and what `lumenc web` looks for. The page tells the
# module where its wasm is, so renaming it past the loader's default costs
# nothing.
#
#   $1  directory to write lumen-web.wasm and lumen-web.js into
#
# Needs cargo and wasm-bindgen on PATH; the wasm-bindgen version has to match
# the workspace's, or the tool refuses the module cargo just built. wasm-opt is
# fetched from the binaryen release named below when it is not already on PATH,
# since binaryen is not published as a crate.

set -euo pipefail

out=${1:?usage: build-web-runtime.sh OUT_DIR}
binaryen_version=version_123

command -v cargo >/dev/null 2>&1 ||
  { echo "build-web-runtime.sh: cargo is not on PATH" >&2; exit 1; }
command -v wasm-bindgen >/dev/null 2>&1 ||
  { echo "build-web-runtime.sh: wasm-bindgen is not on PATH" >&2; exit 1; }

if command -v wasm-opt >/dev/null 2>&1; then
  wasm_opt=wasm-opt
else
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) slug=x86_64-linux ;;
    Linux-aarch64 | Linux-arm64) slug=aarch64-linux ;;
    Darwin-arm64) slug=arm64-macos ;;
    Darwin-x86_64) slug=x86_64-macos ;;
    *)
      echo "build-web-runtime.sh: binaryen publishes no build for $(uname -s) $(uname -m); put wasm-opt on PATH and run this again" >&2
      exit 1
      ;;
  esac
  tools=$(mktemp -d)
  trap 'rm -rf "$tools"' EXIT
  url="https://github.com/WebAssembly/binaryen/releases/download/$binaryen_version/binaryen-$binaryen_version-$slug.tar.gz"
  echo "fetching wasm-opt from $url"
  curl -fsSL "$url" | tar xz -C "$tools"
  wasm_opt="$tools/binaryen-$binaryen_version/bin/wasm-opt"
fi

cargo build -p lumen-web-runtime --target wasm32-unknown-unknown --profile wasm-release

mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-name lumen-web \
  --out-dir "$out" \
  "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/wasm-release/lumen_web_runtime.wasm"
mv "$out/lumen-web_bg.wasm" "$out/lumen-web.wasm"

# wasm-opt validates against the features it is told about, and the ones rustc
# emits for this target are not all on by default.
"$wasm_opt" -Oz \
  --enable-bulk-memory \
  --enable-bulk-memory-opt \
  --enable-nontrapping-float-to-int \
  --enable-sign-ext \
  --enable-mutable-globals \
  --enable-reference-types \
  --enable-multivalue \
  "$out/lumen-web.wasm" \
  -o "$out/lumen-web.wasm"

echo "wrote $out/lumen-web.wasm and $out/lumen-web.js"
