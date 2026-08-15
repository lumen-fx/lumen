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
