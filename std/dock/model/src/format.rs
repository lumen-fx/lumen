//! The saved layout format: a workspace as JSON, with the format version a
//! reader checks before it trusts the rest.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Workspace;

/// The layout format this crate writes and reads. A layout of another
/// version is refused, never guessed at.
pub const FORMAT_VERSION: u32 = 2;

/// Why a saved layout was not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    /// The text is not a layout: not JSON, or no `lumen-dock` version.
    NotALayout(String),
    /// A layout of a format version this build does not read.
    Format(u32),
    /// The app's layout version changed since the layout was saved.
    LayoutVersion {
        /// The version the layout was saved with.
        saved: u32,
        /// The app's version now.
        current: u32,
    },
    /// The layout breaks the model's rules (a panel twice, an empty window).
    Invalid(String),
}

impl RestoreError {
    /// The short reason the `layouterror` event carries: `format`,
    /// `version`, `invalid` or `parse`.
    pub fn reason(&self) -> &'static str {
        match self {
            RestoreError::NotALayout(_) => "parse",
            RestoreError::Format(_) => "format",
            RestoreError::LayoutVersion { .. } => "version",
            RestoreError::Invalid(_) => "invalid",
        }
    }
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreError::NotALayout(e) => write!(f, "not a dock layout: {e}"),
            RestoreError::Format(v) => write!(
                f,
                "a layout in format {v}, and this build reads format {FORMAT_VERSION}"
            ),
            RestoreError::LayoutVersion { saved, current } => write!(
                f,
                "saved for layout version {saved}, and the app is at {current}"
            ),
            RestoreError::Invalid(e) => write!(f, "the layout is not valid: {e}"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// A workspace with its format version, as written.
#[derive(Serialize)]
struct Versioned<'a> {
    #[serde(rename = "lumen-dock")]
    format: u32,
    #[serde(flatten)]
    workspace: &'a Workspace,
}

/// What a dock's auto-saved file holds: the live workspace, and the user's
/// own named layouts beside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutFile {
    /// The live workspace.
    #[serde(flatten)]
    pub live: Workspace,
    /// The user's named layouts, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, Workspace>,
}

impl LayoutFile {
    /// The file's text.
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Out<'a> {
            #[serde(rename = "lumen-dock")]
            format: u32,
            #[serde(flatten)]
            file: &'a LayoutFile,
        }
        serde_json::to_string_pretty(&Out {
            format: FORMAT_VERSION,
            file: self,
        })
        .expect("a layout serializes")
    }

    /// Read a file written by [`Self::to_json`] for an app at layout
    /// version `layout_version`. A live workspace saved under another layout
    /// version is dropped, and so is each named layout that was; what is
    /// left is checked like [`Workspace::from_json`] checks.
    ///
    /// # Errors
    ///
    /// The text is not a layout, or it is in another format version.
    pub fn from_json(
        text: &str,
        layout_version: u32,
    ) -> Result<(Option<Workspace>, BTreeMap<String, Workspace>), RestoreError> {
        let value = versioned(text)?;
        let file: LayoutFile =
            serde_json::from_value(value).map_err(|e| RestoreError::NotALayout(e.to_string()))?;
        let live =
            Some(file.live).filter(|w| w.layout_version == layout_version && w.check().is_ok());
        let layouts = file
            .layouts
            .into_iter()
            .filter(|(_, w)| w.layout_version == layout_version && w.check().is_ok())
            .collect();
        Ok((live, layouts))
    }
}

/// Parse `text` as JSON and check its format version.
fn versioned(text: &str) -> Result<serde_json::Value, RestoreError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| RestoreError::NotALayout(e.to_string()))?;
    let format = value
        .get("lumen-dock")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| RestoreError::NotALayout("no \"lumen-dock\" format version".into()))?;
    if format != u64::from(FORMAT_VERSION) {
        return Err(RestoreError::Format(
            u32::try_from(format).unwrap_or(u32::MAX),
        ));
    }
    Ok(value)
}

impl Workspace {
    /// The workspace as JSON, with the format version.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&Versioned {
            format: FORMAT_VERSION,
            workspace: self,
        })
        .expect("a layout serializes")
    }

    /// Read a workspace from [`Self::to_json`]'s text, for an app at layout
    /// version `layout_version`.
    ///
    /// # Errors
    ///
    /// The text is not a layout, it is in another format version, it was
    /// saved under another layout version, or it breaks the model's rules.
    pub fn from_json(text: &str, layout_version: u32) -> Result<Workspace, RestoreError> {
        let value = versioned(text)?;
        let workspace: Workspace =
            serde_json::from_value(value).map_err(|e| RestoreError::NotALayout(e.to_string()))?;
        if workspace.layout_version != layout_version {
            return Err(RestoreError::LayoutVersion {
                saved: workspace.layout_version,
                current: layout_version,
            });
        }
        workspace.check().map_err(RestoreError::Invalid)?;
        Ok(workspace)
    }
}
