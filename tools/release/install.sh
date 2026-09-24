#!/bin/sh
# Lumen toolchain installer.
#
#   curl -fsSL https://lumenfx.dev/install.sh | sh
#
# Resolves a release of lumen-fx/lumen (latest by default, or the tag given
# by --version), downloads the archive asset matching this platform, verifies
# it against the checksums published with the release, and unpacks it under
# ~/.lumen. Nothing is written outside the prefix except an optional PATH line
# in a shell rc file, which is only added with consent.
#
# There is no separate manifest, no separate download host, and no API call:
# the release itself, at https://github.com/lumen-fx/lumen/releases, is the
# source of both the archives and their checksums, and every request this
# script makes is a plain file download from
#
#   https://github.com/lumen-fx/lumen/releases/download/<tag>/<asset>
#
# The latest tag comes from the redirect that
# https://github.com/lumen-fx/lumen/releases/latest sends: its final URL ends
# in the tag, which is the same resolution lumenc's own update check uses
# (public/lumenc/src/package/update_check.rs). A pinned --version needs no lookup at
# all; it becomes a tag directly, tried as given and then with a "v" prefix.
#
# Checksums live in one asset per release, sha256sums.txt, in sha256sum's own
# format: one "<hex>  <filename>" line per published asset. This script
# downloads it first, reads the line for the asset it wants, and refuses to
# install anything whose download does not match. A release that has no
# sha256sums.txt cannot be installed by this script.
#
# This installs the Lumen toolchain: lumenc, liblumen, the app launcher stub,
# and lumen-server, the production server for server-rendered sites. There is
# nothing else to choose - no component flag, no candela
# option. Candela is a scripting engine linked into liblumen (the
# lumen-script-candela crate, compiled in - see the `host-candela` feature on
# lumen / lumen-runtime), not an external binary this installer runs or
# manages; a Lumen app never shells out to a candela executable. Someone who
# wants the standalone candela language outside a Lumen app installs it from
# candela's own release channel (lumen-fx/candela), independent of this script.
#
# The asset naming below is the contract between the release process and
# this script:
#
#   lumen-<target>.tar.gz     target in {linux-x86_64, linux-aarch64,
#                              macos-x86_64, macos-aarch64}
#   lumen-modules-<target>.tar.gz
#                             the bundled runtime modules for the same
#                             targets, installed into the same bin/ beside
#                             the engine unless --no-modules is given.
#                             Optional per release; absent for Windows,
#                             where only `lumenc package --static` carries
#                             the capabilities, compiled into the executable.
#   lumen-linkkit-<target>.tar.gz
#                             the link kit for the same target, published on
#                             every platform. This script never fetches it:
#                             it is what lumenc downloads on its own for
#                             `lumenc package --static`, the way it downloads
#                             the browser runtime.
#   lumen-windows-<arch>.msi  the Windows installer. This script never
#                             fetches or runs it; the windows branch below
#                             prints its URL and stops.
#   lumen-web.tar.gz          the browser runtime, which belongs to no
#                             platform. lumenc downloads it on its own the
#                             first time a `lumenc web` build needs it, so
#                             this script leaves it alone.
#   sha256sums.txt            checksums covering every asset above.
#
# One thing this script installs comes from elsewhere: lpm, the client of the
# package registry, published from lumen-fx/registry as
# lpm_<version>_<os>_<arch>.tar.gz with checksums.txt beside it. It goes to
# ~/.local/bin/lpm rather than under the prefix, because one copy serves every
# toolchain on the machine and lumenc looks for it there. It is not in the
# receipt and --uninstall leaves it; --no-lpm skips it, and lumenc installs it
# itself the first time an app names a registry package.
#
# tools/release/release-checklist.md documents producing the asset under this
# scheme. The archive holds the tree to install: bin/ for lumenc, with the
# liblumen shared library and the lumen-launcher app stub right next to it in
# the same bin/ directory (see public/lumenc/src/link/loader.rs and
# public/lumenc/src/package/package.rs: both look next to the running executable,
# then an LUMEN_LIB_DIR override, then the platform loader's default search
# path, and never in a sibling lib/ directory), plus the three trees lumenc
# reads from beside itself - the candela standard library in bin/libs, the
# `lumenc new` templates in bin/templates, and the first-party browser add-ons
# in bin/addons - and a shell completion script
# per shell under share/. Every installed path is recorded in a
# receipt under <prefix>/share/lumen, so a later run can replace an old version
# exactly and --uninstall can undo it.
#
# The receipt also records whether the install was pinned. With --version the
# receipt gets a "pinned <version>" line, and lumenc reads that line to stay
# quiet about newer releases: a pinned install is a deliberate choice, not
# something to nag about. Installing without --version rewrites the receipt
# without the line, which is how a pin is lifted.

set -eu

GH_REPO="${LUMEN_GH_REPO:-lumen-fx/lumen}"
GH_URL="https://github.com/$GH_REPO"
PREFIX="${LUMEN_PREFIX:-$HOME/.lumen}"

# lpm is published from the registry's own repository and installed to one
# shared path rather than under the prefix, because one copy serves every
# toolchain on the machine and lumenc looks for it there.
LPM_REPO="${LPM_GH_REPO:-lumen-fx/registry}"
LPM_URL="https://github.com/$LPM_REPO"
LPM_DIR="$HOME/.local/bin"

PIN_VERSION=""
NO_CONFIRM=0
MODIFY_PATH=1
INSTALL_MODULES=1
INSTALL_LPM=1
FORCE=0
UNINSTALL=0

say() { printf '%s\n' "$*"; }
fail() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Lumen toolchain installer.

Usage:
  install.sh [options]

Installs lumenc, the liblumen runtime library, lumen-server, shell
completions, and lpm, the client of the package registry.

Options:
  --prefix DIR         Install root. Default: ~/.lumen
  --version VERSION    Install a pinned release instead of the current one.
                       lumenc never offers to update a pinned install; run
                       the installer again without --version to lift the pin.
  --no-confirm         Run without prompting; still writes a PATH line to a
                       shell rc file unless --no-modify-path is also given.
  --no-modify-path     Never write a PATH line to a shell rc file.
  --no-modules         Skip the bundled runtime modules (the standard module
                       library apps name with `bundled = true`); install the
                       toolchain alone. --uninstall still removes previously
                       installed modules through the receipt.
  --no-lpm             Skip lpm, the registry client. An app that names a
                       registry package then installs it on first use.
  --force              Reinstall even if already at the target version.
  --uninstall          Remove every file this installer put under the prefix.
  -h, --help           Show this help.

Environment:
  LUMEN_GH_REPO    GitHub repo to install from, as owner/name.
                   Default: lumen-fx/lumen
  LUMEN_PREFIX     Same as --prefix.
  LPM_GH_REPO      GitHub repo lpm comes from, as owner/name.
                   Default: lumen-fx/registry
EOF
}

# --- arguments ---------------------------------------------------------------

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || fail "--prefix needs a directory"
      PREFIX="$2"
      shift 2
      ;;
    --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
    --version)
      [ "$#" -ge 2 ] || fail "--version needs a version"
      PIN_VERSION="$2"
      shift 2
      ;;
    --version=*) PIN_VERSION="${1#--version=}"; shift ;;
    --no-confirm) NO_CONFIRM=1; shift ;;
    --no-modify-path) MODIFY_PATH=0; shift ;;
    --no-modules) INSTALL_MODULES=0; shift ;;
    --no-lpm) INSTALL_LPM=0; shift ;;
    --force) FORCE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1 (try --help)" ;;
  esac
done

case "$PREFIX" in
  /*) ;;
  ~*) PREFIX="$HOME${PREFIX#\~}" ;;
  *) PREFIX="$PWD/$PREFIX" ;;
esac

BIN_DIR="$PREFIX/bin"
RECEIPT_DIR="$PREFIX/share/lumen"

# --- tools -------------------------------------------------------------------

if command -v curl >/dev/null 2>&1; then
  DOWNLOADER=curl
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER=wget
else
  DOWNLOADER=none
fi

if command -v sha256sum >/dev/null 2>&1; then
  HASHER=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  HASHER=shasum
else
  HASHER=none
fi

# curl's own --retry covers a timeout, a 429 and a 5xx, and has since 7.12. It
# leaves a reset connection alone: that exits 56 on the first attempt, so one
# passing fault ends the install, and the message the caller prints then blames
# a release that is in fact there. The option that closes the gap is
# --retry-all-errors, which arrived in curl 7.71, and this script runs on
# whatever curl the machine has, including the 7.68 of Ubuntu 20.04 and the
# 7.61 of RHEL 8, where an unknown option exits 2 and takes the install down
# with it. So the loop below covers that half.
#
# It retries whatever curl gave up on, except exit 22: with -f that is a status
# curl already decided was worth no retry, which makes it an answer rather than
# a fault, so a missing asset still fails at once and the pinned-version probe
# below still costs no waiting. stdout is held back until an attempt answers,
# so a --write-out value comes out once rather than once per try; every caller
# sends the download itself to a file with -o, which keeps that buffer small.
#
# The wget branch needs none of this and is left alone: wget retries a reset by
# itself, --tries defaults to 20, and with -O it rewrites the file from the
# start on each try rather than appending to the part it already has.
curl_retry() {
  # curl_retry CURL-ARG... -> stdout of the attempt that answered
  curl_try=1
  while true; do
    if curl_out="$(curl --retry 3 --retry-delay 2 "$@")"; then
      printf '%s' "$curl_out"
      return 0
    else
      curl_rc=$?
    fi
    [ "$curl_rc" -ne 22 ] || return "$curl_rc"
    [ "$curl_try" -lt 3 ] || return "$curl_rc"
    curl_try=$((curl_try + 1))
    sleep 2
  done
}

fetch_quiet() {
  # fetch_quiet URL DEST
  case "$DOWNLOADER" in
    curl) curl_retry -fsSL -o "$2" "$1" ;;
    wget) wget -q -O "$2" "$1" ;;
    *) fail "need curl or wget" ;;
  esac
}

fetch_shown() {
  # fetch_shown URL DEST
  case "$DOWNLOADER" in
    curl) curl_retry -fSL --progress-bar -o "$2" "$1" ;;
    wget) wget -O "$2" "$1" ;;
    *) fail "need curl or wget" ;;
  esac
}

sha256_of() {
  case "$HASHER" in
    sha256sum) sha256sum "$1" | cut -d' ' -f1 ;;
    shasum) shasum -a 256 "$1" | cut -d' ' -f1 ;;
    *) fail "need sha256sum or shasum to verify downloads" ;;
  esac
}

final_url() {
  # final_url URL -> the URL a GET of URL ends at, after redirects.
  case "$DOWNLOADER" in
    curl) curl_retry -fsSL -o /dev/null -w '%{url_effective}' "$1" ;;
    wget)
      # --spider makes it a HEAD; --server-response writes every response
      # header to stderr, so the last Location is the end of the chain.
      wget --server-response --spider "$1" 2>&1 |
        awk 'tolower($1) == "location:" { print $2 }' |
        tail -n 1
      ;;
    *) fail "need curl or wget" ;;
  esac
}

# --- prompts -----------------------------------------------------------------

# Reads from the terminal, not stdin: with `curl ... | sh` stdin is the script
# itself. Without a terminal the answer is no, and --no-confirm is the way
# through.
ask() {
  if [ "$NO_CONFIRM" -eq 1 ]; then
    return 0
  fi
  # In a subshell: with no controlling terminal, opening /dev/tty is a fatal
  # redirection error in some shells, and the subshell contains it.
  if ! ( : >/dev/tty ) 2>/dev/null; then
    say "No terminal to ask on. Re-run with --no-confirm to accept the defaults."
    return 1
  fi
  printf '%s [Y/n] ' "$1" >/dev/tty
  ask_reply=""
  read -r ask_reply </dev/tty 2>/dev/null || ask_reply=n
  case "$ask_reply" in
    ''|y|Y|yes|Yes|YES) return 0 ;;
    *) return 1 ;;
  esac
}

# --- release data --------------------------------------------------------------
#
# Everything the script needs about a release comes out of its sha256sums.txt:
# which assets exist, and what each one hashes to. The file is sha256sum's own
# output, so a line is "<hex>  <filename>", and a filename may carry a leading
# "*" from binary mode.

asset_url() {
  # asset_url NAME -> the download URL for NAME in the resolved release
  printf '%s/releases/download/%s/%s\n' "$GH_URL" "$TAG" "$1"
}

asset_sha() {
  # asset_sha NAME -> the sha256 recorded for NAME, empty if it has no line
  awk -v want="$1" '
    NF >= 2 {
      name = $2
      sub(/^\*/, "", name)
      if (name == want) { print $1; exit }
    }' "$SUMS"
}

published_targets() {
  # published_targets -> one target per line the release has a lumen-*.tar.gz
  # asset for, read off the checksum lines rather than a separate list. The
  # browser runtime is named the same way and is not a platform, so it is
  # skipped rather than reported as one; so are the two per-target assets
  # that are not the toolchain, or a release would report each platform
  # three times.
  awk '
    NF >= 2 {
      name = $2
      sub(/^\*/, "", name)
      if (name == "lumen-web.tar.gz") { next }
      if (index(name, "lumen-modules-") == 1) { next }
      if (index(name, "lumen-linkkit-") == 1) { next }
      if (index(name, "lumen-") != 1) { next }
      if (name !~ /\.tar\.gz$/) { next }
      t = substr(name, length("lumen-") + 1)
      sub(/\.tar\.gz$/, "", t)
      print t
    }' "$SUMS"
}

# --- receipt -------------------------------------------------------------------
#
#   version 0.1.0
#   target linux-x86_64
#   pinned 0.1.0
#   file bin/lumenc
#   file bin/liblumen.so
#
# The "pinned" line is present only for a --version install, and carries the
# resolved release, so it always agrees with the "version" line above it.
# lumenc's update check (public/lumenc/src/package/update_check.rs) treats its
# presence as "leave this install alone".

RECEIPT="$RECEIPT_DIR/lumen.receipt"

receipt_version() {
  [ -f "$RECEIPT" ] || return 0
  awk '$1 == "version" { print $2; exit }' "$RECEIPT"
}

receipt_files() {
  [ -f "$RECEIPT" ] || return 0
  awk '$1 == "file" { print substr($0, 6) }' "$RECEIPT"
}

set_receipt_pin() {
  # set_receipt_pin VERSION|"" -> rewrite an existing receipt with, or
  # without, its "pinned" line and leave every other line alone. Used on the
  # already-up-to-date path, where nothing else is rewritten but the pin still
  # has to follow the flags this run was given.
  [ -f "$RECEIPT" ] || return 0
  srp_tmp="$RECEIPT.tmp.$$"
  {
    awk '$1 != "pinned" && $1 != "file"' "$RECEIPT"
    if [ -n "$1" ]; then
      printf 'pinned %s\n' "$1"
    fi
    awk '$1 == "file"' "$RECEIPT"
  } > "$srp_tmp"
  mv "$srp_tmp" "$RECEIPT"
}

prune_dirs() {
  # Removes directories left empty by a removal. rmdir refuses non-empty ones.
  [ -d "$PREFIX" ] || return 0
  find "$PREFIX" -depth -type d -exec rmdir {} + 2>/dev/null || true
}

# --- uninstall ---------------------------------------------------------------

do_uninstall() {
  if [ ! -f "$RECEIPT" ]; then
    say "Nothing to uninstall: no Lumen install found at $PREFIX"
    exit 0
  fi

  say "Removing from $PREFIX:"
  say "  lumen $(receipt_version)"
  if ! ask "Remove these?"; then
    say "Cancelled."
    exit 1
  fi

  receipt_files | while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    rm -f "$PREFIX/$rel"
  done
  rm -f "$RECEIPT"
  prune_dirs
  say "Removed. If a PATH line for $BIN_DIR is still in a shell rc file, delete it by hand."
  exit 0
}

if [ "$UNINSTALL" -eq 1 ]; then
  do_uninstall
fi

# --- platform ----------------------------------------------------------------

UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"

case "$UNAME_S" in
  Linux) OS=linux ;;
  Darwin) OS=macos ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) OS=windows ;;
  *) fail "unsupported operating system: $UNAME_S. Lumen ships for Linux and macOS." ;;
esac

case "$UNAME_M" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) fail "unsupported architecture: $UNAME_M. Lumen ships for x86_64 and aarch64." ;;
esac

TARGET="$OS-$ARCH"

[ "$DOWNLOADER" != none ] || fail "need curl or wget"
[ "$HASHER" != none ] || fail "need sha256sum or shasum to verify downloads"

# --- resolve the release ------------------------------------------------------

TMP="$(mktemp -d "${TMPDIR:-/tmp}/lumen-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

SUMS="$TMP/sha256sums.txt"

sums_missing() {
  fail "release $1 of $GH_REPO has no sha256sums.txt. Either that release does not exist, or it predates checksum publishing and this installer cannot verify it. See $GH_URL/releases"
}

# A pinned version is tried as given, then with a "v" prefix, since this
# project tags releases vX.Y.Z but --version is documented as taking the bare
# number. The checksum file is the probe: a tag with no sha256sums.txt behind
# it is a tag this script cannot install from, whatever the reason.
if [ -n "$PIN_VERSION" ]; then
  TAG="$PIN_VERSION"
  # The first attempt is a guess at which of the two tag shapes this is, so
  # its failure is not news: keep the downloader quiet about it and let the
  # retry, or the message below, do the talking.
  if ! fetch_quiet "$(asset_url sha256sums.txt)" "$SUMS" 2>/dev/null; then
    case "$PIN_VERSION" in
      v*) sums_missing "$PIN_VERSION" ;;
      *)
        TAG="v$PIN_VERSION"
        fetch_quiet "$(asset_url sha256sums.txt)" "$SUMS" ||
          sums_missing "$PIN_VERSION (tried tags $PIN_VERSION and $TAG)"
        ;;
    esac
  fi
else
  # /releases/latest redirects to /releases/tag/<tag>, so the last path
  # segment of the final URL is the tag. With no releases at all the redirect
  # lands on the release index instead, which is what the guard below catches.
  LATEST_URL="$(final_url "$GH_URL/releases/latest" || true)"
  TAG="${LATEST_URL##*/}"
  case "$TAG" in
    ''|latest|releases)
      fail "could not resolve the latest release of $GH_REPO. Either it has no releases yet, or the request did not get through. See $GH_URL/releases"
      ;;
  esac
  fetch_quiet "$(asset_url sha256sums.txt)" "$SUMS" || sums_missing "$TAG"
fi

[ -s "$SUMS" ] || sums_missing "$TAG"
RELEASE="${TAG#v}"

if [ "$OS" = windows ]; then
  say "This installer covers Linux and macOS."
  if [ -n "$(asset_sha "lumen-windows-$ARCH.msi")" ]; then
    say "For Windows, download and run the installer:"
    say "  $(asset_url "lumen-windows-$ARCH.msi")"
  else
    say "A Windows installer is not published for $ARCH yet. See $GH_URL/releases"
  fi
  exit 1
fi

# --- resolve the asset ---------------------------------------------------------

ASSET_NAME="lumen-$TARGET.tar.gz"
ASSET_SHA="$(asset_sha "$ASSET_NAME")"
[ -n "$ASSET_SHA" ] ||
  fail "no build for $TARGET in release $TAG (published: $(published_targets | tr '\n' ' ' | sed 's/ *$//'))"

INSTALLED="$(receipt_version)"
if [ "$FORCE" -eq 0 ] && [ "$INSTALLED" = "$RELEASE" ]; then
  # Nothing to copy, but the pin still follows this run's flags: --version on
  # the version already installed pins it, and a plain re-run lifts a pin.
  if [ -n "$PIN_VERSION" ]; then
    set_receipt_pin "$RELEASE"
  else
    set_receipt_pin ""
  fi
  say ""
  say "Lumen toolchain installer"
  say ""
  say "  release   $RELEASE"
  say "  target    $TARGET"
  say "  prefix    $PREFIX"
  say ""
  say "Already up to date: lumen $INSTALLED"
  if [ -n "$PIN_VERSION" ]; then
    say "Pinned to $RELEASE. lumenc will not offer newer releases."
  fi
  say ""
  say "Use --force to reinstall."
  exit 0
fi

say ""
say "Lumen toolchain installer"
say ""
say "  release   $RELEASE"
say "  target    $TARGET"
say "  prefix    $PREFIX"
say ""
if [ -n "$INSTALLED" ]; then
  say "  lumen $INSTALLED -> $RELEASE"
else
  say "  lumen $RELEASE"
fi
say "    lumenc, the liblumen runtime library, and lumen-server"
say ""

if ! ask "Install?"; then
  say "Cancelled. Nothing was written."
  exit 1
fi

# --- download and verify -----------------------------------------------------

ASSET_URL="$(asset_url "$ASSET_NAME")"

say "Downloading lumen"
mkdir -p "$TMP/dl"
if ! fetch_shown "$ASSET_URL" "$TMP/dl/lumen.tar.gz"; then
  fail "download failed: $ASSET_URL"
fi

got="$(sha256_of "$TMP/dl/lumen.tar.gz")"
if [ "$got" != "$ASSET_SHA" ]; then
  fail "checksum mismatch for lumen
  expected $ASSET_SHA
  got      $got
Nothing was installed. The download was corrupted, or the asset at $ASSET_URL does not match the checksum published with release $TAG."
fi

# --- unpack and install ------------------------------------------------------

root="$TMP/x"
mkdir -p "$root"
tar -xzf "$TMP/dl/lumen.tar.gz" -C "$root" || fail "could not unpack the lumen archive"

# Tolerate one wrapping directory inside the archive.
if [ ! -d "$root/bin" ]; then
  inner=""
  inner_count=0
  for candidate in "$root"/*; do
    [ -e "$candidate" ] || continue
    inner_count=$((inner_count + 1))
    inner="$candidate"
  done
  if [ "$inner_count" -eq 1 ] && [ -d "$inner/bin" ]; then
    root="$inner"
  fi
fi
[ -d "$root/bin" ] || fail "the lumen archive has no bin/ directory"

# The bundled runtime modules ship as their own asset beside the toolchain
# archive. Unpacked into the same tree they land in bin/, beside the engine,
# which is where the runtime's `bundled = true` probe looks; merging them
# here also puts them on the same file list, so the receipt and any later
# upgrade cover them. A release without the asset installs the toolchain
# alone, and --no-modules chooses the same shape on purpose; an upgrade that
# skips them then removes any previously installed copies through the
# receipt's file list, the same way --uninstall does.
MODULES_NAME="lumen-modules-$TARGET.tar.gz"
MODULES_SHA="$(asset_sha "$MODULES_NAME")"
if [ "$INSTALL_MODULES" -eq 1 ] && [ -n "$MODULES_SHA" ]; then
  say "Downloading the bundled modules"
  if ! fetch_shown "$(asset_url "$MODULES_NAME")" "$TMP/dl/lumen-modules.tar.gz"; then
    fail "download failed: $(asset_url "$MODULES_NAME")"
  fi
  got="$(sha256_of "$TMP/dl/lumen-modules.tar.gz")"
  if [ "$got" != "$MODULES_SHA" ]; then
    fail "checksum mismatch for the bundled modules
  expected $MODULES_SHA
  got      $got
Nothing was installed. The download was corrupted, or the asset does not match the checksum published with release $TAG."
  fi
  tar -xzf "$TMP/dl/lumen-modules.tar.gz" -C "$root" || fail "could not unpack the modules archive"
fi

( cd "$root" && find . \( -type f -o -type l \) -print ) | sed 's|^\./||' | sort > "$TMP/files"
[ -s "$TMP/files" ] || fail "the lumen archive is empty"

say "Installing lumen $RELEASE"
while IFS= read -r rel; do
  dest="$PREFIX/$rel"
  mkdir -p "$(dirname "$dest")"
  rm -f "$dest"
  cp -p "$root/$rel" "$dest"
done < "$TMP/files"

# Files the previous version installed and this one does not.
receipt_files | sort > "$TMP/old" || true
if [ -s "$TMP/old" ]; then
  comm -23 "$TMP/old" "$TMP/files" | while IFS= read -r stale; do
    [ -n "$stale" ] || continue
    rm -f "$PREFIX/$stale"
  done
fi

mkdir -p "$RECEIPT_DIR"
{
  printf 'version %s\n' "$RELEASE"
  printf 'target %s\n' "$TARGET"
  if [ -n "$PIN_VERSION" ]; then
    printf 'pinned %s\n' "$RELEASE"
  fi
  sed 's/^/file /' "$TMP/files"
} > "$RECEIPT"

if [ -d "$BIN_DIR" ]; then
  for exe in "$BIN_DIR"/*; do
    [ -f "$exe" ] || continue
    chmod 755 "$exe"
  done
fi

prune_dirs

# --- lpm ---------------------------------------------------------------------
#
# The client of the package registry, which resolves the `version` sources an
# app declares. It is published from its own repository and installed to one
# shared path, `~/.local/bin/lpm`, rather than under the prefix: one copy
# serves every toolchain on the machine, and lumenc looks for it there. For
# the same reason it is not in the receipt, and --uninstall leaves it alone.
#
# Skipping it costs nothing. lumenc installs it itself the first time an app
# names a registry package.

if [ "$INSTALL_LPM" -eq 1 ]; then
  lpm_tag="$(final_url "$LPM_URL/releases/latest" || true)"
  lpm_tag="${lpm_tag##*/}"
  case "$lpm_tag" in
    ''|latest|releases) lpm_tag="" ;;
  esac
  if [ -z "$lpm_tag" ]; then
    say ""
    say "Could not resolve the latest release of $LPM_REPO, so lpm was not"
    say "installed. lumenc installs it the first time an app needs it."
  else
    lpm_version="${lpm_tag#v}"
    case "$ARCH" in
      aarch64|arm64) lpm_arch="arm64" ;;
      *) lpm_arch="amd64" ;;
    esac
    case "$OS" in
      macos) lpm_os="darwin" ;;
      *) lpm_os="linux" ;;
    esac
    lpm_asset="lpm_${lpm_version}_${lpm_os}_${lpm_arch}.tar.gz"
    lpm_base="$LPM_URL/releases/download/$lpm_tag"
    if ! fetch_quiet "$lpm_base/checksums.txt" "$TMP/lpm-sums.txt"; then
      fail "download failed: $lpm_base/checksums.txt"
    fi
    lpm_sha="$(awk -v want="$lpm_asset" '
      { name = $2; sub(/^\*/, "", name); if (name == want) { print $1; exit } }
    ' "$TMP/lpm-sums.txt")"
    [ -n "$lpm_sha" ] || fail "release $lpm_tag of $LPM_REPO publishes no $lpm_asset"
    say ""
    say "Downloading lpm $lpm_version"
    if ! fetch_shown "$lpm_base/$lpm_asset" "$TMP/dl/lpm.tar.gz"; then
      fail "download failed: $lpm_base/$lpm_asset"
    fi
    got="$(sha256_of "$TMP/dl/lpm.tar.gz")"
    if [ "$got" != "$lpm_sha" ]; then
      fail "checksum mismatch for $lpm_asset
  expected $lpm_sha
  got      $got
Nothing was installed. The download was corrupted, or the asset does not match the checksum published with release $lpm_tag."
    fi
    mkdir -p "$TMP/lpm" "$LPM_DIR"
    tar -xzf "$TMP/dl/lpm.tar.gz" -C "$TMP/lpm" || fail "could not unpack $lpm_asset"
    lpm_bin="$(find "$TMP/lpm" -type f -name lpm -print | head -n 1)"
    [ -n "$lpm_bin" ] || fail "$lpm_asset carries no lpm executable"
    rm -f "$LPM_DIR/lpm"
    cp -p "$lpm_bin" "$LPM_DIR/lpm"
    chmod 755 "$LPM_DIR/lpm"
  fi
fi

# --- PATH --------------------------------------------------------------------

# The rc line keeps $PATH unexpanded on purpose: it is written to the file
# verbatim and expanded by the shell that reads it.
# shellcheck disable=SC2016
path_line_for() {
  case "$1" in
    */fish) printf 'set -gx PATH "%s" $PATH\n' "$BIN_DIR" ;;
    *) printf 'export PATH="%s:$PATH"\n' "$BIN_DIR" ;;
  esac
}

rc_file_for() {
  case "$1" in
    */fish) printf '%s\n' "$HOME/.config/fish/config.fish" ;;
    */zsh) printf '%s\n' "$HOME/.zshrc" ;;
    */bash)
      if [ "$OS" = macos ] && [ -f "$HOME/.bash_profile" ]; then
        printf '%s\n' "$HOME/.bash_profile"
      else
        printf '%s\n' "$HOME/.bashrc"
      fi
      ;;
    *) printf '%s\n' "$HOME/.profile" ;;
  esac
}

on_path=0
case ":$PATH:" in
  *":$BIN_DIR:"*) on_path=1 ;;
esac

say ""
if [ "$on_path" -eq 0 ]; then
  RC="$(rc_file_for "${SHELL:-/bin/sh}")"
  LINE="$(path_line_for "${SHELL:-/bin/sh}")"
  already=0
  if [ -f "$RC" ] && grep -q -F "$BIN_DIR" "$RC" 2>/dev/null; then
    already=1
  fi
  if [ "$already" -eq 1 ]; then
    say "$BIN_DIR is already in $RC. Open a new shell to pick it up."
  elif [ "$MODIFY_PATH" -eq 0 ]; then
    say "Add $BIN_DIR to your PATH:"
    say "  $LINE"
  elif ask "Add $BIN_DIR to your PATH in $RC?"; then
    mkdir -p "$(dirname "$RC")"
    {
      printf '\n# added by the Lumen installer\n'
      printf '%s\n' "$LINE"
    } >> "$RC"
    say "Added to $RC. Open a new shell, or run:"
    say "  $LINE"
  else
    say "Left your shell configuration alone. To use Lumen, add:"
    say "  $LINE"
  fi
fi

# --- shell completions -------------------------------------------------------
#
# The archive carries a completion script per shell under share/, so the copy
# loop above already put them in place and the receipt already covers them.
# What is left is telling the shell where to look, which is the shell's own
# configuration and so is printed rather than written.

BASH_COMPLETION="$PREFIX/share/bash-completion/completions/lumenc"
ZSH_COMPLETION_DIR="$PREFIX/share/zsh/site-functions"
FISH_COMPLETION="$PREFIX/share/fish/vendor_completions.d/lumenc.fish"

case "${SHELL:-/bin/sh}" in
  */bash)
    if [ -f "$BASH_COMPLETION" ]; then
      say ""
      say "Shell completions are installed. To load them, add to your bash rc file:"
      say "  source $BASH_COMPLETION"
    fi
    ;;
  */zsh)
    if [ -f "$ZSH_COMPLETION_DIR/_lumenc" ]; then
      say ""
      say "Shell completions are installed. To load them, add above compinit in ~/.zshrc:"
      say "  fpath=($ZSH_COMPLETION_DIR \$fpath)"
    fi
    ;;
  */fish)
    if [ -f "$FISH_COMPLETION" ]; then
      say ""
      say "Shell completions are installed. To load them:"
      say "  ln -s $FISH_COMPLETION ~/.config/fish/completions/lumenc.fish"
    fi
    ;;
esac

say ""
say "Installed under $PREFIX:"
say "  lumen $(receipt_version)"
if [ -f "$LPM_DIR/lpm" ]; then
  say ""
  say "lpm, the registry client, is at $LPM_DIR/lpm."
  case ":$PATH:" in
    *":$LPM_DIR:"*) ;;
    *)
      say "$LPM_DIR is not on your PATH. lumenc finds lpm there either way;"
      say "add the directory to run lpm yourself."
      ;;
  esac
fi
if [ -n "$PIN_VERSION" ]; then
  say ""
  say "Pinned to $RELEASE. lumenc will not offer newer releases; re-run this"
  say "installer without --version to lift the pin."
fi
say ""
say "Get started:"
say "  lumenc new my-app counter"
say "  lumenc run my-app"
