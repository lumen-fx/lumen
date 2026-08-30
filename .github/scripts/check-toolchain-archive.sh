#!/usr/bin/env bash
# Prove the archives a build leg just produced actually run, before anything
# uploads them.
#
#   check-toolchain-archive.sh <version> <archive>...
#
# Every archive named is unpacked over one fresh prefix outside the build
# tree, which is what install.sh does with the toolchain archive and the
# modules archive on a user's machine, and then the lumenc inside that prefix
# is run and has to report the version this build is. Running the copy in
# dist/ proves less: dist/ sits beside the target directory that filled it.
#
# The launch alone would not have caught what it is here for, though, and
# saying so is the point of this file. v0.0.6's macOS binaries loaded the
# engine by the absolute path it was built at, and on the build machine that
# path is still there, so the binary launches from any directory and only
# fails once it reaches a machine that never built it. So the load commands
# are read as well: a shipped binary may name a library that lives beside it,
# or one the operating system owns, and nothing else. /usr/lib and /System on
# macOS and an ELF soname on Linux are the operating system and resolve out of
# the loader's own cache anywhere.
#
# Windows has no equivalent audit because the defect cannot be expressed
# there: a PE import table records a bare DLL name, never a path.
set -euo pipefail

version="$1"
shift

os="${RUNNER_OS:-}"
if [ -z "$os" ]; then
  case "$(uname -s)" in
    Darwin) os=macOS ;;
    Linux) os=Linux ;;
    *) os=Windows ;;
  esac
fi

dest="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/toolchain-archive-check"
rm -rf "$dest"
mkdir -p "$dest"

for archive in "$@"; do
  echo "unpacking $archive into $dest"
  if [ "$os" = Windows ]; then
    # The bash on a Windows runner is git bash, whose tar reads no zip, so
    # the unpack goes through the shell that ships with the OS.
    powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$(cygpath -w "$archive")' -DestinationPath '$(cygpath -w "$dest")' -Force"
  else
    tar -xzf "$archive" -C "$dest"
  fi
done

lumenc="$dest/bin/lumenc"
if [ ! -f "$lumenc" ]; then
  lumenc="$dest/bin/lumenc.exe"
fi

echo "running $lumenc --version"
# The trailing carriage return is a Windows lumenc writing through a C runtime
# that opens stdout in text mode; the version it printed is the same either
# way.
reported="$("$lumenc" --version | tr -d '\r')"
if [ "$reported" != "lumenc $version" ]; then
  echo "the unpacked archive reports '$reported', this build is '$version'" >&2
  exit 1
fi
echo "$reported"

bad=0

audit_macho() {
  local file="$1" base="$2" path
  while read -r path; do
    case "$path" in
      '' | '@'* | /usr/lib/* | /System/*) ;;
      *)
        echo "$base names $path, which is not in the archive" >&2
        bad=1
        ;;
    esac
  done <<EOF
$( {
  otool -D "$file" | tail -n +2
  otool -L "$file" | tail -n +2 | awk '{print $1}'
} | sort -u)
EOF
}

audit_elf() {
  local file="$1" base="$2" dynamic entry
  dynamic="$(readelf -d "$file")"
  # A soname with a directory in it would be resolved from that directory and
  # nowhere else, exactly the way a Mach-O absolute load name is.
  while read -r entry; do
    case "$entry" in
      '') ;;
      */*)
        echo "$base needs $entry, which is a path rather than a soname" >&2
        bad=1
        ;;
    esac
  done <<EOF
$(printf '%s\n' "$dynamic" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
EOF
  # A search path is only portable while it is written relative to the file
  # doing the searching.
  while read -r entry; do
    case "$entry" in
      '' | \$ORIGIN*) ;;
      *)
        echo "$base searches $entry, which is not relative to itself" >&2
        bad=1
        ;;
    esac
  done <<EOF
$(printf '%s\n' "$dynamic" |
  sed -n 's/.*(RUNPATH).*\[\(.*\)\]/\1/p;s/.*(RPATH).*\[\(.*\)\]/\1/p' |
  tr ':' '\n')
EOF
}

if [ "$os" = Windows ]; then
  echo "load-command audit: nothing to check, a PE import is a bare name"
else
  for file in "$dest"/bin/*; do
    [ -f "$file" ] || continue
    case "$(file -b "$file")" in
      Mach-O*) audit_macho "$file" "$(basename "$file")" ;;
      ELF*) audit_elf "$file" "$(basename "$file")" ;;
      *) continue ;;
    esac
  done
fi

if [ "$bad" != 0 ]; then
  echo "the archive would not run on a machine that did not build it" >&2
  exit 1
fi

echo "the archive runs from a fresh prefix and names nothing outside itself"
