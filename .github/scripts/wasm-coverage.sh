#!/usr/bin/env bash
# Line coverage for the browser suites, written as lcov.
#
# The browser crates only run as wasm32 in a browser, so the host coverage run
# compiles them to nothing. This runs the same suites the `web` job in ci.yml
# runs, instrumented, and converts their counters with the toolchain's own
# llvm-tools, so the report grades the lines those suites reach.
#
# Instrumenting wasm32 needs two unstable compiler flags
# (`-Zno-profiler-runtime` and the `coverage_attribute` feature
# wasm-bindgen-test turns on). `RUSTC_BOOTSTRAP=1` allows them on the pinned
# toolchain, so this run compiles with the same rustc and LLVM as every other
# job rather than a separately pinned nightly. The profiling runtime inside the
# page is the `minicov` crate, and its version has to write the raw profile
# format of that LLVM; Cargo.lock holds it at the release that does.
#
# Needs: cargo-llvm-cov, jq, wasm-bindgen-cli at the workspace's wasm-bindgen
# version, clang with the wasm32 target (minicov compiles C), node, Chrome and a
# matching chromedriver named by CHROMEDRIVER.
#
#   $1  where to write the lcov file (default lcov-wasm.info)

set -euo pipefail

out="${1:-lcov-wasm.info}"
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

export RUSTC_BOOTSTRAP=1
# Cargo adds these to the rustflags .cargo/config.toml sets for wasm32.
# Instrumented code references `__llvm_profile_runtime`, which minicov
# defines; minicov comes in with wasm-bindgen-test, so every artifact that links
# no test harness (the runtime's own cdylib, a unit-test binary with no browser
# tests) would fail to link over it. The allow-list names that one symbol and
# nothing else.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="-Cinstrument-coverage -Zno-profiler-runtime -Clink-args=--no-gc-sections -Clink-arg=--allow-undefined-file=$here/wasm-coverage-undefined.txt --cfg=wasm_bindgen_unstable_test_coverage"
# The stock runner exits before the page has handed its counters over; see
# the comment at the top of the runner for why this one exists.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="node $here/wasm-coverage-runner.mjs"

# Counters left by an earlier run would be merged into this report, and a test
# binary built from older source would be read against the current files, so
# start from none of either.
cargo llvm-cov clean --workspace

# One package per invocation, as the web job in ci.yml runs them.
for package in lumen-web-runtime lumen-web-dom lumen-web-http; do
  cargo llvm-cov test --no-report -p "$package" --target wasm32-unknown-unknown
done

# `report` grades only the packages it is given, and the suites run code from
# across the workspace (the scene layer, the script runtime, the core), so it
# is given every one.
packages=()
while read -r name; do
  packages+=(-p "$name")
done < <(cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name')
cargo llvm-cov report "${packages[@]}" --target wasm32-unknown-unknown --lcov --output-path "$out"
