// The thin compile path is gated behind the parser front-end, like the rest
// of `lumenc::compile`.
#![cfg(feature = "runtime-parse")]

//! What the parser-only compile does with `[dependencies]`.
//!
//! This is the path `lumenc build` takes for an app it turns into an
//! artifact, and it links no runtime, so it reads the table itself. What it
//! has to get right is narrow: publish the tags each declared module's
//! `lumen-module.toml` declares before the markup is parsed, say so when a
//! declaration or a descriptor is wrong or missing, and stay out of the way
//! when there is nothing to read.

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

/// A module root at `<dir>/widgets` whose descriptor reads `descriptor`.
fn module(dir: &std::path::Path, descriptor: &str) {
    std::fs::create_dir_all(dir.join("widgets")).expect("module root");
    std::fs::write(dir.join("widgets/lumen-module.toml"), descriptor).expect("descriptor");
}

const DECLARES_WIDGETS: &str =
    "[app]\nentry = \"main.lmn\"\n\n[dependencies]\nthin-widgets = { path = \"widgets\" }\n";

#[test]
fn a_tag_the_module_declares_is_accepted_by_a_compile_that_loads_no_module() {
    // Nothing is opened here, and the app names no tag: the module's own
    // descriptor is the only thing that can tell the parser the element is
    // real.
    let dir = app(
        "declared",
        DECLARES_WIDGETS,
        "<root><thin-gauge /></root>\n",
    );
    module(&dir, "[module]\ntags = [\"thin-gauge\"]\n");
    let bytes = compile_dir_to_lmna(&dir).expect("the module's tag parses");
    assert!(!bytes.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tags_key_on_the_dependency_is_refused_with_where_tags_live() {
    let dir = app(
        "app-side-tags",
        "[app]\nentry = \"main.lmn\"\n\n[dependencies]\n\
         thin-widgets = { path = \"widgets\", tags = [\"thin-gauge\"] }\n",
        "<root><label text=\"hi\" /></root>\n",
    );
    module(&dir, "[module]\ntags = [\"thin-gauge\"]\n");
    let err = compile_dir_to_lmna(&dir).expect_err("the key is gone");
    assert!(
        matches!(&err, CompileError::Config(m)
            if m.contains("lumen-module.toml") && m.contains("remove the key")),
        "{err}"
    );
    assert!(err.to_string().starts_with("lumen.toml:"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_without_a_descriptor_fails_the_compile_naming_it() {
    let dir = app(
        "no-descriptor",
        DECLARES_WIDGETS,
        "<root><label text=\"hi\" /></root>\n",
    );
    std::fs::create_dir_all(dir.join("widgets")).expect("module root");
    let err = compile_dir_to_lmna(&dir).expect_err("the descriptor is missing");
    assert!(
        matches!(&err, CompileError::Module(m)
            if m.contains("module 'thin-widgets'") && m.contains("lumen-module.toml")),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tag_the_language_owns_cannot_be_declared_away_from_it() {
    let dir = app(
        "reserved",
        DECLARES_WIDGETS,
        "<root><label text=\"hi\" /></root>\n",
    );
    module(&dir, "[module]\ntags = [\"button\"]\n");
    let err = compile_dir_to_lmna(&dir).expect_err("the tag is the language's");
    assert!(
        matches!(&err, CompileError::Module(m) if m.contains("built-in tag")),
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
