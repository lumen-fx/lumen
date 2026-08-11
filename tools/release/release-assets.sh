#!/bin/sh
# Create (or reuse) the Gitea release for a tag and attach files to it.
#
# Usage:
#   GITEA_TOKEN=... [GITEA_SERVER=https://host] [GITEA_REPO=owner/repo] \
#     tools/release-assets.sh <tag> <file> [file...]
#
# In CI the defaults come from the Actions environment:
#   GITEA_TOKEN   falls back to GITHUB_TOKEN (the Actions token)
#   GITEA_SERVER  falls back to GITHUB_SERVER_URL
#   GITEA_REPO    falls back to GITHUB_REPOSITORY
#
# For manual uploads (macOS/Windows binaries) set all three explicitly, e.g.:
#   GITEA_TOKEN=<personal access token, scope write:repository> \
#   GITEA_SERVER=https://git.example.com \
#   GITEA_REPO=lumen-fx/lumen \
#     tools/release-assets.sh v0.4.0 lumenc-macos-aarch64
#
# Safe to re-run: the existing release is reused and assets with the same
# name are replaced. Missing files are skipped with a warning so a partial
# build can still publish what it produced. Requires curl and jq.

set -eu

TAG="${1:?usage: release-assets.sh <tag> <file>...}"
shift
[ "$#" -ge 1 ] || { echo "release-assets.sh: no files given" >&2; exit 1; }

SERVER="${GITEA_SERVER:-${GITHUB_SERVER_URL:?set GITEA_SERVER}}"
REPO="${GITEA_REPO:-${GITHUB_REPOSITORY:?set GITEA_REPO}}"
TOKEN="${GITEA_TOKEN:-${GITHUB_TOKEN:?set GITEA_TOKEN}}"
API="${SERVER%/}/api/v1/repos/$REPO"
AUTH="Authorization: token $TOKEN"

command -v curl >/dev/null 2>&1 || { echo "release-assets.sh: curl not found" >&2; exit 1; }
command -v jq   >/dev/null 2>&1 || { echo "release-assets.sh: jq not found" >&2; exit 1; }

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# --- find or create the release for the tag ---------------------------------
status="$(curl -sS -H "$AUTH" -o "$tmpdir/release.json" -w '%{http_code}' \
  "$API/releases/tags/$TAG")" || status=000

if [ "$status" = "404" ]; then
  prerelease=false
  case "$TAG" in *-*) prerelease=true ;; esac
  printf '{"tag_name":"%s","name":"%s","prerelease":%s}' "$TAG" "$TAG" "$prerelease" \
    > "$tmpdir/create.json"
  curl -sSf -X POST -H "$AUTH" -H 'Content-Type: application/json' \
    --data @"$tmpdir/create.json" "$API/releases" > "$tmpdir/release.json"
  echo "created release $TAG"
elif [ "$status" != "200" ]; then
  echo "release-assets.sh: GET $API/releases/tags/$TAG -> HTTP $status" >&2
  cat "$tmpdir/release.json" >&2 2>/dev/null || true
  exit 1
fi

release_id="$(jq -r '.id' "$tmpdir/release.json")"
case "$release_id" in ''|null) echo "release-assets.sh: no release id in response" >&2; exit 1 ;; esac

# --- upload files, replacing same-named assets -------------------------------
curl -sSf -H "$AUTH" "$API/releases/$release_id/assets" > "$tmpdir/assets.json"

uploaded=0
for f in "$@"; do
  if [ ! -f "$f" ]; then
    echo "release-assets.sh: skipping missing file: $f" >&2
    continue
  fi
  name="$(basename "$f")"
  for old_id in $(jq -r --arg n "$name" '.[] | select(.name == $n) | .id' "$tmpdir/assets.json"); do
    echo "replacing existing asset $name (id $old_id)"
    curl -sSf -X DELETE -H "$AUTH" "$API/releases/$release_id/assets/$old_id" > /dev/null
  done
  echo "uploading $name"
  curl -sSf -X POST -H "$AUTH" -F "attachment=@$f" \
    "$API/releases/$release_id/assets?name=$name" > /dev/null
  uploaded=$((uploaded + 1))
done

[ "$uploaded" -gt 0 ] || { echo "release-assets.sh: nothing uploaded" >&2; exit 1; }
echo "done: $uploaded asset(s) on ${SERVER%/}/$REPO/releases/tag/$TAG"
