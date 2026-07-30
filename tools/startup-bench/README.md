# startup-bench - cold-start + peak-RSS regression harness

Tracks Lumen's process-level startup cost (exec -> first frame rendered)
and peak resident set, head-to-head against two Qt6 baselines of matching
node count. Complements the criterion micro-benches in `benches/` (which
measure hot paths, not boot).

Two Qt baselines, because they answer different questions:

* **Qt Widgets** draws native OS-styled controls through QStyle and paints
  almost nothing itself. It sets the floor for a toolkit that leans on the
  platform, not a fair peer for a runtime that renders its own scene.
* **Qt Quick** builds a scene graph and renders it through the RHI (OpenGL
  on the same offscreen llvmpipe path Lumen's headless wgpu/vello uses),
  with its own glyph atlas and batched geometry. This is the like-for-like
  comparison for Lumen.

Everything runs **without a real window**: Lumen uses `--headless`
(offscreen wgpu/vello, no compositor); Qt uses `QT_QPA_PLATFORM=offscreen`.
Both still exercise the real init / GPU-or-raster / font / paint paths.

## What it measures

* **Cold-start wall time** - `measure.c` wraps the child with `fork` +
  `wait4` and reports `CLOCK_MONOTONIC` elapsed across the child's whole
  lifetime. For a `--headless --ticks 1` (Lumen) / show-one-frame-and-quit
  (Qt) child, that is exec -> first-frame-ready + teardown.
* **Peak RSS** - `getrusage(RUSAGE_CHILDREN).ru_maxrss`, the kernel's own
  high-water mark (the value GNU `time -v` prints as *Maximum resident set
  size*). No polling race. Note this is **RSS**, which counts shared
  library code (Lumen maps ~140 MB of shared-clean Mesa/`libLLVM` when a
  GPU adapter is created); PSS is lower. See the root-cause notes.
* **Binary size** - stripped on-disk size. Not apples-to-apples between a
  statically-linked Lumen binary and a Qt app that dynamically links tens
  of MB of `libQt6*` shared objects; reported for reference only.

`run.py` reports the median +/- (min,max) spread over N runs, and separates
run 1 (caches cold: fontconfig, page cache, GPU shader cache) from the
warm runs.

## Run it

```sh
# 1. build the release lumenc (fat-LTO; slow) into an out-of-tree target
export CARGO_TARGET_DIR=/Storage/lumen-targets/bench-startup
export CARGO_BUILD_JOBS=4
cargo build --release -p lumenc

# 2. run the harness (builds measure.c + the Qt baseline on demand)
python3 tools/startup-bench/run.py \
    --lumenc "$CARGO_TARGET_DIR/release/lumenc" \
    --repo "$PWD" \
    --runs 9
```

The Qt baseline is built + measured only when `qmake6` and a C++ compiler
are present; otherwise it is skipped with a note.

## Boot-phase attribution

The runtime carries an opt-in phase trace (a real feature, off by
default - one `env::var_os` read on the cold path). Set `LUMEN_BOOT_TRACE=1`
to print a phase-by-phase breakdown (build/parse/font-scan, GPU bring-up,
shaper warmup, first frame) plus `VmHWM` and thread count to stderr:

```sh
LUMEN_BOOT_TRACE=1 "$CARGO_TARGET_DIR/release/lumenc" \
    run apps/counter --headless --ticks 1
```
