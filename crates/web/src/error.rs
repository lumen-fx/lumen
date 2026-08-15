//! What can go wrong while emitting a site.

use std::error::Error;
use std::fmt;

/// A site that cannot be emitted as asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitError {
    /// The site has no pages.
    NoPages,
    /// A page has an empty key, so it has no file name.
    EmptyPageKey,
    /// Two pages share a key, so one would overwrite the other.
    DuplicatePage(String),
    /// The entry page is not one of the site's pages.
    UnknownEntry(String),
    /// A page holds a tag with no HTML mapping. Custom widget tags reach the
    /// IR under their own name and have no meaning to the emitter.
    UnknownTag {
        /// Page the tag was found in.
        page: String,
        /// The tag.
        tag: String,
    },
    /// Two nodes of one page claimed the same path. Node paths are what the
    /// browser runtime binds to, so a collision would bind the wrong node.
    DuplicateNodePath {
        /// Page the collision was found in.
        page: String,
        /// The path claimed twice.
        path: String,
    },
    /// The manifest or a seed could not be serialized.
    Serialize(String),
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::NoPages => f.write_str("the site has no pages"),
            EmitError::EmptyPageKey => f.write_str("a page has an empty key"),
            EmitError::DuplicatePage(key) => write!(f, "two pages share the key `{key}`"),
            EmitError::UnknownEntry(key) => write!(f, "entry page `{key}` is not in the site"),
            EmitError::UnknownTag { page, tag } => {
                write!(
                    f,
                    "page `{page}` holds `<{tag}>`, which has no HTML mapping"
                )
            }
            EmitError::DuplicateNodePath { page, path } => {
                write!(f, "page `{page}` gave two nodes the path `{path}`")
            }
            EmitError::Serialize(message) => write!(f, "cannot serialize: {message}"),
        }
    }
}

impl Error for EmitError {}

impl From<serde_json::Error> for EmitError {
    fn from(error: serde_json::Error) -> Self {
        EmitError::Serialize(error.to_string())
    }
}
