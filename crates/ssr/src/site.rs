//! The app a renderer answers requests with.

use std::sync::Arc;

use lumen_html::contract::Seed;
use lumen_ir::artifact::CompiledApp;
use lumen_ir::layout_ir::LayoutIR;
use lumen_web::{LocaleSpec, PageSpec, SiteSpec, WebSpec};

use crate::error::SsrError;

/// A compiled app, ready to be rendered per request.
///
/// Building one is the expensive half and it happens once: the artifact is
/// read, the page set is worked out, and the tree every page is emitted from
/// is put behind an `Arc`. A render then borrows all of it and adds only what
/// belongs to the request it is answering.
///
/// The documents a renderer produces point at the stylesheet, the artifact
/// and the runtime by the paths in [`WebSpec`]. Those files come from
/// `lumenc web`, and the server serves them itself; a renderer produces
/// documents and nothing else.
#[derive(Debug, Clone)]
pub struct SsrSite {
    compiled: CompiledApp,
    spec: SiteSpec,
    ir: Arc<LayoutIR>,
    entry: String,
    keys: Vec<String>,
    seed: Seed,
}

impl SsrSite {
    /// A site that renders `compiled` with the settings in `web`.
    ///
    /// The page set comes from the app, so a request resolves to a page the
    /// same way it does on the desktop and in the browser. `web.entry` has to
    /// name one of them, because it is where a path matching no page lands.
    pub fn new(compiled: CompiledApp, web: WebSpec) -> Result<Self, SsrError> {
        let (entry, keys) = match &compiled.pages {
            Some(pages) => (pages.entry.clone(), pages.keys.clone()),
            None => {
                let entry = if web.entry.is_empty() {
                    "index".to_string()
                } else {
                    web.entry.clone()
                };
                (entry.clone(), vec![entry])
            }
        };
        if !web.entry.is_empty() && !keys.contains(&web.entry) {
            return Err(SsrError::UnknownEntry {
                asked: web.entry,
                pages: keys,
            });
        }
        let entry = if web.entry.is_empty() {
            entry
        } else {
            web.entry.clone()
        };

        // One tree, shared by every page: which page a document shows is a
        // signal inside the tree, so a request changes one string rather than
        // building anything.
        let ir = Arc::new(compiled.ir.clone());
        let spec = SiteSpec {
            pages: keys
                .iter()
                .map(|key| PageSpec::new(key.clone(), Arc::clone(&ir)))
                .collect(),
            web: WebSpec {
                entry: entry.clone(),
                ..web
            },
            ..SiteSpec::default()
        };
        Ok(Self {
            compiled,
            spec,
            ir,
            entry,
            keys,
            seed: Seed::new(),
        })
    }

    /// Render in `locale`, which decides the document's language and writing
    /// direction.
    pub fn with_locale(mut self, locale: LocaleSpec) -> Self {
        self.spec.locale = locale;
        self
    }

    /// Start every render from `seed`.
    ///
    /// These are the values an author declared rather than state an app
    /// produced, so what the app writes over them wins. A markup
    /// `signal-seed` needs nothing here: the app applies its own.
    pub fn with_seed(mut self, seed: Seed) -> Self {
        self.seed = seed;
        self
    }

    /// The site as the emitter sees it.
    ///
    /// Titles and descriptions live on the pages here, and this is where to
    /// set them. Every page shares one tree; replacing a page's `ir` with a
    /// tree of its own gives the renderer a document per page to hold rather
    /// than one for the site.
    pub fn spec_mut(&mut self) -> &mut SiteSpec {
        &mut self.spec
    }

    /// The site as the emitter sees it.
    pub fn spec(&self) -> &SiteSpec {
        &self.spec
    }

    /// The compiled app a render runs.
    pub fn compiled(&self) -> &CompiledApp {
        &self.compiled
    }

    /// The page a path matching nothing lands on.
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Every page key, longest first, which is the order a path resolves in.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// The state every render starts from.
    pub fn seed(&self) -> &Seed {
        &self.seed
    }

    /// The page `key`, ready to be rendered with a request's state.
    pub(crate) fn page(&self, key: &str) -> PageSpec {
        match self.spec.pages.iter().find(|page| page.key == key) {
            Some(page) => page.clone(),
            None => PageSpec::new(key.to_string(), Arc::clone(&self.ir)),
        }
    }
}

#[cfg(test)]
mod tests {
    use lumen_ir::artifact::CompiledPages;

    use super::*;

    fn app_with_pages() -> CompiledApp {
        CompiledApp {
            pages: Some(CompiledPages {
                entry: "index".to_string(),
                keys: vec!["settings".to_string(), "index".to_string()],
            }),
            ..CompiledApp::default()
        }
    }

    #[test]
    fn a_site_holds_one_page_per_key_and_one_tree_between_them() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        assert_eq!(site.keys(), ["settings", "index"]);
        assert_eq!(site.entry(), "index");
        assert_eq!(site.spec().pages.len(), 2);
        assert!(Arc::ptr_eq(
            &site.spec().pages[0].ir,
            &site.spec().pages[1].ir
        ));
    }

    #[test]
    fn a_single_page_app_needs_no_page_set() {
        let site = SsrSite::new(CompiledApp::default(), WebSpec::default())
            .expect("the entry stands in for the page set");
        assert_eq!(site.keys(), ["index"]);
    }

    #[test]
    fn an_entry_the_app_has_no_page_for_is_refused() {
        let web = WebSpec {
            entry: "nowhere".to_string(),
            ..WebSpec::default()
        };
        let error = SsrSite::new(app_with_pages(), web).expect_err("there is no such page");
        assert!(error.to_string().contains("nowhere"), "{error}");
    }
}
