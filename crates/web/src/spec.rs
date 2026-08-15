//! What a site is made of, and what comes back out of emitting one.

use std::collections::HashMap;
use std::path::PathBuf;

use lumen_core::signals::ArrayItem;
use lumen_html::contract::{
    DEFAULT_ARTIFACT_FILE, DEFAULT_CSS_FILE, DEFAULT_JS_FILE, DEFAULT_WASM_FILE, Dir,
    NavigationMode, ScriptRef, Seed,
};
use lumen_i18n::LanguageIdentifier;
use lumen_ir::layout_ir::LayoutIR;

/// The signal state a page is rendered with.
///
/// A page is rendered from the state the app would be in on arrival, so the
/// document a visitor gets already shows the branch that is true and the
/// rows that exist. An empty environment renders the markup alone: a
/// branch is not taken and a list has no rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalEnv {
    globals: HashMap<String, String>,
    arrays: HashMap<String, Vec<ArrayItem>>,
}

impl SignalEnv {
    /// An environment with nothing in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a global signal.
    pub fn with_global(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.globals.insert(name.into(), value.into());
        self
    }

    /// Set an array signal.
    pub fn with_array(mut self, name: impl Into<String>, rows: Vec<ArrayItem>) -> Self {
        self.arrays.insert(name.into(), rows);
        self
    }

    /// The value of a global signal.
    pub fn global(&self, name: &str) -> Option<&str> {
        self.globals.get(name).map(String::as_str)
    }

    /// The rows of an array signal.
    pub fn rows(&self, name: &str) -> Option<&[ArrayItem]> {
        self.arrays.get(name).map(Vec::as_slice)
    }

    /// True when nothing has been set.
    pub fn is_empty(&self) -> bool {
        self.globals.is_empty() && self.arrays.is_empty()
    }

    /// Whether a signal counts as true.
    ///
    /// Unset, empty, `false` and `0` are false and everything else is true.
    /// The desktop reconciler decides it the same way, and a page rendered
    /// on a different rule would disagree with the runtime that adopts it.
    pub fn is_truthy(&self, name: &str) -> bool {
        !matches!(
            self.global(name),
            None | Some("") | Some("false") | Some("0")
        )
    }
}

/// One page of the site.
#[derive(Debug, Clone, Default)]
pub struct PageSpec {
    /// Page key, which is also its file name: `index` becomes `index.html`.
    pub key: String,
    /// The page's markup and stylesheet.
    pub ir: LayoutIR,
    /// Title for this page. Falls back to the site title.
    pub title: Option<String>,
    /// Description for this page. Falls back to the site description.
    pub description: Option<String>,
    /// State the page is rendered with.
    pub signals: SignalEnv,
    /// State the browser runtime starts from, inlined into the document. It
    /// has to be the state the page was rendered with.
    pub seed: Seed,
}

impl PageSpec {
    /// A page with no title, description, state or seed of its own.
    pub fn new(key: impl Into<String>, ir: LayoutIR) -> Self {
        Self {
            key: key.into(),
            ir,
            ..Self::default()
        }
    }

    /// The document this page is emitted as.
    pub fn document(&self) -> String {
        format!("{}.html", self.key)
    }
}

/// Site-wide settings: where it is served from, what it is called, and which
/// runtime files the documents point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSpec {
    /// URL prefix the site is served under, such as `/` or `/docs/`.
    pub base_path: String,
    /// Absolute site URL, such as `https://example.com`. Canonical and
    /// social metadata need it; without it they are left out.
    pub url: Option<String>,
    /// Page key the site opens on.
    pub entry: String,
    /// Title used by any page that does not set its own.
    pub title: String,
    /// Description used by any page that does not set its own.
    pub description: Option<String>,
    /// Image for social previews, relative to the site root or absolute.
    pub og_image: Option<String>,
    /// Compiled app artifact, relative to the site root.
    pub artifact: String,
    /// Stylesheet, relative to the site root.
    pub css: String,
    /// Wasm runtime, relative to the site root.
    pub wasm: String,
    /// JavaScript module that loads the runtime, relative to the site root.
    pub js: String,
    /// How same-site links are followed.
    pub navigation: NavigationMode,
    /// Scripts the runtime loads at boot, in order.
    pub scripts: Vec<ScriptRef>,
}

impl Default for WebSpec {
    fn default() -> Self {
        Self {
            base_path: "/".to_string(),
            url: None,
            entry: "index".to_string(),
            title: String::new(),
            description: None,
            og_image: None,
            artifact: DEFAULT_ARTIFACT_FILE.to_string(),
            css: DEFAULT_CSS_FILE.to_string(),
            wasm: DEFAULT_WASM_FILE.to_string(),
            js: DEFAULT_JS_FILE.to_string(),
            navigation: NavigationMode::default(),
            scripts: Vec::new(),
        }
    }
}

/// The locale one tree of documents is emitted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleSpec {
    /// Locale tag, such as `en-US`.
    pub locale: String,
    /// Base writing direction.
    pub dir: Dir,
    /// The other locales the site is emitted in, for `hreflang` links.
    pub alternates: Vec<String>,
}

impl LocaleSpec {
    /// A locale whose direction is read from the language.
    ///
    /// A tag that does not parse is treated as left to right, which is what
    /// an unknown language falls back to everywhere else in Lumen.
    pub fn new(locale: impl Into<String>) -> Self {
        let locale = locale.into();
        let dir = match locale.parse::<LanguageIdentifier>() {
            Ok(lang) if lumen_i18n::is_rtl(&lang) => Dir::Rtl,
            _ => Dir::Ltr,
        };
        Self {
            locale,
            dir,
            alternates: Vec::new(),
        }
    }
}

impl Default for LocaleSpec {
    fn default() -> Self {
        Self::new("en-US")
    }
}

/// A file the site refers to and the caller has to copy in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    /// Where the file is now.
    pub source: PathBuf,
    /// Where it goes, relative to the site root, with forward slashes.
    pub path: String,
}

impl AssetRef {
    /// An asset copied from `source` to `path`.
    pub fn new(source: impl Into<PathBuf>, path: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            path: path.into(),
        }
    }
}

/// Everything needed to emit a site.
#[derive(Debug, Clone, Default)]
pub struct SiteSpec {
    /// The pages, in the order they were discovered.
    pub pages: Vec<PageSpec>,
    /// Site-wide settings.
    pub web: WebSpec,
    /// The locale this tree is for.
    pub locale: LocaleSpec,
    /// Files the pages refer to.
    pub assets: Vec<AssetRef>,
}

/// One emitted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Path relative to the site root, with forward slashes.
    pub path: String,
    /// File contents.
    pub contents: String,
}

impl OutputFile {
    /// A file at `path` holding `contents`.
    pub fn new(path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

/// An emitted site, held in memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Site {
    /// The files to write, in a stable order.
    pub files: Vec<OutputFile>,
    /// The files to copy in.
    pub assets: Vec<AssetRef>,
}

impl Site {
    /// The emitted file at `path`.
    pub fn file(&self, path: &str) -> Option<&OutputFile> {
        self.files.iter().find(|file| file.path == path)
    }
}
