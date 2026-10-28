// Drives the run, check and AOT paths, which lumenc exposes under `dev-run`.
#![cfg(all(feature = "runtime-parse", feature = "dev-run"))]

//! The markup tags a module brings come from the module's own
//! `lumen-module.toml`, on every path that parses an app.
//!
//! Each test declares a module whose library is not there: the loader
//! banners it and the app keeps going, so nothing installed registers the
//! tag. What the parse accepts, it accepts on the strength of the descriptor
//! alone. The tag registry is process-wide, so every test uses a tag of its
//! own.

use std::path::{Path, PathBuf};

use lumenc::{RunOptions, build_headless_app, check_app, compile_app};

/// An app whose markup holds `<tag>` and whose `lumen.toml` declares the
/// module `widgets` by `path`, with a descriptor declaring `tag` and no
/// library.
fn app(case: &str, tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lumenc-module-tags-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::create_dir_all(dir.join("widgets")).expect("module root");
    std::fs::write(
        dir.join("widgets/lumen-module.toml"),
        format!("[module]\ntags = [\"{tag}\"]\n"),
    )
    .expect("module descriptor");
    std::fs::write(
        dir.join("src/main.lmn"),
        format!("<root>\n  <{tag} />\n</root>\n"),
    )
    .expect("markup");
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n\n\
         [dependencies]\nwidgets = { path = \"widgets\" }\n",
    )
    .expect("lumen.toml");
    dir
}

/// The tags of the elements in the built app's tree.
fn tree_tags(app: &mut lumen_core::app::App) -> Vec<String> {
    app.world
        .query::<&lumen_core::components::LumenTag>()
        .iter(&app.world)
        .map(|t| t.0.to_string())
        .collect()
}

#[test]
fn the_run_path_takes_a_tag_from_the_module_descriptor() {
    let dir = app("run", "run-path-gauge");
    let (mut app, _window) =
        build_headless_app(RunOptions::new(dir.clone())).expect("the app builds");
    let tags = tree_tags(&mut app);
    assert!(tags.iter().any(|t| t == "run-path-gauge"), "{tags:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The runtime reads the descriptors on its own, for an embedder that hands
/// it a parser and nothing `lumenc` resolved.
#[test]
fn the_runtime_alone_takes_a_tag_from_the_module_descriptor() {
    let dir = app("runtime", "runtime-path-gauge");
    let opts = RunOptions::new(dir.clone()).with_parser(lumenc::default_parser());
    let (mut app, _window) = lumen_runtime::build_headless_app(opts).expect("the app builds");
    let tags = tree_tags(&mut app);
    assert!(tags.iter().any(|t| t == "runtime-path-gauge"), "{tags:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_aot_compile_takes_a_tag_from_the_module_descriptor() {
    let dir = app("aot", "aot-path-gauge");
    let compiled = compile_app(&dir).expect("the app compiles");
    assert_eq!(compiled.ir.root.children[0].tag, "aot-path-gauge");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_takes_a_tag_from_the_module_descriptor() {
    let dir = app("check", "check-path-gauge");
    check_app(&dir).expect("the app checks");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A declared module whose descriptor is not there stops the run before
/// anything is parsed, and the message names the module.
#[test]
fn a_run_without_the_module_descriptor_fails_naming_the_module() {
    let dir = app("run-missing", "run-missing-gauge");
    std::fs::remove_file(dir.join("widgets/lumen-module.toml")).expect("remove the descriptor");
    let Err(err) = build_headless_app(RunOptions::new(dir.clone())) else {
        panic!("the app built without its module's descriptor");
    };
    let err = err.to_string();
    assert!(err.contains("module 'widgets'"), "{err}");
    assert!(err.contains("lumen-module.toml"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `path` naming the library file reads the descriptor beside it.
#[test]
fn a_path_to_a_library_reads_the_descriptor_in_its_directory() {
    let dir = app("library-path", "library-path-gauge");
    let lib: &Path = Path::new("widgets/libwidgets.so");
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n\n\
             [dependencies]\nwidgets = {{ path = \"{}\" }}\n",
            lib.display()
        ),
    )
    .expect("lumen.toml");
    let compiled = compile_app(&dir).expect("the app compiles");
    assert_eq!(compiled.ir.root.children[0].tag, "library-path-gauge");
    let _ = std::fs::remove_dir_all(&dir);
}
