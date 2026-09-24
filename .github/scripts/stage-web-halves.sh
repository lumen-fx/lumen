#!/usr/bin/env bash
# Copy the web half of every first-party module into a toolchain's bin/
# directory, as modules/<name>/web, where `lumenc web` looks for the web half
# of a module an app declares with `bundled = true`.
#
# A web half is data: a descriptor and the JavaScript a page loads, the same
# on every platform. So it ships in the toolchain archive, which every
# platform has and every `lumenc` sits in, and not in the per-platform
# modules archive, which Windows does not have and an install may skip.
#
# The modules come out of the tree: every std/<dir>/web holding a
# lumen-addon.toml, named by the package in std/<dir>/Cargo.toml, which is the
# name an app declares. A crate added under std/ with a web half ships without
# this script being touched.
#
#   $1  the bin/ directory being staged, relative to the repository root or
#       absolute
set -euo pipefail

dest="${1:?usage: stage-web-halves.sh BIN_DIR}"
cd "$(dirname "$0")/../.."

staged=0
for web in std/*/web; do
  [ -f "$web/lumen-addon.toml" ] || continue
  crate="$(dirname "$web")"
  name="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$crate/Cargo.toml" | head -1)"
  if [ -z "$name" ]; then
    echo "stage-web-halves.sh: no package name in $crate/Cargo.toml" >&2
    exit 1
  fi
  mkdir -p "$dest/modules/$name"
  cp -R "$web" "$dest/modules/$name/web"
  staged=$((staged + 1))
done
if [ "$staged" -eq 0 ]; then
  echo "stage-web-halves.sh: no module under std/ has a web half" >&2
  exit 1
fi
echo "web halves staged: $staged"
