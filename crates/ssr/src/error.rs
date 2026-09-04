//! What can go wrong around a render.

use std::error::Error;
use std::fmt;

use lumen_web::EmitError;

/// A render that could not happen as asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsrError {
    /// This process already has a renderer.
    ///
    /// A Lumen app reaches its state through buses that belong to the
    /// process, so two apps ticking at once read each other's writes and one
    /// visitor's data lands in another visitor's page. A process renders one
    /// request at a time, and a site that needs more than that runs more
    /// processes.
    AlreadyRunning,
    /// The renderer has stopped, so it will not answer.
    Stopped,
    /// The entry page names a page the app does not have.
    UnknownEntry {
        /// The key that was asked for.
        asked: String,
        /// The pages the app does have.
        pages: Vec<String>,
    },
    /// A tree handed in for a locale cannot answer for the site's pages.
    LocaleTree {
        /// The locale the tree was for.
        locale: String,
        /// What is wrong with it.
        why: String,
    },
    /// The document could not be written.
    Emit(EmitError),
}

impl fmt::Display for SsrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SsrError::AlreadyRunning => f.write_str(
                "this process already has a renderer, and a Lumen app renders one request at a \
                 time per process",
            ),
            SsrError::Stopped => f.write_str("the renderer has stopped"),
            SsrError::UnknownEntry { asked, pages } => write!(
                f,
                "entry page `{asked}` is not one of the app's pages ({})",
                pages.join(", ")
            ),
            SsrError::LocaleTree { locale, why } => write!(
                f,
                "the tree for `{locale}` cannot answer for this site's pages: {why}"
            ),
            SsrError::Emit(error) => write!(f, "cannot write the document: {error}"),
        }
    }
}

impl Error for SsrError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            SsrError::Emit(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EmitError> for SsrError {
    fn from(error: EmitError) -> Self {
        SsrError::Emit(error)
    }
}
