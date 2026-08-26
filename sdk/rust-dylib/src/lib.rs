//! The engine, in a form a Rust program links rather than opens.
//!
//! There is almost no code here. The crate exists for its *shape*: building
//! the engine as a Rust `dylib` produces a shared library carrying Rust
//! metadata, which is what lets a program take the runtime as a dependency
//! and leave it outside its own binary. The C seam does not need that and
//! cannot be built from the same crate target, so the two live apart.
//!
//! Three kinds of program link this crate:
//!
//! - A Rust SDK app, through `lumenui`, which names this crate on the
//!   platforms where it is built.
//! - The shipped `lumenc` binary and the `liblumen` C library, under their
//!   `dynamic-engine` feature. They add the parser/CLI and the C ABI on top
//!   of the engine; the engine itself stays in this one shared library, which
//!   is what lets every process share a single instance with the runtime
//!   modules it dlopens.
//! - A runtime module, through `lumen-module`, which is how the module's
//!   `NEEDED` entry on `liblumen_engine` comes to exist.

/// Everything a Lumen program is written against, re-exported unchanged from
/// the crates that define it. Taking these through the engine (rather than
/// depending on them again) is what keeps an app and the runtime on one copy
/// of every type.
pub mod sdk {
    pub use bevy_ecs;
    pub use lumen_core;
    pub use lumen_runtime;
    pub use lumen_script;
    pub use lumen_widget;
    pub use lumen_widget_macros;
    pub use rhai;
}

// Feature pinning only; see the note in Cargo.toml.
use lumen_script_candela as _;

include!(concat!(env!("OUT_DIR"), "/build_id.rs"));

/// [`BUILD_ID`], readable from outside the Rust type system.
///
/// Exported so the module loader can ask the *running process* two questions
/// with one `dlsym`: whether this library is loaded at all (a process that
/// compiled the engine in has no such dynamic symbol, and dlopening a module
/// there would map a second engine instance), and which exact build it is.
/// Returns a NUL-terminated static; the pointer is valid for the process
/// lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_engine_build_id() -> *const std::os::raw::c_char {
    BUILD_ID_C.as_ptr() as *const std::os::raw::c_char
}
