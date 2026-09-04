//! Emits a Lumen app as a static site.
//!
//! Every page becomes its own HTML document, with the markup already in it
//! rather than a shell a script fills in later. That is what makes a Lumen
//! app on the web readable to a search engine, to a screen reader, and to a
//! browser with no scripting at all: the text is text, a link is an `<a
//! href>`, and the first paint needs nothing but the document.
//!
//! The emitter is pure. It takes a [`SiteSpec`] and hands back the files it
//! would write; opening, copying and writing them is the caller's job.
//!
//! What each document is written against, from element names to the
//! `data-lm-*` attributes, is [`lumen_html`]'s to decide. Nothing here
//! carries a second copy of it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bindings;
pub mod css;
pub mod error;
pub mod html;
pub mod markup;
pub mod names;
pub mod seo;
pub mod site;
pub mod snapshot;
pub mod spec;
pub mod urls;

pub use css::{RESET_CSS, rules_css, styles_css, token_warnings};
pub use error::EmitError;
pub use markup::{MarkupSheet, lift as lift_markup_styles};
pub use names::{build_id, content_name, fnv1a64};
pub use site::{NOT_FOUND_FILE, SITEMAP_FILE, document, emit, shell};
pub use snapshot::{State, state_of};
pub use spec::{
    AssetRef, CssMode, HostRewrite, LocaleSpec, OutputFile, PageSpec, SignalEnv, Site, SiteSpec,
    WebSpec, document_key, document_name,
};
