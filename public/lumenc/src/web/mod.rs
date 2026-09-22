//! `lumenc web` - emit an app as a static site, serve it locally, and render
//! it per-request over SSR.

/// `lumenc web` - emit an app as a static site. Compiles the app the way
/// `build` does, so it needs the same parser (`runtime-parse`) and runtime
/// (`dev-run`), plus the emitter behind the default-on `web` feature.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod cli;
/// Filling a component that has to run while the site is built, so its body is
/// in the page a crawler reads. Needs what `cli` needs.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod component_fill;
