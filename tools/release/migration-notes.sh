#!/usr/bin/env bash
# Write the migration part of a release's notes.
#
#   tools/release/migration-notes.sh <tag> <out-file>
#
# Reads the migration notes in the tree of <tag> (docs/migration/unreleased/,
# which in that tree is the guide for <tag>) and writes a short section for
# the top of the GitHub release body: which release it migrates from, a link
# to the full guide on the docs site, and the heading of each note. A release
# with no notes gets an empty file, so the release body is GitHub's generated
# notes alone.
#
# Only a plain vX.Y.Z tag gets the section. The notes in a prerelease's tree
# belong to the plain release that follows it, which is where the guide on the
# docs site lives, so a prerelease gets an empty file.
#
# The previous release is the highest plain vX.Y.Z tag below <tag> among the
# tags in this checkout, so fetch the release tags first. With none, the
# section is titled for <tag> alone.
set -euo pipefail

tag=${1:?usage: migration-notes.sh <tag> <out-file>}
out=${2:?usage: migration-notes.sh <tag> <out-file>}
src=docs/migration/unreleased
guide="https://docs.lumenfx.dev/migration/$tag/"

if ! printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  : > "$out"
  exit 0
fi

if ! git rev-parse --quiet --verify "refs/tags/$tag^{commit}" >/dev/null; then
  echo "migration-notes.sh: tag $tag is not in this checkout; fetch it first" >&2
  exit 1
fi

headings=()
while IFS= read -r path; do
  [ "${path##*/}" = .gitkeep ] && continue
  first=$(git show "refs/tags/$tag:$path" | head -n 1)
  case $first in
    "# "*) headings+=("${first#\# }") ;;
    *)
      echo "migration-notes.sh: $path in $tag does not start with a '# ' heading" >&2
      exit 1
      ;;
  esac
done < <(git ls-tree "refs/tags/$tag" -- "$src/" | awk -F'\t' '$1 ~ / blob / { print $2 }')

if [ ${#headings[@]} -eq 0 ]; then
  : > "$out"
  exit 0
fi

# Sort the candidates by version and keep the one just below this tag.
# Prerelease and other tag shapes are not releases anyone migrates from.
previous=$(
  { git tag --list 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true; echo "$tag"; } |
    sort -uV | awk -v t="$tag" '$0 == t { print prev; exit } { prev = $0 }'
)

{
  if [ -n "$previous" ]; then
    echo "## Migrating from $previous"
  else
    echo "## Migrating to $tag"
  fi
  echo
  echo "This release changes things an existing app may have to act on. The"
  echo "[migration guide]($guide) says what changed and what to do for each:"
  echo
  for heading in "${headings[@]}"; do
    echo "- $heading"
  done
} > "$out"
