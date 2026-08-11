//! SDK error surface.

/// Errors returned by [`App::run`](crate::App::run) /
/// [`App::run_headless`](crate::App::run_headless) and the
/// [`simple::AppBuilder::run`](crate::simple::AppBuilder::run) facade.
#[derive(Debug)]
pub enum Error {
    /// Builder mis-configuration caught before the runtime started
    /// (missing markup source, unreadable working directory, ...).
    Setup(String),
    /// Failure from the underlying runtime pipeline: markup parse, CSS
    /// parse/apply, asset load, or the window backend. See
    /// [`lumen_runtime::RunError`] for the variants.
    Run(lumen_runtime::RunError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Setup(msg) => write!(f, "lumen setup: {msg}"),
            Error::Run(e) => write!(f, "lumen: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Setup(_) => None,
            Error::Run(e) => Some(e),
        }
    }
}

impl From<lumen_runtime::RunError> for Error {
    fn from(e: lumen_runtime::RunError) -> Self {
        Error::Run(e)
    }
}

/// Convenience alias used across the SDK; `fn main() -> lumenui::Result<()>`
/// is the idiomatic app signature.
pub type Result<T> = std::result::Result<T, Error>;
