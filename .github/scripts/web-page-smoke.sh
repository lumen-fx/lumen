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
#   $1  directory holding lumen-web.wasm and lumen-web.js
#   $2  the app to emit (default apps/widget-garden)

set -euo pipefail

lib_dir=$(realpath "${1:?usage: web-page-smoke.sh LIB_DIR [APP_DIR]}")
app="${2:-apps/widget-garden}"
out=$(mktemp -d)
log=$(mktemp)
dom=$(mktemp)
port=8799
chrome="${CHROME_BIN:-google-chrome}"

cargo run -p lumenc -- web "$app" --out "$out" --lib-dir "$lib_dir" \
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

echo "--- console ---"
grep -F ':CONSOLE:' "$log" || echo "(nothing)"

fail() {
  echo "web page smoke: $1" >&2
  exit 1
}

# Anything the page logged that is not the runtime's own boot line is a
# failure. A warning counts: the hydration mismatch report is a warning.
if grep -F ':CONSOLE:' "$log" | grep -qv 'lumen: hydrated'; then
  fail "the page logged something"
fi
grep -qF 'lumen: hydrated' "$log" || fail "the runtime never hydrated the page"
grep -F 'lumen: hydrated' "$log" | grep -qF 'built 0' ||
  fail "the runtime had to build nodes the page should already have had"

# The runtime took the page over: this mark is the current tab's, and the
# emitter and the runtime both write it, so it survives a reload either way.
grep -qF 'data-lm-selected' "$dom" || fail "no element carries the current-tab mark"
# A dialog whose signal is false is closed. It is the one piece of state that
# is wrong in both directions when a half misreads the other.
grep -qE '<dialog[^>]*data-lm-hidden' "$dom" || fail "the dialog is not closed"
if grep -qE '<dialog[^>]*open=' "$dom"; then
  fail "the dialog is open"
fi

echo "web page smoke: the page boots clean and the runtime owns it"
