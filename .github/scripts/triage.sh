#!/usr/bin/env bash
# Triage one pull request from its current state on GitHub: label it by the
# paths it touches and its title, move its S- label to match its checks, and
# publish a "triage" check run whose summary tells the author what is still
# missing. Every write is a function of what the API reports; nothing here
# reads or runs the pull request's code, so it is safe under
# pull_request_target. TRIAGE_DRY_RUN=1 prints the writes instead of making
# them, which is how to try it against a live pull request from a laptop.
#
#   .github/scripts/triage.sh <pull request number>
set -euo pipefail

repo=${GITHUB_REPOSITORY:-lumen-fx/lumen}
pr=${1:?usage: triage.sh <pull request number>}
dry=${TRIAGE_DRY_RUN:-}

# The three S- labels this script owns. Any other S- label on the pull
# request was set by a maintainer and is left alone.
new_label="S-Needs-Triage"
red_label="S-Waiting-On-Author"
green_label="S-Needs-Review"

write() {
  # write <method> <path> [gh api args...]
  if [ -n "$dry" ]; then
    echo "dry-run: $*"
  else
    gh api --method "$@" >/dev/null
  fi
}

pull=$(gh api "repos/$repo/pulls/$pr")
sha=$(jq -r .head.sha <<<"$pull")
draft=$(jq -r .draft <<<"$pull")
title=$(jq -r .title <<<"$pull")
author=$(jq -r .user.login <<<"$pull")
labels=$(jq -r '.labels[].name' <<<"$pull")
files=$(gh api --paginate "repos/$repo/pulls/$pr/files" --jq '.[].filename')

has_label() { grep -qx -- "$1" <<<"$labels"; }

# Area from the paths a file lives under; unmapped paths add nothing and the
# maintainer picks the area by hand.
area_for() {
  case "$1" in
    core/script/*|std/*) echo A-Scripting ;;
    core/text/*) echo A-Text ;;
    core/input/*) echo A-Input ;;
    core/ir/*|web/html/*) echo A-Markup ;;
    core/widget/*|core/widget-macros/*|core/primitives/*) echo A-Widgets ;;
    core/i18n/*) echo A-I18n ;;
    core/assets/*) echo A-Assets ;;
    core/runtime/*|core/launcher/*|src/*) echo A-Runtime ;;
    core/mcp/*|dev/mcp-server/*|capabilities/mcp/*) echo A-MCP ;;
    core/plugin-abi/*|sdk/*|public/lumen-dylib/*) echo A-FFI ;;
    core/*) echo A-Core ;;
    dev/lsp/*) echo A-LSP ;;
    dev/devtools/*|capabilities/devtools/*) echo A-Devtools ;;
    web/*) echo O-Web ;;
    os/clipboard/*) echo A-Clipboard ;;
    # os/mime exists to serve both drag-and-drop (payload type negotiation)
    # and clipboard (paste format negotiation); grouped with drag-and-drop
    # rather than split, since dnd is the more direct consumer.
    os/dnd/*|os/mime/*) echo A-Drag-And-Drop ;;
    os/menu/*) echo A-Menus ;;
    os/tray/*|capabilities/os-tray/*) echo A-Tray ;;
    os/filedialog/*|capabilities/os-filedialog/*) echo A-Dialogs ;;
    os/notify/*|capabilities/os-notify/*) echo A-Notifications ;;
    os/hotkey/*|capabilities/os-hotkey/*) echo A-Hotkeys ;;
    os/launcher/*|capabilities/os-launcher/*) echo A-Launcher ;;
    os/power/*|capabilities/os-power/*) echo A-Power ;;
    os/lifecycle/*|capabilities/os-lifecycle/*) echo A-Lifecycle ;;
    backends/render-wgpu/*|backends/render-headless/*) echo A-Rendering ;;
    backends/window-winit/*) echo A-Windowing ;;
    backends/layout-taffy/*) echo A-Layout ;;
    backends/text-cosmic/*) echo A-Text ;;
    backends/a11y-accesskit/*) echo A-Accessibility ;;
    backends/async-tokio/*|capabilities/async/*) echo A-Async ;;
    # No networking label exists in the repo's A- set, and the HTTP client is
    # a runtime capability either way, so A-Runtime is the least-bad existing
    # fit for both the native backend and its capability shim. Deliberate.
    backends/http-ureq/*|capabilities/http-fetch/*) echo A-Runtime ;;
    public/lumenc/*|public/lumenc-plugin/*) echo A-CLI ;;
    tools/release/*|tools/setup-lumen/*|.github/*) echo A-Packaging ;;
    tools/vscode-lumen/*|tools/jetbrains-lumen/*|tools/zed-lumen/*|tools/tree-sitter-lumen/*) echo A-Editor-Plugin ;;
    apps/*) echo C-Examples ;;
  esac
}

# Class from the conventional-commit prefix of the title.
class_for() {
  case "$1" in
    fix*) echo C-Bug ;;
    feat*) echo C-Feature ;;
    docs*) echo C-Docs ;;
    test*) echo C-Testing ;;
    perf*) echo C-Performance ;;
    ci*|chore*|refactor*) echo C-Code-Quality ;;
    build*|deps*) echo C-Dependencies ;;
  esac
}

add=()
if ! grep -q '^A-' <<<"$labels"; then
  while IFS= read -r f; do area_for "$f"; done <<<"$files" | sort -u | while IFS= read -r l; do
    [ -n "$l" ] && ! has_label "$l" && echo "$l"
  done > /tmp/triage-areas
  while IFS= read -r l; do [ -n "$l" ] && add+=("$l"); done < /tmp/triage-areas
fi
if ! grep -q '^C-' <<<"$labels"; then
  c=$(class_for "$title")
  [ -n "$c" ] && add+=("$c")
fi

# Checks on the head commit, newest run of each name wins.
runs=$(gh api --paginate "repos/$repo/commits/$sha/check-runs" --jq '.check_runs[]' | jq -s 'sort_by(.id) | group_by(.name) | map(last)')
required=$(gh api "repos/$repo/rules/branches/main" --jq '.[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context')

state_of() {
  # state_of <check name> -> ok | failed | pending
  jq -r --arg n "$1" '
    map(select(.name == $n)) | first |
    if . == null or .status != "completed" then "pending"
    elif (.conclusion | IN("success", "skipped", "neutral")) then "ok"
    else "failed" end' <<<"$runs"
}
url_of() { jq -r --arg n "$1" 'map(select(.name == $n)) | first | .html_url // ""' <<<"$runs"; }

failed=0; pending=0; check_lines=""
while IFS= read -r name; do
  [ -z "$name" ] && continue
  s=$(state_of "$name")
  case "$s" in failed) failed=$((failed + 1)) ;; pending) pending=$((pending + 1)) ;; esac
  u=$(url_of "$name")
  if [ -n "$u" ]; then check_lines+="- $s: [$name]($u)"$'\n'; else check_lines+="- $s: $name"$'\n'; fi
done <<<"$required"
cla=$(state_of cla)

# Docs ride with the change: code touched and no doc touched is the one
# thing a reviewer sends back most.
code_touched=$(grep -cE '^(core|backends|os|dev|capabilities|web|public|std|sdk|src|tools)/' <<<"$files" || true)
docs_touched=$(grep -cE '^docs/|\.md$' <<<"$files" || true)

# S- transition, only between the labels this script owns.
current_s=$(grep '^S-' <<<"$labels" || true)
owned_s=$(grep -xE "$new_label|$red_label|$green_label" <<<"$current_s" || true)
target=""
if [ -z "$current_s" ] || [ "$current_s" = "$owned_s" ]; then
  if [ "$failed" -gt 0 ]; then target=$red_label
  elif [ "$pending" -eq 0 ] && [ "$cla" = ok ] && [ "$draft" = false ]; then target=$green_label
  elif [ -z "$current_s" ]; then target=$new_label
  fi
fi
remove=()
if [ -n "$target" ]; then
  has_label "$target" || add+=("$target")
  while IFS= read -r l; do [ -n "$l" ] && [ "$l" != "$target" ] && remove+=("$l"); done <<<"$owned_s"
fi

if [ "${#add[@]}" -gt 0 ]; then
  label_args=()
  for l in "${add[@]}"; do label_args+=(-f "labels[]=$l"); done
  write POST "repos/$repo/issues/$pr/labels" "${label_args[@]}"
fi
for l in "${remove[@]}"; do
  write DELETE "repos/$repo/issues/$pr/labels/$l"
done

# The summary the author reads on the Checks tab.
case "$cla" in
  ok) cla_line="signed" ;;
  failed) cla_line="not signed. Sign it by posting the sentence the CLA check asks for; the text is in CLA.md." ;;
  *) cla_line="pending" ;;
esac
if [ "$failed" -gt 0 ]; then
  next="A required check is red. Fix it and push; the labels follow the checks."
elif [ "$pending" -gt 0 ]; then
  next="Checks are still running. A first pull request waits for a maintainer to start them."
elif [ "$cla" != ok ]; then
  next="Checks are green; the CLA is the last gate before review."
elif [ "$draft" = true ]; then
  next="Checks are green. Mark the pull request ready for review when it is."
else
  next="Checks are green and the CLA is signed. A maintainer reviews from here."
fi
docs_line=""
if [ "$code_touched" -gt 0 ] && [ "$docs_touched" -eq 0 ]; then
  docs_line="Docs: code changed and no page under docs/ did. A change a user can observe updates its page in the same pull request; see CONTRIBUTING.md."
fi
final_labels=$( { printf '%s\n' "$labels"; printf '%s\n' "${add[@]}"; } | grep -v -xF -f <(printf '%s\n' "${remove[@]}" /dev/null) | grep -v '^$' | sort -u | tr '\n' ' ')
summary=$(cat <<SUMMARY
Author: @$author

CLA: $cla_line

Required checks:
$check_lines
Labels: $final_labels

$docs_line

Next: $next

Gates, invariants, and the label scheme are in CONTRIBUTING.md.
SUMMARY
)

write POST "repos/$repo/check-runs" \
  -f name=triage -f head_sha="$sha" -f status=completed -f conclusion=neutral \
  -f "output[title]=$next" -f "output[summary]=$summary"

if [ -n "$dry" ]; then printf '%s\n' "$summary"; fi
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '## PR #%s\n\n%s\n' "$pr" "$summary" >> "$GITHUB_STEP_SUMMARY"; fi
