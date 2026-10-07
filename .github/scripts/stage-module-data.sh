#!/usr/bin/env bash
# Copy the platform-independent files of every first-party module into a
# toolchain's bin/ directory, under modules/<name>/:
#
#   - the web half, as modules/<name>/web, where `lumenc web` looks for the
#     web half of a module an app declares with `bundled = true`;
#   - the language descriptor, as modules/<name>/lumen-language.toml, which is
#     how `lumenc` and a run from source learn which module runs a script
#     language (see core/modules/src/language.rs).
#
# Both are data, the same on every platform. So they ship in the toolchain
# archive, which every platform has and every `lumenc` sits in, and not in the
# per-platform modules archive, which Windows does not have and an install may
# skip.
#
# The modules come out of the tree: every std/<dir> holding a web half
# (std/<dir>/web/lumen-addon.toml) or a descriptor (std/<dir>/lumen-language.toml),
# named by the package in std/<dir>/Cargo.toml, which is the name an app
# declares and the loader opens. A crate added under std/ ships its files
# without this script being touched.
#
#   $1  the bin/ directory being staged, relative to the repository root or
#       absolute
set -euo pipefail

dest="${1:?usage: stage-module-data.sh BIN_DIR}"
cd "$(dirname "$0")/../.."

package_name() {
  local name
  name="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$1/Cargo.toml" | head -1)"
  if [ -z "$name" ]; then
    echo "stage-module-data.sh: no package name in $1/Cargo.toml" >&2
    exit 1
  fi
  printf '%s' "$name"
}

halves=0
for web in std/*/web; do
  [ -f "$web/lumen-addon.toml" ] || continue
  crate="$(dirname "$web")"
  name="$(package_name "$crate")"
  mkdir -p "$dest/modules/$name"
  cp -R "$web" "$dest/modules/$name/web"
  halves=$((halves + 1))
done
if [ "$halves" -eq 0 ]; then
  echo "stage-module-data.sh: no module under std/ has a web half" >&2
  exit 1
fi

languages=0
for descriptor in std/*/lumen-language.toml; do
  [ -f "$descriptor" ] || continue
  crate="$(dirname "$descriptor")"
  name="$(package_name "$crate")"
  mkdir -p "$dest/modules/$name"
  cp "$descriptor" "$dest/modules/$name/lumen-language.toml"
  languages=$((languages + 1))
done
if [ "$languages" -eq 0 ]; then
  echo "stage-module-data.sh: no module under std/ runs a script language" >&2
  exit 1
fi
echo "web halves staged: $halves; language descriptors staged: $languages"
