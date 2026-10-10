#!/usr/bin/env bash
# Reports, or waits for, the `ci` run on main for one commit.
#
#   tools/release/main-ci.sh check <sha>
#   tools/release/main-ci.sh wait <sha> [timeout-minutes]
#
# A release is cut only on a commit whose `ci` run on main passed, and this is
# the one place that question is asked. It reads the run `ci.yml` started for
# the push of <sha> to main, the latest attempt if it was re-run.
#
# `check` is the early question: is this commit already known to be bad? It
# succeeds while the run is queued or in progress, and when no run exists yet,
# and fails only on a run that finished without passing.
#
# `wait` is the gate: it polls until the run finishes and succeeds only if it
# passed. A run that does not exist yet is waited for too, because a commit
# pushed a moment ago has none until GitHub starts it. The default timeout is
# 120 minutes, comfortably above a full `ci` run.
#
# Needs `gh` with a token that can read the repository's Actions runs, and
# GITHUB_REPOSITORY naming the repository (Actions sets it).
set -euo pipefail

mode=${1:?usage: main-ci.sh check|wait <sha> [timeout-minutes]}
sha=${2:?usage: main-ci.sh check|wait <sha> [timeout-minutes]}
timeout=${3:-120}
repo=${GITHUB_REPOSITORY:?main-ci.sh: GITHUB_REPOSITORY names no repository}
poll=${MAIN_CI_POLL_SECONDS:-30}

case $mode in
  check | wait) ;;
  *)
    echo "main-ci.sh: the mode is check or wait, not $mode" >&2
    exit 2
    ;;
esac

# The newest run for the commit, as "<status> <conclusion> <url>", or nothing.
latest() {
  gh api "repos/$repo/actions/workflows/ci.yml/runs?head_sha=$sha&event=push&branch=main&per_page=20" \
    --jq '.workflow_runs | sort_by(.created_at) | last | select(. != null) |
          "\(.status) \(.conclusion // "none") \(.html_url)"'
}

deadline=$(($(date +%s) + timeout * 60))
errors=0
while :; do
  # A failed request is not an answer. A few in a row, over a wait that can
  # last two hours, are tolerated; more than that stop the run.
  if ! out=$(latest); then
    errors=$((errors + 1))
    if [ "$errors" -ge 5 ]; then
      echo "main-ci.sh: cannot read the ci runs of $repo" >&2
      exit 1
    fi
    sleep "$poll"
    continue
  fi
  errors=0
  status="" conclusion="" url=""
  read -r status conclusion url <<< "$out" || true
  if [ -z "$status" ]; then
    if [ "$mode" = check ]; then
      echo "no ci run on main for $sha yet"
      exit 0
    fi
    echo "no ci run on main for $sha yet; waiting for it to start"
  elif [ "$status" = completed ]; then
    if [ "$conclusion" = success ]; then
      echo "ci passed on main for $sha: $url"
      exit 0
    fi
    echo "main-ci.sh: ci on main for $sha finished as $conclusion: $url" >&2
    if [ "$conclusion" = cancelled ]; then
      echo "A run on main is cancelled when a newer push to main starts its own." >&2
      echo "Start the release again; it will pick up main as it is now." >&2
    else
      echo "Fix main, or re-run the failed jobs of that run, and start the release again." >&2
    fi
    exit 1
  else
    if [ "$mode" = check ]; then
      echo "ci on main for $sha is $status: $url"
      exit 0
    fi
    echo "ci on main for $sha is $status: $url"
  fi

  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "main-ci.sh: ci on main for $sha had not finished after $timeout minutes${url:+: $url}" >&2
    echo "Start the release again once it has." >&2
    exit 1
  fi
  sleep "$poll"
done
