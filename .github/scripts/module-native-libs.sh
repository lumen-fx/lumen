#!/usr/bin/env bash
# The native libraries each first-party module alone puts on the link line,
# one per line, tab separated:
#
#   <declared name>	<lib>,<lib>
#
# A module contributes more to a link than its own objects: a `-sys` crate in
# its dependency graph asks the linker for a system library, and the recorded
# line carries that request whoever it came from. A replay that leaves the
# module out has to leave those requests out with it, or the executable
# depends on a library it makes no calls into - and refuses to start on a
# machine that does not have it. `lumenc link-kit emit --module-libs` is what
# records the attribution; this works out what to pass.
#
# Two inputs, and neither is a second build:
#
#   $1  The recorded launcher build's `--message-format=json` output. Its
#       `build-script-executed` lines say which package asked for which
#       library, which is the only place that answer exists.
#   $2  The Rust target triple, so the dependency graph is the one that build
#       resolved rather than every platform's.
#
# A library is attributed to a module when the package that asked for it is
# reachable from that module and from nothing else the launcher links. A
# library the rest of the graph also asks for stays unattributed and therefore
# stays on every line, which is the answer that cannot break a link.
#
# Needs jq, which every GitHub runner image has.
set -euo pipefail

messages="${1:?usage: module-native-libs.sh <build-messages.jsonl> <triple>}"
triple="${2:?usage: module-native-libs.sh <build-messages.jsonl> <triple>}"

cd "$(dirname "$0")/../.."

# The module list comes from the tree, the same file every other step reads.
modules="$(.github/scripts/first-party-modules.sh)"
test -n "$modules"

packages="$(while IFS=$'\t' read -r name package _lib; do
  printf '%s\t%s\n' "$name" "$package"
done <<<"$modules")"

# jq opens both of the inputs below by path itself, so neither can be a
# process substitution: on Windows the shell is git bash and jq is a native
# Windows binary, which cannot open the `/proc/<pid>/fd/N` name msys hands it.
# Real files work everywhere, and cygpath - present only under git bash -
# gives jq a path in the shape it can resolve.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

jq -c 'select(.reason == "build-script-executed")' "$messages" >"$work/messages.jsonl"
printf '%s' "$packages" >"$work/packages.tsv"

for_jq="$work"
if command -v cygpath >/dev/null 2>&1; then
  for_jq="$(cygpath -m "$work")"
fi

# The graph the recorded build resolved: `static-run` is the feature that puts
# the modules in it at all.
cargo metadata --format-version 1 --filter-platform "$triple" \
    --features lumen-launcher/static-run |
  jq -r --slurpfile msgs "$for_jq/messages.jsonl" \
        --rawfile names "$for_jq/packages.tsv" '
    # package id -> the ids it depends on, every kind of dependency followed.
    # More edges can only pull a package into the shared base and out of a
    # module|s exclusive set, which loses an attribution rather than inventing
    # one.
    (reduce .resolve.nodes[] as $n ({}; .[$n.id] = [$n.deps[].pkg])) as $graph
    | (reduce .packages[] as $p ({}; .[$p.name] = $p.id)) as $id
    | ($names | split("\n") | map(select(length > 0) | split("\t"))) as $modules
    | ($modules | map($id[.[1]]) | map(select(. != null))) as $module_ids
    | ($id["lumen-launcher"]) as $launcher

    | def reach($roots):
        { seen: {}, next: $roots }
        | until(.next | length == 0;
            .seen as $seen
            | (.next | map(select($seen[.] | not)) | unique) as $new
            | { seen: (reduce $new[] as $p ($seen; .[$p] = true)),
                next: ([$new[] | $graph[.] // []] | add // []) })
        | .seen;

    # Everything the launcher reaches without going through a module.
    ((($graph[$launcher] // []) - $module_ids) | reach(.)) as $base

    # A build script writes the library kind into the request
    # (`dylib=asound`), and the link line carries the name alone.
    | (reduce ($msgs[] | select((.linked_libs // []) | length > 0)) as $m
        ({}; .[$m.package_id] =
          ((.[$m.package_id] // []) + ($m.linked_libs | map(split("=") | last))))) as $asked

    | $modules[]
    | . as [$name, $package]
    | ($id[$package]) as $root
    | select($root != null)
    | (reach([$root]) | keys | map(select($base[.] | not))) as $only
    | ([$only[] | $asked[.] // []] | add // [] | unique) as $libs
    | select($libs | length > 0)
    | [$name, ($libs | join(","))]
    | @tsv
  '
