//! The engine, in a form a Rust program links rather than opens.
//!
//! There is no code here. The crate exists for its *shape*: building the
//! engine as a Rust `dylib` produces a shared library carrying Rust metadata,
//! which is what lets an app take the runtime as a dependency and leave it
//! outside its own binary. The C seam does not need that and cannot be built
//! from the same crate target, so the two live apart.
//!
//! The Rust SDK names this crate rather than `lumen` on the platforms where it
//! is built, and that naming is what puts the shared library in the app's link
//! graph. Everything reachable here is the engine's own surface, re-exported
//! unchanged.

pub use lumen::sdk;
