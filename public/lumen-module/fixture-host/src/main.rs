//! Boots `<app-dir>` headless for `<ticks>` ticks the way a dylib-linked Rust
//! app would, then prints what the module loader recorded. Each tick runs
//! under `catch_unwind` so a panicking module system proves the app stays
//! alive rather than killing the run.

#[cfg(windows)]
fn main() {
    eprintln!("the module fixture host has no Windows form (no engine dylib exists there)");
    std::process::exit(2);
}

#[cfg(not(windows))]
fn main() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::PathBuf;

    use lumen_engine::sdk::lumen_core::property_store::PropertyStore;
    use lumen_engine::sdk::lumen_runtime;

    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: lumen-module-fixture-host <app-dir> [ticks]");
    let ticks: u32 = args
        .next()
        .as_deref()
        .unwrap_or("5")
        .parse()
        .expect("ticks must be a number");

    let mut opts = lumen_runtime::RunOptions::new(PathBuf::from(&dir));
    opts.hot_reload = false;
    opts.bounded = true;
    // Through `lumenc`'s wrapper, the way `lumenc run` builds an app: it
    // injects the default parser and the compiler-side resolutions
    // (compiler-plugin chain, `version`-source module paths).
    let (mut app, _window) = lumenc::build_headless_app(opts).expect("the app builds headless");

    for _ in 0..ticks {
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| app.tick())) {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            println!("HOST tick-panic-caught: {msg}");
        }
        // Give off-thread work (the asset server's decode pool, mainly) wall
        // time between ticks, the way a windowed loop would.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // The signal values a test asked to see, read from the shared store the
    // way a bound label would.
    if let Ok(names) = std::env::var("LUMEN_FIXTURE_SIGNALS") {
        let store = app.world.resource::<PropertyStore>();
        for name in names.split(',').filter(|n| !n.is_empty()) {
            match store.get_global_str(name) {
                Some(value) => println!("HOST signal {name}={value}"),
                None => println!("HOST signal {name}=<unset>"),
            }
        }
    }

    let modules = app
        .world
        .resource::<lumen_runtime::modules::LoadedModules>();
    for m in &modules.loaded {
        println!(
            "HOST loaded name={} kind={:?} build_id={}",
            m.name, m.kind, m.build_id
        );
    }
    for f in &modules.failed {
        println!(
            "HOST failed name={} reason={}",
            f.name,
            f.reason.replace('\n', " / ")
        );
    }
    println!("HOST done");
}
