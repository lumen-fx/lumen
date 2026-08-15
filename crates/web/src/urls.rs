//! Building the URLs a document points at.
//!
//! Every reference a document makes is rooted at the site's base path
//! rather than at the document. A page key can contain a slash, so a
//! relative reference would resolve differently depending on which page it
//! was written into.

/// A base path with the slashes it needs: one at each end.
pub fn normalize_base(base: &str) -> String {
    let trimmed = base.trim().trim_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("/{trimmed}/")
    }
}

/// A site-relative path as an absolute URL path under `base`.
pub fn join(base: &str, path: &str) -> String {
    format!("{}{}", normalize_base(base), path.trim_start_matches('/'))
}

/// A site-relative path as a full URL under `url`.
pub fn absolute(url: &str, base: &str, path: &str) -> String {
    format!("{}{}", url.trim_end_matches('/'), join(base, path))
}

/// True when a reference names somewhere other than this site: another
/// origin, another scheme, or a place inside the current document.
///
/// Such a reference is written into the document as the author wrote it.
pub fn is_external(href: &str) -> bool {
    let href = href.trim();
    if href.starts_with("//") || href.starts_with('#') {
        return true;
    }
    // A scheme is a name, then a colon, before any slash: `mailto:`,
    // `https:`, `tel:`. A path segment holding a colon is not one.
    match href.split_once(':') {
        Some((scheme, _)) => {
            !scheme.is_empty()
                && !scheme.contains('/')
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// The URL a `<a href>` is written into the document as.
///
/// A link to a page becomes that page's document. A link to a deeper path
/// (`user/42`, where only `user` is a page) stays the path the author wrote,
/// because that is the URL a visitor should see and share; the app resolves
/// the leftover segment itself, the same way it does on the desktop.
pub fn page_href(href: &str, base: &str, keys: &[String], entry: &str) -> String {
    if is_external(href) {
        return href.to_string();
    }
    let (key, segment) = lumen_core::nav::resolve_path(href, keys, entry);
    if segment.is_empty() {
        return join(base, &crate::spec::document_name(&key, entry));
    }
    join(base, href.trim_start_matches('/'))
}

/// The URL an asset reference is written into the document as.
pub fn asset_src(src: &str, base: &str) -> String {
    if is_external(src) {
        return src.to_string();
    }
    join(base, src.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_path_gets_the_slashes_it_needs() {
        assert_eq!(normalize_base(""), "/");
        assert_eq!(normalize_base("/"), "/");
        assert_eq!(normalize_base("docs"), "/docs/");
        assert_eq!(normalize_base("/docs"), "/docs/");
        assert_eq!(normalize_base("/docs/"), "/docs/");
    }

    #[test]
    fn paths_are_rooted_at_the_base() {
        assert_eq!(join("/", "styles.css"), "/styles.css");
        assert_eq!(join("/docs/", "styles.css"), "/docs/styles.css");
        assert_eq!(join("/docs", "/styles.css"), "/docs/styles.css");
        assert_eq!(join("/", "user/profile.html"), "/user/profile.html");
    }

    fn keys() -> Vec<String> {
        vec![
            "settings".to_string(),
            "user".to_string(),
            "main".to_string(),
        ]
    }

    #[test]
    fn a_reference_off_the_site_is_left_alone() {
        for href in [
            "https://example.com",
            "http://example.com/x",
            "//cdn.example.com/x.png",
            "mailto:hi@example.com",
            "tel:+15550100",
            "#section",
        ] {
            assert!(is_external(href), "{href} names somewhere else");
            assert_eq!(page_href(href, "/docs", &keys(), "main"), href);
        }
        for href in ["settings", "/settings", "user/42"] {
            assert!(!is_external(href), "{href} is on this site");
        }
    }

    #[test]
    fn a_link_to_a_page_becomes_that_page_s_document() {
        assert_eq!(
            page_href("settings", "/", &keys(), "main"),
            "/settings.html"
        );
        assert_eq!(
            page_href("/settings", "/docs", &keys(), "main"),
            "/docs/settings.html"
        );
        // The entry page is the site root's document whatever it is keyed as.
        assert_eq!(page_href("main", "/", &keys(), "main"), "/index.html");
        assert_eq!(page_href("", "/", &keys(), "main"), "/index.html");
    }

    #[test]
    fn a_deeper_path_stays_the_path_the_author_wrote() {
        assert_eq!(page_href("user/42", "/", &keys(), "main"), "/user/42");
        assert_eq!(
            page_href("/user/42", "/docs", &keys(), "main"),
            "/docs/user/42"
        );
        // A path matching no page at all is left to the app to answer, the
        // same way the desktop resolver leaves it.
        assert_eq!(page_href("nowhere", "/", &keys(), "main"), "/nowhere");
    }

    #[test]
    fn an_asset_reference_is_rooted_at_the_base() {
        assert_eq!(asset_src("assets/logo.png", "/"), "/assets/logo.png");
        assert_eq!(
            asset_src("assets/logo.png", "/docs"),
            "/docs/assets/logo.png"
        );
        assert_eq!(
            asset_src("https://cdn.example.com/logo.png", "/docs"),
            "https://cdn.example.com/logo.png"
        );
    }

    #[test]
    fn absolute_urls_keep_one_slash_at_the_seam() {
        assert_eq!(
            absolute("https://example.com", "/", "index.html"),
            "https://example.com/index.html"
        );
        assert_eq!(
            absolute("https://example.com/", "/docs", "index.html"),
            "https://example.com/docs/index.html"
        );
    }
}
