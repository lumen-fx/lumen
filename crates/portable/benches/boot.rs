//! What one Lumen app costs from nothing to rendered state.
//!
//! A renderer that isolates requests from each other builds an app, ticks it
//! until its state stops moving, reads that state and drops the app, once per
//! request. This measures exactly that sequence, phase by phase, because what
//! a slow boot costs is only actionable if it says which phase is slow.
//!
//! It takes compiled apps, not app directories: the compiler is not part of a
//! request, and putting it in would measure the wrong thing.
//!
//! ```sh
//! cargo run -p lumenc --release -- build apps/tracker /tmp/tracker.lmna
//! cargo bench -p lumen-portable --bench boot -- /tmp/tracker.lmna
//! ```
//!
//! `--iterations N` sets the sample count, 300 by default.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumen_core::prelude::App;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_ir::artifact::{self, CompiledApp};
use lumen_portable::{hosts, portable_app};
use lumen_scene::routing::install_routing;
use lumen_scene::spawn::SpawnIntoWorld;

/// How many ticks an app gets to stop moving. An app whose state is still
/// changing after this many is reported rather than waited for: a spinner
/// never converges, and a renderer cannot wait for one.
const SETTLE_BUDGET: u32 = 64;

/// Sample count when the command line names none.
const DEFAULT_ITERATIONS: usize = 300;

/// How many slices of the run the trend line is reported in.
const BUCKETS: usize = 10;

/// Where the script an artifact carries is said to have come from, in a load
/// error. A benchmark has no file to name.
const SCRIPT_URI: &str = "<artifact>";

fn main() -> ExitCode {
    let mut iterations = DEFAULT_ITERATIONS;
    let mut artifacts = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iterations" {
            match args.next().and_then(|n| n.parse().ok()) {
                Some(n) if n > 0 => iterations = n,
                _ => {
                    eprintln!("--iterations wants a positive number");
                    return ExitCode::FAILURE;
                }
            }
        } else if arg.starts_with("--") {
            // `cargo bench` passes the harness its own flags; ignore them
            // rather than refusing to run under it.
            continue;
        } else {
            artifacts.push(arg);
        }
    }
    if artifacts.is_empty() {
        eprintln!(
            "usage: boot [--iterations N] <app.lmna>...\n\
             build one with `cargo run -p lumenc --release -- build <dir> <app.lmna>`"
        );
        return ExitCode::FAILURE;
    }

    for path in &artifacts {
        match measure(Path::new(path), iterations) {
            Ok(report) => report.print(path, iterations),
            Err(reason) => {
                eprintln!("{path}: {reason}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Boot `path` `iterations` times, timing each phase of every boot.
fn measure(path: &Path, iterations: usize) -> Result<Report, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let compiled = artifact::read_bytes(&bytes).map_err(|e| e.to_string())?;

    let mut report = Report::with_capacity(iterations);
    // One boot before the samples: the first of anything pays for lazily
    // initialised process-wide state (the task pool, the external buses) that
    // no later request pays again.
    drop(boot(&compiled, &mut Report::with_capacity(1)));
    for _ in 0..iterations {
        let app = boot(&compiled, &mut report);
        let start = Instant::now();
        drop(app);
        report.drop_app.push(start.elapsed());
    }
    Ok(report)
}

/// One request's worth of app: the sequence a browser's `boot` runs, minus the
/// browser.
fn boot(compiled: &CompiledApp, report: &mut Report) -> App {
    let start = Instant::now();
    let mut app = portable_app();
    report.build.push(start.elapsed());

    // The host goes in before the scene, because `on_start` publishes the
    // signals the markup binds to. Only the bytecode form: a request that
    // compiled its own scripts would be measuring the compiler.
    let start = Instant::now();
    for script in &compiled.scripts {
        if let Some(bytecode) = &script.bytecode {
            hosts::install(&mut app, &script.engine, bytecode, SCRIPT_URI)
                .expect("the artifact names an engine this build has a host for");
        }
    }
    report.host.push(start.elapsed());

    let start = Instant::now();
    if let Some(pages) = &compiled.pages {
        install_routing(&mut app, pages.entry.clone(), pages.keys.clone());
    }
    report.routing.push(start.elapsed());

    let start = Instant::now();
    let root = compiled.ir.spawn_into(&mut app.world);
    report.spawn.push(start.elapsed());
    black_box(root);

    // The first tick is its own phase. It is where bevy resolves the system
    // graph of every stage, which is work an app pays once and a request pays
    // again, so lumping it in with the ticks that follow would hide the one
    // number a cheaper boot would have to attack.
    let start = Instant::now();
    let mut previous = state_of(&app.world);
    app.tick();
    let mut ticks = 1;
    report.first_tick.push(start.elapsed());

    let start = Instant::now();
    while ticks < SETTLE_BUDGET {
        let current = state_of(&app.world);
        if current == previous {
            break;
        }
        previous = current;
        app.tick();
        ticks += 1;
    }
    report.settle.push(start.elapsed());
    report.ticks.push(ticks);

    let start = Instant::now();
    let state = state_of(&app.world);
    report.read.push(start.elapsed());
    report.globals.push(state.globals.len());
    report
        .rows
        .push(state.arrays.values().map(Vec::len).sum::<usize>());
    black_box(state);

    app
}

/// The state a rendered document is produced from: the global signals and the
/// rows of every array.
#[derive(PartialEq)]
struct State {
    globals: Vec<(Arc<str>, Value)>,
    arrays: HashMap<String, Vec<ArrayItem>>,
}

/// A property value that can be compared for equality. [`PropertyValue`] has
/// structural equality but no `PartialEq`, because its escape-hatch variant
/// has none to have.
struct Value(PropertyValue);

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_value(&other.0)
    }
}

/// Read the settled state out of a world.
fn state_of(world: &bevy_ecs::world::World) -> State {
    let mut globals: Vec<(Arc<str>, Value)> = world
        .resource::<PropertyStore>()
        .iter()
        .filter_map(|(key, value)| match key {
            PropertyKey::Global(name) => Some((Arc::clone(name), Value(value.clone()))),
            PropertyKey::Entity(..) => None,
        })
        .collect();
    globals.sort_by(|(a, _), (b, _)| a.cmp(b));
    State {
        globals,
        arrays: world.resource::<ArraySignals>().0.clone(),
    }
}

/// Every sample of every phase, in boot order.
struct Report {
    build: Vec<Duration>,
    host: Vec<Duration>,
    routing: Vec<Duration>,
    spawn: Vec<Duration>,
    first_tick: Vec<Duration>,
    settle: Vec<Duration>,
    read: Vec<Duration>,
    drop_app: Vec<Duration>,
    ticks: Vec<u32>,
    globals: Vec<usize>,
    rows: Vec<usize>,
}

impl Report {
    fn with_capacity(n: usize) -> Self {
        Self {
            build: Vec::with_capacity(n),
            host: Vec::with_capacity(n),
            routing: Vec::with_capacity(n),
            spawn: Vec::with_capacity(n),
            first_tick: Vec::with_capacity(n),
            settle: Vec::with_capacity(n),
            read: Vec::with_capacity(n),
            drop_app: Vec::with_capacity(n),
            ticks: Vec::with_capacity(n),
            globals: Vec::with_capacity(n),
            rows: Vec::with_capacity(n),
        }
    }

    /// Every phase, plus the whole boot, as one table.
    fn print(&self, name: &str, iterations: usize) {
        let phases: [(&str, &Vec<Duration>); 8] = [
            ("app construction", &self.build),
            ("host install", &self.host),
            ("routing", &self.routing),
            ("spawn", &self.spawn),
            ("first tick", &self.first_tick),
            ("settle", &self.settle),
            ("state read", &self.read),
            ("drop", &self.drop_app),
        ];
        println!("\n{name}, {iterations} boots");
        println!("{:<18} {:>12} {:>12}", "phase", "p50", "p99");
        for (label, samples) in phases {
            println!(
                "{:<18} {:>12} {:>12}",
                label,
                millis(percentile(samples, 50)),
                millis(percentile(samples, 99))
            );
        }
        let totals: Vec<Duration> = (0..self.build.len())
            .map(|i| {
                phases
                    .iter()
                    .map(|(_, samples)| samples[i])
                    .sum::<Duration>()
            })
            .collect();
        println!(
            "{:<18} {:>12} {:>12}",
            "whole boot",
            millis(percentile(&totals, 50)),
            millis(percentile(&totals, 99))
        );

        let mut ticks = self.ticks.clone();
        ticks.sort_unstable();
        let most = ticks[ticks.len() - 1];
        println!(
            "settled after {} ticks at p50, {most} at most{}",
            ticks[ticks.len() / 2],
            if most >= SETTLE_BUDGET {
                ", which is the budget: this app never stopped moving"
            } else {
                ""
            }
        );
        // What the boot produced, so a number that came out of an app whose
        // state never arrived is not mistaken for a fast one.
        println!(
            "settled state: {} globals, {} array rows",
            self.globals[self.globals.len() - 1],
            self.rows[self.rows.len() - 1]
        );

        // A boot leaves process-global state behind: interned handles, the
        // binding vector, the external buses. Whether the boot after it pays
        // for that is the question a per-request renderer lives or dies on, so
        // the run is reported in tenths rather than as one number that hides
        // a trend.
        let bucket = totals.len().div_ceil(BUCKETS);
        let trend: Vec<String> = totals
            .chunks(bucket)
            .map(|chunk| millis(percentile(chunk, 50)))
            .collect();
        println!("p50 per tenth of the run: {}", trend.join("  "));
    }
}

/// The `p`th percentile of `samples`, nearest-rank.
fn percentile(samples: &[Duration], p: usize) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// A duration in milliseconds, to three decimals.
fn millis(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1e3)
}
