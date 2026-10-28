//! What the build script reports when it cannot find the candela standard
//! library to stage.
//!
//! The build script includes this file by path; the library compiles it only
//! under test, because cargo runs a build script but never tests one.

/// The warning for a standard library that was not staged, where `cause`
/// says why.
pub fn library_not_staged(cause: &str) -> String {
    format!(
        "{cause}, so the candela standard library was not staged. \
         `import \"std/...\"` will not resolve."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_names_the_cause_and_what_stops_resolving() {
        let message = library_not_staged("the resolved graph carries no candela package");
        assert_eq!(
            message,
            "the resolved graph carries no candela package, so the candela standard \
             library was not staged. `import \"std/...\"` will not resolve."
        );
    }
}
