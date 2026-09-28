#!/usr/bin/env bash
# Check that a pull request's C-Breaking-Change label and its migration note
# agree, and that each note it adds has the shape the docs site expects.
#
#   .github/scripts/breaking-change.sh <pull request number>
#
# It fails when:
#   - the pull request carries C-Breaking-Change and adds no note under
#     docs/migration/unreleased/;
#   - it adds a note there and does not carry the label;
#   - a note it adds is not a .md file, does not start with a `# ` heading, or
#     holds a byte outside ASCII.
#
# The labels and the list of added files come from GitHub; the notes are read
# from the working tree, so run it in a checkout of the pull request.
# BREAKING_CHANGE_JSON=<file> reads the `gh pr view --json labels,files` output
# from a file instead of asking GitHub, which is how to try a case locally.
# CONTRIBUTING.md says what a note is for and what goes in it.
set -euo pipefail

label="C-Breaking-Change"
dir="docs/migration/unreleased/"

if [ -n "${BREAKING_CHANGE_JSON:-}" ]; then
  json=$(cat "$BREAKING_CHANGE_JSON")
else
  pr=${1:?usage: breaking-change.sh <pull request number>}
  repo=${GITHUB_REPOSITORY:-lumen-fx/lumen}
  # `gh pr view --json files` stops at 100 files, so the file list comes from
  # the paginated REST endpoint and is reshaped into the same form.
  labels=$(gh pr view "$pr" --repo "$repo" --json labels)
  files=$(gh api --paginate "repos/$repo/pulls/$pr/files" \
    --jq '.[] | {path: .filename, changeType: (.status | ascii_upcase)}' | jq -s '{files: .}')
  json=$(jq -s '.[0] * .[1]' <<<"$labels$files")
fi

labelled=$(jq --arg l "$label" '[.labels[].name] | index($l) != null' <<<"$json")
mapfile -t added < <(jq -r --arg d "$dir" '
  .files[]
  | select(.changeType == "ADDED")
  | .path
  | select(startswith($d) and (ltrimstr($d) != ".gitkeep"))
' <<<"$json")

failed=0
fail() {
  failed=1
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo "::error::$*"
  else
    echo "breaking-change.sh: $*" >&2
  fi
}

if [ "$labelled" = true ] && [ ${#added[@]} -eq 0 ]; then
  fail "labelled $label but adds no migration note under $dir; add one (see CONTRIBUTING.md, Breaking changes) or remove the label"
fi
if [ "$labelled" != true ] && [ ${#added[@]} -gt 0 ]; then
  fail "adds a migration note (${added[*]}) but is not labelled $label; add the label, or drop the note if nothing breaks"
fi

for path in "${added[@]}"; do
  case $path in
    *.md) ;;
    *)
      fail "$path: a migration note is a .md file"
      continue
      ;;
  esac
  if [ ! -f "$path" ]; then
    fail "$path is listed as added but is not in this checkout"
    continue
  fi
  first=$(head -n 1 "$path")
  case $first in
    "# "?*) ;;
    *) fail "$path: the first line must be a '# ' heading that states the break" ;;
  esac
  if LC_ALL=C grep -qP '[^\x00-\x7F]' "$path"; then
    line=$(LC_ALL=C grep -nP '[^\x00-\x7F]' "$path" | head -n 1 | cut -d: -f1)
    fail "$path:$line: migration notes are ASCII only"
  fi
done

if [ "$failed" -eq 0 ]; then
  if [ "$labelled" = true ]; then
    echo "breaking change with its migration note: ${added[*]}"
  else
    echo "no breaking change and no migration note"
  fi
fi
exit "$failed"
