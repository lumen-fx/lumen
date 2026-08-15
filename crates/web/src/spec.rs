//! What a site is made of, and what comes back out of emitting one.

use std::collections::HashMap;
use std::path::PathBuf;

use lumen_core::signals::{ArrayItem, signal_is_truthy};
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
    /// The rule lives in [`lumen_core::signals::signal_is_truthy`], which is
    /// where the reconciler reads it too. A page rendered on a different rule
    /// would disagree with the runtime that adopts it.
    pub fn is_truthy(&self, name: &str) -> bool {
        self.global(name).is_some_and(signal_is_truthy)
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

    /// The document this page is emitted as, given the site's entry key.
    ///
    /// The entry page is `index.html` whatever it is keyed as, because that
    /// is the file a server hands out for the directory itself.
    pub fn document(&self, entry: &str) -> String {
        document_name(&self.key, entry)
    }
}

/// The document a page key is emitted as. See [`PageSpec::document`].
pub fn document_name(key: &str, entry: &str) -> String {
    if key == entry {
        "index.html".to_string()
    } else {
        format!("{key}.html")
    }
}

/// Where a site is deployed, and so what it takes for a deep path to reach
/// the app instead of the host's own not-found page.
///
/// Every site is emitted with a `404.html` holding the app shell, which is
/// all a plain file server needs. A host that can rewrite instead gets the
/// file that tells it to, so the visitor's URL is served with a 200.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostRewrite {
    /// A plain file server: the `404.html` alone.
    #[default]
    Static,
    /// Netlify: `_redirects`.
    Netlify,
    /// Vercel: `vercel.json`.
    Vercel,
    /// Apache: `.htaccess`.
    Apache,
    /// nginx: `nginx.conf`, to include from a server block.
    Nginx,
}

/// How a page's styling reaches the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CssMode {
    /// As a stylesheet, with the selectors, states and media queries the
    /// app was written with. This is how a site ships: the browser runs
    /// the cascade, so a rule still applies to a row that appears later.
    #[default]
    Sheet,
    /// As the values Lumen's own cascade resolved, written onto each
    /// element as an inline style.
    ///
    /// Nothing is left to match on: a state, a media query and an element
    /// created after the page loaded all lose their styling. It is here to
    /// answer what Lumen resolved, which makes it the thing to compare the
    /// stylesheet against when the two disagree.
    Computed,
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
    /// Absolute URL the pages declare as canonical, for a site published at
    /// more than one address. Falls back to [`Self::url`].
    pub canonical: Option<String>,
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
    /// How the pages are styled.
    pub css_mode: CssMode,
    /// Wasm runtime, relative to the site root.
    pub wasm: String,
    /// JavaScript module that loads the runtime, relative to the site root.
    pub js: String,
    /// How same-site links are followed.
    pub navigation: NavigationMode,
    /// Scripts the runtime loads at boot, in order.
    pub scripts: Vec<ScriptRef>,
    /// Where the site is deployed, which decides the deep-path rewrite file.
    pub host: HostRewrite,
    /// Whether the documents load the browser runtime.
    ///
    /// A site emitted without it is the degraded mode: the pages read, the
    /// links work as ordinary links, and nothing runs. The documents then
    /// say so by carrying no boot script at all, rather than pointing at a
    /// runtime that is not there.
    pub runtime: bool,
    /// Write `sitemap.xml`. Needs [`Self::url`].
    pub sitemap: bool,
}

impl Default for WebSpec {
    fn default() -> Self {
        Self {
            base_path: "/".to_string(),
            url: None,
            canonical: None,
            entry: "index".to_string(),
            title: String::new(),
            description: None,
            og_image: None,
            artifact: DEFAULT_ARTIFACT_FILE.to_string(),
            css: DEFAULT_CSS_FILE.to_string(),
            css_mode: CssMode::default(),
            wasm: DEFAULT_WASM_FILE.to_string(),
            js: DEFAULT_JS_FILE.to_string(),
            navigation: NavigationMode::default(),
            scripts: Vec::new(),
            host: HostRewrite::default(),
            runtime: true,
            sitemap: false,
        }
    }
}

/// The locale one tree of documents is emitted for.
///
/// A site emitted in more than one locale is one tree per locale. The
/// default locale's tree sits at the site root and every other one sits
/// under its own tag, so `/settings.html` and `/de-DE/settings.html` are the
/// same page in two languages. What the whole site shares - the stylesheet,
/// the compiled app, the runtime, the assets - stays at the root and is
/// referenced from every tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleSpec {
    /// Locale tag, such as `en-US`.
    pub locale: String,
    /// Base writing direction.
    pub dir: Dir,
    /// The locale whose tree sits at the site root.
    pub default_locale: String,
    /// The other locales the site is emitted in, for `hreflang` links.
    pub alternates: Vec<String>,
}

impl LocaleSpec {
    /// A locale whose direction is read from the language, alone at the site
    /// root.
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
            default_locale: locale.clone(),
            locale,
            dir,
            alternates: Vec::new(),
        }
    }

    /// True when this tree is the one at the site root.
    pub fn is_root(&self) -> bool {
        self.locale == self.default_locale
    }

    /// What every document of `locale` hangs off, under the site's base
    /// path: nothing for the default locale, `<tag>/` for the rest.
    pub fn prefix_of(&self, locale: &str) -> String {
        if locale == self.default_locale {
            String::new()
        } else {
            format!("{locale}/")
        }
    }

    /// This tree's own prefix. See [`Self::prefix_of`].
    pub fn prefix(&self) -> String {
        self.prefix_of(&self.locale)
    }

    /// Every locale the site is emitted in, this tree's first.
    pub fn all(&self) -> Vec<String> {
        let mut locales = vec![self.locale.clone()];
        for alternate in &self.alternates {
            if !locales.contains(alternate) {
                locales.push(alternate.clone());
            }
        }
        locales
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

impl SiteSpec {
    /// The document a page key is emitted as.
    pub fn document_for(&self, key: &str) -> String {
        document_name(key, &self.web.entry)
    }

    /// Every page key, longest first, which is the order a path is resolved
    /// against by [`lumen_core::nav::resolve_path`].
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.pages.iter().map(|page| page.key.clone()).collect();
        keys.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        keys
    }
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
