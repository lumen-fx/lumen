//! AOT compiled-app artifact - the on-disk product of `lumenc build`.
//!
//! Today `lumenc run` parses `main.lmn` + `main.css` from source on every
//! launch. The AOT model resolves that work **once** at build time and bakes
//! the result - a fully cascaded [`LayoutIR`] plus the combined script
//! source - into a compact binary blob the runtime deserializes directly,
//! with the markup parser removed from the shipped binary (see the
//! `runtime-parse` cargo feature).
//!
//! This mirrors how other toolkits ship compiled UI:
//!
//! - **Qt** precompiles `.qml` into bytecode via `qmlcachegen` / the QML disk
//!   cache and bakes it into the binary, so the QML *parser* need not run (or
//!   even ship) at launch. This module is the direct analogue: the artifact
//!   is Lumen's "QML cache".
//! - **Slint** compiles `.slint` to Rust at build time. We keep Lumen's model
//!   data-driven rather than codegen (an artifact, not generated Rust) so the
//!   same runtime consumes it - but the "resolve markup once at build time"
//!   principle is identical.
//!
//! Where these conflict with Lumen philosophy, philosophy wins: the artifact
//! carries the *design-token-reachable* cascaded stylesheet
//! ([`LayoutIR::combined_stylesheet`]) verbatim, so colors/metrics stay
//! CSS-reachable at runtime (the `<for>` reconciler re-applies it) - the AOT
//! step resolves the cascade, it does not freeze visuals into hardcoded
//! values.
//!
//! ## Container layout
//!
//! ```text
//! magic  : 4 bytes  b"LMNA"   (Lumen Markup Native Artifact)
//! version: 2 bytes  u16 LE    (FORMAT_VERSION)
//! body   : bincode(CompiledApp)
//! ```
//!
//! bincode is used over RON/JSON because the artifact is shipped/embedded and
//! binary size is the priority: bincode stores no field names or schema, so a
//! cascaded stylesheet with hundreds of declarations costs a fraction of its
//! textual form. The magic + version prefix lets the loader reject a stale or
//! foreign file cleanly instead of failing deep inside the decoder.

use crate::layout_ir::LayoutIR;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Upper bound on the decoded bincode body, in bytes. The trust model is
/// "built by our own `lumenc`", so this is cheap hardening rather than a
/// security boundary: it stops a truncated/corrupt length prefix from making
/// the decoder pre-allocate an absurd buffer. 512 MiB is far above any real
/// compiled app.
const MAX_ARTIFACT_BODY: u64 = 512 * 1024 * 1024;

/// Container magic - "Lumen Markup Native Artifact".
pub const MAGIC: [u8; 4] = *b"LMNA";

/// Container format version. Bump on any incompatible change to
/// [`CompiledApp`] or the framing. The loader refuses mismatches so a
/// runtime never mis-decodes an artifact built by a different toolchain.
///
/// `2`: the skin-tokens CSS batch adds a run of new fields to
/// [`crate::layout_ir::Attributes`] (`knob-inset`, `thumb-size`,
/// `popup-gap`, `progress-chunk`, `disabled-opacity`, `caret-width`,
/// `caret-blink`, `password-character`, `line-height`, and the
/// `scrollbar-*` geometry/timing/paint properties), changing the
/// bincode wire shape.
///
/// `3`: the `translatable="<key>"` attribute adds a field to
/// [`crate::layout_ir::Attributes`].
///
/// `4`: [`CompiledApp::scripts`] records which engine runs each part of the
/// app's script, so an app that ships more than one language keeps running
/// under one host per language after compilation, and
/// [`CompiledApp::pages`] carries a multi-page app's page set so navigation
/// works without the page files on disk.
pub const FORMAT_VERSION: u16 = 4;

/// The navigable page set of a compiled multi-page app.
///
/// The tree in [`CompiledApp::ir`] already holds every page, each behind the
/// gate that mounts it while it is the active one, exactly as a from-source
/// load assembles it. What a compiled app still needs is the routing data:
/// which page names exist, and which one is home. The run path reads that from
/// here rather than by looking for `.lmn` files, which a shipped app does not
/// carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledPages {
    /// Home page key: the page that mounts before any navigation happens, and
    /// the fallback for a path matching no page.
    pub entry: String,
    /// Every navigable page key, longest first, which is the order path
    /// resolution walks them in.
    pub keys: Vec<String>,
}

/// One engine's whole program, as baked at build time.
///
/// A script file picks its engine from its own extension, so an app that
/// mixes languages compiles to one entry per engine. The engine is stored by
/// name (`candela`, `lua`, `rhai`) rather than as a typed enum because the
/// enum lives in the runtime's config layer, which this crate sits below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledScript {
    /// Engine name: `candela`, `lua`, or `rhai`. A name the loading runtime
    /// does not recognise falls back to the default engine.
    pub engine: String,
    /// Every source file that engine owns, concatenated in source order.
    pub source: String,
}

/// The precompiled application: everything the runtime needs to spawn the UI
/// without touching the markup / CSS parser.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompiledApp {
    /// Fully-resolved layout IR: includes cascaded inline attributes plus the
    /// combined (skin + user) stylesheet on
    /// [`LayoutIR::combined_stylesheet`], with `<include>` / `@import`
    /// directives spliced away. A relative `<image src>` resolves against the
    /// directory the artifact is run with; an absolute one is used as written.
    pub ir: LayoutIR,
    /// Combined script source (inline `<script>` body + every external
    /// `<script src="...">` file, concatenated in source order). Baked at build
    /// time so the parser-free runtime never reads `.rhai` files from disk to
    /// reconstruct the script host input. Empty when the app ships no script.
    ///
    /// This is the flattened form, which is what `[script] engine` runs when
    /// an app puts every language on one host. The per-engine split in
    /// [`Self::scripts`] is what a multi-language app runs.
    pub script_source: String,
    /// The same program split by the engine that runs each part, in the
    /// runtime's fixed host order. Empty when the app ships no script.
    pub scripts: Vec<CompiledScript>,
    /// The page set, for an app with more than one page. `None` for a
    /// single-page app, which needs no routing at all.
    pub pages: Option<CompiledPages>,
}

/// Errors from (de)serializing or reading/writing an artifact.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// I/O failure reading or writing the artifact file.
    #[error("artifact io {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
    /// The file is too short to contain the container header.
    #[error("artifact too small ({0} bytes) - not a Lumen artifact")]
    TooSmall(usize),
    /// The 4-byte magic did not match [`MAGIC`].
    #[error("bad artifact magic - not a Lumen artifact")]
    BadMagic,
    /// The container version is not one this runtime understands.
    #[error("unsupported artifact version {found} (this build reads {expected})")]
    Version {
        /// Version read from the file.
        found: u16,
        /// Version this build supports ([`FORMAT_VERSION`]).
        expected: u16,
    },
    /// The bincode body failed to decode.
    #[error("artifact decode: {0}")]
    Decode(String),
    /// The [`CompiledApp`] failed to encode.
    #[error("artifact encode: {0}")]
    Encode(String),
}

/// Serialize a [`CompiledApp`] into the framed container bytes.
pub fn serialize(app: &CompiledApp) -> Result<Vec<u8>, ArtifactError> {
    let body = bincode::serialize(app).map_err(|e| ArtifactError::Encode(e.to_string()))?;
    let mut out = Vec::with_capacity(body.len() + MAGIC.len() + 2);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Deserialize framed container bytes back into a [`CompiledApp`], validating
/// the magic and version first.
pub fn deserialize(bytes: &[u8]) -> Result<CompiledApp, ArtifactError> {
    const HEADER: usize = 4 + 2;
    if bytes.len() < HEADER {
        return Err(ArtifactError::TooSmall(bytes.len()));
    }
    if bytes[0..4] != MAGIC {
        return Err(ArtifactError::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != FORMAT_VERSION {
        return Err(ArtifactError::Version {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    // Match the wire format of `bincode::serialize` (fixint, little-endian,
    // reject-trailing) exactly - `DefaultOptions` alone defaults to *varint*,
    // which would mis-decode - while adding a size limit so a bad length
    // prefix can't trigger an enormous pre-allocation.
    use bincode::Options;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_ARTIFACT_BODY)
        .deserialize(&bytes[HEADER..])
        .map_err(|e| ArtifactError::Decode(e.to_string()))
}

/// Write an artifact to `path`.
pub fn write(path: &Path, app: &CompiledApp) -> Result<(), ArtifactError> {
    let bytes = serialize(app)?;
    std::fs::write(path, bytes).map_err(|e| ArtifactError::Io(path.to_path_buf(), e))
}

/// Read and decode an artifact from `path`.
pub fn read(path: &Path) -> Result<CompiledApp, ArtifactError> {
    let bytes = std::fs::read(path).map_err(|e| ArtifactError::Io(path.to_path_buf(), e))?;
    deserialize(&bytes)
}

/// Decode an artifact from an in-memory byte slice: the byte-slice
/// counterpart of [`read`]. Used by the link-not-embed launcher path, where
/// the compiler produces LMNA bytes in-process and hands them across the
/// C-ABI (`lumen_app_new_from_lmna`) instead of writing a file. Thin alias
/// over [`deserialize`]; validates magic + version identically.
pub fn read_bytes(bytes: &[u8]) -> Result<CompiledApp, ArtifactError> {
    deserialize(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CompiledApp {
        CompiledApp {
            ir: LayoutIR::default(),
            script_source: "let x = 1;".to_string(),
            scripts: vec![CompiledScript {
                engine: "rhai".to_string(),
                source: "let x = 1;".to_string(),
            }],
            pages: Some(CompiledPages {
                entry: "index".to_string(),
                keys: vec!["settings".to_string(), "index".to_string()],
            }),
        }
    }

    #[test]
    fn round_trip_bytes() {
        let app = sample();
        let bytes = serialize(&app).expect("serialize");
        assert_eq!(&bytes[0..4], &MAGIC);
        let back = deserialize(&bytes).expect("deserialize");
        assert_eq!(back.script_source, app.script_source);
        assert_eq!(back.scripts.len(), 1);
        assert_eq!(back.scripts[0].engine, "rhai");
        let pages = back.pages.expect("the page set round-trips");
        assert_eq!(pages.entry, "index");
        assert_eq!(pages.keys.len(), 2);
    }

    #[test]
    fn read_bytes_matches_deserialize() {
        let app = sample();
        let bytes = serialize(&app).expect("serialize");
        let back = read_bytes(&bytes).expect("read_bytes");
        assert_eq!(back.script_source, app.script_source);
    }

    #[test]
    fn rejects_foreign_bytes() {
        assert!(matches!(
            deserialize(b"not-an-artifact-blob"),
            Err(ArtifactError::BadMagic)
        ));
        assert!(matches!(
            deserialize(b"LM"),
            Err(ArtifactError::TooSmall(_))
        ));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = serialize(&sample()).expect("serialize");
        bytes[4] = 0xFF;
        bytes[5] = 0xFF;
        assert!(matches!(
            deserialize(&bytes),
            Err(ArtifactError::Version { .. })
        ));
    }
}
