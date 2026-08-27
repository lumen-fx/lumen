// Names `lumenc::parser_html::KNOWN_TAGS` and `lumenc::parse_css`, both of
// which a parser-free (`--no-default-features`) build drops.
#![cfg(feature = "runtime-parse")]

//! What the web target's CSS has to agree with, checked from the side
//! that owns the originals.
//!
//! The emitter cannot see either of these. One is the set of per-tag
//! defaults the markup parser bakes into `Attributes` instead of writing
//! as CSS, which crates/web/src/reset.css copies. The other is the skins
//! this crate parses, which are the largest body of Lumen CSS there is and
//! the first thing a missing property rewrite would show up in.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use lumen_html::html_tag_for;
use lumen_html::style::{Emission, UNKNOWN_PROPERTY, WebDecl, rewrite_property};
use lumen_ir::css::STYLE_PROPERTIES;
use lumen_web::{RESET_CSS, rules_css};
use lumenc::parse_css;
use lumenc::parser_html::KNOWN_TAGS;

/// Tags with nothing to reset. A `<tile>` and a `<div>` are plain boxes
/// with no default of their own, and an `<if>` takes the same direction
/// every element does, so the rule that covers every element covers them.
const NO_DEFAULTS: &[&str] = &["tile", "div", "if"];

fn skins() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/runtime/src/skins")
        .canonicalize()
        .expect("the skins directory is where it was");
    let mut found: Vec<(String, String)> = fs::read_dir(&dir)
        .expect("the skins directory reads")
        .filter_map(|entry| {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_none_or(|ext| ext != "css") {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            Some((name, fs::read_to_string(&path).expect("a skin reads")))
        })
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no skins found in {}", dir.display());
    found
}

/// The property names in an emitted stylesheet, custom properties aside.
fn emitted_properties(css: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in css.lines() {
        let line = line.trim();
        if line.starts_with("--") || line.ends_with('{') || line.starts_with('@') {
            continue;
        }
        if let Some((name, _)) = line.split_once(':') {
            names.insert(name.trim().to_string());
        }
    }
    names
}

/// Property names that mean something only to Lumen: the ones the
/// rewriter does not write out under the name they were authored with.
fn lumen_only_properties() -> BTreeSet<String> {
    STYLE_PROPERTIES
        .iter()
        .filter(|name| {
            !matches!(
                rewrite_property(name, "1"),
                Emission::Plain(ref decls) if decls.iter().any(|WebDecl { name: written, .. }| written == *name)
            )
        })
        .map(|name| (*name).to_string())
        .collect()
}

#[test]
fn every_tag_with_a_default_has_it_in_the_reset() {
    for tag in KNOWN_TAGS {
        if html_tag_for(tag).is_none() {
            // The parser resolves it away before the IR exists, so no
            // document ever carries it.
            continue;
        }
        if NO_DEFAULTS.contains(tag) {
            assert!(
                !RESET_CSS.contains(&format!(".lm-{tag}")),
                "`{tag}` is listed as having no defaults but the reset styles it"
            );
            continue;
        }
        assert!(
            RESET_CSS.contains(&format!(".lm-{tag}")),
            "`<{tag}>` reaches a document and the reset says nothing about it"
        );
    }
}

#[test]
fn the_reset_only_names_tags_that_reach_a_document() {
    for tag in KNOWN_TAGS {
        if html_tag_for(tag).is_some() {
            continue;
        }
        assert!(
            !RESET_CSS.contains(&format!(".lm-{tag}")),
            "the reset styles `{tag}`, which the parser resolves away"
        );
    }
}

#[test]
fn no_shipped_skin_leaves_a_lumen_property_in_the_output() {
    let lumen_only = lumen_only_properties();
    for (name, source) in skins() {
        let sheet = parse_css(&source).unwrap_or_else(|e| panic!("`{name}` parses: {e}"));
        let emitted = rules_css(&sheet);
        for property in emitted_properties(&emitted) {
            assert!(
                !lumen_only.contains(&property),
                "`{name}` emits `{property}`, which only Lumen reads"
            );
        }
    }
}

/// A property the rewriter has no answer for is dropped silently, so a
/// missing rewrite costs a whole declaration and says nothing about it. The
/// focus rings every skin writes went that way once.
#[test]
fn no_shipped_skin_writes_a_property_the_rewriter_has_no_answer_for() {
    for (name, source) in skins() {
        let sheet = parse_css(&source).unwrap_or_else(|e| panic!("`{name}` parses: {e}"));
        for rule in &sheet.rules {
            for decl in &rule.declarations {
                assert!(
                    !matches!(
                        rewrite_property(&decl.name, &decl.value),
                        Emission::Drop(UNKNOWN_PROPERTY)
                    ),
                    "`{name}` writes `{}`, which reaches no browser and says so nowhere",
                    decl.name
                );
            }
        }
    }
}

#[test]
fn every_shipped_skin_still_says_something_after_the_rewrite() {
    for (name, source) in skins() {
        let sheet = parse_css(&source).unwrap_or_else(|e| panic!("`{name}` parses: {e}"));
        let emitted = rules_css(&sheet);
        assert!(
            emitted.contains(" {\n  "),
            "`{name}` emitted no rule with anything in it:\n{emitted}"
        );
        assert_eq!(emitted, rules_css(&sheet), "`{name}` emits the same twice");
    }
}
