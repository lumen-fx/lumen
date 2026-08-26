//! Compiler plugins through the whole pipeline: the `[[plugins]]` chain in
//! `lumen.toml`, driven by `compile_app` / `check_app` (the fat path) and
//! `compile::compile_dir_to_lmna` (the thin launcher path), against the
//! built fixture cdylib.

use std::path::PathBuf;

use lumen_ir::layout_ir::Element;
use lumenc_plugin::testing::fixture_cdylib;

/// Write a temp app declaring the fixture plugin `entries` times, each with
/// its own config table body.
fn app(tag: &str, markup: &str, css: &str, configs: &[&str]) -> PathBuf {
    // Keyed by pid so two concurrent runs of this binary cannot collide.
    let dir = std::env::temp_dir().join(format!(
        "lumenc-compiler-plugins-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), markup).unwrap();
    if !css.is_empty() {
        std::fs::write(src.join("main.css"), css).unwrap();
    }
    let lib = fixture_cdylib();
    let mut toml = String::new();
    for config in configs {
        toml.push_str(&format!(
            "[[plugins]]\nname = \"lumenc-plugin-fixture\"\npath = '{}'\n[plugins.config]\n{config}\n",
            lib.display()
        ));
    }
    std::fs::write(dir.join("lumen.toml"), toml).unwrap();
    dir
}

fn texts(el: &Element, out: &mut Vec<String>) {
    if let Some(t) = &el.attrs.text {
        out.push(t.clone());
    }
    for c in &el.children {
        texts(c, out);
    }
}

fn find_injected(el: &Element) -> Option<&Element> {
    if el.attrs.classes.iter().any(|c| c == "from-plugin") {
        return Some(el);
    }
    el.children.iter().find_map(find_injected)
}

#[test]
fn source_transforms_reach_the_artifact() {
    let dir = app(
        "source",
        "<root><label text=\"FROM_PLUGIN_MARKUP\"/></root>",
        ".x { color: PLUGIN_COLOR; }",
        &[""],
    );
    let compiled = lumenc::compile_app(&dir).unwrap();
    let mut all = Vec::new();
    texts(&compiled.ir.root, &mut all);
    assert!(
        all.iter().any(|t| t == "markup-transformed"),
        "markup transform missing from artifact: {all:?}"
    );
}

#[test]
fn injected_elements_are_cascaded() {
    let dir = app(
        "inject",
        "<root><label text=\"hi\"/></root>",
        ".from-plugin { color: #ff0000; }",
        &["inject_text = \"hello\""],
    );
    let compiled = lumenc::compile_app(&dir).unwrap();
    let injected = find_injected(&compiled.ir.root).expect("injected element in the tree");
    assert_eq!(injected.attrs.text.as_deref(), Some("hello"));
    // The injected element carries cascaded style: this pins the IR hook
    // ahead of the cascade. Moving the hook after it fails here.
    assert!(
        injected.attrs.text_color.is_some(),
        "injected element was not styled by the cascade"
    );
}

#[test]
fn check_runs_plugins_without_writing_outputs() {
    let dir = app(
        "check",
        "<root><label text=\"hi\"/></root>",
        "",
        &["lint_message = \"from lint\"\nemit_path = \"report.txt\""],
    );
    lumenc::check_app(&dir).unwrap();
    assert!(
        !dir.join(".lumen").exists(),
        "check must not write generated outputs"
    );
    // The same chain under build writes them.
    lumenc::compile_app(&dir).unwrap();
    let report = dir.join(".lumen/generated/lumenc-plugin-fixture/report.txt");
    assert!(report.is_file(), "build did not write {}", report.display());
}

#[test]
fn the_thin_path_matches_the_fat_path() {
    let dir = app(
        "thin",
        "<root><label text=\"FROM_PLUGIN_MARKUP\"/></root>",
        ".from-plugin { color: #ff0000; }",
        &["inject_text = \"hello\"\nlint_message = \"thin lint\""],
    );
    let thin = lumenc::compile::compile_dir_to_lmna(&dir).unwrap();
    let thin = lumen_ir::artifact::read_bytes(&thin).unwrap();
    let fat = lumenc::compile_app(&dir).unwrap();

    let collect = |root: &Element| {
        let mut all = Vec::new();
        texts(root, &mut all);
        all
    };
    assert_eq!(collect(&thin.ir.root), collect(&fat.ir.root));
    let thin_injected = find_injected(&thin.ir.root).expect("thin path ran the IR hook");
    assert!(thin_injected.attrs.text_color.is_some());
}

#[test]
fn plugins_compose_in_declaration_order() {
    let dir = app(
        "order",
        "<root><label text=\"/END\"/></root>",
        "",
        &["order = \"a\"", "order = \"b\"", "order = \"c\""],
    );
    let compiled = lumenc::compile_app(&dir).unwrap();
    let mut all = Vec::new();
    texts(&compiled.ir.root, &mut all);
    assert!(all.iter().any(|t| t == "abc/END"), "{all:?}");
}

#[test]
fn a_failing_hook_fails_build_and_check_naming_the_plugin() {
    let dir = app("fail", "<root/>", "", &["fail = \"ir\""]);
    let build_err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(
        build_err.contains("plugin 'lumenc-plugin-fixture'"),
        "{build_err}"
    );
    assert!(build_err.contains("fixture failure in ir"), "{build_err}");
    let check_err = lumenc::check_app(&dir).unwrap_err().to_string();
    assert!(
        check_err.contains("plugin 'lumenc-plugin-fixture'"),
        "{check_err}"
    );
}

#[test]
fn a_panicking_hook_fails_the_compile_not_the_process() {
    let dir = app("panic", "<root/>", "", &["panic_in = \"lint\""]);
    let err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(err.contains("panicked"), "{err}");
    assert!(err.contains("fixture panic in lint"), "{err}");
}

#[test]
fn a_version_source_resolves_through_cache_and_lock() {
    // Install the fixture into a private cache as version 1.0.0, point
    // LUMEN_PLUGIN_CACHE at it, and declare a version source.
    let base = std::env::temp_dir().join(format!(
        "lumenc-compiler-plugins-{}-versioned",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let cache = base.join("cache");
    let ver_dir = cache.join("lumenc-plugin-fixture").join("1.0.0");
    std::fs::create_dir_all(&ver_dir).unwrap();
    let lib = fixture_cdylib();
    // The cache spelling the resolver probes: `lib<name>.<ext>` on unix,
    // `<name>.dll` on Windows.
    let spelled = match lib.extension().and_then(|e| e.to_str()) {
        Some("dll") => "lumenc-plugin-fixture.dll".to_string(),
        Some(ext) => format!("liblumenc-plugin-fixture.{ext}"),
        None => panic!("fixture cdylib has no extension"),
    };
    let cached = ver_dir.join(spelled);
    std::fs::copy(&lib, &cached).unwrap();

    let dir = base.join("app");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><label text=\"FROM_PLUGIN_MARKUP\"/></root>",
    )
    .unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[[plugins]]\nname = \"lumenc-plugin-fixture\"\nversion = \"1\"\n",
    )
    .unwrap();

    // `set_var` is process-global; this is the only test in the binary that
    // touches it, which is what keeps the unsafe sound.
    unsafe { std::env::set_var("LUMEN_PLUGIN_CACHE", &cache) };
    let compiled = lumenc::compile_app(&dir).unwrap();
    unsafe { std::env::remove_var("LUMEN_PLUGIN_CACHE") };

    let mut all = Vec::new();
    texts(&compiled.ir.root, &mut all);
    assert!(all.iter().any(|t| t == "markup-transformed"), "{all:?}");
    let lock = std::fs::read_to_string(dir.join("lumen.lock")).unwrap();
    assert!(lock.contains("version = \"1.0.0\""), "{lock}");
}

#[test]
fn the_markup_transform_survives_a_multipage_app() {
    let dir = app(
        "multipage",
        "<root><label text=\"FROM_PLUGIN_MARKUP\"/>/END</root>",
        "",
        &["order = \"m\""],
    );
    std::fs::write(
        dir.join("about.lmn"),
        "<root><label text=\"about\"/></root>",
    )
    .unwrap();
    let compiled = lumenc::compile_app(&dir).unwrap();
    let mut all = Vec::new();
    texts(&compiled.ir.root, &mut all);
    // Both markers prove the entry page parsed from the transformed text,
    // not from a disk re-read during page assembly.
    assert!(all.iter().any(|t| t == "markup-transformed"), "{all:?}");
    assert!(all.iter().any(|t| t.contains("m/END")), "{all:?}");
}

#[test]
fn a_plugin_synthesizes_the_stylesheet_when_the_app_ships_none() {
    let dir = app(
        "no-css",
        "<root><label text=\"hi\"/></root>",
        "",
        &["inject_text = \"styled\"\nsynthesize_css = \".from-plugin { color: #00ff00; }\""],
    );
    assert!(!dir.join("src").join("main.css").exists());
    let compiled = lumenc::compile_app(&dir).unwrap();
    let injected = find_injected(&compiled.ir.root).expect("injected element");
    assert!(
        injected.attrs.text_color.is_some(),
        "synthesized stylesheet was not cascaded"
    );
}

#[test]
fn a_parse_error_in_rewritten_markup_says_so() {
    let dir = app("invalid", "<root/>", "", &["invalid_markup = true"]);
    let err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(err.contains("rewritten by compiler plugins"), "{err}");
    // The thin path reports the same attribution.
    let err = lumenc::compile::compile_dir_to_lmna(&dir)
        .unwrap_err()
        .to_string();
    assert!(err.contains("rewritten by compiler plugins"), "{err}");
}

#[test]
fn a_malformed_declaration_fails_the_compile_naming_the_plugin() {
    let dir = app("bad-decl", "<root/>", "", &[]);
    std::fs::write(
        dir.join("lumen.toml"),
        "[[plugins]]\nname = \"x\"\ngit = \"https://example.com\"\n",
    )
    .unwrap();
    let err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(err.contains("plugin 'x'"), "{err}");
    assert!(err.contains("not supported yet"), "{err}");
    let err = lumenc::check_app(&dir).unwrap_err().to_string();
    assert!(err.contains("not supported yet"), "{err}");
}

#[test]
fn a_plugin_free_parse_error_carries_no_attribution() {
    let dir = app("plain-invalid", "<root><label></root>", "", &[]);
    std::fs::write(dir.join("lumen.toml"), "").unwrap();
    let err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(!err.contains("rewritten by compiler plugins"), "{err}");
    let err = lumenc::compile::compile_dir_to_lmna(&dir)
        .unwrap_err()
        .to_string();
    assert!(!err.contains("rewritten by compiler plugins"), "{err}");
}

#[test]
fn a_rewritten_tree_the_parser_rejects_is_attributed_too() {
    // `wrong_root` is well-formed XML, so the failure lands in the markup
    // parser proper rather than the include resolver.
    let dir = app("wrong-root", "<root/>", "", &["wrong_root = true"]);
    let err = lumenc::compile_app(&dir).unwrap_err().to_string();
    assert!(err.contains("rewritten by compiler plugins"), "{err}");
    let err = lumenc::compile::compile_dir_to_lmna(&dir)
        .unwrap_err()
        .to_string();
    assert!(err.contains("rewritten by compiler plugins"), "{err}");
}

#[test]
fn a_missing_script_reference_passes_through_untouched() {
    // The transform changed the markup, and the failure afterwards is a
    // file read, not a parse: it must not gain the rewrite attribution.
    let dir = app(
        "missing-script",
        "<root><label text=\"/END\"/><script src=\"nope.cdl\"/></root>",
        "",
        &["order = \"x\""],
    );
    let err = lumenc::compile::compile_dir_to_lmna(&dir)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope.cdl"), "{err}");
    assert!(!err.contains("rewritten by compiler plugins"), "{err}");
}

#[test]
fn an_unparseable_config_declares_no_plugins() {
    // Matches `LumenToml::load_or_default` leniency: a broken lumen.toml is
    // "no config" here; the config loader is what reports it.
    let dir = std::env::temp_dir().join(format!(
        "lumenc-compiler-plugins-{}-bad-toml",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "not toml ][").unwrap();
    assert!(
        lumenc::plugin_host::read_plugin_cfgs(&dir)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_app_without_a_config_declares_no_plugins() {
    let dir = std::env::temp_dir().join(format!(
        "lumenc-compiler-plugins-{}-no-toml",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        lumenc::plugin_host::read_plugin_cfgs(&dir)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_run_wrappers_reject_a_bad_declaration_before_any_window() {
    let dir = app("run-fastfail", "<root/>", "", &[]);
    std::fs::write(
        dir.join("lumen.toml"),
        "[[plugins]]\nname = \"x\"\ngit = \"https://example.com\"\n",
    )
    .unwrap();
    let opts = lumen_runtime::RunOptions::new(&dir);
    let err = lumenc::run_app(opts).unwrap_err().to_string();
    assert!(err.contains("not supported yet"), "{err}");
}

#[test]
fn the_headless_wrapper_runs_the_declared_chain() {
    let dir = app(
        "run-headless",
        "<root><label text=\"FROM_PLUGIN_MARKUP\"/></root>",
        "",
        &["emit_path = \"ran.txt\""],
    );
    // MCP off so the bounded run binds no socket.
    let toml = std::fs::read_to_string(dir.join("lumen.toml")).unwrap();
    std::fs::write(dir.join("lumen.toml"), format!("[mcp]\nport = 0\n{toml}")).unwrap();
    let mut opts = lumen_runtime::RunOptions::new(&dir);
    opts.hot_reload = false;
    lumenc::run_app_headless(opts, 1).unwrap();
    assert!(
        dir.join(".lumen/generated/lumenc-plugin-fixture/ran.txt")
            .is_file()
    );
}
