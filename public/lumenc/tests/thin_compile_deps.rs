// The thin compile path is gated behind the parser front-end, like the rest
// of `lumenc::compile`.
#![cfg(feature = "runtime-parse")]

//! What the parser-only compile does with `[dependencies]`.
//!
//! This is the path `lumenc build` takes for an app it turns into an
//! artifact, and it links no runtime, so it reads the table itself. What it
//! has to get right is narrow: publish the tags an app declares before the
//! markup is parsed, say so when the declaration is wrong, and stay out of
//! the way when there is nothing to read.

use lumenc::compile::{CompileError, compile_dir_to_lmna};

/// An app directory with the given `lumen.toml` and markup.
fn app(case: &str, config: &str, markup: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenc-thin-deps-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::write(dir.join("src/main.lmn"), markup).expect("markup");
    std::fs::write(dir.join("lumen.toml"), config).expect("lumen.toml");
    dir
}

#[test]
fn a_declared_tag_is_accepted_by_a_compile_that_loads_no_module() {
    // The whole reason the key exists: nothing is opened here, so the
    // declaration is the only thing that can tell the parser the element is
    // real.
    let dir = app(
        "declared",
        "[app]\nentry = \"main.lmn\"\n\n[dependencies]\n\
         thin-widgets = { bundled = true, tags = [\"thin-gauge\"] }\n",
        "<root><thin-gauge /></root>\n",
    );
    let bytes = compile_dir_to_lmna(&dir).expect("the declared tag parses");
    assert!(!bytes.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dependencies_table_that_is_wrong_is_reported_rather_than_ignored() {
    // A silent miss here surfaces later as an unknown tag with no
    // explanation, so the compile says what it could not read.
    let dir = app(
        "malformed",
        "[app]\nentry = \"main.lmn\"\n\n[dependencies]\n\
         thin-widgets = { bundled = true, tags = \"thin-gauge\" }\n",
        "<root><label text=\"hi\" /></root>\n",
    );
    let err = compile_dir_to_lmna(&dir).expect_err("the table is wrong");
    assert!(
        matches!(&err, CompileError::Config(m) if m.contains("array of strings")),
        "{err}"
    );
    assert!(err.to_string().starts_with("lumen.toml:"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tag_the_language_owns_cannot_be_declared_away_from_it() {
    let dir = app(
        "reserved",
        "[app]\nentry = \"main.lmn\"\n\n[dependencies]\n\
         thin-widgets = { bundled = true, tags = [\"button\"] }\n",
        "<root><label text=\"hi\" /></root>\n",
    );
    let err = compile_dir_to_lmna(&dir).expect_err("the tag is the language's");
    assert!(
        matches!(&err, CompileError::Config(m) if m.contains("built-in tag")),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_app_with_nothing_to_declare_compiles_the_same() {
    // Three ways of saying nothing: no table, no config file at all, and a
    // config that is not TOML. None of them is this step's business, and none
    // of them stops a compile that would otherwise succeed.
    for (case, config) in [
        ("no-table", "[app]\nentry = \"main.lmn\"\n"),
        ("no-file", ""),
        ("not-toml", "this is not [ toml"),
    ] {
        let dir = app(case, config, "<root><label text=\"hi\" /></root>\n");
        if case == "no-file" {
            std::fs::remove_file(dir.join("lumen.toml")).expect("remove the config");
        }
        assert!(
            compile_dir_to_lmna(&dir).is_ok(),
            "{case} should compile without a dependency declaration"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
