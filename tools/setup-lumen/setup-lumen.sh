#!/usr/bin/env bash
#
# The download, verify, and unpack half of the setup-lumen action. Two
# commands, split where the action's cache sits between them:
#
#   resolve [VERSION]        Work out the release tag, this machine's target,
#                            and where the toolchain goes. VERSION is a
#                            release number, a tag, or "latest" (the default).
#   install [--no-modules] TAG TARGET DIR
#                            Download lumen-<TARGET> and the bundled runtime
#                            modules from release TAG, check each against that
#                            release's sha256sums.txt, and unpack them into
#                            DIR. --no-modules installs the toolchain alone.
#
# resolve prints "key=value" lines on stdout and appends them to $GITHUB_OUTPUT
# when that is set, so the same script runs in a workflow and in a plain shell.
# Outside a runner it reads the platform from uname instead of RUNNER_OS and
# RUNNER_ARCH, which is what makes the whole path testable without a runner.
#
# There is no GitHub API call here. The latest tag comes from the redirect
# that https://github.com/<repo>/releases/latest sends, and everything else is
# a plain file download from releases/download/<tag>/<asset>. The anonymous
# API rate limit is shared by every job running on a hosted runner, so a
# toolchain install that reads it fails on a busy morning for reasons the
# person who wrote the workflow cannot see or fix.
#
# Assets come out of .github/workflows/build-toolchain.yml:
#
#   lumen-<target>.tar.gz     linux and macos, x86_64 and aarch64
#   lumen-modules-<target>.tar.gz
#                             the bundled runtime modules for those same four
#                             Unix targets
#   lumen-windows-<arch>.zip  the portable Windows archive
#   sha256sums.txt            one "<hex>  <filename>" line per asset above
#
# The zip is what this installs on Windows, never the MSI beside it. The MSI
# writes an install receipt, and a receipt is what turns lumenc's update check
# on; a runner has no use for either.
#
# Every archive holds bin/, with lumenc, the liblumen shared library, and the
# lumen-launcher app stub together in it. lumenc loads liblumen from next to
# its own executable, so bin/ moves as one directory or not at all.
#
# The modules archive has the same bin/ layout, so unpacking it over the same
# tree drops each module beside the engine, which is where the runtime's
# `bundled = true` probe looks. A job that installs without them runs an app
# declaring [dependencies] with every call into those namespaces failing. This
# is the shape install.sh installs in, and --no-modules is the same opt-out it
# offers. A release whose sha256sums.txt has no line for the asset installs
# the toolchain alone, which is what every Windows target does: the Windows
# legs publish no modules archive and compile the same capabilities in.

set -euo pipefail

REPO="${LUMEN_GH_REPO:-lumen-fx/lumen}"
GH_URL="https://github.com/$REPO"

fail() {
  printf 'setup-lumen: %s\n' "$*" >&2
  exit 1
}

# --- platform ----------------------------------------------------------------

detect_target() {
  local os arch
  case "${RUNNER_OS:-}" in
    Linux) os=linux ;;
    macOS) os=macos ;;
    Windows) os=windows ;;
    '')
      case "$(uname -s)" in
        Linux) os=linux ;;
        Darwin) os=macos ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT) os=windows ;;
        *) fail "unsupported operating system: $(uname -s)" ;;
      esac
      ;;
    *) fail "unsupported runner OS: $RUNNER_OS" ;;
  esac

  case "${RUNNER_ARCH:-}" in
    X64) arch=x86_64 ;;
    ARM64) arch=aarch64 ;;
    '')
      case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "unsupported architecture: $(uname -m)" ;;
      esac
      ;;
    *) fail "unsupported runner architecture: $RUNNER_ARCH" ;;
  esac

  if [ "$os" = windows ] && [ "$arch" != x86_64 ]; then
    fail "no Lumen build for windows-$arch. Windows releases are x86_64 only; see $GH_URL/releases"
  fi

  printf '%s-%s\n' "$os" "$arch"
}

asset_for() {
  # asset_for TARGET -> the archive asset name for that target
  case "$1" in
    windows-*) printf 'lumen-%s.zip\n' "$1" ;;
    *) printf 'lumen-%s.tar.gz\n' "$1" ;;
  esac
}

asset_url() {
  # asset_url TAG NAME
  printf '%s/releases/download/%s/%s\n' "$GH_URL" "$1" "$2"
}

# Windows paths, for the few tools that are Windows programs rather than
# shell builtins. Everywhere else the forward-slash form is the one to use.
native_path() {
  if command -v cygpath > /dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum > /dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v certutil > /dev/null 2>&1; then
    certutil -hashfile "$(native_path "$1")" SHA256 |
      sed -n 2p | tr -d ' \r' | tr '[:upper:]' '[:lower:]'
  else
    fail "no sha256 tool found (looked for sha256sum, shasum, certutil)"
  fi
}

unpack() {
  # unpack ARCHIVE DEST
  case "$1" in
    *.tar.gz)
      tar -xzf "$1" -C "$2"
      ;;
    *.zip)
      if command -v unzip > /dev/null 2>&1; then
        unzip -q "$1" -d "$2"
      elif command -v 7z > /dev/null 2>&1; then
        7z x -y -o"$2" "$1" > /dev/null
      elif command -v powershell > /dev/null 2>&1; then
        powershell -NoProfile -NonInteractive -Command \
          "Expand-Archive -LiteralPath '$(native_path "$1")' -DestinationPath '$(native_path "$2")' -Force"
      else
        fail "no way to unpack a zip (looked for unzip, 7z, powershell)"
      fi
      ;;
    *) fail "unknown archive format: $1" ;;
  esac
}

# --- download -----------------------------------------------------------------

# curl's own --retry covers a timeout, a 429 and a 5xx, and has since 7.12. It
# leaves a reset connection alone: that exits 56 on the first attempt, and a
# caller reading the exit status sees a fault it cannot tell from a real
# answer. The option that closes that gap is --retry-all-errors, which arrived
# in curl 7.71, and this action is published for anyone to run, on runners and
# in plain shells whose curl is older than that, where an unknown option exits
# 2 and takes the install down with it. So the loop below does that half.
#
# It retries whatever curl gave up on, except exit 22: with -f that is a status
# curl already decided was worth no retry, which makes it an answer rather than
# a fault. stdout is held back until an attempt succeeds, so a --write-out
# value comes out once rather than once per try; every caller writes its
# download to a file with -o, which is what keeps that buffer small.
curl_retry() {
  # curl_retry CURL-ARG... -> stdout of the attempt that answered
  local attempt=1 out rc
  while true; do
    if out="$(curl --retry 3 --retry-delay 2 "$@")"; then
      printf '%s' "$out"
      return 0
    else
      rc=$?
    fi
    [ "$rc" -ne 22 ] || return "$rc"
    [ "$attempt" -lt 3 ] || return "$rc"
    attempt=$((attempt + 1))
    sleep 2
  done
}

sums_sha() {
  # sums_sha SUMS NAME -> the sha256 SUMS records for NAME, empty if it has none
  #
  # sha256sums.txt is sha256sum's own output: "<hex>  <name>", with a "*"
  # before the name when it was hashed in binary mode. An asset with no line
  # is one the release did not publish, which is a question this answers
  # rather than an error.
  awk -v want="$2" '
    NF >= 2 {
      name = $2
      sub(/^\*/, "", name)
      if (name == want) { print $1; exit }
    }' "$1"
}

fetch_verified() {
  # fetch_verified TAG NAME WANT DEST
  local url got
  url="$(asset_url "$1" "$2")"
  printf 'setup-lumen: downloading %s\n' "$url"
  curl_retry -fsSL -o "$4" "$url" || fail "could not download $url"
  got="$(sha256_of "$4")"
  if [ "$got" != "$3" ]; then
    fail "checksum mismatch for $2
  expected $3
  got      $got
Nothing was installed. The download was corrupted, or the asset does not match the checksum published with release $1."
  fi
  printf 'setup-lumen: checksum ok (%s)\n' "$3"
}

# --- release lookup -----------------------------------------------------------

# A release this script can install is one that published sha256sums.txt.
# Asking for that file is therefore both the existence check for a tag and the
# first half of the download, so the two never disagree.
#
# This asks a question rather than fetching something. A 404 is the answer "no
# release under that name" and the caller goes on to try the other spelling of
# the version, so curl_retry spends no attempts on it. A reset is not an
# answer, and reporting one as a missing release is how a passing fault turns
# into "no installable release".
has_release() {
  # has_release TAG
  curl_retry -fsSLI -o /dev/null "$(asset_url "$1" sha256sums.txt)" 2> /dev/null
}

resolve_tag() {
  # resolve_tag VERSION -> the git tag to install from
  local spec="$1" url tag
  if [ "$spec" = latest ]; then
    url="$(curl_retry -fsSLI -o /dev/null -w '%{url_effective}' \
      "$GH_URL/releases/latest")" ||
      fail "could not reach $GH_URL/releases/latest"
    tag="${url##*/}"
    case "$tag" in
      '' | latest | releases)
        fail "could not resolve the latest release of $REPO. It may have no releases yet; see $GH_URL/releases"
        ;;
    esac
    printf '%s\n' "$tag"
    return
  fi

  # A pinned version is written either way round: the tags are vX.Y.Z and the
  # number people quote is X.Y.Z. Try what was asked for, then the other one.
  if has_release "$spec"; then
    printf '%s\n' "$spec"
    return
  fi
  case "$spec" in
    v*) fail "release $spec of $REPO has no sha256sums.txt. Either it does not exist, or it predates checksum publishing and cannot be verified; see $GH_URL/releases" ;;
  esac
  if has_release "v$spec"; then
    printf 'v%s\n' "$spec"
    return
  fi
  fail "no installable release $spec of $REPO (tried tags $spec and v$spec); see $GH_URL/releases"
}

# --- resolve ------------------------------------------------------------------

cmd_resolve() {
  local spec="${1:-latest}" tag version target tool_root install_dir bin_dir
  [ -n "$spec" ] || spec=latest

  target="$(detect_target)"
  tag="$(resolve_tag "$spec")"
  version="${tag#v}"

  # The runner tool cache is the conventional home for a toolchain a workflow
  # installs, and it is a plain directory the job's own cache step can hand
  # back on the next run. Off a runner it falls back to a cache directory so
  # the script still has somewhere sensible to put things.
  tool_root="${RUNNER_TOOL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}}"
  tool_root="${tool_root//\\//}"
  install_dir="$tool_root/lumen/$version/$target"
  bin_dir="$install_dir/bin"

  emit() {
    printf '%s=%s\n' "$1" "$2"
    if [ -n "${GITHUB_OUTPUT:-}" ]; then
      printf '%s=%s\n' "$1" "$2" >> "$GITHUB_OUTPUT"
    fi
  }

  emit tag "$tag"
  emit version "$version"
  emit target "$target"
  emit asset "$(asset_for "$target")"
  emit install-dir "$install_dir"
  emit bin-dir "$bin_dir"
}

# --- install ------------------------------------------------------------------

cmd_install() {
  local usage='usage: setup-lumen.sh install [--no-modules] TAG TARGET DIR'
  local install_modules=1

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --no-modules)
        install_modules=0
        shift
        ;;
      --)
        shift
        break
        ;;
      -*) fail "unknown option: $1 ($usage)" ;;
      *) break ;;
    esac
  done

  local tag="${1:?$usage}"
  local target="${2:?$usage}"
  local dest="${3:?$usage}"
  local asset sums sums_url want tmp root inner inner_count modules modules_want

  asset="$(asset_for "$target")"
  sums_url="$(asset_url "$tag" sha256sums.txt)"

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/setup-lumen.XXXXXX")"
  # shellcheck disable=SC2064 # $tmp is fixed now; expanding it later is wrong.
  trap "rm -rf '$tmp'" EXIT HUP INT TERM
  sums="$tmp/sha256sums.txt"

  printf 'setup-lumen: release %s, target %s\n' "$tag" "$target"

  curl_retry -fsSL -o "$sums" "$sums_url" ||
    fail "could not download $sums_url"

  want="$(sums_sha "$sums" "$asset")"

  if [ -z "$want" ]; then
    fail "release $tag has no $asset. Published: $(awk '
      NF >= 2 {
        name = $2
        sub(/^\*/, "", name)
        printf "%s%s", sep, name
        sep = ", "
      }' "$sums")"
  fi

  fetch_verified "$tag" "$asset" "$want" "$tmp/$asset"

  root="$tmp/x"
  mkdir -p "$root"
  unpack "$tmp/$asset" "$root"

  # The Unix archives are packed from the staging root, so bin/ is at the top.
  # Tolerate one wrapping directory anyway, which is what a repacked archive
  # tends to grow.
  if [ ! -d "$root/bin" ]; then
    inner=''
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
  [ -d "$root/bin" ] || fail "the $asset archive has no bin/ directory"

  # The bundled runtime modules are their own asset beside the toolchain
  # archive, published for the four Unix targets. Unpacking them over the tree
  # above lands them in bin/, beside the engine, so the one copy below installs
  # both. A release that published no such asset, and every Windows target,
  # take the toolchain alone; so does --no-modules, on purpose.
  modules="lumen-modules-$target.tar.gz"
  modules_want="$(sums_sha "$sums" "$modules")"
  if [ "$install_modules" -eq 1 ] && [ -n "$modules_want" ]; then
    fetch_verified "$tag" "$modules" "$modules_want" "$tmp/$modules"
    tar -xzf "$tmp/$modules" -C "$root" ||
      fail "could not unpack the modules archive"
  fi

  mkdir -p "$dest"
  cp -R "$root/." "$dest/"

  for exe in "$dest"/bin/*; do
    [ -f "$exe" ] || continue
    chmod 755 "$exe"
  done

  printf 'setup-lumen: installed into %s\n' "$dest"
}

# --- entry point ---------------------------------------------------------------

case "${1:-}" in
  resolve)
    shift
    cmd_resolve "$@"
    ;;
  install)
    shift
    cmd_install "$@"
    ;;
  *)
    fail "usage: setup-lumen.sh resolve [VERSION] | setup-lumen.sh install [--no-modules] TAG TARGET DIR"
    ;;
esac
