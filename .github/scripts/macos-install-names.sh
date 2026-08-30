#!/usr/bin/env bash
# Give every Lumen Mach-O in the named directories an @rpath install name.
#
# ld64 defaults a dylib's LC_ID_DYLIB to the path it was written to, and every
# dependent records that same absolute path as its LC_LOAD_DYLIB. Those paths
# point into the build machine's target directory, which exists nowhere else,
# and the @loader_path rpath the link already adds cannot rescue them: dyld
# consults the rpath list only for a name that starts with @rpath/. That is
# how the v0.0.6 macOS archives shipped a lumenc that aborts on any machine
# but the one that built it.
#
# Fixing it at link time would need a per-crate install name, and there is no
# such knob: RUSTFLAGS covers a whole cargo invocation, and the invocation
# that builds the engine builds liblumen and every bundled module beside it,
# so one -install_name would stamp one id on all of them. So the names are
# rewritten after the link instead. Each dylib takes @rpath/<its own file
# name> as its id, each reference to a file built beside it becomes
# @rpath/<that file name>, and anything that gained such a reference gets an
# @loader_path rpath if it does not already carry one - which is what makes
# "beside me" the answer, wherever the archive is unpacked.
#
# /usr/lib and /System are left alone. They are the operating system, they
# resolve out of the dyld shared cache, and they are absolute on every macOS
# machine by design.
#
# Editing load commands invalidates the ad-hoc signature ld64 wrote, and the
# kernel refuses to launch an arm64 binary whose signature does not match, so
# every file this touches is re-signed ad-hoc.
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
  echo "install names are a Mach-O concern; nothing to do on $(uname -s)"
  exit 0
fi

for dir in "$@"; do
  for file in "$dir"/*; do
    [ -f "$file" ] || continue
    case "$(file -b "$file")" in
      Mach-O*) ;;
      *) continue ;;
    esac

    base="$(basename "$file")"
    # Empty for an executable; a dylib prints its own id on the second line.
    id="$(otool -D "$file" | sed -n '2p')"
    touched=0
    rewrote=0

    # otool -L lists the id first on a dylib, so it is filtered out here
    # rather than relied on being ignored: install_name_tool -change edits
    # LC_LOAD_DYLIB and leaves LC_ID_DYLIB alone, but a silent no-op would
    # still count as a change below.
    while read -r dep; do
      case "$dep" in
        '' | '@'* | /usr/lib/* | /System/*) continue ;;
      esac
      if [ "$dep" = "$id" ]; then
        continue
      fi
      echo "$base: $dep -> @rpath/$(basename "$dep")"
      install_name_tool -change "$dep" "@rpath/$(basename "$dep")" "$file"
      touched=1
      rewrote=1
    done <<EOF
$(otool -L "$file" | tail -n +2 | awk '{print $1}')
EOF

    if [ -n "$id" ] && [ "$id" != "@rpath/$base" ]; then
      echo "$base: id $id -> @rpath/$base"
      install_name_tool -id "@rpath/$base" "$file"
      touched=1
    fi

    if [ "$rewrote" = 1 ] &&
      ! otool -l "$file" | grep -q '^ *path @loader_path (offset'; then
      echo "$base: adding an @loader_path rpath"
      install_name_tool -add_rpath @loader_path "$file"
    fi

    if [ "$touched" = 1 ]; then
      codesign -f -s - "$file"
    fi
  done
done
