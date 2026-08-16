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
# Two apps are opened. The first is the app under test, which has a script the
# browser runs. The second is written in a language no browser host answers
# for: its page has to come up as a page anyway, because an app whose script
# cannot run is still an app a visitor can read.
#
#   $1  directory holding lumen-web.wasm and lumen-web.js
#   $2  the app to emit (default apps/widget-garden)

set -euo pipefail

lib_dir=$(realpath "${1:?usage: web-page-smoke.sh LIB_DIR [APP_DIR]}")
app="${2:-apps/widget-garden}"
scriptless="apps/weather"
chrome="${CHROME_BIN:-google-chrome}"
port=8799

# Build before serving. Backgrounding `cargo run` backgrounds the compile
# with it, and on a cold cache the wait below expires while the compiler is
# still working, which reads as a server that never came up.
cargo build -p lumenc

fail() {
  echo "web page smoke: $1" >&2
  exit 1
}

# Serve one app and leave the page it rendered in $dom and what it logged in
# $log. Every console line but the runtime's own boot report is a failure; a
# warning counts, because the hydration mismatch report is a warning.
open_page() {
  local target="$1"
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
    "http://127.0.0.1:$port/" 2>"$log" >"$dom"

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

open_page "$app"

# The runtime took the page over: this mark is the current tab's, and the
# emitter and the runtime both write it, so it survives a reload either way.
grep -qF 'data-lm-selected' "$dom" || fail "no element carries the current-tab mark"
# A dialog whose signal is false is closed. It is the one piece of state that
# is wrong in both directions when a half misreads the other.
grep -qE '<dialog[^>]*data-lm-hidden' "$dom" || fail "the dialog is not closed"
if grep -qE '<dialog[^>]*open=' "$dom"; then
  fail "the dialog is open"
fi

# An app whose language no host in this build runs still boots. Its systems
# read the same command stream a script would write to, and a reader whose
# message type nothing registered ends the tick as a trap the page reports as
# `unreachable`.
open_page "$scriptless"

echo "web page smoke: both pages boot clean and the runtime owns them"
