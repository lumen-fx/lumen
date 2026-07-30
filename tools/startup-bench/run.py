#!/usr/bin/env python3
"""Startup-time + peak-RSS regression harness for Lumen vs Qt Widgets.

Measures, for each target, the wall time from process exec to first
frame rendered (Lumen: `--headless --ticks 1`; Qt: show + one event-loop
turn + quit) and the kernel peak resident set (getrusage ru_maxrss via
the `measure` wrapper - the value GNU `time -v` reports, no polling
race). Reports median +/- spread over N runs, separating the first
(cold-ish) run from the warm runs, plus stripped binary size.

Never opens a real window: Lumen runs headless, Qt runs under
QT_QPA_PLATFORM=offscreen. Both still exercise real init / GPU-or-raster
/ font / paint paths.

Usage:
    python3 run.py --lumenc /path/to/lumenc [--runs 9] [--repo /path]

The Qt baseline is built + measured only when qmake6 + a C++ compiler
are present; otherwise it is skipped with a note.
"""
import argparse
import os
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def build_measure() -> Path:
    out = HERE / "measure"
    src = HERE / "measure.c"
    if not out.exists() or out.stat().st_mtime < src.stat().st_mtime:
        cc = shutil.which("cc") or shutil.which("gcc")
        if not cc:
            sys.exit("no C compiler (cc/gcc) to build measure.c")
        sh([cc, "-O2", "-o", str(out), str(src)])
    return out


def _build_qt_project(subdir: str, proj_file: str, target: str) -> Path | None:
    qmake = shutil.which("qmake6") or shutil.which("qmake")
    if not qmake or not (shutil.which("g++") or shutil.which("c++")):
        print(f"note: qmake6 / C++ compiler not found - skipping {subdir}")
        return None
    proj = HERE / subdir
    build = proj / "build"
    build.mkdir(exist_ok=True)
    try:
        sh([qmake, str(proj / proj_file)], cwd=build)
        sh(["make", "-j4", "-s"], cwd=build)
    except subprocess.CalledProcessError:
        print(f"note: {subdir} failed to build (module missing?) - skipping")
        return None
    binp = build / target
    return binp if binp.exists() else None


def build_qt_widgets() -> Path | None:
    return _build_qt_project("qt-baseline", "qt-baseline.pro", "qt-baseline")


def build_qt_quick() -> Path | None:
    # The fair peer: Qt Quick composites its own scene graph on the GPU,
    # like Lumen. Needs the Qt Quick module; skipped with a note if absent.
    return _build_qt_project(
        "qtquick-baseline", "qtquick-baseline.pro", "qtquick-baseline")


def measure_one(measure: Path, argv, env, runs: int):
    """Return (cold_ms, cold_kb, warm_ms_list, warm_kb_list)."""
    ms_all, kb_all = [], []
    for _ in range(runs):
        p = subprocess.run([str(measure), *argv], env=env,
                           capture_output=True, text=True)
        # measure prints its line to stdout after the child's own output.
        line = [l for l in p.stdout.splitlines()
                if l.startswith("ELAPSED_MS=")]
        if not line:
            sys.exit(f"measure produced no result for {argv}\n"
                     f"stdout:\n{p.stdout}\nstderr:\n{p.stderr}")
        fields = dict(tok.split("=") for tok in line[-1].split())
        if int(fields["EXIT"]) != 0:
            sys.exit(f"child {argv} exited {fields['EXIT']}\n"
                     f"stderr:\n{p.stderr}")
        ms_all.append(float(fields["ELAPSED_MS"]))
        kb_all.append(int(fields["MAXRSS_KB"]))
    return ms_all[0], kb_all[0], ms_all[1:], kb_all[1:]


def fmt_stats(vals):
    if not vals:
        return "n/a"
    med = statistics.median(vals)
    return f"{med:.1f} (min {min(vals):.1f}, max {max(vals):.1f})"


def report(name, cold_ms, cold_kb, warm_ms, warm_kb, binsize_kb):
    print(f"\n== {name} ==")
    print(f"  binary size          : {binsize_kb/1024:.2f} MB "
          f"({binsize_kb} KB)")
    print(f"  cold-ish run 1  time : {cold_ms:.1f} ms   RSS {cold_kb/1024:.1f} MB")
    if warm_ms:
        print(f"  warm  time (ms)      : {fmt_stats(warm_ms)}")
        print(f"  warm  peak RSS (MB)  : "
              f"{statistics.median(warm_kb)/1024:.1f} "
              f"(min {min(warm_kb)/1024:.1f}, max {max(warm_kb)/1024:.1f})")
    return {
        "name": name,
        "cold_ms": cold_ms,
        "cold_rss_mb": cold_kb / 1024,
        "warm_ms_median": statistics.median(warm_ms) if warm_ms else cold_ms,
        "warm_rss_mb_median": (statistics.median(warm_kb) / 1024
                               if warm_kb else cold_kb / 1024),
        "binary_mb": binsize_kb / 1024,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lumenc", required=True, help="path to release lumenc")
    ap.add_argument("--runs", type=int, default=9,
                    help="runs per target (>=5 recommended); run 1 = cold")
    ap.add_argument("--repo", default=str(HERE.parents[2]),
                    help="repo root holding apps/")
    args = ap.parse_args()

    lumenc = Path(args.lumenc).resolve()
    if not lumenc.exists():
        sys.exit(f"lumenc not found: {lumenc}")
    repo = Path(args.repo).resolve()

    measure = build_measure()
    qt_widgets_bin = build_qt_widgets()
    qt_quick_bin = build_qt_quick()

    env = dict(os.environ)
    # Keep the MCP server from binding a port during the bench.
    env.setdefault("RUST_LOG", "error")

    rows = []

    for app in ("blank-no-css", "counter"):
        appdir = repo / "apps" / app
        if not appdir.exists():
            print(f"note: {appdir} missing - skipping")
            continue
        argv = [str(lumenc), "run", str(appdir), "--headless", "--ticks", "1"]
        cold_ms, cold_kb, wm, wk = measure_one(measure, argv, env, args.runs)
        rows.append(report(f"lumen:{app}", cold_ms, cold_kb, wm, wk,
                           lumenc.stat().st_size // 1024))

    qenv = dict(env)
    qenv["QT_QPA_PLATFORM"] = "offscreen"
    for label, qt_bin in (("qt:widgets", qt_widgets_bin),
                          ("qt:quick", qt_quick_bin)):
        if not qt_bin:
            continue
        cold_ms, cold_kb, wm, wk = measure_one(
            measure, [str(qt_bin)], qenv, args.runs)
        rows.append(report(label, cold_ms, cold_kb, wm, wk,
                           qt_bin.stat().st_size // 1024))

    # Clean summary table.
    print("\n\n=== SUMMARY (warm median) ===")
    print(f"{'target':<20} {'cold ms':>9} {'warm ms':>9} "
          f"{'RSS MB':>8} {'bin MB':>8}")
    for r in rows:
        print(f"{r['name']:<20} {r['cold_ms']:>9.1f} "
              f"{r['warm_ms_median']:>9.1f} "
              f"{r['warm_rss_mb_median']:>8.1f} {r['binary_mb']:>8.2f}")


if __name__ == "__main__":
    main()
