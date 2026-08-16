//! `lumenc web` end to end, on the apps in this repository.
//!
//! Each case runs the real binary against a real app and reads what landed on
//! disk: the file set, the documents themselves, and whether a link in one
//! points at another. The browser runtime stands in as two files in a
//! directory named by `--lib-dir`, so a build here copies what it is given
//! and never reaches the network.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use lumenc::web_serve::Server;

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
    std::fs::create_dir_all(&app).expect("create the app directory");
    std::fs::write(
        app.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\nid = \"lumen.test.exports\"\n\n[script]\nengine = \
         \"candela\"\n",
    )
    .expect("write lumen.toml");
    std::fs::write(
        app.join("main.lmn"),
        "<root>\n  <label bind-text=\"label\" />\n  <script src=\"main.cdl\" />\n</root>\n",
    )
    .expect("write the markup");
    std::fs::write(
        app.join("main.cdl"),
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

    let server = Server::bind(&out, "/", 0).expect("bind a free port");
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
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
            .args(&args)
            .output()
            .expect("running lumenc web");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }

    // Rendering a page by booting the app is not implemented; say so rather
    // than quietly rendering it some other way.
    let output = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["web"])
        .arg(repo().join("apps/pages-demo"))
        .arg("--out")
        .arg(out.join("site"))
        .args(["--prerender", "run"])
        .output()
        .expect("running lumenc web");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not implemented yet"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_list_named_in_the_config_is_in_the_document() {
    let scratch = scratch("seed-rows");
    let app = scratch.join("app");
    std::fs::create_dir_all(&app).expect("create the app directory");
    std::fs::write(
        app.join("main.lmn"),
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
