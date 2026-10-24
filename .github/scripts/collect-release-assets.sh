#!/usr/bin/env bash
# Prepares a directory of freshly built toolchain artifacts for upload, and
# prints the asset names one per line on stdout.
#
# release.yml and nightly.yml publish the same set of files, so both call this
# rather than each keeping its own copy of the rules. It does three things:
#
#   - Fails when no desktop toolchain archive was built, so a run where every
#     toolchain leg broke publishes nothing, even if the web runtime and the
#     Rust engine sets built. One broken target still lets the others ship.
#   - Writes sha256sums.txt in sha256sum's own format, covering every asset
#     about to be uploaded. install.sh downloads that file first and refuses
#     to install anything whose download does not match the line for it, so a
#     release without it is a release nobody can install from. Generating it
#     here, from the artifacts as they are, is what keeps it in step with a
#     partial run where one build leg failed.
#   - Lists what it found, so the caller can compare the release's assets
#     against this run's output.
#
# The names it writes and prints are bare, with no directory prefix, because
# that is what an asset is called once it is on the release, and
# `sha256sum -c` in the same directory as the downloads then works.
set -euo pipefail

dir="${1:?usage: collect-release-assets.sh <artifact-dir>}"
cd "$dir"

shopt -s nullglob
files=(*.tar.gz *.zip *.msi *.tar.zst lumenui-engine-*.json)
toolchains=(lumen-linux-*.tar.gz lumen-macos-*.tar.gz lumen-windows-*.zip lumen-windows-*.tar.gz)
if [ "${#toolchains[@]}" -eq 0 ]; then
  echo "collect-release-assets.sh: no desktop toolchain archive was built, nothing to publish" >&2
  exit 1
fi

sha256sum "${files[@]}" >sha256sums.txt
cat sha256sums.txt >&2

printf '%s\n' "${files[@]}" sha256sums.txt
