#!/bin/sh
# Downloads the `lumenc new` templates.
#
# Each template is a repository of its own under lumen-fx, and the tree on its
# `main` branch is the template. Lumen keeps no copy: a release downloads them
# with this script and packages them beside the toolchain, and a checkout
# downloads them with this script so `lumenc new`, and the tests that scaffold,
# find them the same way an installed lumenc does
# (public/lumenc/src/cli/scaffold.rs).
#
#   tools/fetch-templates.sh [directory]
#   tools/fetch-templates.sh --resolve
#
# The default directory is `templates` at the root of cargo's target
# directory, which is where lumenc looks in a checkout. Every run replaces
# what it finds.
#
# A template is downloaded by commit, never by branch name, so that every
# build leg of one release packages the same trees even if a template's
# `main` moves while the release is building. LUMEN_TEMPLATE_REVS names the
# commit for each template, as `name=<sha>` words separated by spaces;
# `--resolve` prints that list for the current `main` of every template and
# downloads nothing, which is how build-toolchain.yml settles the commits
# once per run and hands the same list to every leg. Unset, each template's
# `main` is resolved as it is downloaded, which is what a checkout and the
# test workflows want.
#
# The archive GitHub serves for a commit is the tree as committed. The
# repository's own CI configuration in .github/ is no part of an app, so it is
# left out, and lumen.toml has to sit at the root of what is left.
#
# LUMEN_TEMPLATE_OWNER points the download at another GitHub owner, for
# testing a template change from a fork.

set -eu

OWNER="${LUMEN_TEMPLATE_OWNER:-lumen-fx}"
REVS="${LUMEN_TEMPLATE_REVS:-}"

RESOLVE=""
if [ "${1:-}" = "--resolve" ]; then
  RESOLVE=1
  shift
fi
DEST="${1:-${CARGO_TARGET_DIR:-target}/templates}"

# The gallery, which is also scaffold::TEMPLATES in gallery order. The two
# lists are compared by public/lumenc/tests/templates.rs, so a template added
# to one and not the other turns the suite red rather than going unnoticed.
set -- blank hello counter form todo dashboard settings hotkeys

# The commit `main` points at in the template's repository. git asks the
# server directly, with no API call and so no rate limit to run into.
# A missing repository fails rather than waiting at a password prompt.
resolve() {
  sha="$(GIT_TERMINAL_PROMPT=0 git ls-remote "https://github.com/$OWNER/$1.git" refs/heads/main | cut -f1)"
  if ! printf '%s' "$sha" | grep -Eqx '[0-9a-f]{40}'; then
    echo "fetch-templates.sh: cannot read the main branch of $OWNER/$1" >&2
    exit 1
  fi
  printf '%s\n' "$sha"
}

# The commit LUMEN_TEMPLATE_REVS names for a template, or nothing.
pinned() {
  printf '%s\n' "$REVS" | tr -s '[:blank:]' '\n' | sed -n "s/^$1=//p" | head -n 1
}

if [ -n "$RESOLVE" ]; then
  line=""
  for name in "$@"; do
    line="$line${line:+ }$name=$(resolve "$name")"
  done
  printf '%s\n' "$line"
  exit 0
fi

# A list that leaves a template out was settled against another gallery, and
# falling back to `main` for the missing one would mix trees from two
# different moments into one toolchain. Checked before anything is replaced.
if [ -n "$REVS" ]; then
  for name in "$@"; do
    if ! pinned "$name" | grep -Eqx '[0-9a-f]{40}'; then
      echo "fetch-templates.sh: LUMEN_TEMPLATE_REVS names no commit for $name" >&2
      exit 1
    fi
  done
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

mkdir -p "$DEST"
for name in "$@"; do
  if [ -n "$REVS" ]; then
    sha="$(pinned "$name")"
  else
    sha="$(resolve "$name")"
  fi
  url="https://github.com/$OWNER/$name/archive/$sha.tar.gz"
  printf 'fetching %s at %s\n' "$OWNER/$name" "$sha"
  # A dropped connection on any one of these fails the whole job, and a CI run
  # downloads all eight on each of three operating systems, so a transient
  # reset is worth a few more attempts. A commit that does not exist still
  # fails, a couple of seconds later.
  if ! curl -fsSL --retry 3 --retry-all-errors --retry-delay 2 \
       "$url" -o "$TMP/$name.tar.gz"; then
    echo "fetch-templates.sh: cannot download $url" >&2
    exit 1
  fi
  # The archive holds one directory, <name>-<sha>, with the tree inside it.
  mkdir -p "$TMP/$name"
  tar -xzf "$TMP/$name.tar.gz" -C "$TMP/$name" --strip-components=1
  rm -rf "$TMP/$name/.github"
  # An app tree, with lumen.toml at the root. Anything else is a template
  # repository whose main branch holds something other than an app, and
  # unpacking it over a good copy would leave a directory `lumenc new` cannot
  # scaffold.
  if [ ! -f "$TMP/$name/lumen.toml" ]; then
    echo "fetch-templates.sh: $OWNER/$name at $sha carries no lumen.toml at its root" >&2
    exit 1
  fi
  rm -rf "${DEST:?}/$name"
  mv "$TMP/$name" "$DEST/$name"
done

printf 'templates are in %s\n' "$DEST"
