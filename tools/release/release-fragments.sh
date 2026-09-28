#!/usr/bin/env bash
# Move the migration notes a release shipped with into that release's own
# directory.
#
#   tools/release/release-fragments.sh <tag>
#
# A pull request that breaks something adds a note under
# docs/migration/unreleased/. In the tree of tag vX.Y.Z, that directory is the
# migration guide for vX.Y.Z. Once the release is out, this moves exactly the
# notes the tag's tree holds into docs/migration/vX.Y.Z/ with `git mv`, so the
# move is staged and rides in whatever commit follows. A note merged to main
# after the tag was cut is not in the tag's tree, so it stays in unreleased/
# and belongs to the next release.
#
# Only a plain vX.Y.Z tag files notes. A prerelease or any other tag shape
# never becomes the release a user upgrades to, so its notes stay in
# unreleased/ for the plain release that follows, and this does nothing.
#
# Run it from the root of a checkout of main that has the tag fetched. It is a
# function of the tag and of main alone, which is what lets release.yml commit
# its result straight to main. Running it twice is safe: a note already moved is
# no longer in unreleased/ and is skipped. It prints one line per note moved and
# nothing when there is nothing to move.
set -euo pipefail

tag=${1:?usage: release-fragments.sh <tag>}
src=docs/migration/unreleased
dst=docs/migration/$tag

if ! printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "release-fragments.sh: $tag is not a plain vX.Y.Z tag; its notes stay in $src" >&2
  exit 0
fi

if ! git rev-parse --quiet --verify "refs/tags/$tag^{commit}" >/dev/null; then
  echo "release-fragments.sh: tag $tag is not in this checkout; fetch it first" >&2
  exit 1
fi

# Only blobs, so a directory someone nested under unreleased/ is not mistaken
# for a note. The keep-file holds the directory open and never moves.
git ls-tree "refs/tags/$tag" -- "$src/" |
  awk -F'\t' '$1 ~ / blob / { print $2 }' |
  while IFS= read -r path; do
    name=${path##*/}
    [ "$name" = .gitkeep ] && continue
    # Already moved by an earlier run of this release, or deleted on main
    # since the tag. Either way there is nothing here to attribute.
    [ -f "$path" ] || continue
    mkdir -p "$dst"
    git mv -- "$path" "$dst/$name"
    echo "$path -> $dst/$name"
  done
