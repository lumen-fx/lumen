//! The document around a page: the head, and the boot script under it.
//!
//! What goes in the head is what a crawler, a social preview and a browser
//! need before anything runs: a title, a description, the canonical URL,
//! the stylesheet, and hints for the two files the runtime fetches. The
//! only script in the document loads that runtime; everything the app knows
//! travels as data.

use lumen_html::contract::{
    DATA_LM_BASE, DATA_LM_CONTRACT, DATA_LM_LOCALE, DATA_LM_PAGE, LM_CONTRACT_VERSION,
    SEED_SCRIPT_ID,
};
use lumen_html::{escape_attr, escape_text};

use crate::error::EmitError;
use crate::spec::{PageSpec, SiteSpec};
use crate::urls;

/// Write everything above the page's own elements.
pub fn open_document(out: &mut String, page: &PageSpec, spec: &SiteSpec) -> Result<(), EmitError> {
    let web = &spec.web;
    let base = urls::normalize_base(&web.base_path);
    let title = page.title.clone().unwrap_or_else(|| web.title.clone());
    let description = page.description.as_ref().or(web.description.as_ref());
    let canonical = web
        .url
        .as_ref()
        .map(|url| urls::absolute(url, &base, &page.document()));

    out.push_str("<!doctype html>\n<html");
    attr(out, "lang", &spec.locale.locale);
    attr(out, "dir", spec.locale.dir.as_str());
    attr(out, DATA_LM_CONTRACT, &LM_CONTRACT_VERSION.to_string());
    attr(out, DATA_LM_PAGE, &page.key);
    attr(out, DATA_LM_BASE, &base);
    attr(out, DATA_LM_LOCALE, &spec.locale.locale);
    out.push_str(">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    out.push_str(&escape_text(&title));
    out.push_str("</title>\n");
    if let Some(description) = description {
        meta_named(out, "description", description);
    }
    if let Some(canonical) = &canonical {
        out.push_str("<link rel=\"canonical\"");
        attr(out, "href", canonical);
        out.push_str(">\n");
    }

    meta_property(out, "og:type", "website");
    meta_property(out, "og:title", &title);
    if let Some(description) = description {
        meta_property(out, "og:description", description);
    }
    if let Some(canonical) = &canonical {
        meta_property(out, "og:url", canonical);
    }
    if let Some(image) = &web.og_image {
        let image = match (&web.url, image.starts_with("http")) {
            (Some(url), false) => urls::absolute(url, &base, image),
            _ => image.clone(),
        };
        meta_property(out, "og:image", &image);
        meta_named(out, "twitter:card", "summary_large_image");
    } else {
        meta_named(out, "twitter:card", "summary");
    }
    meta_named(out, "twitter:title", &title);
    if let Some(description) = description {
        meta_named(out, "twitter:description", description);
    }

    // Alternates need absolute URLs to be worth anything to a crawler, so a
    // site with no URL configured gets none.
    if let Some(url) = &web.url {
        for locale in &spec.locale.alternates {
            let href = urls::absolute(url, &base, &format!("{locale}/{}", page.document()));
            out.push_str("<link rel=\"alternate\"");
            attr(out, "hreflang", locale);
            attr(out, "href", &href);
            out.push_str(">\n");
        }
        if !spec.locale.alternates.is_empty()
            && let Some(canonical) = &canonical
        {
            out.push_str("<link rel=\"alternate\" hreflang=\"x-default\"");
            attr(out, "href", canonical);
            out.push_str(">\n");
        }
    }

    out.push_str("<link rel=\"stylesheet\"");
    attr(out, "href", &urls::join(&base, &web.css));
    out.push_str(">\n");
    out.push_str("<link rel=\"modulepreload\"");
    attr(out, "href", &urls::join(&base, &web.js));
    out.push_str(">\n");
    out.push_str("<link rel=\"preload\" as=\"fetch\" type=\"application/wasm\" crossorigin");
    attr(out, "href", &urls::join(&base, &web.wasm));
    out.push_str(">\n");
    out.push_str("<link rel=\"preload\" as=\"fetch\" crossorigin");
    attr(out, "href", &urls::join(&base, &web.artifact));
    out.push_str(">\n");

    out.push_str("<script type=\"application/json\"");
    attr(out, "id", SEED_SCRIPT_ID);
    out.push('>');
    out.push_str(&page.seed.to_script_json()?);
    out.push_str("</script>\n");
    out.push_str("</head>\n<body>\n");
    Ok(())
}

/// Write the boot script and close the document.
///
/// The script is the same on every page of a site: it loads the runtime,
/// which reads the page it landed on out of the document.
pub fn close_document(out: &mut String, spec: &SiteSpec) {
    let base = urls::normalize_base(&spec.web.base_path);
    out.push_str("\n<script type=\"module\">import init, { boot } from \"");
    out.push_str(&escape_text(&urls::join(&base, &spec.web.js)));
    out.push_str("\";init().then(boot);</script>\n</body>\n</html>\n");
}

fn attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(&escape_attr(value));
    out.push('"');
}

fn meta_named(out: &mut String, name: &str, content: &str) {
    out.push_str("<meta");
    attr(out, "name", name);
    attr(out, "content", content);
    out.push_str(">\n");
}

fn meta_property(out: &mut String, property: &str, content: &str) {
    out.push_str("<meta");
    attr(out, "property", property);
    attr(out, "content", content);
    out.push_str(">\n");
}
