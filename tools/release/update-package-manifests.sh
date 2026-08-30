#!/bin/sh
# Point the OS package manifests at a release.
#
# Usage:
#   tools/release/update-package-manifests.sh <version> [sha256sums.txt]
#
# The version is the release without its leading "v" (0.1.0, not v0.1.0). The
# checksum file is the sha256sums.txt asset of that release; when it is not
# given it is downloaded from the release itself. Releases published before
# sha256sums.txt existed cannot be used here, because there is nothing to write
# into the manifests.
#
# Rewritten in place:
#
#   aur/PKGBUILD                              pkgver, pkgrel, both archive sums
#   homebrew/lumen.rb                         version, four URLs, four sums
#   scoop/lumen.json                          version, the zip URL and its sum
#   winget/LumenFX.Lumen*.yaml                versions, the MSI URL and its sum
#
# .github/workflows/publish-packages.yml runs this once per release and then
# pushes each file to the repository that serves it. Run it by hand to see what
# a release would publish, or to repair a manifest that drifted.
#
# Every rewrite is checked afterwards: the script fails if a file does not end
# up carrying the version and the checksum it was told to, so a manifest that
# silently missed an edit never reaches a package manager.

set -eu

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "usage: update-package-manifests.sh <version> [sha256sums.txt]" >&2
  exit 2
fi
VERSION="${VERSION#v}"
case "$VERSION" in
  *[!0-9.]* | '' | *..*)
    echo "update-package-manifests.sh: '$VERSION' is not a release version" >&2
    exit 2
    ;;
esac

DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
RELEASES="https://github.com/lumen-fx/lumen/releases/download"

# --- the checksums ------------------------------------------------------------

SUMS="${2:-}"
CLEANUP=''
if [ -z "$SUMS" ]; then
  command -v curl >/dev/null 2>&1 || {
    echo "update-package-manifests.sh: no checksum file given and no curl to fetch one" >&2
    exit 1
  }
  SUMS="$(mktemp)"
  CLEANUP="$SUMS"
  trap 'rm -f "$CLEANUP"' EXIT
  if ! curl -fsSL -o "$SUMS" "$RELEASES/v$VERSION/sha256sums.txt"; then
    echo "update-package-manifests.sh: v$VERSION publishes no sha256sums.txt" >&2
    exit 1
  fi
fi
[ -f "$SUMS" ] || { echo "update-package-manifests.sh: no such file: $SUMS" >&2; exit 1; }

# The sha256sum format is "<hash>  <name>", with a "*" before the name for a
# file read in binary mode.
hash_for() {
  awk -v want="$1" '
    { name = $2; sub(/^\*/, "", name); if (name == want) { print $1; exit } }
  ' "$SUMS"
}

require_hash() {
  h="$(hash_for "$1")"
  if [ -z "$h" ]; then
    echo "update-package-manifests.sh: $SUMS has no line for $1" >&2
    exit 1
  fi
  printf '%s\n' "$h"
}

LINUX_X86="$(require_hash lumen-linux-x86_64.tar.gz)"
LINUX_ARM="$(require_hash lumen-linux-aarch64.tar.gz)"
MACOS_X86="$(require_hash lumen-macos-x86_64.tar.gz)"
MACOS_ARM="$(require_hash lumen-macos-aarch64.tar.gz)"
WINDOWS_ZIP="$(require_hash lumen-windows-x86_64.zip)"
WINDOWS_MSI="$(require_hash lumen-windows-x86_64.msi)"
WINDOWS_ARM_ZIP="$(require_hash lumen-windows-aarch64.zip)"
WINDOWS_ARM_MSI="$(require_hash lumen-windows-aarch64.msi)"

# --- helpers ------------------------------------------------------------------

# The version already in the manifests, so the URLs carrying it can be moved on.
OLD="$(sed -n 's/^pkgver=//p' "$DIR/aur/PKGBUILD")"
[ -n "$OLD" ] || { echo "update-package-manifests.sh: no pkgver in aur/PKGBUILD" >&2; exit 1; }
OLD_RE="$(printf '%s' "$OLD" | sed 's/\./\\./g')"

edit() {
  # edit FILE SED-EXPRESSION...
  file="$1"
  shift
  sed "$@" "$file" > "$file.new"
  mv "$file.new" "$file"
}

# Rewrite each checksum in a file that lists a download URL and its checksum on
# separate lines: the asset name on the URL line says which checksum belongs to
# the line that follows. Order does not matter, so the manifests can be
# rearranged without breaking this.
#
# KIND picks the shape of the checksum line: rb for a Homebrew formula, json
# for a Scoop manifest, yaml for a winget manifest (whose checksums are written
# in upper case, as winget's own tooling writes them).
rewrite_sums() {
  file="$1"
  kind="$2"
  awk -v sums="$SUMS" -v kind="$kind" '
    BEGIN {
      while ((getline line < sums) > 0) {
        n = split(line, f, " ")
        if (n < 2) continue
        name = f[2]
        sub(/^\*/, "", name)
        hash[name] = f[1]
      }
      close(sums)
    }
    {
      for (name in hash) {
        if (index($0, name) > 0) last = name
      }
      if (last != "") {
        h = hash[last]
        if (kind == "rb" && $0 ~ /sha256 "/) {
          sub(/sha256 "[^"]*"/, "sha256 \"" h "\"")
          last = ""
        } else if (kind == "json" && $0 ~ /"hash":/) {
          sub(/"hash": "[^"]*"/, "\"hash\": \"" h "\"")
          last = ""
        } else if (kind == "yaml" && $0 ~ /InstallerSha256:/) {
          sub(/InstallerSha256:.*/, "InstallerSha256: " toupper(h))
          last = ""
        }
      }
      print
    }
  ' "$file" > "$file.new"
  mv "$file.new" "$file"
}

# --- aur ----------------------------------------------------------------------

edit "$DIR/aur/PKGBUILD" \
  -e "s|^pkgver=.*|pkgver=$VERSION|" \
  -e "s|^pkgrel=.*|pkgrel=1|" \
  -e "s|^sha256sums_x86_64=.*|sha256sums_x86_64=('$LINUX_X86')|" \
  -e "s|^sha256sums_aarch64=.*|sha256sums_aarch64=('$LINUX_ARM')|"

# The AUR reads .SRCINFO, not the PKGBUILD, so the two have to agree. Only
# makepkg can write it; a machine without makepkg leaves the committed copy
# alone and the AUR job regenerates it before pushing.
if command -v makepkg >/dev/null 2>&1; then
  (cd "$DIR/aur" && makepkg --printsrcinfo > .SRCINFO.new && mv .SRCINFO.new .SRCINFO)
  echo "regenerated aur/.SRCINFO"
else
  echo "makepkg not found: aur/.SRCINFO left as it was" >&2
fi

# --- homebrew -----------------------------------------------------------------

edit "$DIR/homebrew/lumen.rb" \
  -e "s|^\( *\)version \".*\"|\1version \"$VERSION\"|" \
  -e "s|/download/v$OLD_RE/|/download/v$VERSION/|g"
rewrite_sums "$DIR/homebrew/lumen.rb" rb

# --- scoop --------------------------------------------------------------------

edit "$DIR/scoop/lumen.json" \
  -e "s|^\( *\)\"version\": \".*\"|\1\"version\": \"$VERSION\"|" \
  -e "s|/download/v$OLD_RE/|/download/v$VERSION/|g"
rewrite_sums "$DIR/scoop/lumen.json" json

# --- winget -------------------------------------------------------------------

for file in "$DIR"/winget/LumenFX.Lumen*.yaml; do
  edit "$file" \
    -e "s|^PackageVersion: .*|PackageVersion: $VERSION|" \
    -e "s|^\( *\)DisplayVersion: .*|\1DisplayVersion: $VERSION|" \
    -e "s|/download/v$OLD_RE/|/download/v$VERSION/|g" \
    -e "s|/releases/tag/v$OLD_RE|/releases/tag/v$VERSION|g"
  rewrite_sums "$file" yaml
done

# --- check the rewrites landed ------------------------------------------------

carries() {
  # carries FILE PATTERN DESCRIPTION
  if ! grep -qi -- "$2" "$1"; then
    echo "update-package-manifests.sh: $1 is missing $3" >&2
    exit 1
  fi
}

carries "$DIR/aur/PKGBUILD" "^pkgver=$VERSION\$" "version $VERSION"
carries "$DIR/aur/PKGBUILD" "$LINUX_X86" "the linux-x86_64 checksum"
carries "$DIR/aur/PKGBUILD" "$LINUX_ARM" "the linux-aarch64 checksum"

carries "$DIR/homebrew/lumen.rb" "version \"$VERSION\"" "version $VERSION"
for h in "$MACOS_ARM" "$MACOS_X86" "$LINUX_ARM" "$LINUX_X86"; do
  carries "$DIR/homebrew/lumen.rb" "$h" "a checksum"
done

carries "$DIR/scoop/lumen.json" "\"version\": \"$VERSION\"" "version $VERSION"
carries "$DIR/scoop/lumen.json" "$WINDOWS_ZIP" "the windows zip checksum"
carries "$DIR/scoop/lumen.json" "$WINDOWS_ARM_ZIP" "the windows arm64 zip checksum"

for file in "$DIR"/winget/LumenFX.Lumen*.yaml; do
  carries "$file" "^PackageVersion: $VERSION\$" "version $VERSION"
done
carries "$DIR/winget/LumenFX.Lumen.installer.yaml" "$WINDOWS_MSI" "the msi checksum"
carries "$DIR/winget/LumenFX.Lumen.installer.yaml" "$WINDOWS_ARM_MSI" \
        "the arm64 msi checksum"

# Nothing may still point at the release that was there before.
for file in "$DIR/aur/PKGBUILD" "$DIR/homebrew/lumen.rb" "$DIR/scoop/lumen.json" \
            "$DIR"/winget/LumenFX.Lumen*.yaml; do
  if [ "$OLD" != "$VERSION" ] && grep -q -- "/v$OLD_RE/" "$file"; then
    echo "update-package-manifests.sh: $file still links to v$OLD" >&2
    exit 1
  fi
done

echo "package manifests now point at v$VERSION"
