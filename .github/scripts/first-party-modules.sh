#!/usr/bin/env bash
# The first-party runtime modules, one per line, tab separated:
#
#   <declared name>	<cargo package>	<cargo library name>
#
# The declared name is what an app writes in `[dependencies]` and what
# `lumen_module!` was given, read out of the module's own source rather than
# guessed from the package name: the two agree today, and the loader spells
# its symbols from the declared one, so the source is where the answer is.
#
# Every step that has to enumerate the modules reads this instead of listing
# them: the release workflow packaging `lumen-modules-<target>.tar.gz` and the
# one naming the modules a link kit carries. Adding a crate under std/ is then
# the whole of adding a first-party module.
#
# Needs jq, which every GitHub runner image has. On the Windows runner the
# shell is git bash and the tools in the pipeline below are native Windows
# binaries, whose C runtime opens stdout in text mode and turns every newline
# they write into CRLF. The shell splits on the newline alone, so the carriage
# return stays on the last field of the line and reaches the caller as part of
# the library name, which then matches no rlib. Carriage returns come off at
# the end of the pipeline, so it does not matter which tool in it added them.
set -euo pipefail

# cargo is told where to look by the working directory rather than by
# `--manifest-path`: on Windows this script runs under a shell whose paths a
# Windows cargo cannot follow, and a directory change it can.
cd "$(dirname "$0")/../.."

cargo metadata --format-version 1 --no-deps |
  jq -r '
    # A Windows cargo reports backslashes, which neither the match below nor
    # the shell reads as separators, so the path is normalized first.
    .packages[]
    | .manifest_path |= gsub("\\\\"; "/")
    | select(.manifest_path | test("/std/[^/]+/Cargo.toml$"))
    | [.name, .manifest_path, ([.targets[] | select(.kind | index("lib")) | .name] | first)]
    | @tsv
  ' |
  sort |
  tr -d '\r' |
  while IFS=$'\t' read -r package manifest lib; do
    src="$(dirname "$manifest")/src"
    name="$(grep -rhoE 'lumen_module!\("[^"]+"' "$src" | head -1 | sed 's/.*"\(.*\)"/\1/')"
    if [ -z "$name" ]; then
      echo "first-party-modules.sh: no lumen_module! declaration under $src" >&2
      exit 1
    fi
    printf '%s\t%s\t%s\n' "$name" "$package" "$lib"
  done
