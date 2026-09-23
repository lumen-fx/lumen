//! The app a renderer answers requests with.

use std::sync::Arc;

use lumen_core::nav;
use lumen_html::contract::Seed;
use lumen_i18n::Catalogues;
use lumen_ir::artifact::CompiledApp;
use lumen_prerender::Language;
use lumen_web::{PageSpec, ServerPolicy, ServerSpec, SiteLocales, SiteSpec, WebSpec};

use crate::error::SsrError;
use crate::locale::negotiate;
use crate::request::SsrRequest;

/// The header a browser lists the languages it wants in.
const ACCEPT_LANGUAGE: &str = "accept-language";

/// A compiled app, ready to be rendered per request.
///
/// Building one is the expensive half and it happens once: the artifact is
/// read, the page set is worked out, and the tree every page is emitted from
/// is put behind an `Arc`. A render then borrows all of it and adds only what
/// belongs to the request it is answering.
///
/// A site holds one tree per language it answers in. The trees are handed in
/// already translated, by [`Self::with_locale`], because which strings a tree
/// carries is decided when it is built rather than when it is rendered;
/// [`lumen_web::locale_trees`] is what builds them. The catalogues themselves
/// go in with [`Self::with_catalogues`], because the app reads them while it
/// runs: a script's `t()`, and every row it builds.
///
/// The documents a renderer produces point at the stylesheet, the artifact
/// and the runtime by the paths in [`WebSpec`]. Those files come from
/// `lumenc web`, and the server serves them itself; a renderer produces
/// documents and nothing else.
#[derive(Debug, Clone)]
pub struct SsrSite {
    compiled: CompiledApp,
    /// One tree per language, the default locale's first. Every one of them
    /// answers for the same page keys.
    trees: Vec<SiteSpec>,
    entry: String,
    keys: Vec<String>,
    seed: Seed,
    /// Every locale's catalogue, parsed once, and the chain a key missing
    /// from the active one falls through. A render builds its app's registry
    /// from them.
    catalogues: Catalogues,
    /// What the app allows a render to reach.
    policy: ServerPolicy,
}

impl SsrSite {
    /// A site that renders `compiled` with the settings in `web`.
    ///
    /// The page set comes from the app, so a request resolves to a page the
    /// same way it does on the desktop and in the browser. `web.entry` has to
    /// name one of them, because it is the page the site root opens on.
    ///
    /// The site starts with one tree, in the default locale. Add the others
    /// with [`Self::with_locale`].
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
            trees: vec![spec],
            entry,
            keys,
            seed: Seed::new(),
            catalogues: Catalogues::default(),
            policy: ServerPolicy::default(),
        })
    }

    /// The site a build rendered per request, from the files it wrote.
    ///
    /// `artifact` is the compiled app's bytes, the file `spec.web.artifact`
    /// names. `spec` is [`lumen_web::SERVER_SPEC_FILE`], read with
    /// [`ServerSpec::from_json`]. `catalogues` pairs each tag in
    /// `spec.web.catalogues` with the source of the file it names; a build
    /// whose pages carry no runtime names none, and empty takes the
    /// catalogues the compiled app carries. Everything arrives already read,
    /// so where the files live and how they are read stays the server's
    /// business.
    ///
    /// The site this builds is the one `lumenc web --render ssr --serve`
    /// renders: a tree per locale the build emitted, translated from the
    /// catalogues, with the titles, image sizes, declared state and policy
    /// the build wrote down.
    pub fn from_build(
        artifact: &[u8],
        spec: &ServerSpec,
        catalogues: Vec<(String, String)>,
    ) -> Result<Self, SsrError> {
        let compiled = lumen_ir::artifact::deserialize(artifact)
            .map_err(|error| SsrError::Artifact(error.to_string()))?;
        // A build whose pages load no catalogue writes none beside them, and
        // the compiled app carries its own.
        let catalogues = if catalogues.is_empty() {
            compiled.i18n.catalogues.clone()
        } else {
            catalogues
        };
        let mut site = Self::new(compiled, spec.web.clone())?
            .with_catalogues(catalogues, spec.fallback.clone())?
            .with_seed(spec.seed.clone());
        site.policy = spec.policy.clone();
        let locales = if spec.locales.is_empty() {
            vec![site.trees[0].locale.locale.clone()]
        } else {
            spec.locales.clone()
        };
        let shared = SiteSpec {
            web: WebSpec {
                entry: site.entry.clone(),
                per_request: true,
                ..spec.web.clone()
            },
            assets: spec.assets(),
            ..SiteSpec::default()
        };
        // The trees the build wrote, built the way the build built them. What
        // they could say about a locale the build said already.
        let trees = lumen_web::locale_trees(
            SiteLocales {
                ir: &site.compiled.ir,
                keys: &site.keys,
                heads: &spec.pages,
                locales: &locales,
                catalogues: &site.catalogues,
                shared: &shared,
            },
            &mut Vec::new(),
        );
        for tree in trees {
            site = site.with_locale(tree.spec)?;
        }
        Ok(site)
    }

    /// Also answer in `tree`'s locale, from the strings `tree` carries.
    ///
    /// The tree is the app in one language: the same pages, with every
    /// `translatable` element resolved for that locale. A build hands the
    /// renderer the trees it emitted; an embedder builds them with
    /// [`lumen_web::locale_trees`], or one with [`lumen_web::translate_ir`],
    /// over the catalogues it has.
    ///
    /// A tree marked as the root, whose locale is its default, replaces the
    /// tree at the site root, and any other tree the site holds for that
    /// locale goes. Any other tree replaces the one the site holds for its
    /// locale, or is added. A tree in the root's locale that is not marked as
    /// the root is refused, because two trees would answer for one language.
    /// Every tree's alternates and default locale are then set from the trees
    /// the site holds, so the links between languages stay whole.
    ///
    /// A tree that cannot answer for every page the site has is refused too:
    /// a request would otherwise reach a page in the language it asked for
    /// and the page beside it in another.
    pub fn with_locale(mut self, tree: SiteSpec) -> Result<Self, SsrError> {
        if tree.pages.is_empty() {
            return Err(SsrError::LocaleTree {
                locale: tree.locale.locale,
                why: "it has no pages".to_string(),
            });
        }
        if let Some(missing) = self
            .keys
            .iter()
            .find(|key| !tree.pages.iter().any(|page| &&page.key == key))
        {
            return Err(SsrError::LocaleTree {
                locale: tree.locale.locale,
                why: format!("it has no `{missing}` page, and the site does"),
            });
        }
        let same = |held: &SiteSpec| held.locale.locale.eq_ignore_ascii_case(&tree.locale.locale);
        if tree.locale.is_root() {
            let mut index = 0;
            self.trees.retain(|held| {
                index += 1;
                index == 1 || !same(held)
            });
            self.trees[0] = tree;
        } else if same(&self.trees[0]) {
            return Err(SsrError::LocaleTree {
                locale: tree.locale.locale,
                why: "it is the locale of the tree at the site root, and it is not marked as the \
                      root; give it that locale as its default to replace the root"
                    .to_string(),
            });
        } else {
            match self.trees[1..].iter().position(same) {
                Some(index) => self.trees[index + 1] = tree,
                None => self.trees.push(tree),
            }
        }
        self.link_locales();
        Ok(self)
    }

    /// Point every tree at the root and at each other, from the trees held.
    fn link_locales(&mut self) {
        let locales: Vec<String> = self
            .trees
            .iter()
            .map(|tree| tree.locale.locale.clone())
            .collect();
        for tree in &mut self.trees {
            tree.locale.default_locale = locales[0].clone();
            tree.locale.alternates = locales
                .iter()
                .filter(|other| **other != tree.locale.locale)
                .cloned()
                .collect();
        }
    }

    /// Every locale the site answers in, the default one first.
    pub fn locales(&self) -> Vec<&str> {
        self.trees
            .iter()
            .map(|tree| tree.locale.locale.as_str())
            .collect()
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

    /// Run every render with `catalogues`, a BCP-47 tag and that locale's
    /// Fluent source each, falling through `fallback` on a miss.
    ///
    /// The app of a render starts in the locale of the tree the request
    /// resolved to, so a script's `t()` answers in the language the document
    /// is written in, and a row the app builds reads in it too. Empty
    /// `fallback` takes the chain a desktop app gets when `[app]
    /// fallback_locale` is unset. A site with no catalogues runs its renders
    /// with no translator, and `t()` answers with the key.
    ///
    /// Every catalogue is parsed here, once, so one that would fail a render
    /// fails the site instead, and a render only builds its registry.
    pub fn with_catalogues(
        mut self,
        catalogues: Vec<(String, String)>,
        fallback: Vec<String>,
    ) -> Result<Self, SsrError> {
        self.catalogues = Catalogues::parse(&catalogues, &fallback)
            .map_err(|error| SsrError::Catalogue(error.to_string()))?;
        Ok(self)
    }

    /// What the app allows a render to reach, as the build wrote it down.
    ///
    /// [`crate::RenderOptions::with_policy`] applies it. A site built with
    /// [`Self::new`] carries the empty policy: a render reaches no host.
    pub fn policy(&self) -> &ServerPolicy {
        &self.policy
    }

    /// The language a render in `tree` runs in.
    pub(crate) fn language(&self, tree: usize) -> Language<'_> {
        Language {
            locale: &self.trees[tree].locale.locale,
            catalogues: &self.catalogues,
        }
    }

    /// The default locale's tree, as the emitter sees it.
    ///
    /// Titles and descriptions live on the pages here, and this is where to
    /// set them. Every page shares one tree; replacing a page's `ir` with a
    /// tree of its own gives the renderer a document per page to hold rather
    /// than one for the site.
    pub fn spec_mut(&mut self) -> &mut SiteSpec {
        &mut self.trees[0]
    }

    /// The default locale's tree, as the emitter sees it.
    pub fn spec(&self) -> &SiteSpec {
        &self.trees[0]
    }

    /// The compiled app a render runs.
    pub fn compiled(&self) -> &CompiledApp {
        &self.compiled
    }

    /// The page the site root opens on.
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

    /// The page `path` names, and the part of the path that page answers for.
    ///
    /// Two shapes of address reach the same page. A link inside an emitted
    /// site points at the document a build wrote, so `/settings.html` is a
    /// request for the `settings` page; a link an author wrote, and any path
    /// deeper than a page, resolves the way the desktop resolves it, leaving
    /// the rest of the path as the segment. A document a build never wrote is
    /// not a page, so it goes through the resolver like anything else.
    ///
    /// A path that opens with a locale the site holds names a page of that
    /// language's tree, so `/de-DE/settings.html` is the `settings` page too.
    ///
    /// `None` when no page answers for the address, which is what a render
    /// answers with a 404. Ask this before rendering to give such an address
    /// an answer of your own.
    pub fn page_for(&self, path: &str) -> Option<(String, String)> {
        self.resolve(self.strip_locale(path).1)
    }

    /// Which tree answers `request`, the page it asked for, and anything
    /// worth saying about how that was decided.
    ///
    /// The locale is settled before the page, because the page is resolved
    /// against the path a locale prefix has been taken off.
    pub(crate) fn route(&self, request: &SsrRequest) -> Route {
        let mut warnings = Vec::new();
        // An embedder whose proxy or language cookie has already decided says
        // so on the request, and that is the end of it. A tag the site has no
        // tree for is a spelling mistake worth saying out loud rather than a
        // reason to refuse the page.
        let asked = match self.tree_for(&request.locale) {
            Some(index) => Some(index),
            None => {
                if !request.locale.is_empty() {
                    warnings.push(format!(
                        "the request asks to be rendered in `{}`, which this site holds no tree \
                         for; it is answered in {}",
                        request.locale, self.trees[0].locale.locale
                    ));
                }
                None
            }
        };
        let (prefixed, path) = self.strip_locale(&request.path);
        // Which document the server sends is the server's decision, so it is
        // read off the header as it arrived rather than through the policy
        // that governs what the app's own scripts may read.
        let negotiated = || {
            let held = self.locales();
            request
                .headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(ACCEPT_LANGUAGE))
                .find_map(|(_, value)| negotiate(value, &held))
                .and_then(|tag| self.tree_for(tag))
        };
        let tree = asked.or(prefixed).or_else(negotiated).unwrap_or(0);
        Route {
            tree,
            page: self.resolve(path),
            warnings,
        }
    }

    /// The tree held for `locale`, matched as the tag was written.
    fn tree_for(&self, locale: &str) -> Option<usize> {
        if locale.is_empty() {
            return None;
        }
        self.trees
            .iter()
            .position(|tree| tree.locale.locale.eq_ignore_ascii_case(locale))
    }

    /// Take a locale prefix off `path`, when it names a tree the site holds
    /// under one. The tree at the site root has no prefix, so nothing there
    /// is stripped and `/de-DE-notes.html` stays the page it names.
    fn strip_locale<'a>(&self, path: &'a str) -> (Option<usize>, &'a str) {
        let rest = path.trim_start_matches('/');
        let (first, tail) = match rest.split_once('/') {
            Some((first, tail)) => (first, tail),
            None => (rest, ""),
        };
        match self.tree_for(first).filter(|index| *index != 0) {
            Some(index) => (Some(index), &path[path.len() - tail.len()..]),
            None => (None, path),
        }
    }

    /// The page a path with no locale prefix on it names.
    fn resolve(&self, path: &str) -> Option<(String, String)> {
        if let Some(key) = lumen_web::document_key(path, &self.entry)
            && self.keys.contains(&key)
        {
            return Some((key, String::new()));
        }
        nav::match_path(path, &self.keys, &self.entry)
    }

    /// The document an address no page answers for is sent, in `tree`'s
    /// language.
    ///
    /// This is [`lumen_web::shell`] over that tree: the same `404.html` a
    /// build writes. It holds no state, so it does not depend on the request
    /// and is worked out once per tree.
    pub(crate) fn not_found_body(&self, tree: usize) -> Result<(String, Vec<String>), SsrError> {
        let mut warnings = Vec::new();
        let body = lumen_web::shell(&self.trees[tree], &mut warnings)?;
        Ok((body, warnings))
    }

    /// The tree at `index`, which a render writes its document against.
    pub(crate) fn tree(&self, index: usize) -> &SiteSpec {
        &self.trees[index]
    }

    /// The page `key` of `tree`, ready to be rendered with a request's state.
    pub(crate) fn page(&self, key: &str, tree: &SiteSpec) -> PageSpec {
        match tree.pages.iter().find(|page| page.key == key) {
            Some(page) => page.clone(),
            None => PageSpec::new(key.to_string(), Arc::clone(&tree.pages[0].ir)),
        }
    }
}

/// What a request resolved to: the language it is answered in, the page it
/// named, and anything the resolution is worth saying about.
pub(crate) struct Route {
    /// Index into the site's trees.
    pub(crate) tree: usize,
    /// The page and the segment, or `None` for an address no page answers
    /// for.
    pub(crate) page: Option<(String, String)>,
    pub(crate) warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use lumen_ir::artifact::CompiledPages;
    use lumen_web::LocaleSpec;

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

    /// The site's own tree, re-keyed for `locale`, which is what a build
    /// hands over once it has translated it.
    fn tree_for(site: &SsrSite, locale: &str) -> SiteSpec {
        SiteSpec {
            locale: LocaleSpec {
                default_locale: "en-US".to_string(),
                ..LocaleSpec::new(locale)
            },
            ..site.spec().clone()
        }
    }

    fn asking(path: &str) -> SsrRequest {
        SsrRequest::get(path)
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

    #[test]
    fn a_tree_that_cannot_answer_for_every_page_is_refused() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let mut partial = tree_for(&site, "de-DE");
        partial.pages.retain(|page| page.key == "index");
        let error = site
            .clone()
            .with_locale(partial)
            .expect_err("a tree missing a page would answer for it in another language");
        assert!(error.to_string().contains("settings"), "{error}");

        let empty = SiteSpec {
            locale: LocaleSpec::new("de-DE"),
            ..SiteSpec::default()
        };
        assert!(site.with_locale(empty).is_err());
    }

    #[test]
    fn the_default_locales_tree_is_the_one_at_the_site_root() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let root = tree_for(&site, "en-US");
        let german = tree_for(&site, "de-DE");
        let site = site
            .with_locale(german)
            .and_then(|site| site.with_locale(root))
            .expect("both trees answer for every page");
        // The tree of the default locale replaces the one the site started
        // with rather than being added beside it.
        assert_eq!(site.locales(), ["en-US", "de-DE"]);
    }

    #[test]
    fn a_root_tree_in_another_locale_takes_the_roots_place() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let french = SiteSpec {
            locale: LocaleSpec::new("fr-FR"),
            ..site.spec().clone()
        };
        let site = site.with_locale(french).expect("it answers for every page");
        // The tree `new` started with was the default locale's stand-in, and a
        // site has one root.
        assert_eq!(site.locales(), ["fr-FR"]);
    }

    #[test]
    fn a_second_tree_for_a_locale_replaces_the_first() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let mut renamed = tree_for(&site, "de-DE");
        renamed.web.title = "Zweite".to_string();
        let german = tree_for(&site, "de-DE");
        let site = site
            .with_locale(german)
            .and_then(|site| site.with_locale(renamed))
            .expect("both trees answer for every page");
        assert_eq!(site.locales(), ["en-US", "de-DE"]);
        assert_eq!(site.tree(1).web.title, "Zweite");
    }

    #[test]
    fn a_root_tree_takes_the_root_and_drops_the_tree_it_was_beside() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let german = tree_for(&site, "de-DE");
        let french = tree_for(&site, "fr-FR");
        let site = site
            .with_locale(german)
            .and_then(|site| site.with_locale(french))
            .expect("both trees answer for every page");
        // German becomes the root: the tree it was beside goes, and so does
        // the English root it replaces.
        let root = SiteSpec {
            locale: LocaleSpec::new("de-DE"),
            ..site.spec().clone()
        };
        let site = site.with_locale(root).expect("it answers for every page");
        assert_eq!(site.locales(), ["de-DE", "fr-FR"]);
        assert!(site.tree(0).locale.is_root());
        // Every tree names the new root and only the trees still held.
        assert_eq!(site.tree(1).locale.default_locale, "de-DE");
        assert_eq!(site.tree(1).locale.alternates, ["de-DE"]);
        assert_eq!(site.tree(0).locale.alternates, ["fr-FR"]);
    }

    #[test]
    fn a_tree_in_the_roots_locale_that_is_not_the_root_is_refused() {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let stray = SiteSpec {
            locale: LocaleSpec {
                default_locale: "fr-FR".to_string(),
                ..LocaleSpec::new("en-US")
            },
            ..site.spec().clone()
        };
        let error = site
            .with_locale(stray)
            .expect_err("two trees would answer for en-US");
        assert!(error.to_string().contains("en-US"), "{error}");
    }

    #[test]
    fn every_tree_names_the_others_as_they_arrive() {
        let site = two_tree_site()
            .with_locale(SiteSpec {
                locale: LocaleSpec {
                    // Written against another root; the site's root wins.
                    default_locale: "ja-JP".to_string(),
                    ..LocaleSpec::new("fr-FR")
                },
                ..SsrSite::new(app_with_pages(), WebSpec::default())
                    .expect("the entry is a page")
                    .spec()
                    .clone()
            })
            .expect("it answers for every page");
        assert_eq!(site.locales(), ["en-US", "de-DE", "fr-FR"]);
        assert_eq!(site.tree(0).locale.alternates, ["de-DE", "fr-FR"]);
        assert_eq!(site.tree(1).locale.alternates, ["en-US", "fr-FR"]);
        assert_eq!(site.tree(2).locale.alternates, ["en-US", "de-DE"]);
        for index in 0..3 {
            assert_eq!(site.tree(index).locale.default_locale, "en-US");
        }
    }

    fn two_tree_site() -> SsrSite {
        let site = SsrSite::new(app_with_pages(), WebSpec::default()).expect("the entry is a page");
        let german = tree_for(&site, "de-DE");
        site.with_locale(german).expect("it answers for every page")
    }

    #[test]
    fn the_locale_on_the_request_is_the_one_it_is_answered_in() {
        let site = two_tree_site();
        let route = site.route(&asking("/settings.html").with_locale("de-DE"));
        assert_eq!(route.tree, 1);
        assert_eq!(route.page, Some(("settings".to_string(), String::new())));
        assert!(route.warnings.is_empty());

        // And it wins over what the browser asked for.
        let asked = asking("/")
            .with_locale("en-US")
            .with_header("Accept-Language", "de-DE");
        assert_eq!(site.route(&asked).tree, 0);
    }

    #[test]
    fn a_locale_the_site_holds_no_tree_for_is_said_out_loud_and_answered_anyway() {
        let site = two_tree_site();
        let route = site.route(&asking("/").with_locale("fr-FR"));
        assert_eq!(route.tree, 0);
        assert_eq!(route.page, Some(("index".to_string(), String::new())));
        assert!(
            route
                .warnings
                .iter()
                .any(|warning| warning.contains("fr-FR")),
            "{:?}",
            route.warnings
        );
    }

    #[test]
    fn a_path_under_a_locale_reaches_that_locales_tree() {
        let site = two_tree_site();
        let route = site.route(&asking("/de-DE/settings.html"));
        assert_eq!(route.tree, 1);
        assert_eq!(route.page, Some(("settings".to_string(), String::new())));

        // The tree at the site root has no prefix, so a page whose key merely
        // starts the same way is still that page.
        assert_eq!(site.route(&asking("/settings.html")).tree, 0);
        assert!(site.route(&asking("/en-US/settings.html")).page.is_none());

        // A deep path under a prefix keeps its segment.
        assert_eq!(
            site.route(&asking("/de-DE/settings/theme")).page,
            Some(("settings".to_string(), "/theme".to_string()))
        );
        // And the prefix alone is the entry page.
        assert_eq!(
            site.route(&asking("/de-DE/")).page,
            Some(("index".to_string(), String::new()))
        );
    }

    #[test]
    fn the_header_decides_when_nothing_else_has() {
        let site = two_tree_site();
        let asked = asking("/").with_header("Accept-Language", "de-DE,de;q=0.9,en;q=0.5");
        assert_eq!(site.route(&asked).tree, 1);

        // A language the site holds no tree for falls back to the root.
        let french = asking("/").with_header("Accept-Language", "fr-FR");
        assert_eq!(site.route(&french).tree, 0);
        assert!(site.route(&french).warnings.is_empty());

        // A path prefix is a choice already made, so the header does not
        // reopen it.
        let prefixed = asking("/de-DE/").with_header("Accept-Language", "en-US");
        assert_eq!(site.route(&prefixed).tree, 1);
    }

    #[test]
    fn an_address_no_page_answers_for_is_still_none_under_a_prefix() {
        let site = two_tree_site();
        assert!(site.page_for("/de-DE/nowhere.html").is_none());
        assert_eq!(
            site.page_for("/de-DE/settings.html"),
            Some(("settings".to_string(), String::new()))
        );
        let route = site.route(&asking("/de-DE/nowhere.html"));
        assert_eq!(route.tree, 1);
        assert!(route.page.is_none());
    }
}
