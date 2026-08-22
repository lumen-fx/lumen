//! The constructor-panic path needs first-call construction, and the
//! fixture's singleton is per process, so this test lives in its own binary:
//! no sibling test can construct the instance before the env var is set.

use lumenc_plugin::{PluginCfg, PluginError, PluginSet, SourceKind, testing};

#[test]
fn a_panicking_constructor_fails_the_compile_not_the_process() {
    let dir = std::env::temp_dir().join(format!("lumenc-plugin-ctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = testing::fixture_cdylib();
    let doc: toml::Table = toml::from_str(&format!(
        "[[plugins]]\nname = \"lumen-plugin-fixture\"\npath = '{}'\n",
        lib.display()
    ))
    .unwrap();
    let set = PluginSet::load(&dir, &PluginCfg::from_document(&doc).unwrap()).unwrap();

    unsafe { std::env::set_var("LUMEN_FIXTURE_CTOR_PANIC", "1") };
    let entry = dir.join("main.lmn");
    let err = set
        .transform_source(SourceKind::Markup, "x".to_string(), &entry, &entry)
        .unwrap_err();
    assert!(matches!(err, PluginError::Panicked { .. }), "{err}");
    assert!(
        err.to_string().contains("fixture panic in constructor"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
