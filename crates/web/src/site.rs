//! Putting the files of a site together.

use std::collections::BTreeSet;

use lumen_html::contract::{DEFAULT_MANIFEST_FILE, LM_CONTRACT_VERSION, Manifest};
use lumen_ir::css::Stylesheet;

use crate::css;
use crate::error::EmitError;
use crate::html;
use crate::seo;
use crate::spec::{OutputFile, PageSpec, Site, SiteSpec};
use crate::urls;

/// Emit the site: one document per page, the stylesheet, and the manifest.
///
/// Nothing is written to disk. The same spec always emits the same bytes,
/// so a build can be compared against the one before it.
pub fn emit(spec: &SiteSpec) -> Result<Site, EmitError> {
    if spec.pages.is_empty() {
        return Err(EmitError::NoPages);
    }
    let mut keys = BTreeSet::new();
    for page in &spec.pages {
        if page.key.is_empty() {
            return Err(EmitError::EmptyPageKey);
        }
        if !keys.insert(page.key.as_str()) {
            return Err(EmitError::DuplicatePage(page.key.clone()));
        }
    }
    if !spec.web.entry.is_empty() && !keys.contains(spec.web.entry.as_str()) {
        return Err(EmitError::UnknownEntry(spec.web.entry.clone()));
    }

    let mut files = Vec::with_capacity(spec.pages.len() + 2);
    for page in &spec.pages {
        files.push(OutputFile::new(page.document(), document(page, spec)?));
    }
    files.push(OutputFile::new(
        spec.web.css.clone(),
        css::styles_css(stylesheet(spec), spec.web.css_mode),
    ));
    let manifest = serde_json::to_string_pretty(&manifest(spec))?;
    files.push(OutputFile::new(
        DEFAULT_MANIFEST_FILE,
        format!("{manifest}\n"),
    ));

    Ok(Site {
        files,
        assets: spec.assets.clone(),
    })
}

/// The whole HTML document for one page.
pub fn document(page: &PageSpec, spec: &SiteSpec) -> Result<String, EmitError> {
    let mut out = String::new();
    seo::open_document(&mut out, page, spec)?;
    out.push_str(&html::emit_tree(page, spec.web.css_mode)?);
    seo::close_document(&mut out, spec);
    Ok(out)
}

/// The stylesheet the site is styled by.
///
/// One app has one stylesheet: its pages are separate documents but they
/// were compiled from the same CSS, so the entry page's copy is the
/// site's. A site whose entry page has none is emitted with the reset
/// alone.
fn stylesheet(spec: &SiteSpec) -> Option<&Stylesheet> {
    spec.pages
        .iter()
        .find(|page| page.key == spec.web.entry)
        .or_else(|| spec.pages.first())
        .and_then(|page| page.ir.combined_stylesheet.as_ref())
}

/// The manifest the browser runtime reads before it loads anything else.
pub fn manifest(spec: &SiteSpec) -> Manifest {
    let web = &spec.web;
    Manifest {
        contract_version: LM_CONTRACT_VERSION,
        base_path: urls::normalize_base(&web.base_path),
        entry: web.entry.clone(),
        artifact: web.artifact.clone(),
        css: web.css.clone(),
        wasm: web.wasm.clone(),
        js: web.js.clone(),
        locale: spec.locale.locale.clone(),
        dir: spec.locale.dir,
        locales: locales(spec),
        navigation: web.navigation,
        pages: spec
            .pages
            .iter()
            .map(|page| (page.key.clone(), page.document()))
            .collect(),
        scripts: web.scripts.clone(),
    }
}

/// Every locale the site is emitted in, this tree's first.
fn locales(spec: &SiteSpec) -> Vec<String> {
    let mut locales = vec![spec.locale.locale.clone()];
    for alternate in &spec.locale.alternates {
        if !locales.contains(alternate) {
            locales.push(alternate.clone());
        }
    }
    locales
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{LocaleSpec, WebSpec};
    use lumen_html::contract::NavigationMode;
    use lumen_ir::layout_ir::LayoutIR;

    fn spec() -> SiteSpec {
        SiteSpec {
            pages: vec![
                PageSpec::new("index", LayoutIR::default()),
                PageSpec::new("settings", LayoutIR::default()),
            ],
            web: WebSpec {
                base_path: "/docs".into(),
                entry: "index".into(),
                navigation: NavigationMode::Hard,
                ..WebSpec::default()
            },
            locale: LocaleSpec::new("ar-EG"),
            assets: Vec::new(),
        }
    }

    #[test]
    fn the_manifest_lists_every_page() {
        let manifest = manifest(&spec());
        assert_eq!(
            manifest.pages.get("index").map(String::as_str),
            Some("index.html")
        );
        assert_eq!(
            manifest.pages.get("settings").map(String::as_str),
            Some("settings.html")
        );
        assert_eq!(manifest.base_path, "/docs/");
        assert_eq!(manifest.entry, "index");
        assert_eq!(manifest.contract_version, LM_CONTRACT_VERSION);
        assert_eq!(manifest.navigation, NavigationMode::Hard);
    }

    #[test]
    fn an_rtl_locale_leads_its_own_locale_list() {
        let mut spec = spec();
        spec.locale.alternates = vec!["en-US".into(), "ar-EG".into()];
        let manifest = manifest(&spec);
        assert_eq!(manifest.dir, lumen_html::contract::Dir::Rtl);
        assert_eq!(manifest.locales, vec!["ar-EG", "en-US"]);
    }
}
