//! Ahead-of-time extraction of the `lmn!` blocks a candela script writes.
//!
//! A shipped app parses no markup, so every block a script can instantiate is
//! compiled here and travels in the artifact's fragment table. Extraction runs
//! wherever an app is read from source: `lumenc run`, `lumenc build`, `lumenc
//! check`, and each hot reload, so a malformed block fails the check rather
//! than the window.
//!
//! What a block means is decided in one place,
//! [`lumen_script_candela::lmn`], which the macro expander inside the candela
//! compiler reads as well. This module adds the half that needs the markup
//! front-end: the block body becomes a
//! [`Fragment`](lumen_ir::fragment::Fragment) through the same element builder
//! a `<template>` goes through.

use lumen_ir::fragment::{FragmentComponent, FragmentOrigin, FragmentTable};
use lumen_ir::layout_ir::ParseError;
use lumen_script_candela::lmn;

/// Read every `lmn!` block in one candela source into a fragment table.
///
/// `uri` is where the source came from, and lands on each fragment's origin so
/// a later collision can name the file and line.
///
/// A block that is a single component element declares no fragment: it stands
/// for that component's own call.
///
/// A block written as the whole body of a capitalized function carries that
/// function's name, which is what lets markup write the function as a tag.
///
/// # Errors
///
/// A rendered message when a block is malformed, naming the file, line, and
/// column the block was written at.
pub fn script_fragments(source: &str, uri: &str) -> Result<FragmentTable, String> {
    let mut table = FragmentTable::new();
    let index = lmn::FnIndex::scan(source);
    for region in lmn::regions(source) {
        let at = region.body_start;
        let block = lmn::analyze(region.body)
            .map_err(|e| located(source, at + e.offset, uri, &e.message))?;
        if block.lone_component {
            continue;
        }
        let (line, col) = crate::parser_html::line_col_of(source, at);
        let origin = FragmentOrigin {
            file: uri.to_string(),
            line: line as u32,
            col: col as u32,
        };
        let mut fragment = crate::parser_html::fragment_from_markup(
            &block.markup,
            &block.key,
            &block.args,
            origin,
        )
        .map_err(|e| located(source, at, uri, &render(&e)))?;
        if let Some(component) = lmn::component_at(source, &region.span, &index) {
            fragment.components.push(FragmentComponent {
                name: component.name,
                inlinable: component.inlinable,
            });
        }
        table
            .insert(fragment)
            .map_err(|e| located(source, at, uri, &e.to_string()))?;
    }
    Ok(table)
}

/// A parse failure's own words, without the `xml parse error:` label the
/// [`Display`](std::fmt::Display) impl adds: [`located`] puts the block's file
/// and line in front of it, and one position per message is enough.
fn render(error: &ParseError) -> String {
    match error {
        ParseError::Xml(message) => message.clone(),
        other => other.to_string(),
    }
}

/// Render a message with the file and position it is about.
fn located(source: &str, offset: usize, uri: &str, message: &str) -> String {
    let (line, col) = crate::parser_html::line_col_of(source, offset);
    format!("{uri}:{line}:{col}: lmn!: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_becomes_a_fragment_keyed_by_its_body() {
        let src = "fn Home(name) { return lmn!(<label text=\"home for $name\"/>); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        let key = lmn::key_of("<label text=\"home for $name\"/>");
        let fragment = table.get(&key).expect("the block is in the table");
        assert_eq!(fragment.kind, lumen_ir::fragment::FragmentKind::Markup);
        assert_eq!(fragment.params.len(), 1);
        assert_eq!(fragment.params[0].name, "name");
        assert_eq!(fragment.body.len(), 1);
        assert_eq!(fragment.origins[0].file, "main.cdl");
        assert_eq!(fragment.origins[0].line, 1);
    }

    #[test]
    fn an_argument_marker_resolves_from_the_arguments() {
        use lumen_ir::layout_ir::InterpolationSlot;
        let src = "fn Home(name) { return lmn!(<label text=\"$name\"/>); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        let key = lmn::key_of("<label text=\"$name\"/>");
        let fragment = table.get(&key).expect("the block is in the table");
        assert_eq!(
            fragment.body[0].interpolations,
            [InterpolationSlot::Arg("name".to_string())]
        );
    }

    #[test]
    fn a_signal_marker_stays_global() {
        use lumen_ir::layout_ir::InterpolationSlot;
        let src = "fn Home() { return lmn!(<label text=\"{count}\"/>); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        let key = lmn::key_of("<label text=\"{count}\"/>");
        let fragment = table.get(&key).expect("the block is in the table");
        assert_eq!(
            fragment.body[0].interpolations,
            [InterpolationSlot::Global("count".to_string())]
        );
        assert!(fragment.params.is_empty());
    }

    #[test]
    fn a_component_leaves_a_slot_in_the_body() {
        let src = "fn App() { return lmn!(<column><Home/></column>); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        let key = lmn::key_of("<column><Home/></column>");
        let fragment = table.get(&key).expect("the block is in the table");
        assert_eq!(fragment.body[0].children.len(), 1);
        assert_eq!(
            fragment.body[0].children[0].attrs.slot_name.as_deref(),
            Some(lmn::slot_name(0).as_str())
        );
    }

    #[test]
    fn a_block_that_is_one_component_declares_nothing() {
        let src = "fn App() { return lmn!(<Home name=\"bob\"/>); }";
        assert!(
            script_fragments(src, "main.cdl")
                .expect("extracts")
                .is_empty()
        );
    }

    #[test]
    fn two_blocks_with_the_same_body_share_one_fragment() {
        let src = "fn A() { return lmn!(<label text=\"hi\"/>); }\n\
                   fn B() { return lmn!(<label text=\"hi\"/>); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        assert_eq!(table.len(), 1);
        assert_eq!(
            table
                .get(&lmn::key_of("<label text=\"hi\"/>"))
                .expect("the shared block")
                .origins
                .len(),
            2
        );
    }

    /// Keys are content-addressed, so two different bodies never claim one
    /// naturally. If they ever did, the table refuses rather than picking
    /// whichever body it saw first.
    #[test]
    fn two_bodies_under_one_key_collide() {
        let origin = FragmentOrigin {
            file: "main.cdl".to_string(),
            line: 1,
            col: 1,
        };
        let one = crate::parser_html::fragment_from_markup(
            "<label text=\"a\"/>",
            "shared",
            &[],
            origin.clone(),
        )
        .expect("a body");
        let other =
            crate::parser_html::fragment_from_markup("<label text=\"b\"/>", "shared", &[], origin)
                .expect("a body");
        let mut table = FragmentTable::new();
        table.insert(one).expect("first");
        table
            .insert(other)
            .expect_err("a different body under the same key is a collision");
    }

    #[test]
    fn whitespace_around_a_body_does_not_change_its_key() {
        let src = "fn A() { return lmn!(  <label text=\"hi\"/>\n  ); }";
        let table = script_fragments(src, "main.cdl").expect("extracts");
        assert!(table.get(&lmn::key_of("<label text=\"hi\"/>")).is_some());
    }

    #[test]
    fn a_block_with_two_roots_is_refused() {
        let src = "fn A() { return lmn!(<label/><label/>); }";
        let err = script_fragments(src, "main.cdl").expect_err("two roots");
        assert!(err.contains("one root element"), "{err}");
    }

    #[test]
    fn a_malformed_block_names_its_line() {
        let src = "fn A() {\n  let x = 1;\n  return lmn!(<label);\n}";
        let err = script_fragments(src, "main.cdl").expect_err("malformed");
        assert!(err.contains("main.cdl:3:"), "{err}");
    }

    #[test]
    fn a_component_with_children_names_its_line() {
        let src = "fn A() {\n  return lmn!(<column><Card><label/></Card></column>);\n}";
        let err = script_fragments(src, "main.cdl").expect_err("children");
        assert!(err.contains("main.cdl:2:"), "{err}");
        assert!(err.contains("no markup children"), "{err}");
    }
}
