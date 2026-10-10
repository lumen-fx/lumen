// Exercises `lumenc::check_app` and the linked runtime, which lumenc only
// exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! A compile error in a script names the file and line the author wrote: the
//! script file for a `<script src>`, and the markup file for an inline block,
//! in every language. The program a language runs is all of its pieces joined
//! into one text, so without the map back an error named the markup file at a
//! line of the joined text.

use std::path::{Path, PathBuf};

use lumenc::{RunOptions, build_headless_app};

/// Write `files` (path relative to the app root, contents) into a fresh app
/// directory named after `tag`.
fn write_app(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen_script_error_locations_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    for (path, body) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }
    dir
}

/// The error `lumenc check` reports for the app in `dir`.
fn check_error(dir: &Path) -> String {
    lumenc::check_app(dir)
        .expect_err("the script does not compile")
        .to_string()
}

/// The load failure a run of the app in `dir` records.
fn load_error(dir: &Path) -> String {
    let (app, _window) =
        build_headless_app(RunOptions::new(dir.to_path_buf())).expect("build_headless_app");
    app.world
        .get_resource::<lumen_script::ScriptLoadFailure>()
        .map(|f| f.0.clone())
        .expect("the script failed to load")
}

fn at(dir: &Path, file: &str, line: u32) -> String {
    format!("{}:{line}:", dir.join(file).display())
}

const CANDELA_BROKEN: &str = "import \"lumen.cdl\";\nfn on_ready() {\n    let n = 1;\n    \
                              missing_function(n);\n}\nfn main() {}\n";

#[test]
fn a_candela_error_in_one_script_file_names_that_file() {
    let dir = write_app(
        "candela_one",
        &[
            (
                "src/main.lmn",
                "<root>\n  <script src=\"main.cdl\" />\n</root>\n",
            ),
            ("src/main.cdl", CANDELA_BROKEN),
        ],
    );
    let err = check_error(&dir);
    assert!(err.contains(&at(&dir, "src/main.cdl", 4)), "{err}");
    assert!(!err.contains("main.lmn"), "{err}");
}

#[test]
fn a_candela_error_in_the_second_of_two_files_names_it_at_its_own_line() {
    let dir = write_app(
        "candela_two",
        &[
            (
                "src/main.lmn",
                "<root>\n  <script src=\"a.cdl\" />\n  <script src=\"b.cdl\" />\n</root>\n",
            ),
            (
                "src/a.cdl",
                "import \"lumen.cdl\";\nfn helper() -> int {\n    return 1;\n}\nfn main() {}\n",
            ),
            (
                "src/b.cdl",
                "fn on_ready() {\n    missing_function(helper());\n}\n",
            ),
        ],
    );
    let err = check_error(&dir);
    assert!(err.contains(&at(&dir, "src/b.cdl", 2)), "{err}");
}

#[test]
fn an_error_in_an_inline_block_names_its_line_in_the_markup() {
    let dir = write_app(
        "candela_inline",
        &[(
            "src/main.lmn",
            "<root>\n  <label text=\"hi\" />\n  <script>\nimport \"lumen.cdl\";\nfn on_ready() \
             {\n    missing_function(1);\n}\nfn main() {}\n  </script>\n</root>\n",
        )],
    );
    let err = check_error(&dir);
    assert!(err.contains(&at(&dir, "src/main.lmn", 6)), "{err}");
}

const RHAI_A: &str = "fn helper() {\n    1\n}\n";
const RHAI_B: &str = "fn on_ready() {\n    let x = ;\n}\n";

#[test]
fn a_rhai_error_names_its_file_when_checked_and_when_run() {
    let dir = write_app(
        "rhai_two",
        &[
            (
                "src/main.lmn",
                "<root>\n  <script src=\"a.rhai\" />\n  <script src=\"b.rhai\" />\n</root>\n",
            ),
            ("src/a.rhai", RHAI_A),
            ("src/b.rhai", RHAI_B),
        ],
    );
    let want = at(&dir, "src/b.rhai", 2);
    let err = check_error(&dir);
    assert!(err.contains(&want), "check: {err}");
    let err = load_error(&dir);
    assert!(err.contains(&want), "run: {err}");
}

const LUA_A: &str = "function helper()\n    return 1\nend\n";
const LUA_B: &str = "function on_ready()\n    local x = = 1\nend\n";

#[test]
fn a_lua_error_names_its_file_when_checked_and_when_run() {
    let dir = write_app(
        "lua_two",
        &[
            (
                "src/main.lmn",
                "<root>\n  <script src=\"a.lua\" />\n  <script src=\"b.lua\" />\n</root>\n",
            ),
            ("src/a.lua", LUA_A),
            ("src/b.lua", LUA_B),
        ],
    );
    let want = at(&dir, "src/b.lua", 2);
    let err = check_error(&dir);
    assert!(err.contains(&want), "check: {err}");
    let err = load_error(&dir);
    assert!(err.contains(&want), "run: {err}");
}

/// A language that ships as source compiles when the packaged app starts, so
/// the artifact carries the map and the error still names the script file.
#[test]
fn a_rhai_error_in_a_compiled_artifact_names_its_file() {
    let dir = write_app(
        "rhai_artifact",
        &[
            (
                "src/main.lmn",
                "<root>\n  <script src=\"a.rhai\" />\n  <script src=\"b.rhai\" />\n</root>\n",
            ),
            ("src/a.rhai", RHAI_A),
            ("src/b.rhai", RHAI_B),
        ],
    );
    let compiled = lumenc::compile_app(&dir).expect("a source-form language compiles as text");
    let bytes = lumen_ir::artifact::serialize(&compiled).expect("serialize");
    let (app, _window) = build_headless_app(RunOptions::new(&dir).with_artifact_bytes(bytes))
        .expect("build_headless_app");
    let err = app
        .world
        .get_resource::<lumen_script::ScriptLoadFailure>()
        .map(|f| f.0.clone())
        .expect("the script failed to load");
    assert!(err.contains(&at(&dir, "src/b.rhai", 2)), "{err}");
}
