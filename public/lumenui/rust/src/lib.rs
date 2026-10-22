//! # Lumen Rust SDK
//!
//! Write a [Lumen](https://github.com/lumen-fx/lumen) app's code in Rust. The
//! app is described by its `lumen.toml`, its UI lives in `src/`, and the build
//! compiles the UI into the executable; Rust code drives it with handlers on
//! element ids, signals and the element tree, and with `bevy_ecs` systems
//! where they fit. The [guide](https://docs.lumenfx.dev/reference/ffi/#rust-sdk)
//! shows how to write an app, and the
//! [API reference](https://api.lumenfx.dev/lumenui_engine/) documents every
//! item this crate re-exports.
//!
//! ```ignore
//! // build.rs, with `lumenui-build` under [build-dependencies]
//! fn main() {
//!     lumenui_build::compile();
//! }
//!
//! // src/main.rs
//! use lumenui::prelude::*;
//!
//! fn main() -> lumenui::Result<()> {
//!     lumenui::app!()
//!         .signal("clicks", 0i64)
//!         .on_click("bump", |ui, _| ui.update("clicks", |n: i64| n + 1))
//!         .run()
//! }
//! ```
//!
//! ## How it builds
//!
//! The engine is not compiled with your app. Each Lumen release ships, for
//! every desktop target, the engine's compiled crates; this crate's build
//! script downloads the set for its own version, verifies it against the
//! release's `sha256sums.txt`, keeps it in the user cache directory, and
//! points the compiler at it. Two consequences:
//!
//! - **The compiler is pinned.** Compiled Rust crates are readable only by the
//!   exact `rustc` that produced them, so an app builds with the release's
//!   toolchain. Put a `rust-toolchain.toml` with that channel beside your
//!   `Cargo.toml` (`lumenc new` writes it); the build says which version it
//!   needs when the one in use differs.
//! - **Use the re-exports, never the engine's crates.** `bevy_ecs`, `glam`,
//!   and the `lumen-*` crates are already inside the engine. A direct
//!   dependency on any of them compiles a second copy, whose types are not the
//!   engine's, and nothing typed against it fits. Everything an app needs is
//!   reachable here: `lumenui::prelude` for the everyday items, `lumenui::bevy_ecs`
//!   and the other crate re-exports for the rest.
//!
//! Set `LUMEN_ENGINE_ARTIFACTS` to a directory holding an engine set (one
//! built by `tools/release/lumenui-engine.py` in a Lumen checkout) to use it
//! instead of the release's.

#![forbid(unsafe_code)]

#[cfg(not(lumenui_docs_only))]
extern crate lumenui_engine;

#[cfg(not(lumenui_docs_only))]
pub use lumenui_engine::*;
