//! Where a `dylib "..."` import looks for its shared library.
//!
//! candela resolves a bare library name beside the file that imports it, which
//! is not where a Lumen app keeps one: the scripts are under `src/` and the
//! native libraries under `lib/`, both at the app root. The hosts name that
//! directory to `candela_vm::set_dylib_dir` for the span of a compile or an
//! artifact load, which is when candela reads it.
//!
//! The setting is per-thread, so it has to be put in place on whichever thread
//! runs the compile, and put back afterwards: a Lumen process can be running
//! more than one app, and one app's `lib/` is not another's.

use std::path::{Path, PathBuf};

/// Names `dir` as the library directory until it is dropped, then restores
/// whatever was named before.
pub(crate) struct LibraryDir(Option<PathBuf>);

impl LibraryDir {
    /// Name `dir`; `None` goes back to searching beside the importing file.
    pub(crate) fn set(dir: Option<&Path>) -> Self {
        Self(candela_vm::set_dylib_dir(dir.map(Path::to_path_buf)))
    }
}

impl Drop for LibraryDir {
    fn drop(&mut self) {
        candela_vm::set_dylib_dir(self.0.take());
    }
}
