//! The `lmn!` markup blocks a candela script writes, read out of its source
//! before anything runs.
//!
//! A shipped app parses no markup, so every block a script can instantiate is
//! read here and compiled by the app assembly into the fragment table the
//! artifact carries. This module finds the blocks with candela's own macro
//! scanner and hands each one over as a
//! [`MarkupBlock`](lumen_script::MarkupBlock): its markup, its key, the
//! argument sites it binds, and the component function it is the body of. What
//! a block means is decided in one place,
//! [`lumen_candela_host::lmn`], which the macro expander inside the compiler reads
//! as well, so the extraction and the compiled call cannot disagree.

use std::ops::Range;

use lumen_candela_host::lmn;
use lumen_ir::fragment::FragmentComponent;
use lumen_script::{MarkupBlock, MarkupBlockError};

/// One `lmn!( ... )` invocation in a candela source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmnRegion<'a> {
    /// The block body, the text between the parentheses.
    pub body: &'a str,
    /// Byte offset of `body` in the source it was scanned from.
    pub body_start: usize,
    /// Byte range of the whole invocation, `lmn!` through the closing
    /// parenthesis.
    pub span: Range<usize>,
}

/// Every `lmn!( ... )` invocation in a candela source, in source order.
///
/// The scan is candela's own, so a region written inside a string literal or a
/// comment is not one.
#[must_use]
pub fn regions(src: &str) -> Vec<LmnRegion<'_>> {
    candela::macros::scan_regions(src, lmn::MACRO_NAME)
        .into_iter()
        .map(|region| LmnRegion {
            body: region.body,
            body_start: region.body_start,
            span: region.span,
        })
        .collect()
}

/// Read every `lmn!` block in one candela source.
///
/// # Errors
///
/// A block is malformed; the error's offset is into `source`.
pub fn markup_blocks(source: &str) -> Result<Vec<MarkupBlock>, MarkupBlockError> {
    let index = lmn::FnIndex::scan(source);
    let mut blocks = Vec::new();
    for region in regions(source) {
        let at = region.body_start;
        let block = lmn::analyze(region.body).map_err(|e| MarkupBlockError {
            offset: at + e.offset,
            message: e.message,
        })?;
        let component =
            lmn::component_at(source, &region.span, &index, &block.args).map(|component| {
                FragmentComponent {
                    name: component.name,
                    params: component.params,
                    inlinable: component.inlinable,
                }
            });
        blocks.push(MarkupBlock {
            offset: at,
            markup: block.markup,
            key: block.key,
            args: block.args,
            component,
        });
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_candela_host::lmn::{ComponentFn, FnIndex, analyze, component_at};

    fn index(src: &str) -> FnIndex {
        FnIndex::scan(src)
    }

    #[test]
    fn regions_skip_strings_and_comments() {
        let src = "fn a() { return lmn!(<b/>); }\n\
                   fn b() { let s = \"lmn!(<c/>)\"; }\n\
                   // lmn!(<d/>)\n";
        let found: Vec<&str> = regions(src).into_iter().map(|r| r.body).collect();
        assert_eq!(found, ["<b/>"]);
    }

    #[test]
    fn a_region_offset_points_into_the_source() {
        let src = "fn a() { return lmn!(<b/>); }";
        let region = &regions(src)[0];
        assert_eq!(
            &src[region.body_start..region.body_start + region.body.len()],
            "<b/>"
        );
        assert_eq!(&src[region.span.clone()], "lmn!(<b/>)");
    }

    /// One component, read: the block is the whole body and every value in it
    /// came from a parameter, so the build can stand in for the call.
    #[test]
    fn a_forwarded_block_stands_in_for_the_call() {
        let src = "fn Home(name) { return lmn!(<label text=\"$name\"/>); }";
        assert_eq!(
            read_component_fn(src),
            Some(ComponentFn {
                name: "Home".to_string(),
                params: vec!["name".to_string()],
                inlinable: true,
            })
        );
    }

    #[test]
    fn a_return_without_a_trailing_semicolon_is_still_the_whole_body() {
        let src = "fn Home() { return lmn!(<label/>) }";
        assert!(read_component_fn(src).expect("Home").inlinable);
    }

    /// A value the function worked out is not one the caller passed, so the
    /// block cannot stand in for the call and the function has to run.
    #[test]
    fn a_computed_value_needs_the_function_to_run() {
        let src = "fn Greet(n) { let u = upper(n); return lmn!(<label text=\"$u\"/>); }";
        let component = read_component_fn(src).expect("Greet");
        assert_eq!(component.params, ["n"]);
        assert!(!component.inlinable);
    }

    #[test]
    fn two_returns_leave_no_one_block_to_stand_in() {
        let src = "fn Toggle(on) {\n\
                       if on { return lmn!(<label text=\"on\"/>); }\n\
                       return lmn!(<label text=\"off\"/>);\n\
                   }";
        let index = index(src);
        for region in regions(src) {
            let args = analyze(region.body).expect("a block").args;
            let component = component_at(src, &region.span, &index, &args).expect("Toggle");
            assert_eq!(component.name, "Toggle");
            assert!(!component.inlinable, "{region:?}");
        }
    }

    #[test]
    fn a_statement_before_the_return_leaves_no_block_to_stand_in() {
        let src = "fn Home() { let x = 1; return lmn!(<label/>); }";
        assert!(!read_component_fn(src).expect("Home").inlinable);
    }

    #[test]
    fn a_lowercase_function_is_not_a_component() {
        let src = "fn home() { return lmn!(<label/>); }";
        assert_eq!(read_component_fn(src), None);
    }

    /// The component the first block in `src` belongs to.
    fn read_component_fn(src: &str) -> Option<ComponentFn> {
        let region = &regions(src)[0];
        let args = analyze(region.body).expect("a block").args;
        component_at(src, &region.span, &index(src), &args)
    }

    #[test]
    fn a_block_reads_with_its_key_arguments_and_component() {
        let src = "fn Home(name) { return lmn!(<label text=\"$name\"/>); }";
        let blocks = markup_blocks(src).expect("one block");
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.key, lmn::key_of("<label text=\"$name\"/>"));
        assert_eq!(block.args, ["name"]);
        assert_eq!(&src[block.offset..block.offset + 4], "<lab");
        let component = block.component.as_ref().expect("Home is a component");
        assert_eq!(component.name, "Home");
        assert!(component.inlinable);
    }

    #[test]
    fn a_malformed_block_points_into_the_source() {
        let src = "fn A() {\n  return lmn!(<column><Card><label/></Card></column>);\n}";
        let err = markup_blocks(src).expect_err("a component element takes no children");
        assert!(err.message.contains("no markup children"), "{err:?}");
        assert!(
            err.offset >= src.find("<column").expect("the block"),
            "{err:?}"
        );
    }
}
