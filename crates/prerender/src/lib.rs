//! Runs a Lumen app and reads the state it settles into.
//!
//! A page can be written with state already in it: the app boots, its
//! `on_start` publishes what the markup binds to, and the document is written
//! from that rather than from an empty tree a script fills in later. Doing it
//! at build time is prehydration; doing it per request is a server. Both ask
//! the same question of the same app, so both ask it here.
//!
//! What comes back is a [`lumen_web::State`], which is the pair of forms a
//! document needs: the text the markup is rendered with, and the typed seed
//! the runtime adopts it with.
//!
//! Two things bound a run. The network is answered without leaving the
//! machine, so a page written on one computer is the page written on any
//! other, and every address the app asked for is reported. And a run stops
//! when the app's state stops changing rather than when the frame loop goes
//! quiet: an app with a spinner never stops drawing, and a document does not
//! wait for a spinner.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod deny;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumen_core::app::App;
use lumen_core::property_store::{
    PropertyStore, discard_external_properties, external_properties_pending,
};
use lumen_core::signals::{ArraySignals, discard_external_signals};
use lumen_html::contract::Seed;
use lumen_ir::artifact::CompiledApp;
use lumen_portable::{apply_seed, hosts, portable_app};
use lumen_scene::routing::install_routing;
use lumen_scene::spawn::SpawnIntoWorld;
use lumen_script::FetchRegistry;
use lumen_web::{State, state_of};

pub use deny::DenyDispatch;

/// Where a script an app carries is said to have come from, in a load error.
/// A run has the compiled program and not the file it was written in.
const SCRIPT_URI: &str = "<artifact>";

/// How long an app gets to settle.
///
/// An app whose state is still moving when it runs out is reported rather
/// than waited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The most ticks a run takes.
    pub ticks: u32,
    /// The longest a run takes, whichever comes first.
    pub time: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            ticks: 64,
            time: Duration::from_secs(2),
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The app's state stopped changing after this many ticks.
    At(u32),
    /// The app was still changing when it ran out of budget, at this many
    /// ticks. Whatever it had reached is what the page holds.
    Capped(u32),
}

impl fmt::Display for Settled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Settled::At(ticks) => write!(f, "settled after {ticks} ticks"),
            Settled::Capped(ticks) => {
                write!(f, "still changing after {ticks} ticks, which is the budget")
            }
        }
    }
}

/// What one run of one page produced.
#[derive(Debug, Clone)]
pub struct Prerendered {
    /// The state the page is written with.
    pub state: State,
    /// How the run ended.
    pub settled: Settled,
    /// Every address the app asked for, once each, in the order it asked.
    pub denied: Vec<String>,
    /// Engines the app carries a program for that this build has no host for.
    /// Their part of the state is missing from [`Self::state`].
    pub unsupported_engines: Vec<String>,
}

/// Run `compiled` as the page `key` and read the state it settles into.
///
/// `seed` is the state the app starts from, the values an author declared;
/// what the app writes over them wins, the same way it does in a browser.
/// Routing is installed before the first tick, so an `on_start` that asks
/// which page it is on gets the answer this run is for.
///
/// One run at a time in a process: the external buses are shared, and a run
/// empties them on the way in so it starts from its own state alone.
pub fn page(compiled: &CompiledApp, key: &str, seed: &Seed, budget: Budget) -> Prerendered {
    // The external buses belong to the process, not to an app, so the page
    // before this one can leave writes queued on them. They are this run's
    // to start empty.
    discard_external_properties();
    discard_external_signals();

    let mut app = portable_app();

    // Ahead of the hosts, which take whatever dispatch is already installed
    // and otherwise install one that would go to the network.
    let denied = DenyDispatch::default();
    app.world
        .insert_resource(FetchRegistry::with_dispatch(Arc::new(denied.clone())));

    // An engine with no host here is reported and passed over rather than
    // refused: what it would have published is missing from the page, which
    // is a page written with less state, not a build that cannot happen.
    let mut unsupported_engines = Vec::new();
    for script in &compiled.scripts {
        let Some(bytecode) = &script.bytecode else {
            continue;
        };
        if hosts::install(&mut app, &script.engine, bytecode, SCRIPT_URI).is_err() {
            unsupported_engines.push(script.engine.clone());
        }
    }

    let keys = compiled
        .pages
        .as_ref()
        .map(|pages| pages.keys.clone())
        .unwrap_or_else(|| vec![key.to_string()]);
    install_routing(&mut app, key.to_string(), keys);

    compiled.ir.spawn_into(&mut app.world);
    apply_seed(&mut app.world, seed);

    let (state, settled) = settle(&mut app, budget);
    Prerendered {
        state,
        settled,
        denied: denied.take(),
        unsupported_engines,
    }
}

/// Tick `app` until its state stops changing, and read that state.
///
/// Settled means two ticks in a row produced the same globals and the same
/// rows and nothing is waiting on the external bus. It is not the frame
/// predicate [`lumen_core::tick::work_pending`]: that one answers "does this
/// app want another frame", which an animation answers yes to forever.
///
/// The loop always ends. A write of a value a cell already holds does not
/// dirty it (see `lumen_primitives`' wake path), so an app that has reached
/// its answer keeps producing that answer, and one that has not runs out of
/// [`Budget`].
pub fn settle(app: &mut App, budget: Budget) -> (State, Settled) {
    let deadline = Instant::now() + budget.time;
    let mut previous = state(app);
    app.tick();
    let mut ticks = 1;
    loop {
        let current = state(app);
        if current == previous && !external_properties_pending() {
            return (current, Settled::At(ticks));
        }
        if ticks >= budget.ticks || Instant::now() >= deadline {
            return (current, Settled::Capped(ticks));
        }
        previous = current;
        app.tick();
        ticks += 1;
    }
}

/// The state an app is holding right now, in the form a page is written from.
pub fn state(app: &App) -> State {
    state_of(
        app.world.resource::<PropertyStore>(),
        app.world.resource::<ArraySignals>(),
    )
}
