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
/// The loopback HTTP server behind `lumenc web --serve`. A browser needs a
/// real origin and real content types to load a site; this is that, for one
/// directory on one machine.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod serve;
/// `lumenc web --render ssr --serve` - the server's pages come from a render
/// of the app for the request that asked, through [`lumen_ssr`].
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "web"))]
pub mod ssr;
