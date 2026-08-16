// `check_app` runs through the linked runtime, so a thin
// (`--no-default-features`) build compiles this file out.
#![cfg(feature = "dev-run")]

//! The complete programs in the documentation compile.
//!
//! A reader copies a code block out of the docs and expects it to run. Most
//! blocks are excerpts, though, and an excerpt cannot compile on its own, so
//! the check needs a rule for which blocks are whole programs.
//!
//! **A candela block is a whole script when it carries the prelude import
//! line `import "lumen.cdl";`.** That import is the first line of every
//! script the repo ships and the line that puts the Lumen surface in scope,
//! so a block with it is offering itself as something to copy. Leave it out
//! and the block is an excerpt: it is skipped, and the surrounding prose
//! explains where the lines go.
//!
//! **A markup block is a whole document when it opens `<root` and its last
//! line closes it.** A `<root dir="rtl">` shown on its own to name an
//! attribute is an excerpt and is skipped.
//!
//! Both are compiled with `lumenc check`, which is the same front end
//! `lumenc run` uses. A markup document gets a stub written for every file
//! its `src` attributes name, so what fails is the block rather than the
//! files a page cannot ship.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The line that puts the Lumen host surface in scope. A candela block
/// carrying it is a whole script.
const PRELUDE_IMPORT: &str = r#"import "lumen.cdl";"#;

/// Markup a whole document opens with.
const DOCUMENT_OPEN: &str = "<root";

/// Markup a whole document closes with.
const DOCUMENT_CLOSE: &str = "</root>";

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/lumenc`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/lumenc sits two levels below the workspace root")
        .to_path_buf()
}

/// One fenced block, with where to point when it fails.
struct Block {
    /// `docs/docs/guides/styling.md:122`, the line the fence opens on.
    origin: String,
    body: String,
}

/// Every fenced block in the documentation git tracks.
fn doc_blocks() -> Vec<Block> {
    let root = workspace_root();
    let listing = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "--full-name", "docs/docs"])
        .output()
        .unwrap_or_else(|e| panic!("ask git which docs are tracked: {e}"));
    assert!(
        listing.status.success(),
        "git ls-files docs/docs failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );

    let mut blocks = Vec::new();
    for page in String::from_utf8_lossy(&listing.stdout).lines() {
        if !page.ends_with(".md") {
            continue;
        }
        let text =
            std::fs::read_to_string(root.join(page)).unwrap_or_else(|e| panic!("read {page}: {e}"));
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            // An opening fence names a language; the closing one is bare.
            if !lines[i].starts_with("```") || lines[i].trim_end() == "```" {
                i += 1;
                continue;
            }
            let open = i;
            i += 1;
            while i < lines.len() && !lines[i].starts_with("```") {
                i += 1;
            }
            blocks.push(Block {
                origin: format!("{page}:{}", open + 1),
                body: lines[open + 1..i].join("\n"),
            });
            i += 1;
        }
    }
    assert!(!blocks.is_empty(), "the documentation has code blocks");
    blocks
}

/// Write a throwaway app directory and hand it to `lumenc check`. `Err`
/// carries what a reader copying the block would see.
fn check_app(label: &str, files: &[(&str, &str)]) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "lumenc_doc_snippet_{}_{}",
        std::process::id(),
        label.replace(['/', '.', ':'], "_")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the snippet app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    for (rel, body) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the snippet subdirectory");
        }
        std::fs::write(&path, body).unwrap_or_else(|e| panic!("write {rel}: {e}"));
    }

    let out = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("check")
        .arg(&dir)
        .output()
        .unwrap_or_else(|e| panic!("check {label}: {e}"));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{label}\n{}", stderr.trim_end()))
    }
}

/// Fail with every block that did not compile, so one run names them all.
fn report(failures: Vec<String>, checked: usize, what: &str) {
    assert!(checked > 0, "the docs show {what}");
    assert!(
        failures.is_empty(),
        "{} of {checked} {what} do not compile as written, so a reader \
         copying one gets this:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Files a markup document names but a page cannot ship. A stub keeps the
/// failure about the block.
fn stubs_for(body: &str) -> Vec<(String, &'static str)> {
    let mut stubs = Vec::new();
    for src in body
        .split("src=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
    {
        // `src="icons/{icon}.png"` is a fragment marker, filled per use.
        if src.contains('{') || Path::new(src).is_absolute() {
            continue;
        }
        let stub = match Path::new(src).extension().and_then(|e| e.to_str()) {
            Some("lmn") => "<label text=\"stub\"/>\n",
            Some("cdl") => "import \"lumen.cdl\";\n\nfn main() {}\n",
            Some("rhai" | "lua") => "\n",
            // An image is read when it is drawn, not when it is compiled.
            _ => continue,
        };
        stubs.push((src.to_string(), stub));
    }
    stubs
}

/// Every complete candela script in the documentation compiles.
#[test]
fn every_whole_candela_snippet_compiles() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for block in doc_blocks() {
        if !block.body.contains(PRELUDE_IMPORT) {
            continue;
        }
        checked += 1;
        if let Err(e) = check_app(
            &block.origin,
            &[
                (
                    "main.lmn",
                    "<root>\n  <script src=\"main.cdl\" />\n</root>\n",
                ),
                ("main.cdl", &block.body),
            ],
        ) {
            failures.push(e);
        }
    }
    report(failures, checked, "whole candela scripts");
}

/// Every complete markup document in the documentation compiles.
#[test]
fn every_whole_markup_snippet_compiles() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for block in doc_blocks() {
        let body = block.body.trim();
        if !body.starts_with(DOCUMENT_OPEN) || !body.ends_with(DOCUMENT_CLOSE) {
            continue;
        }
        let stubs = stubs_for(body);
        let mut files: Vec<(&str, &str)> = vec![("main.lmn", &block.body)];
        files.extend(stubs.iter().map(|(rel, stub)| (rel.as_str(), *stub)));
        checked += 1;
        if let Err(e) = check_app(&block.origin, &files) {
            failures.push(e);
        }
    }
    report(failures, checked, "whole markup documents");
}
