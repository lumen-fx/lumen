#!/usr/bin/env bash
# Package apps against the engine kit a build leg just wrote, and prove the
# engine each package carries is the app's own and works.
#
#   engine-kit-smoke.sh <engine kit dir> <archive>...
#
# The archives are unpacked over one fresh prefix the way install.sh does it,
# and the lumenc inside it packages every app here with LUMEN_ENGINE_KIT_DIR
# pointing at the kit. A kit and a lumenc that reads it exist together
# nowhere else until the run that built them publishes both, so this is
# where the consumer meets the producer before anything uploads.
#
# What each package has to show:
#
#   - The summary says the engine was linked for the app, not that the full
#     engine travelled. A replay that fell back is a failure here.
#   - The engine exports the register symbol of every capability it carries
#     and of none it left out.
#   - The app runs headless against it, and a runtime module it declares
#     loads (Linux and macOS, where modules exist): the module was compiled
#     against the toolchain's engine, so its loading is what proves the
#     relinked one kept what it needs and kept the same build id.
#   - An app that turns a capability off while calling it is warned at
#     package time and once more, once, when the call runs.
set -euo pipefail

kit="$1"
shift

os="${RUNNER_OS:-}"
if [ -z "$os" ]; then
  case "$(uname -s)" in
    Darwin) os=macOS ;;
    Linux) os=Linux ;;
    *) os=Windows ;;
  esac
fi

here="$(cd "$(dirname "$0")/../.." && pwd)"
work="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/engine-kit-smoke"
rm -rf "$work"
prefix="$work/prefix"
mkdir -p "$prefix"
for archive in "$@"; do
  echo "unpacking $archive into $prefix"
  if [ "$os" = Windows ]; then
    powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$(cygpath -w "$archive")' -DestinationPath '$(cygpath -w "$prefix")' -Force"
  else
    tar -xzf "$archive" -C "$prefix"
  fi
done

if [ "$os" = Windows ]; then
  lumenc="$prefix/bin/lumenc.exe"
  exe=".exe"
  engine="lumen.dll"
  kit="$(cygpath -w "$kit")"
else
  lumenc="$prefix/bin/lumenc"
  exe=""
  case "$os" in
    macOS) engine="liblumen_engine.dylib" ;;
    *) engine="liblumen_engine.so" ;;
  esac
fi
export LUMEN_ENGINE_KIT_DIR="$kit"

llvm="$(rustc --print sysroot | tr '\134' '/')/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p' | tr -d '\r')/bin"

# The names a library exports, one per line, in the spelling a C program
# uses for them.
exports() {
  case "$os" in
    Linux) "$llvm/llvm-nm" -D --defined-only --format=just-symbols "$1" ;;
    macOS) "$llvm/llvm-nm" -gU --format=just-symbols "$1" | sed 's/^_//' ;;
    *) "$llvm/llvm-readobj" --coff-exports "$1" | sed -n 's/^ *Name: //p' | tr -d '\r' ;;
  esac
}

# The engine build id a library carries.
build_id() {
  LC_ALL=C grep -aoE 'lumen-engine [^ ]+ [^ ]+ rustc:[0-9a-f]{16}' "$1" | head -1
}

fail() {
  echo "engine-kit-smoke: $*" >&2
  exit 1
}

# package <app dir> <out dir>: package, and require the engine to be the
# app's own.
package() {
  "$lumenc" package "$1" "$2" >"$work/package.out" 2>"$work/package.err" ||
    { cat "$work/package.out" "$work/package.err" >&2; fail "$1 did not package"; }
  cat "$work/package.out"
  cat "$work/package.err" >&2
  grep -q "engine linked for the app" "$work/package.out" ||
    fail "$1 was packaged with the full engine"
}

# run <out dir> <exe stem>: run the package headless, keeping what it says.
run() {
  (cd "$1" && "./$2$exe" --headless --ticks 5) >"$work/run.out" 2>"$work/run.err" ||
    { cat "$work/run.out" "$work/run.err" >&2; fail "$1 did not run"; }
  cat "$work/run.err" >&2
  if grep -q "MODULE LOAD FAILED\|dependency '.*' skipped" "$work/run.err"; then
    fail "$1 did not load its modules against the relinked engine"
  fi
}

# carries <out dir> <capability> yes|no
carries() {
  local symbol="lumen_capability_register_${2//-/_}"
  if exports "$1/$engine" | grep -qx "$symbol"; then
    [ "$3" = yes ] || fail "$1 carries $2 and should not"
  else
    [ "$3" = no ] || fail "$1 does not carry $2"
  fi
}

# An app that uses none of the optional capabilities: only what every app
# carries goes in.
package "$here/apps/kanban" "$work/kanban"
run "$work/kanban" kanban
carries "$work/kanban" layout-taffy yes
carries "$work/kanban" os-tray no
carries "$work/kanban" devtools no
carries "$work/kanban" http-fetch no

if [ "$os" != Windows ]; then
  [ "$(build_id "$work/kanban/$engine")" = "$(build_id "$prefix/bin/$engine")" ] ||
    fail "the relinked engine's build id is not the toolchain engine's"
  # A runtime module, compiled against the toolchain's engine, loads into
  # the relinked one.
  package "$here/apps/music" "$work/music"
  run "$work/music" music
fi
if [ "$os" = macOS ]; then
  codesign --verify "$work/kanban/$engine"
fi

# An app in a deprecated language runs on its host: linked into the app's
# own lumen.dll on Windows, loaded from the package's modules/ elsewhere.
package "$here/apps/weather" "$work/weather"
run "$work/weather" weather

# An app that calls the tray carries it, and one that says it does not want
# the tray is told twice that its calls do nothing.
tray="$work/tray-app"
mkdir -p "$tray/src"
cat >"$tray/src/main.lmn" <<'EOF'
<root>
  <label text="tray"/>
  <script src="main.cdl"/>
</root>
EOF
cat >"$tray/src/main.cdl" <<'EOF'
import "lumen.cdl";

fn on_start() {
    lumen::tray_icon("main", "icon.png", "");
}

fn main() {}
EOF
printf '[app]\nentry = "main.lmn"\n' >"$tray/lumen.toml"
package "$tray" "$work/tray"
carries "$work/tray" os-tray yes

printf '[app]\nentry = "main.lmn"\n\n[capabilities]\nos-tray = false\n' >"$tray/lumen.toml"
package "$tray" "$work/no-tray"
carries "$work/no-tray" os-tray no
grep -q "leaves os-tray out, and the app calls \`tray_icon\`" "$work/package.err" ||
  fail "packaging without the tray did not warn about the tray_icon call"
run "$work/no-tray" tray-app
[ "$(grep -c "\`tray_icon\` does nothing here" "$work/run.err")" = 1 ] ||
  fail "the tray_icon call did not warn exactly once at run time"

echo "engine-kit-smoke: every package carries an engine linked for its app"
