//! What a web build copies into the site for a module's web half: every file
//! the descriptor names, once, under one content-named directory, with the
//! integrity values the documents check them against.

use std::path::PathBuf;

use lumen_modules::addon::{ADDON_MANIFEST, WEB_DIR, read_web_half};
use lumenc::addons::site::{SITE_DIR, integrity, ship};

/// A module root whose web half holds `files`, each with its own path as its
/// contents, beside a descriptor reading `descriptor`.
fn package(test: &str, files: &[&str], descriptor: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumenc-addon-ship-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let web = dir.join(WEB_DIR);
    for file in files {
        let path = web.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, file).expect("write");
    }
    std::fs::write(web.join(ADDON_MANIFEST), descriptor).expect("write the descriptor");
    dir
}

#[test]
fn a_package_ships_every_file_it_names_once_under_one_content_root() {
    // The module is named again under `files`, and `assets` stands for
    // everything under it.
    let dir = package(
        "ship",
        &[
            "echo.js",
            "echo.css",
            "early.js",
            "assets/b.txt",
            "assets/sub/a.txt",
        ],
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\nstyles = [\"echo.css\"]\nhead = \
         \"early.js\"\nfiles = [\"assets\", \"echo.js\"]\n",
    );
    let (addon, files) = ship(&read_web_half("echo", &dir).unwrap()).unwrap();

    let root = addon
        .module
        .path
        .strip_suffix("/echo.js")
        .expect("the module sits at the web half's root")
        .to_owned();
    assert!(root.starts_with(&format!("{SITE_DIR}/echo")), "{root}");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let expected: Vec<String> = [
        "assets/b.txt",
        "assets/sub/a.txt",
        "early.js",
        "echo.css",
        "echo.js",
    ]
    .iter()
    .map(|path| format!("{root}/{path}"))
    .collect();
    assert_eq!(paths, expected);
    assert_eq!(files[0].bytes, b"assets/b.txt");
    assert_eq!(addon.module.integrity, integrity(b"echo.js"));
    assert_eq!(addon.styles[0].path, format!("{root}/echo.css"));
    assert_eq!(
        addon.head.as_ref().map(|head| head.integrity.clone()),
        Some(integrity(b"early.js"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_named_file_that_cannot_be_read_is_named_in_the_refusal() {
    // A file removed between reading the descriptor and building the site is
    // reported by path rather than skipped.
    let dir = package(
        "unreadable",
        &["echo.js", "assets/gone.bin"],
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\nfiles = [\"assets\"]\n",
    );
    let read = read_web_half("echo", &dir).unwrap();
    std::fs::remove_file(dir.join("web/assets/gone.bin")).unwrap();
    std::fs::remove_dir(dir.join("web/assets")).unwrap();
    let Err(err) = ship(&read) else {
        panic!("a missing file is refused");
    };
    assert!(err.contains("assets"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
