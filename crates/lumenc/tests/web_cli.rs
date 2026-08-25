//! `lumenc web` end to end, on the apps in this repository.
//!
//! Each case runs the real binary against a real app and reads what landed on
//! disk: the file set, the documents themselves, and whether a link in one
//! points at another. The browser runtime stands in as two files in a
//! directory named by `--lib-dir`, so a build here copies what it is given
//! and never reaches the network.

use std::collections::BTreeSet;
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use lumenc::web_serve::{LOOPBACK, Server};

/// The repository this test is built from.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/lumenc sits two levels under the repository")
        .to_path_buf()
}

/// A fresh directory of its own for one case.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-web-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// A stand-in for the prebuilt browser runtime: what a build copies is not
/// this test's subject, only that it copies it.
fn runtime_dir(scratch: &Path) -> PathBuf {
    let dir = scratch.join("lib");
    std::fs::create_dir_all(&dir).expect("create the runtime directory");
    std::fs::write(dir.join("lumen-web.wasm"), b"\0asm\x01\0\0\0").expect("write the wasm stub");
    std::fs::write(dir.join("lumen-web.js"), b"export function boot() {}\n")
        .expect("write the module stub");
    dir
}

/// Build `app` into `out`, and hand back what the command printed.
fn web(app: &str, out: &Path, extra: &[&str]) -> String {
    let scratch = out.parent().expect("the site sits inside a scratch dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_lumenc"));
    command
        .arg("web")
        .arg(repo().join(app))
        .arg("--out")
        .arg(out)
        .arg("--lib-dir")
        .arg(runtime_dir(scratch))
        .args(extra);
    let output = command.output().expect("running lumenc web");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "lumenc web {app} failed:\n{text}");
    text
}

/// Every file in the site, as paths relative to its root.
fn files(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read the site").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.insert(
                    path.strip_prefix(root)
                        .expect("inside the site")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    found
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Every document in the site.
fn documents(root: &Path) -> Vec<String> {
    files(root)
        .into_iter()
        .filter(|path| path.ends_with(".html"))
        .collect()
}

/// Fail unless every element in `html` is closed, in order.
///
/// An HTML document is not XML - `<img>` and `<meta>` carry no end tag - so
/// this walks the tags rather than handing them to the XML parser.
fn assert_well_formed(name: &str, html: &str) {
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(at) = html[i..].find('<') {
        let start = i + at;
        if html[start..].starts_with("<!") {
            i = start + html[start..].find('>').expect("unterminated declaration") + 1;
            continue;
        }
        let end = start + html[start..].find('>').expect("unterminated tag");
        let inner = &html[start + 1..end];
        i = end + 1;
        if let Some(close) = inner.strip_prefix('/') {
            assert_eq!(
                stack.pop().as_deref(),
                Some(close.trim()),
                "{name}: `</{close}>` closes nothing"
            );
            continue;
        }
        let tag = inner
            .split(|c: char| c.is_whitespace())
            .next()
            .expect("a tag name")
            .to_string();
        assert!(!tag.is_empty(), "{name}: empty tag name");
        if lumen_html::is_void(&tag) {
            continue;
        }
        if tag == "script" || tag == "style" {
            let close = format!("</{tag}>");
            i += html[i..].find(&close).expect("unterminated script") + close.len();
            continue;
        }
        stack.push(tag);
    }
    assert!(stack.is_empty(), "{name}: unclosed elements {stack:?}");
}

/// Every value of one attribute in a document, in document order.
fn attribute_values(html: &str, attribute: &str) -> Vec<String> {
    let needle = format!("{attribute}=\"");
    let mut values = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        let end = rest.find('"').expect("unterminated attribute");
        values.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    values
}

/// Fail unless every node of the page has a path of its own: the browser
/// runtime binds to those, so a repeat would bind the wrong node.
fn assert_paths_are_unique(name: &str, html: &str) {
    let paths = attribute_values(html, "data-lm");
    let mut unique: Vec<String> = paths.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        paths.len(),
        unique.len(),
        "{name}: repeated data-lm {paths:?}"
    );
}

/// Fail unless every link into the site reaches a document, or the shell
/// that answers for a path with no document of its own.
fn assert_links_resolve(root: &Path, base: &str, name: &str, html: &str) {
    for href in attribute_values(html, "href") {
        if !href.starts_with(base) {
            continue;
        }
        let relative = href.trim_start_matches(base);
        if relative.is_empty() {
            continue;
        }
        let target = root.join(relative);
        assert!(
            target.exists() || root.join("404.html").exists(),
            "{name}: `{href}` reaches neither a file nor the shell"
        );
    }
}

fn check_documents(root: &Path, base: &str) {
    for name in documents(root) {
        let html = read(root, &name);
        assert_well_formed(&name, &html);
        assert_paths_are_unique(&name, &html);
        assert_links_resolve(root, base, &name, &html);
    }
}

#[test]
fn a_page_of_the_app_becomes_a_document_of_the_site() {
    let scratch = scratch("pages");
    let out = scratch.join("site");
    web("apps/pages-demo", &out, &[]);

    let files = files(&out);
    for expected in [
        "index.html",
        "settings.html",
        "user.html",
        "404.html",
        "styles.css",
        "lumen.web.json",
        "app.lmna",
        "lumen-web.wasm",
        "lumen-web.js",
    ] {
        assert!(files.contains(expected), "no `{expected}` in {files:?}");
    }
    check_documents(&out, "/");

    let index = read(&out, "index.html");
    // The home page's own content is in the document, not fetched later.
    assert!(
        index.contains("Welcome to the file-based pages demo."),
        "{index}"
    );
    // A link to a page is a link to that page's document; a deeper path
    // stays the path the author wrote, for the app to resolve.
    assert!(index.contains(r#"href="/settings.html""#), "{index}");
    assert!(index.contains(r#"href="/user/42""#), "{index}");
    // A page the visitor is not on is an empty anchor waiting for it.
    let settings = read(&out, "settings.html");
    assert!(
        settings.contains(r#"data-lm-page="settings""#),
        "{settings}"
    );
    assert!(!settings.contains("Welcome to the file-based pages demo."));
}

#[test]
fn a_static_site_is_the_same_pages_with_nothing_to_run_them() {
    let scratch = scratch("static");
    let out = scratch.join("site");
    // No `--lib-dir`: a static site is asked for the pages alone, so there is
    // nothing for the build to go looking for.
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["web"])
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(&out)
        .args(["--render", "static"])
        .output()
        .expect("running lumenc web");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{printed}");
    // A missing runtime is what a `csr` build warns about, and this one was
    // never looking for one.
    assert!(
        !printed.contains("nothing runs in the browser"),
        "{printed}"
    );

    let files = files(&out);
    for expected in ["index.html", "settings.html", "404.html", "styles.css"] {
        assert!(files.contains(expected), "no `{expected}` in {files:?}");
    }
    for absent in [
        "lumen-web.wasm",
        "lumen-web.js",
        "lumen.web.json",
        "app.lmna",
    ] {
        assert!(!files.contains(absent), "`{absent}` in {files:?}");
    }
    check_documents(&out, "/");

    // The markup is all there; what is gone is everything that would run it.
    let index = read(&out, "index.html");
    assert!(
        index.contains("Welcome to the file-based pages demo."),
        "{index}"
    );
    assert!(index.contains(r#"href="/settings.html""#), "{index}");
    assert!(!index.contains("<script"), "{index}");
}

#[test]
fn a_static_site_says_a_runtime_it_was_handed_has_nowhere_to_go() {
    let scratch = scratch("static-lib");
    let out = scratch.join("site");
    let printed = web("apps/pages-demo", &out, &["--render", "static"]);
    assert!(printed.contains("--lib-dir"), "{printed}");
    assert!(!files(&out).contains("lumen-web.wasm"), "{:?}", files(&out));
}

#[test]
fn the_shell_answers_for_a_path_with_no_document() {
    let scratch = scratch("shell");
    let out = scratch.join("site");
    let printed = web("apps/pages-demo", &out, &[]);

    let shell = read(&out, "404.html");
    assert!(shell.contains("data-lm-contract"), "{shell}");
    // It carries the app and shows no page: the address bar picks that.
    assert!(
        !shell.contains("Welcome to the file-based pages demo."),
        "{shell}"
    );
    // A page that reads the leftover part of a path is named, because a
    // plain file server answers those paths through the shell.
    assert!(printed.contains("route.segment"), "{printed}");
    assert!(printed.contains("/user/42"), "{printed}");
}

#[test]
fn an_app_s_assets_travel_with_the_site() {
    let scratch = scratch("assets");
    let out = scratch.join("site");
    web("apps/kanban", &out, &[]);

    let files = files(&out);
    assert!(
        files.iter().any(|path| path.starts_with("assets/")),
        "no assets were copied: {files:?}"
    );
    assert!(files.contains("assets/icons/close.png"), "{files:?}");
    check_documents(&out, "/");
}

#[test]
fn a_candela_app_ships_the_program_the_browser_runs() {
    let scratch = scratch("candela");
    let out = scratch.join("site");
    web("fixtures/candela-smoke", &out, &[]);

    assert!(out.join("app.cdlb").is_file(), "no bytecode was written");
    let manifest = read(&out, "lumen.web.json");
    assert!(manifest.contains(r#""engine": "candela""#), "{manifest}");
    assert!(manifest.contains(r#""path": "app.cdlb""#), "{manifest}");
    assert!(manifest.contains(r#""format": "cdlb""#), "{manifest}");
    check_documents(&out, "/");
}

#[test]
fn a_function_the_program_cannot_be_called_by_is_named() {
    // candela exports a function only when every parameter it takes is
    // annotated. One written without an annotation compiles and ships and is
    // then never called, and on the desktop it works anyway, because the
    // compiler is in the process there. The build has to say so.
    let scratch = scratch("exports");
    let app = scratch.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create the app directory");
    std::fs::write(
        app.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\nid = \"lumen.test.exports\"\n\n[script]\nengine = \
         \"candela\"\n",
    )
    .expect("write lumen.toml");
    std::fs::write(
        app.join("src").join("main.lmn"),
        "<root>\n  <label bind-text=\"label\" />\n  <script src=\"main.cdl\" />\n</root>\n",
    )
    .expect("write the markup");
    std::fs::write(
        app.join("src").join("main.cdl"),
        "import \"lumen.cdl\";\n\nfn calc_label(n) { return \"seen\"; }\n\nfn on_start() {\n    \
         lumen::derive(\"label\", [\"count\"], \"calc_label\");\n}\n\nfn main() {}\n",
    )
    .expect("write the script");

    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(&app)
        .arg("--out")
        .arg(scratch.join("site"))
        .arg("--lib-dir")
        .arg(runtime_dir(&scratch))
        .output()
        .expect("running lumenc web");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("`calc_label`"), "{stderr}");
    assert!(stderr.contains("does not export it"), "{stderr}");
}

#[test]
fn a_base_path_roots_every_reference() {
    let scratch = scratch("base");
    let out = scratch.join("site");
    web("apps/pages-demo", &out, &["--base", "/docs"]);

    let index = read(&out, "index.html");
    assert!(index.contains(r#"href="/docs/styles.css""#), "{index}");
    assert!(index.contains(r#"href="/docs/settings.html""#), "{index}");
    assert!(index.contains(r#"data-lm-base="/docs/""#), "{index}");
    check_documents(&out, "/docs/");
}

#[test]
fn a_locale_gets_a_tree_of_its_own_under_its_tag() {
    let scratch = scratch("locales");
    let out = scratch.join("site");
    web(
        "apps/pages-demo",
        &out,
        &["--locale", "en-US", "--locale", "de-DE"],
    );

    let files = files(&out);
    assert!(files.contains("index.html"), "{files:?}");
    assert!(files.contains("de-DE/index.html"), "{files:?}");
    // What the whole site shares is written once, at its root.
    assert!(!files.contains("de-DE/styles.css"), "{files:?}");
    assert!(!files.contains("de-DE/lumen.web.json"), "{files:?}");

    let german = read(&out, "de-DE/index.html");
    assert!(german.contains(r#"<html lang="de-DE""#), "{german}");
    assert!(
        german.contains(r#"href="/de-DE/settings.html""#),
        "{german}"
    );
    assert!(german.contains(r#"href="/styles.css""#), "{german}");
    check_documents(&out, "/");
}

#[test]
fn two_builds_of_one_app_write_the_same_bytes() {
    let scratch = scratch("repeat");
    let first = scratch.join("first");
    let second = scratch.join("second");
    web("apps/pages-demo", &first, &[]);
    web("apps/pages-demo", &second, &[]);

    assert_eq!(files(&first), files(&second));
    for name in files(&first) {
        assert_eq!(
            std::fs::read(first.join(&name)).expect("read the first build"),
            std::fs::read(second.join(&name)).expect("read the second build"),
            "`{name}` differs between two builds of the same app"
        );
    }
}

#[test]
fn the_served_site_is_what_a_browser_needs() {
    let scratch = scratch("serve");
    let out = scratch.join("site");
    web("apps/pages-demo", &out, &[]);

    let server = Server::bind(&out, "/", LOOPBACK, 0).expect("bind a free port");
    let address = server.addr();
    std::thread::spawn(move || server.run());

    let root = request(address, "/");
    assert!(root.starts_with("HTTP/1.1 200 "), "{root}");
    assert!(root.contains("Content-Type: text/html"), "{root}");
    assert!(root.contains("<!doctype html>"), "{root}");

    // The one that breaks silently: a browser refuses to instantiate a
    // streamed module served as anything else.
    let wasm = request(address, "/lumen-web.wasm");
    assert!(wasm.starts_with("HTTP/1.1 200 "), "{wasm}");
    assert!(wasm.contains("Content-Type: application/wasm"), "{wasm}");

    // A deep path has no file, so it is answered by the shell, with the
    // status a static host would send.
    let deep = request(address, "/user/42");
    assert!(deep.starts_with("HTTP/1.1 404 "), "{deep}");
    assert!(deep.contains("Content-Type: text/html"), "{deep}");
    assert!(deep.contains("data-lm-contract"), "{deep}");
}

/// One GET, start to finish.
fn request(address: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("connect to the server");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("send the request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read the response");
    String::from_utf8_lossy(&response).into_owned()
}

#[test]
fn the_command_says_what_it_cannot_do() {
    let out = scratch("usage");
    for (args, expected) in [
        (vec!["web"], "missing <app_dir>"),
        (vec!["web", "--frobnicate"], "unknown flag"),
        // Rendering per request is a value of --render, and the flag it was
        // once asked for separately is gone rather than kept beside it.
        (vec!["web", "--ssr"], "unknown flag"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
            .args(&args)
            .output()
            .expect("running lumenc web");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }

    // A mode the build does not have is named back rather than swapped for
    // one it does.
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["web"])
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(out.join("site"))
        .args(["--prerender", "sometimes"])
        .output()
        .expect("running lumenc web");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown --prerender mode `sometimes`"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_list_named_in_the_config_is_in_the_document() {
    let scratch = scratch("seed-rows");
    let app = scratch.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create the app directory");
    std::fs::write(
        app.join("src").join("main.lmn"),
        "<root>\n  <for each=\"todos\" key=\"id\">\n    <label class=\"todo\" \
         text=\"{row.title}\" />\n  </for>\n</root>\n",
    )
    .expect("write the markup");
    std::fs::write(
        app.join("lumen.toml"),
        "[app]\nid = \"com.lumen.tests.seed-rows\"\n\n[[web.seed.todos]]\ntitle = \"write it \
         down\"\n\n[[web.seed.todos]]\ntitle = \"do it\"\n",
    )
    .expect("write the config");

    let out = scratch.join("site");
    web(app.to_str().expect("a scratch path is text"), &out, &[]);

    let index = read(&out, "index.html");
    check_documents(&out, "/");
    assert!(
        index.contains("write it down") && index.contains("do it"),
        "the rows the config names are in the page: {index}"
    );
    assert!(
        index.contains(r#"data-lm="0.0::0""#) && index.contains(r#"data-lm="0.0::1""#),
        "each row carries the path the runtime looks it up by: {index}"
    );
    // The runtime starts from the same list the document shows, so the rows
    // are adopted rather than built a second time.
    assert!(
        index.contains(r#""todos":[{"title":"write it down"}"#),
        "{index}"
    );
}

/// An app whose list and whose branch come from its own script, so nothing in
/// the page can be traced back to a declared value.
fn app_that_publishes_on_start(scratch: &Path) -> PathBuf {
    let app = scratch.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create the app directory");
    std::fs::write(
        app.join("src").join("main.lmn"),
        "<root>\n  <if signal=\"loaded\">\n    <label class=\"banner\" text=\"ready\" />\n  \
         </if>\n  <for each=\"todos\" key=\"id\">\n    <label class=\"todo\" \
         text=\"{row.title}\" />\n  </for>\n  <script src=\"main.cdl\" />\n</root>\n",
    )
    .expect("write the markup");
    std::fs::write(
        app.join("src").join("main.cdl"),
        "import \"lumen.cdl\";\n\nfn main() {}\n\nfn on_start() {\n    \
         lumen::signal_set(\"loaded\", \"true\");\n    lumen::signal_array_set(\"todos\", \
         [\n        {\"id\": \"1\", \"title\": \"written by the app\"},\n        {\"id\": \"2\", \
         \"title\": \"and so was this\"}\n    ]);\n}\n",
    )
    .expect("write the script");
    std::fs::write(
        app.join("lumen.toml"),
        "[app]\nid = \"com.lumen.tests.prehydrate\"\n\n[script]\nengine = \
         \"candela\"\n\n[mcp]\nport = 0\n",
    )
    .expect("write the config");
    app
}

#[test]
fn the_state_the_app_settles_into_is_in_the_page() {
    let scratch = scratch("prerender-run");
    let app = app_that_publishes_on_start(&scratch);
    let out = scratch.join("site");
    let report = web(
        app.to_str().expect("a scratch path is text"),
        &out,
        &["--prerender", "run", "--render", "static"],
    );

    let index = read(&out, "index.html");
    check_documents(&out, "/");
    assert!(
        index.contains("written by the app") && index.contains("and so was this"),
        "the list the script published is in the page: {index}"
    );
    assert!(
        index.contains(">ready<"),
        "the branch the script opened is taken: {index}"
    );
    // A static site carries no runtime and so no seed block, and the pages
    // still hold everything the run produced.
    assert!(!index.contains("lm-seed"), "{index}");
    // An app that publishes on start reaches its answer, and reaches the same
    // one twice.
    assert!(
        !report.contains("still changing") && !report.contains("settled differently"),
        "{report}"
    );
}

#[test]
fn a_page_written_from_a_run_carries_the_state_the_runtime_adopts() {
    let scratch = scratch("prerender-run-csr");
    let app = app_that_publishes_on_start(&scratch);
    let out = scratch.join("site");
    web(
        app.to_str().expect("a scratch path is text"),
        &out,
        &["--prerender", "run"],
    );

    let index = read(&out, "index.html");
    assert!(
        index.contains(r#""todos":[{"id":"1","title":"written by the app"}"#),
        "the runtime starts from the list the document shows: {index}"
    );
    assert!(
        index.contains(r#""loaded":{"t":"str","v":"true"}"#),
        "{index}"
    );
}

#[test]
fn an_address_a_run_will_not_ask_for_is_reported() {
    let scratch = scratch("prerender-run-fetch");
    let app = scratch.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create the app directory");
    std::fs::write(
        app.join("src").join("main.lmn"),
        "<root>\n  <label bind-text=\"status\" text=\"\" />\n  <script src=\"main.cdl\" \
         />\n</root>\n",
    )
    .expect("write the markup");
    std::fs::write(
        app.join("src").join("main.cdl"),
        "import \"lumen.cdl\";\n\nfn main() {}\n\nfn on_start() {\n    \
         lumen::fetch(\"https://example.invalid/items.json\", \"items\");\n}\n",
    )
    .expect("write the script");
    std::fs::write(
        app.join("lumen.toml"),
        "[app]\nid = \"com.lumen.tests.prehydrate-fetch\"\n\n[script]\nengine = \
         \"candela\"\n\n[mcp]\nport = 0\n",
    )
    .expect("write the config");

    let out = scratch.join("site");
    let report = web(
        app.to_str().expect("a scratch path is text"),
        &out,
        &["--prerender", "run"],
    );
    assert!(
        report.contains("https://example.invalid/items.json"),
        "the build says which address went unanswered: {report}"
    );
}

#[test]
fn a_served_render_answers_every_link_a_document_carries() {
    let scratch = scratch("serve-ssr-links");
    // The same app written out as documents, which is where the addresses a
    // link in the site produces come from. A rendered site writes none of
    // them, and the two are the same pages either way.
    let built = scratch.join("built");
    web("apps/pages-demo", &built, &["--render", "static"]);

    let out = scratch.join("site");
    let mut child = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(&out)
        .arg("--lib-dir")
        .arg(runtime_dir(&scratch))
        .args(["--render", "ssr", "--serve", "--port", "0"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("running lumenc web --render ssr");
    let address = match serving_at(&mut child) {
        Some(address) => address,
        None => {
            stop(child);
            panic!("the server never said where it was listening");
        }
    };

    // Every document a build of the app writes, which is every URL a link in
    // the site points at. What each one is expected to answer with comes from
    // the file itself, so a page added to the app is walked here without this
    // test being told about it.
    let mut asked: Vec<(String, String)> = documents(&built)
        .into_iter()
        .filter(|name| name != "404.html")
        .map(|name| {
            let document = read(&built, &name);
            let page = page_of(&document)
                .unwrap_or_else(|| panic!("the built `{name}` says which page it is"));
            (format!("/{name}"), page)
        })
        .collect();
    assert!(
        asked.len() > 1,
        "the app under test has more than one page: {asked:?}"
    );

    // The addresses beside the documents: the site root, a page named the way
    // its author wrote it, a path deeper than a page, and a document carrying
    // a query string or a fragment. All of them name a page.
    let entry_page = page_of(&read(&built, "index.html")).expect("the entry document names a page");
    let settings =
        page_of(&read(&built, "settings.html")).expect("the settings document names one");
    let user = page_of(&read(&built, "user.html")).expect("the user document names one");
    asked.push(("/".to_string(), entry_page));
    asked.push(("/settings".to_string(), settings.clone()));
    asked.push(("/settings.html?from=nav".to_string(), settings.clone()));
    asked.push(("/settings.html#top".to_string(), settings));
    asked.push(("/user/42".to_string(), user));

    let answers: Vec<(String, String, String)> = asked
        .into_iter()
        .map(|(path, page)| {
            let answer = request(address, &path);
            (path, page, answer)
        })
        .collect();
    stop(child);

    for (path, page, answer) in answers {
        assert!(answer.starts_with("HTTP/1.1 200 "), "{path}: {answer}");
        assert_eq!(
            page_of(&answer).as_deref(),
            Some(page.as_str()),
            "{path} was answered with another page: {answer}"
        );
    }
}

/// The page a document says it is, read off the `data-lm-page` attribute the
/// emitter writes on `<html>`.
fn page_of(document: &str) -> Option<String> {
    document
        .split_once("data-lm-page=\"")
        .and_then(|(_, rest)| rest.split('"').next())
        .map(str::to_string)
}

#[test]
fn a_served_render_answers_a_path_no_document_stands_for() {
    let scratch = scratch("serve-ssr");
    let out = scratch.join("site");
    let mut child = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(&out)
        .arg("--lib-dir")
        .arg(runtime_dir(&scratch))
        .args(["--render", "ssr", "--serve", "--port", "0"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("running lumenc web --render ssr");
    let address = match serving_at(&mut child) {
        Some(address) => address,
        None => {
            stop(child);
            panic!("the server never said where it was listening");
        }
    };

    // A path deeper than a page, which a file server has nothing for and a
    // render answers with the page it resolves to.
    let deep = request(address, "/user/42");
    stop(child);

    assert!(deep.starts_with("HTTP/1.1 200 "), "{deep}");
    assert!(deep.contains(r#"data-lm-page="user""#), "{deep}");
    // Nothing on disk could have answered it: the pages are the render's, and
    // the directory holds what a render needs beside them.
    assert!(
        documents(&out).is_empty(),
        "a rendered site wrote documents: {:?}",
        documents(&out)
    );
}

#[test]
fn a_rendered_site_is_the_files_a_render_needs_and_no_documents() {
    let scratch = scratch("ssr-files");
    let out = scratch.join("site");
    let printed = web("apps/pages-demo", &out, &["--render", "ssr"]);
    // A build that renders nothing itself says how the pages are reached.
    assert!(printed.contains("--serve"), "{printed}");
    assert!(printed.contains("lumen-ssr"), "{printed}");

    let files = files(&out);
    for expected in [
        "styles.css",
        "app.lmna",
        "lumen.web.json",
        "lumen-web.wasm",
        "lumen-web.js",
    ] {
        assert!(files.contains(expected), "no `{expected}` in {files:?}");
    }
    assert!(
        documents(&out).is_empty(),
        "a rendered site wrote documents: {:?}",
        documents(&out)
    );
}

#[test]
fn a_rendered_site_can_be_asked_for_pages_with_nothing_to_run_them() {
    let scratch = scratch("ssr-no-runtime");
    let out = scratch.join("site");
    let printed = web(
        "apps/pages-demo",
        &out,
        &["--render", "ssr", "--no-runtime"],
    );
    // The runtime the helper handed it has nowhere to go, and the build says
    // so rather than copying it in for nobody.
    assert!(printed.contains("--lib-dir"), "{printed}");

    let files = files(&out);
    // What the server renders from stays; what only a browser would load goes.
    assert!(files.contains("styles.css"), "{files:?}");
    assert!(files.contains("app.lmna"), "{files:?}");
    for absent in [
        "lumen.web.json",
        "lumen-web.wasm",
        "lumen-web.js",
        "app.cdlb",
    ] {
        assert!(!files.contains(absent), "`{absent}` in {files:?}");
    }
    assert!(
        documents(&out).is_empty(),
        "a rendered site wrote documents: {:?}",
        documents(&out)
    );
}

#[test]
fn a_rendered_page_with_no_runtime_carries_nothing_to_run() {
    let scratch = scratch("serve-ssr-no-runtime");
    let out = scratch.join("site");
    let mut child = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("web")
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(&out)
        .args(["--render", "ssr", "--no-runtime", "--serve", "--port", "0"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("running lumenc web --render ssr --no-runtime");
    let address = match serving_at(&mut child) {
        Some(address) => address,
        None => {
            stop(child);
            panic!("the server never said where it was listening");
        }
    };
    let page = request(address, "/settings.html");
    stop(child);

    assert!(page.starts_with("HTTP/1.1 200 "), "{page}");
    // The page is the app's, rendered for this request.
    assert!(page.contains(r#"data-lm-page="settings""#), "{page}");
    // And there is nothing in it to take it over.
    assert!(!page.contains("<script"), "{page}");
    assert!(!page.contains("lumen-web"), "{page}");
}

#[test]
fn a_runtime_setting_that_contradicts_the_render_mode_is_refused() {
    // Each mode that answers the runtime question itself, and the setting
    // that says the opposite. The message names the mode that means it.
    for (mode, flag, named) in [
        ("static", "--runtime", "csr"),
        ("csr", "--no-runtime", "static"),
    ] {
        let out = scratch(&format!("runtime-{mode}"));
        let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
            .args(["web"])
            .arg(repo().join("apps/pages-demo"))
            .arg("--out")
            .arg(out.join("site"))
            .args(["--render", mode, flag])
            .output()
            .expect("running lumenc web");
        assert!(
            !output.status.success(),
            "`--render {mode} {flag}` was built"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("render `{mode}`")), "{stderr}");
        assert!(stderr.contains(&format!("render `{named}`")), "{stderr}");
    }
}

#[test]
fn a_runtime_setting_the_render_mode_agrees_with_is_taken() {
    let scratch = scratch("runtime-agrees");
    let out = scratch.join("site");
    // `static` already means no runtime, so saying it again changes nothing.
    web(
        "apps/pages-demo",
        &out,
        &["--render", "static", "--no-runtime"],
    );
    let files = files(&out);
    assert!(files.contains("index.html"), "{files:?}");
    assert!(!files.contains("lumen-web.wasm"), "{files:?}");
}

#[test]
fn a_page_comes_from_a_run_or_from_the_request_and_not_from_both() {
    let out = scratch("ssr-prerender-run");
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["web"])
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(out.join("site"))
        .args(["--render", "ssr", "--prerender", "run"])
        .output()
        .expect("running lumenc web");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("render `ssr`"), "{stderr}");
    assert!(stderr.contains("prerender `run`"), "{stderr}");
}

/// Stop a served process and collect it, so a case leaves nothing behind.
fn stop(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Read the server's output on a thread of its own, and hand back the address
/// it says it is listening on.
///
/// The reader keeps reading for as long as the server runs, rather than
/// stopping at the line this wants. A pipe nobody reads fills up, and a pipe
/// whose reader has gone is broken for the writer; neither belongs between a
/// test and the process it is asking for pages.
fn serving_at(child: &mut std::process::Child) -> Option<std::net::SocketAddr> {
    let stdout = child.stdout.take()?;
    let (found, address) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(url) = line.split(" at http://").nth(1) {
                let _ = found.send(url.trim_end_matches('/').to_string());
            }
        }
    });
    address
        .recv_timeout(std::time::Duration::from_secs(120))
        .ok()?
        .parse()
        .ok()
}
