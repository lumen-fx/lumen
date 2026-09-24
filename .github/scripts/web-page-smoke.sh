#!/usr/bin/env bash
# Emit an app for the web with the runtime built in this run, open it in a
# real browser, and check that it comes up alive.
#
# This is the only check that sees both halves of the web target at once. The
# emitter writes the page and the runtime takes it over, and neither crate's
# own tests can tell whether they still agree about the boot call, the file
# names or the node paths: the emitter has no runtime and the runtime has no
# emitter. A page that loads to a console error and sits there reads exactly
# like a page that works, which is why nothing below settles for "it loaded".
#
# Four apps are opened. The first is the app under test, which has a script
# the browser runs. The second is written in a language no browser host
# answers for: its page has to come up as a page anyway, because an app whose
# script cannot run is still an app a visitor can read. The third depends on a
# browser add-on, and every kind of call it offers has to come back into the
# page. The fourth uses every first-party add-on under std/addons, and is
# driven over WebDriver by web-addons-smoke.py, because its answers arrive
# after the few frames a dumped page gets.
#
#   $1  directory holding lumen-web.wasm and lumen-web.js
#   $2  the app to emit (default apps/widget-garden)
#   $3  the document to open, site-relative (default /), for an app whose
#       page under test is not the one at the site root

set -euo pipefail

lib_dir=$(realpath "${1:?usage: web-page-smoke.sh LIB_DIR [APP_DIR [PAGE]]}")
app="${2:-apps/widget-garden}"
page="${3:-/}"
scriptless="apps/weather"
with_addon="web/tests/fixtures/addon-echo"
with_std_addons="web/tests/fixtures/std-addons"
chrome="${CHROME_BIN:-google-chrome}"
# The chromedriver matching that Chrome: CHROMEDRIVER, else the one a GitHub
# runner image ships under CHROMEWEBDRIVER, else whichever is on PATH.
driver="${CHROMEDRIVER:-${CHROMEWEBDRIVER:+$CHROMEWEBDRIVER/chromedriver}}"
driver="${driver:-chromedriver}"
port=8799

# Build before serving. Backgrounding `cargo run` backgrounds the compile
# with it, and on a cold cache the wait below expires while the compiler is
# still working, which reads as a server that never came up. `--serve` runs
# the lumen-server beside lumenc, so it is built into the same directory.
cargo build -p lumenc -p lumen-server

fail() {
  echo "web page smoke: $1" >&2
  exit 1
}

# Serve one app and leave the page it rendered in $dom and what it logged in
# $log. Every console line but the runtime's own boot report is a failure; a
# warning counts, because the hydration mismatch report is a warning.
open_page() {
  local target="$1"
  local document="${2:-/}"
  out=$(mktemp -d)
  log=$(mktemp)
  dom=$(mktemp)

  cargo run -p lumenc -- web "$target" --out "$out" --lib-dir "$lib_dir" \
    --serve --port "$port" &
  server=$!
  trap 'kill "$server" 2>/dev/null || true' EXIT

  for _ in $(seq 60); do
    curl -sf -o /dev/null "http://127.0.0.1:$port/" && break
    sleep 1
  done
  curl -sf -o /dev/null "http://127.0.0.1:$port/"

  "$chrome" --headless --disable-gpu --no-sandbox --virtual-time-budget=20000 \
    --enable-logging=stderr --log-level=0 --dump-dom \
    "http://127.0.0.1:$port$document" 2>"$log" >"$dom"

  kill "$server" 2>/dev/null || true
  wait "$server" 2>/dev/null || true

  echo "--- console ($target) ---"
  grep -F ':CONSOLE:' "$log" || echo "(nothing)"

  if grep -F ':CONSOLE:' "$log" | grep -qv 'lumen: hydrated'; then
    fail "$target logged something"
  fi
  grep -qF 'lumen: hydrated' "$log" || fail "$target: the runtime never hydrated the page"
  grep -F 'lumen: hydrated' "$log" | grep -qF 'built 0' ||
    fail "$target: the runtime had to build nodes the page should already have had"
}

open_page "$app" "$page"

# The runtime took the page over: this mark is the current tab's, and the
# emitter and the runtime both write it, so it survives a reload either way.
grep -qF 'data-lm-selected' "$dom" || fail "no element carries the current-tab mark"
# A dialog whose signal is false is closed, and `open` is the whole of what
# says so: the browser maintains it and hides the element itself. It is the
# one piece of state that is wrong in both directions when a half misreads
# the other.
grep -qE '<dialog[^>]*>' "$dom" || fail "the page has no dialog"
if grep -qE '<dialog[^>]*open=' "$dom"; then
  fail "the dialog is open"
fi

# An app whose language no host in this build runs still boots. Its systems
# read the same command stream a script would write to, and a reader whose
# message type nothing registered ends the tick as a trap the page reports as
# `unreachable`.
open_page "$scriptless"

# An add-on's module is loaded, checked against the hash the build wrote, and
# handed to the runtime; each thing the fixture calls writes its answer into a
# label of its own, so a label still reading `waiting` names the leg that broke.
# Headless Chrome runs only a few animation frames before it dumps the page,
# which is why the fixture starts every leg before its first tick.
open_page "$with_addon"
expect() {
  grep -qF "$1" "$dom" || fail "$with_addon: $2"
}
expect '>HELLO!<' "a function that answers at once did not answer"
expect '>first: world later<' "an async function's answer never arrived as its event"
expect '>second: on purpose<' "an async function's failure never arrived as its error event"
expect '>install hello<' "an event the module raised never reached the script"
expect '>set by echo<' "a signal the module wrote never reached the page"
expect 'data-echo="mounted"' "the module was never handed its element"
expect 'data-echo-read="HELLO!"' "the module could not read a signal the script wrote"
expect 'data-echo-data-mood="calm"' "an attribute the script set never reached the module"

# The first-party add-ons, found in this checkout's std/addons the way an
# installed toolchain finds its own addons/ directory.
out=$(mktemp -d)
cargo run -p lumenc -- web "$with_std_addons" --out "$out" --lib-dir "$lib_dir" \
  --serve --port "$port" &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT
for _ in $(seq 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$port/" && break
  sleep 1
done
curl -sf -o /dev/null "http://127.0.0.1:$port/"
status=0
CHROME_BIN="$(command -v "$chrome" || echo "$chrome")" CHROMEDRIVER="$driver" \
  python3 .github/scripts/web-addons-smoke.py "http://127.0.0.1:$port/" || status=$?
kill "$server" 2>/dev/null || true
wait "$server" 2>/dev/null || true
[ "$status" -eq 0 ] || fail "$with_std_addons: a first-party add-on did not answer"

echo "web page smoke: every page boots clean, the runtime owns it, and the add-ons answer"
