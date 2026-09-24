//! Reading a module's web half: every shape `lumen-addon.toml` can take that
//! the reader refuses, and the one path spelling it tidies.

use std::path::PathBuf;

use lumen_modules::addon::{ADDON_MANIFEST, WEB_DIR, read_web_half};

/// A module root whose web half holds `files` and a descriptor reading
/// `text`, for one test.
fn package(test: &str, files: &[&str], text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-modules-addon-descriptor-{}-{test}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let web = dir.join(WEB_DIR);
    for file in files {
        let path = web.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, "").expect("write");
    }
    std::fs::create_dir_all(&web).expect("mkdir");
    std::fs::write(web.join(ADDON_MANIFEST), text).expect("write the descriptor");
    dir
}

/// What reading a web half with `echo.js` and `echo.css` beside a descriptor
/// reading `text` says.
fn refusal(test: &str, text: &str) -> String {
    let dir = package(test, &["echo.js", "echo.css"], text);
    let Err(err) = read_web_half("echo", &dir) else {
        panic!("the descriptor is refused");
    };
    let _ = std::fs::remove_dir_all(&dir);
    err
}

#[test]
fn a_namespace_is_an_identifier() {
    let err = refusal(
        "ns-shape",
        "[addon]\nnamespace = \"Echo-1\"\nmodule = \"echo.js\"\n",
    );
    assert!(
        err.contains("namespace `Echo-1` must be lowercase"),
        "{err}"
    );
}

#[test]
fn the_module_is_a_javascript_module() {
    let err = refusal(
        "module-kind",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.css\"\n",
    );
    assert!(err.contains("must be a JavaScript module"), "{err}");
}

#[test]
fn a_module_path_through_the_current_directory_is_written_without_it() {
    let dir = package(
        "curdir",
        &["lib/echo.mjs"],
        "[addon]\nnamespace = \"echo\"\nmodule = \"./lib/./echo.mjs\"\n",
    );
    let package = read_web_half("echo", &dir).unwrap();
    assert_eq!(package.module, "lib/echo.mjs");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_function_name_is_an_identifier_and_declared_once() {
    let err = refusal(
        "fn-shape",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \"Shout\"\n",
    );
    assert!(err.contains("function `Shout` must be lowercase"), "{err}");
    let err = refusal(
        "fn-twice",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \
         \"shout\"\n[[function]]\nname = \"shout\"\n",
    );
    assert!(err.contains("function `shout` is declared twice"), "{err}");
}

#[test]
fn a_parameter_needs_a_name_and_a_type_either_side_of_the_colon() {
    for (test, spelling) in [
        ("no-name", ": string"),
        ("no-type", "text:"),
        ("bad-name", "Text: string"),
    ] {
        let err = refusal(
            test,
            &format!(
                "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \
                 \"shout\"\nparams = [\"{spelling}\"]\n"
            ),
        );
        assert!(
            err.contains("must be written `name: type`"),
            "{spelling}: {err}"
        );
    }
}

#[test]
fn an_async_event_is_an_identifier() {
    let err = refusal(
        "event-shape",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \
         \"later\"\nasync = \"on-echo\"\n",
    );
    assert!(err.contains("event `on-echo` must be lowercase"), "{err}");
}

#[test]
fn an_element_is_declared_once_and_stands_for_an_html_element() {
    let err = refusal(
        "element-twice",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[element]]\ntag = \
         \"echo-view\"\nhtml = \"div\"\n[[element]]\ntag = \"echo-view\"\nhtml = \"span\"\n",
    );
    assert!(
        err.contains("element `echo-view` is declared twice"),
        "{err}"
    );
    let err = refusal(
        "element-html",
        "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[element]]\ntag = \
         \"echo-view\"\nhtml = \"<div>\"\n",
    );
    assert!(err.contains("must be an HTML element name"), "{err}");
}
