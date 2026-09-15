//! Building the URLs a document points at.
//!
//! Every reference a document makes is rooted at the site's base path
//! rather than at the document. A page key can contain a slash, so a
//! relative reference would resolve differently depending on which page it
//! was written into.
//!
//! Both halves of the web target hang addresses off the base: the emitter
//! writes them into the markup, and the runtime builds the same ones again
//! when it fetches a file or puts a page's address in the bar. They only
//! agree if they agree exactly, which is why the rule lives here.

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

/// True when a reference names somewhere other than this site: another
/// origin, another scheme, or a place inside the current document.
///
/// Such a reference is left exactly as the author wrote it, by the emitter
/// writing a document and by the runtime mounting an element into one.
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

    #[test]
    fn a_reference_off_the_site_is_recognised_by_its_scheme() {
        for href in [
            "https://example.com",
            "http://example.com/x",
            "//cdn.example.com/x.png",
            "mailto:hi@example.com",
            "tel:+15550100",
            "#section",
        ] {
            assert!(is_external(href), "{href} names somewhere else");
        }
        for href in ["settings", "/settings", "user/42", "user/4:2"] {
            assert!(!is_external(href), "{href} is on this site");
        }
    }
}
