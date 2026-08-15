//! The contract shared by the two halves of Lumen's web target.
//!
//! One half is the emitter, which turns a compiled app into static HTML.
//! The other is the runtime that boots in the browser and takes that HTML
//! over. They only agree if they agree exactly, so the things both must
//! know live here and nowhere else: which HTML element an IR tag becomes,
//! how a node is named, which `data-lm-*` attributes carry state, and the
//! shape of the manifest and seed files the runtime loads.
//!
//! Nothing here reads or writes files, and nothing here knows what a page
//! is. It is a vocabulary, not a pipeline.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod attrs;
pub mod contract;
pub mod escape;
pub mod tags;

pub use attrs::{class_list, html_attrs};
pub use contract::{
    DATA_LM, DATA_LM_AUX, DATA_LM_BASE, DATA_LM_CHECKED, DATA_LM_CONTRACT, DATA_LM_DISABLED,
    DATA_LM_DRAG_OVER, DATA_LM_HIDDEN, DATA_LM_KEY, DATA_LM_LOCALE, DATA_LM_PAGE, DATA_LM_SELECTED,
    DATA_LM_WIDGET, DEFAULT_ARTIFACT_FILE, DEFAULT_CSS_FILE, DEFAULT_JS_FILE,
    DEFAULT_MANIFEST_FILE, DEFAULT_WASM_FILE, Dir, LM_CONTRACT_VERSION, Manifest, NavigationMode,
    NodePath, PathError, PathStep, SEED_SCRIPT_ID, ScriptFormat, ScriptRef, Seed, SeedValue,
    UnsupportedSeedValue,
};
pub use escape::{escape_attr, escape_text};
pub use tags::{HtmlTag, MAPPED_TAGS, VOID_ELEMENTS, html_tag_for, is_void, lm_class};
