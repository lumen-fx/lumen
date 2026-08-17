//! Putting the files of a site together.

use std::collections::BTreeSet;

use lumen_html::contract::{DEFAULT_MANIFEST_FILE, LM_CONTRACT_VERSION, Manifest, Seed};
use lumen_html::escape_text;
use lumen_ir::css::Stylesheet;

use crate::css;
use crate::error::EmitError;
use crate::html;
use crate::seo;
use crate::spec::{HostRewrite, OutputFile, PageSpec, SignalEnv, Site, SiteSpec};
use crate::urls;

/// Emit the site: one document per page, the shell a deep path falls back
/// to, the stylesheet, and the manifest.
///
/// Nothing is written to disk. The same spec always emits the same bytes,
/// so a build can be compared against the one before it.
pub fn emit(spec: &SiteSpec) -> Result<Site, EmitError> {
    if spec.pages.is_empty() {
        return Err(EmitError::NoPages);
    }
    let mut keys = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for page in &spec.pages {
        if page.key.is_empty() {
            return Err(EmitError::EmptyPageKey);
        }
        if !keys.insert(page.key.as_str()) {
            return Err(EmitError::DuplicatePage(page.key.clone()));
        }
        let document = page.document(&spec.web.entry);
        if !documents.insert(document.clone()) {
            return Err(EmitError::DuplicateDocument(document));
        }
    }
    if !spec.web.entry.is_empty() && !keys.contains(spec.web.entry.as_str()) {
        return Err(EmitError::UnknownEntry(spec.web.entry.clone()));
    }

    // Documents sit under this tree's locale prefix; what the whole site
    // shares sits at the root and is emitted with the root tree.
    let prefix = spec.locale.prefix();
    let mut warnings = Vec::new();
    let mut files = Vec::with_capacity(spec.pages.len() + 4);
    for page in &spec.pages {
        files.push(OutputFile::new(
            format!("{prefix}{}", page.document(&spec.web.entry)),
            document(page, spec, &mut warnings)?,
        ));
    }
    files.push(OutputFile::new(
        format!("{prefix}{NOT_FOUND_FILE}"),
        shell(spec, &mut warnings)?,
    ));
    if spec.locale.is_root() {
        files.push(OutputFile::new(
            spec.web.css.clone(),
            css::styles_css(stylesheet(spec), &spec.markup, spec.web.css_mode),
        ));
        warnings.extend(css::token_warnings(stylesheet(spec), spec.web.css_mode));
        // The manifest is what the runtime reads to find everything else, so
        // a site that loads no runtime has nobody to read it, and writing one
        // would name a wasm module the site does not carry.
        if spec.web.runtime {
            let manifest = serde_json::to_string_pretty(&manifest(spec))?;
            files.push(OutputFile::new(
                DEFAULT_MANIFEST_FILE,
                format!("{manifest}\n"),
            ));
        }
        if let Some((name, contents)) = rewrite_file(spec) {
            files.push(OutputFile::new(name, contents));
        }
        if let Some(sitemap) = sitemap(spec) {
            files.push(OutputFile::new(SITEMAP_FILE, sitemap));
        }
    }

    Ok(Site {
        files,
        assets: spec.assets.clone(),
        warnings,
    })
}

/// The document a server hands out for a path it has no file for.
pub const NOT_FOUND_FILE: &str = "404.html";

/// The list of the site's pages, for a crawler.
pub const SITEMAP_FILE: &str = "sitemap.xml";

/// The app shell: the entry page's markup with no page selected.
///
/// A deep path like `/user/42` is not a file, so a static host serves this.
/// It carries the whole app but shows none of it, and the runtime picks the
/// page from the address bar the way the desktop resolves a navigation.
fn shell(spec: &SiteSpec, warnings: &mut Vec<String>) -> Result<String, EmitError> {
    let entry = spec
        .pages
        .iter()
        .find(|page| page.key == spec.web.entry)
        .or_else(|| spec.pages.first())
        .ok_or(EmitError::NoPages)?;
    let shell = PageSpec {
        key: entry.key.clone(),
        ir: entry.ir.clone(),
        title: entry.title.clone(),
        description: entry.description.clone(),
        signals: SignalEnv::new(),
        seed: Seed::new(),
    };
    document(&shell, spec, warnings)
}

/// The deployment file that makes a deep path serve the shell, for a host
/// that can rewrite. A plain file server needs none: it already serves
/// [`NOT_FOUND_FILE`].
fn rewrite_file(spec: &SiteSpec) -> Option<(&'static str, String)> {
    let base = urls::normalize_base(&spec.web.base_path);
    let shell = urls::join(&base, NOT_FOUND_FILE);
    let contents = match spec.web.host {
        HostRewrite::Static => return None,
        HostRewrite::Netlify => format!("{base}*  {shell}  200\n"),
        HostRewrite::Vercel => format!(
            "{{\n  \"rewrites\": [\n    {{ \"source\": \"{base}(.*)\", \"destination\": \
             \"{shell}\" }}\n  ]\n}}\n"
        ),
        HostRewrite::Apache => format!(
            "RewriteEngine On\nRewriteBase {base}\nRewriteCond %{{REQUEST_FILENAME}} \
             !-f\nRewriteCond %{{REQUEST_FILENAME}} !-d\nRewriteRule . {shell} [L]\n"
        ),
        HostRewrite::Nginx => format!(
            "# Include this from the server block that serves the site.\nlocation {base} {{\n    \
             try_files $uri $uri/ {shell};\n}}\n"
        ),
    };
    Some((
        match spec.web.host {
            HostRewrite::Static => unreachable!("a plain file server needs no rewrite file"),
            HostRewrite::Netlify => "_redirects",
            HostRewrite::Vercel => "vercel.json",
            HostRewrite::Apache => ".htaccess",
            HostRewrite::Nginx => "nginx.conf",
        },
        contents,
    ))
}

/// Every page of every locale, as absolute URLs, or `None` when the site has
/// no URL to build them from.
fn sitemap(spec: &SiteSpec) -> Option<String> {
    if !spec.web.sitemap {
        return None;
    }
    let url = spec.web.url.as_ref()?;
    let base = urls::normalize_base(&spec.web.base_path);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for page in &spec.pages {
        let document = page.document(&spec.web.entry);
        let locations = spec.locale.all().into_iter().map(|locale| {
            let path = format!("{}{document}", spec.locale.prefix_of(&locale));
            urls::absolute(url, &base, &path)
        });
        for location in locations {
            out.push_str("  <url><loc>");
            out.push_str(&escape_text(&location));
            out.push_str("</loc></url>\n");
        }
    }
    out.push_str("</urlset>\n");
    Some(out)
}

/// The whole HTML document for one page.
pub fn document(
    page: &PageSpec,
    spec: &SiteSpec,
    warnings: &mut Vec<String>,
) -> Result<String, EmitError> {
    let mut out = String::new();
    seo::open_document(&mut out, page, spec)?;
    out.push_str(&html::emit_tree(page, spec, warnings)?);
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
        locales: spec.locale.all(),
        navigation: web.navigation,
        pages: spec
            .pages
            .iter()
            .map(|page| (page.key.clone(), page.document(&web.entry)))
            .collect(),
        scripts: web.scripts.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markup::MarkupSheet;
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
            markup: MarkupSheet::default(),
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
