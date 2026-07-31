#!/bin/sh
# Build the release manifest that install.sh and the self-updaters read.
#
# Usage:
#   tools/make-manifest.sh --version X.Y.Z [options] SPEC [SPEC...]
#
#   SPEC is component:target:file, for example
#     lumen:linux-x86_64:dist/lumen-0.4.0-linux-x86_64.tar.gz
#
# Options:
#   --version VERSION   Release version. Required.
#   --channel NAME      Release channel. Default: alpha
#   --base-url URL      Download host. Default: https://dl.lumenfx.dev
#   --default LIST      Comma-separated components installed by default.
#                       Default: lumen
#   --out FILE          Write here instead of stdout.
#
# For every spec the script hashes the file, records its size, and derives the
# download URL as <base-url>/<component>/<version>/<basename>. That is the path
# the artifacts are uploaded to in R2, so the manifest and the bucket stay in
# step by construction.
#
# Targets are <os>-<arch>: linux-x86_64, linux-aarch64, macos-x86_64,
# macos-aarch64, windows-x86_64, windows-aarch64. The archive holds the tree to
# install, with executables in bin/ and libraries in lib/.
#
# Output (schema_version 1):
#
#   {
#     "schema_version": 1,
#     "channel": "alpha",
#     "version": "0.4.0",
#     "generated": "2026-07-30T12:00:00Z",
#     "base_url": "https://dl.lumenfx.dev",
#     "components": {
#       "<name>": {
#         "description": "...",
#         "default": true,
#         "targets": {
#           "<target>": {
#             "version": "0.4.0",
#             "url": "...",
#             "sha256": "...",
#             "size": 4194304,
#             "format": "tar.gz"
#           }
#         }
#       }
#     }
#   }
#
# Example:
#
#   tools/make-manifest.sh --version 0.4.0 --out manifest.json \
#     lumen:linux-x86_64:dist/lumen-0.4.0-linux-x86_64.tar.gz \
#     lumen:macos-aarch64:dist/lumen-0.4.0-macos-aarch64.tar.gz \
#     candela:linux-x86_64:dist/candela-0.4.0-linux-x86_64.tar.gz

set -eu

VERSION=""
CHANNEL="alpha"
BASE_URL="https://dl.lumenfx.dev"
DEFAULTS="lumen"
OUT=""

fail() { printf 'make-manifest.sh: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Build the release manifest served at https://lumenfx.dev/install/manifest.json.

Usage:
  tools/make-manifest.sh --version X.Y.Z [options] SPEC [SPEC...]

  SPEC is component:target:file, for example
    lumen:linux-x86_64:dist/lumen-0.4.0-linux-x86_64.tar.gz

Options:
  --version VERSION   Release version. Required.
  --channel NAME      Release channel. Default: alpha
  --base-url URL      Download host. Default: https://dl.lumenfx.dev
  --default LIST      Comma-separated components installed by default.
                      Default: lumen
  --out FILE          Write here instead of stdout.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) [ "$#" -ge 2 ] || fail "--version needs a value"; VERSION="$2"; shift 2 ;;
    --version=*) VERSION="${1#--version=}"; shift ;;
    --channel) [ "$#" -ge 2 ] || fail "--channel needs a value"; CHANNEL="$2"; shift 2 ;;
    --channel=*) CHANNEL="${1#--channel=}"; shift ;;
    --base-url) [ "$#" -ge 2 ] || fail "--base-url needs a value"; BASE_URL="$2"; shift 2 ;;
    --base-url=*) BASE_URL="${1#--base-url=}"; shift ;;
    --default) [ "$#" -ge 2 ] || fail "--default needs a value"; DEFAULTS="$2"; shift 2 ;;
    --default=*) DEFAULTS="${1#--default=}"; shift ;;
    --out) [ "$#" -ge 2 ] || fail "--out needs a path"; OUT="$2"; shift 2 ;;
    --out=*) OUT="${1#--out=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) fail "unknown option: $1" ;;
    *) break ;;
  esac
done

[ -n "$VERSION" ] || fail "--version is required"
[ "$#" -ge 1 ] || fail "no artifacts given (component:target:file)"
BASE_URL="${BASE_URL%/}"

if command -v sha256sum >/dev/null 2>&1; then
  HASHER=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  HASHER=shasum
else
  fail "need sha256sum or shasum"
fi

sha256_of() {
  case "$HASHER" in
    sha256sum) sha256sum "$1" | cut -d' ' -f1 ;;
    shasum) shasum -a 256 "$1" | cut -d' ' -f1 ;;
  esac
}

describe() {
  case "$1" in
    lumen) printf '%s' "lumenc and the liblumen runtime library" ;;
    candela) printf '%s' "The standalone candela toolchain: candela and candela-vm" ;;
    *) printf '%s' "" ;;
  esac
}

is_default() {
  case ",$DEFAULTS," in
    *",$1,"*) printf 'true' ;;
    *) printf 'false' ;;
  esac
}

# Collect the specs as "component target file" lines, keeping the order they
# were given so the manifest is stable across runs with the same arguments.
work="$(mktemp)"
trap 'rm -f "$work"' EXIT HUP INT TERM

for spec in "$@"; do
  component="${spec%%:*}"
  rest="${spec#*:}"
  target="${rest%%:*}"
  file="${rest#*:}"
  [ -n "$component" ] && [ -n "$target" ] && [ -n "$file" ] && [ "$file" != "$rest" ] ||
    fail "bad spec: $spec (expected component:target:file)"
  [ -f "$file" ] || fail "no such file: $file"
  case "$target" in
    linux-x86_64|linux-aarch64|macos-x86_64|macos-aarch64|windows-x86_64|windows-aarch64) ;;
    *) fail "unknown target: $target" ;;
  esac
  printf '%s %s %s\n' "$component" "$target" "$file" >> "$work"
done

emit() {
  printf '{\n'
  printf '  "schema_version": 1,\n'
  printf '  "channel": "%s",\n' "$CHANNEL"
  printf '  "version": "%s",\n' "$VERSION"
  printf '  "generated": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '  "base_url": "%s",\n' "$BASE_URL"
  printf '  "components": {\n'

  components="$(awk '{ print $1 }' "$work" | awk '!seen[$0]++')"
  first_component=1
  for component in $components; do
    [ "$first_component" -eq 1 ] || printf ',\n'
    first_component=0
    printf '    "%s": {\n' "$component"
    printf '      "description": "%s",\n' "$(describe "$component")"
    printf '      "default": %s,\n' "$(is_default "$component")"
    printf '      "targets": {\n'

    first_target=1
    while read -r c t f; do
      [ "$c" = "$component" ] || continue
      case "$f" in
        *.tar.gz|*.tgz) format="tar.gz" ;;
        *.zip) format="zip" ;;
        *.msi) format="msi" ;;
        *) fail "cannot tell the archive format of $f" ;;
      esac
      [ "$first_target" -eq 1 ] || printf ',\n'
      first_target=0
      printf '        "%s": {\n' "$t"
      printf '          "version": "%s",\n' "$VERSION"
      printf '          "url": "%s/%s/%s/%s",\n' "$BASE_URL" "$component" "$VERSION" "$(basename "$f")"
      printf '          "sha256": "%s",\n' "$(sha256_of "$f")"
      printf '          "size": %s,\n' "$(wc -c < "$f" | tr -d ' ')"
      printf '          "format": "%s"\n' "$format"
      printf '        }'
    done < "$work"

    printf '\n      }\n'
    printf '    }'
  done

  printf '\n  }\n'
  printf '}\n'
}

if [ -n "$OUT" ]; then
  emit > "$OUT"
  printf 'wrote %s (%s %s, %s component(s))\n' \
    "$OUT" "$CHANNEL" "$VERSION" "$(awk '{ print $1 }' "$work" | awk '!seen[$0]++' | wc -l | tr -d ' ')"
else
  emit
fi
