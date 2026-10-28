//! What the source-install build script reports when the script standard
//! library does not reach the install directory.
//!
//! The build script includes this file by path; the library compiles it only
//! under test, because cargo runs a build script but never tests one.

/// Directory the script standard library sits in, beside the binaries that
/// read it. Matches `SCRIPT_LIBRARY_DIR` in `src/package/cli.rs`, the release
/// archives, and the Windows installer.
pub const SCRIPT_LIBRARY_DIR: &str = "libs";

/// The warning for a standard library that did not land beside `lumenc`,
/// where `cause` says what went wrong.
pub fn library_not_beside_lumenc(cause: &str) -> String {
    format!(
        "{cause}, so the candela standard library is not beside lumenc. \
         `import \"std/...\"` will not resolve until a {SCRIPT_LIBRARY_DIR} tree \
         is there or CANDELA_LIB_PATH names one."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_names_the_cause_the_library_dir_and_the_override() {
        let message = library_not_beside_lumenc("cannot read /x/libs");
        assert!(message.starts_with("cannot read /x/libs, so the candela standard library"));
        assert!(message.contains("until a libs tree is there"));
        assert!(message.contains("CANDELA_LIB_PATH names one"));
        assert!(message.contains("`import \"std/...\"` will not resolve"));
    }
}
