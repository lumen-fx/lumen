//! Where a `dylib "..."` import looks for its shared library.
//!
//! candela resolves a bare library name beside the file that imports it, which
//! is not where a Lumen app keeps one: the scripts are under `src/` and the
//! native libraries under `lib/`, both at the app root. The hosts name those
//! directories to `candela_vm::set_dylib_dirs` for the span of a compile or an
//! artifact load, which is when candela reads them.
//!
//! A registry package brings its own, so the setting is a list: the app's
//! `lib/` first, then one directory per package that carries native libraries
//! of its own.
//!
//! The setting is per-thread, so it has to be put in place on whichever thread
//! runs the compile, and put back afterwards: a Lumen process can be running
//! more than one app, and one app's `lib/` is not another's.

use std::path::PathBuf;

/// Names `dirs` as the library search path until it is dropped, then restores
/// whatever was named before.
pub(crate) struct LibraryDir(Vec<PathBuf>);

impl LibraryDir {
    /// Name `dirs`; an empty list goes back to searching beside the importing
    /// file.
    pub(crate) fn set(dirs: Vec<PathBuf>) -> Self {
        Self(candela_vm::set_dylib_dirs(dirs))
    }
}

impl Drop for LibraryDir {
    fn drop(&mut self) {
        candela_vm::set_dylib_dirs(std::mem::take(&mut self.0));
    }
}
