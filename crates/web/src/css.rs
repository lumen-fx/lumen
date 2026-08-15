//! Writing a compiled stylesheet out as the site's `styles.css`.
//!
//! The rules come from the cascade Lumen already resolved, not from the
//! attributes it resolved them into: a stylesheet still has the selectors,
//! the states and the media queries an author wrote, and the browser is
//! perfectly able to run that cascade itself. What it cannot do is agree
//! with Lumen about which of two rules wins, because a Lumen tag becomes a
//! class here and a class outranks a tag. Two things settle that: every
//! tag selector is wrapped so it counts for nothing, and the rules are
//! written out in the order Lumen's own cascade put them in, so wherever
//! the browser sees a tie it breaks it the way Lumen did.
//!
//! What each property becomes is [`lumen_html::style`]'s to say. This
//! module decides where the result goes.

use std::collections::BTreeMap;

use lumen_core::palette::Palette;
use lumen_html::style::{Emission, WebDecl, rewrite_property};
use lumen_html::web_names;
use lumen_ir::css::{
    Origin, Rule, Specificity, Stylesheet, media_query_to_css, palette_root_css, selector_to_web,
};

use crate::spec::CssMode;

/// The stylesheet every emitted site starts with: the browser defaults
/// Lumen does not share, and the per-tag defaults Lumen bakes into markup
/// rather than into CSS.
pub const RESET_CSS: &str = include_str!("reset.css");

/// The whole `styles.css` for a site.
///
/// In [`CssMode::Computed`] the file is the reset alone: the elements
/// carry what the cascade resolved as inline styles instead, and a second
/// copy of the rules would only argue with them.
pub fn styles_css(sheet: Option<&Stylesheet>, mode: CssMode) -> String {
    let mut out = String::from(RESET_CSS);
    if mode == CssMode::Computed {
        return out;
    }
    let Some(sheet) = sheet else {
        return out;
    };
    if palette_missing(sheet) {
        out.push('\n');
        out.push_str(&palette_root_css());
    }
    out.push('\n');
    out.push_str(&rules_css(sheet));
    out
}

/// Every rule of `sheet`, in cascade order.
pub fn rules_css(sheet: &Stylesheet) -> String {
    let mut blocks: Vec<Block> = sheet.rules.iter().flat_map(blocks_for).collect();
    blocks.sort_by_key(|block| block.key);

    let mut out = String::new();
    let mut open: Option<&str> = None;
    for block in &blocks {
        let media = block.media.as_deref();
        if media != open {
            if open.is_some() {
                out.push_str("}\n");
            }
            if let Some(query) = media {
                out.push_str("@media ");
                out.push_str(query);
                out.push_str(" {\n");
            }
            open = media;
        }
        for rule in &block.rules {
            write_rule(&mut out, rule, media.is_some());
        }
    }
    if open.is_some() {
        out.push_str("}\n");
    }
    out
}

/// One rule as it is written out.
struct OutRule {
    selector: String,
    decls: Vec<(WebDecl, bool)>,
}

/// A rule and the state rules it generated, which stay beside it.
struct Block {
    key: SortKey,
    media: Option<String>,
    rules: Vec<OutRule>,
}

/// Where a block lands in the cascade. The browser reads a wrapped tag
/// selector as weighing nothing, so any two rules it cannot tell apart are
/// separated by the order they are written in, which is this.
type SortKey = (Origin, Specificity, usize, usize);

/// The blocks one source rule becomes.
///
/// A rule whose selectors do not all weigh the same is split, one block
/// per weight, because a single position in the file cannot stand for two
/// places in the cascade. Selectors that do weigh the same stay together.
fn blocks_for(rule: &Rule) -> Vec<Block> {
    let names = web_names();
    let mut groups: BTreeMap<Specificity, Vec<String>> = BTreeMap::new();
    let mut order: Vec<Specificity> = Vec::new();
    for selector in &rule.selectors {
        let specificity = selector.specificity();
        if !groups.contains_key(&specificity) {
            order.push(specificity);
        }
        groups
            .entry(specificity)
            .or_default()
            .push(selector_to_web(selector, &names));
    }

    let media = rule.media.as_ref().map(media_query_to_css);
    let (plain, states) = split_declarations(rule);
    order
        .into_iter()
        .enumerate()
        .filter_map(|(index, specificity)| {
            let selectors = groups.get(&specificity)?;
            let mut rules = Vec::new();
            if !plain.is_empty() {
                rules.push(OutRule {
                    selector: selectors.join(", "),
                    decls: plain.clone(),
                });
            }
            for (pseudo, decls) in &states {
                rules.push(OutRule {
                    selector: selectors
                        .iter()
                        .map(|s| format!("{s}{pseudo}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    decls: decls.clone(),
                });
            }
            if rules.is_empty() {
                return None;
            }
            Some(Block {
                key: (rule.origin, specificity, rule.source_order, index),
                media: media.clone(),
                rules,
            })
        })
        .collect()
}

/// The rule's declarations, split into the ones that stay on it and the
/// ones that need a rule of their own, each keeping source order.
type Declarations = Vec<(WebDecl, bool)>;

fn split_declarations(rule: &Rule) -> (Declarations, Vec<(&'static str, Declarations)>) {
    let mut plain: Declarations = Vec::new();
    let mut states: Vec<(&'static str, Declarations)> = Vec::new();
    for decl in &rule.declarations {
        match rewrite_property(&decl.name, &decl.value) {
            Emission::Plain(written) => {
                plain.extend(written.into_iter().map(|d| (d, decl.important)));
            }
            Emission::CustomProp(written) => plain.push((written, decl.important)),
            Emission::StateRule { pseudo, decls } => {
                let written = decls.into_iter().map(|d| (d, decl.important));
                match states.iter_mut().find(|(p, _)| *p == pseudo) {
                    Some((_, existing)) => existing.extend(written),
                    None => states.push((pseudo, written.collect())),
                }
            }
            Emission::Drop(_) => {}
        }
    }
    (plain, states)
}

fn write_rule(out: &mut String, rule: &OutRule, nested: bool) {
    let indent = if nested { "  " } else { "" };
    out.push_str(indent);
    out.push_str(&rule.selector);
    out.push_str(" {\n");
    for (decl, important) in &rule.decls {
        out.push_str(indent);
        out.push_str("  ");
        out.push_str(&decl.name);
        out.push_str(": ");
        out.push_str(&decl.value);
        if *important {
            out.push_str(" !important");
        }
        out.push_str(";\n");
    }
    out.push_str(indent);
    out.push_str("}\n");
}

/// True when the sheet does not already carry the built-in palette.
///
/// Both compile paths fold the palette in as ordinary `:root` rules
/// before they hand the stylesheet on, so this is normally false and the
/// text is not prepended. A stylesheet assembled by hand still gets the
/// tokens the shipped skins are written against.
fn palette_missing(sheet: &Stylesheet) -> bool {
    let present = sheet.root_vars();
    Palette::adwaita_light()
        .root_vars()
        .keys()
        .any(|name| !present.contains_key(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_is_the_whole_file_in_computed_mode() {
        assert_eq!(styles_css(None, CssMode::Computed), RESET_CSS);
    }

    #[test]
    fn a_site_with_no_stylesheet_still_gets_the_reset() {
        assert_eq!(styles_css(None, CssMode::Sheet), RESET_CSS);
    }
}
