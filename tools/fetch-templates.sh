#!/bin/sh
# Downloads the `lumenc new` templates.
#
# Each template is a repository of its own under lumen-fx, and each publishes
# its tree as template.tar.gz on its latest release. Lumen keeps no copy: a
# release downloads them with this script and packages them beside the
# toolchain, and a checkout downloads them with this script so `lumenc new`,
# and the tests that scaffold, find them the same way an installed lumenc does
# (public/lumenc/src/scaffold.rs).
#
#   tools/fetch-templates.sh [directory]
#
# The default directory is `templates` at the root of cargo's target
# directory, which is where lumenc looks in a checkout. Every run replaces
# what it finds, so the tree always matches what the template repositories
# publish now.
#
# LUMEN_TEMPLATE_OWNER points the download at another GitHub owner, for
# testing a template change from a fork.

set -eu

OWNER="${LUMEN_TEMPLATE_OWNER:-lumen-fx}"
DEST="${1:-${CARGO_TARGET_DIR:-target}/templates}"

# The gallery, which is also scaffold::TEMPLATES in gallery order. The two
# lists are compared by public/lumenc/tests/templates.rs, so a template added
# to one and not the other turns the suite red rather than going unnoticed.
set -- blank hello counter form todo dashboard settings hotkeys

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

mkdir -p "$DEST"
for name in "$@"; do
  url="https://github.com/$OWNER/$name/releases/latest/download/template.tar.gz"
  printf 'fetching %s\n' "$url"
  if ! curl -fsSL "$url" -o "$TMP/$name.tar.gz"; then
    echo "fetch-templates.sh: cannot download $url" >&2
    exit 1
  fi
  mkdir -p "$TMP/$name"
  tar -xzf "$TMP/$name.tar.gz" -C "$TMP/$name"
  # An app tree, with lumen.toml at the root of the archive. Anything else is
  # a template repository that published the wrong thing, and unpacking it
  # over a good copy would leave a directory `lumenc new` cannot scaffold.
  if [ ! -f "$TMP/$name/lumen.toml" ]; then
    echo "fetch-templates.sh: $url carries no lumen.toml at its root" >&2
    exit 1
  fi
  rm -rf "${DEST:?}/$name"
  mv "$TMP/$name" "$DEST/$name"
done

printf 'templates are in %s\n' "$DEST"
