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
}
