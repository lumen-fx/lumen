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

use std::collections::{BTreeMap, BTreeSet};

use lumen_core::palette::Palette;
use lumen_html::style::{
    Emission, WebDecl, is_bare_number, is_length_property, lengths, rewrite_property,
};
use lumen_html::web_names;
use lumen_ir::css::{
    Origin, Rule, Specificity, Stylesheet, media_query_to_css, palette_root_css, selector_to_web,
};

use crate::markup::MarkupSheet;
use crate::spec::CssMode;

/// The stylesheet every emitted site starts with: the browser defaults
/// Lumen does not share, and the per-tag defaults Lumen bakes into markup
/// rather than into CSS.
pub const RESET_CSS: &str = include_str!("reset.css");

/// The layers the file declares, weakest first.
///
/// A normal declaration in no layer at all beats one in any layer, whatever
/// the selectors weigh. That is what puts a style written on an element above
/// the stylesheet without `!important`, and `!important` is what an author
/// needs left free to animate: a declaration marked important cannot be
/// overridden by `:hover`, a media query or a keyframe.
const LAYER_ORDER: &str = "@layer lumen.reset, lumen.sheet;\n";

/// The whole `styles.css` for a site.
///
/// In [`CssMode::Computed`] the file is the reset alone: the elements
/// carry what the cascade resolved as inline styles instead, and a second
/// copy of the rules would only argue with them.
pub fn styles_css(sheet: Option<&Stylesheet>, markup: &MarkupSheet, mode: CssMode) -> String {
    let mut out = String::from(LAYER_ORDER);
    layer(&mut out, "lumen.reset", RESET_CSS);
    if mode == CssMode::Computed {
        return out;
    }
    if let Some(sheet) = sheet {
        let mut authored = String::new();
        if palette_missing(sheet) {
            authored.push_str(&palette_root_css());
            authored.push('\n');
        }
        authored.push_str(&rules_css(sheet));
        layer(&mut out, "lumen.sheet", &authored);
    }
    out.push_str(&markup_css(markup));
    out
}

/// Wrap `body` in `@layer <name>`.
fn layer(out: &mut String, name: &str, body: &str) {
    out.push_str("@layer ");
    out.push_str(name);
    out.push_str(" {\n");
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("}\n");
}

/// The rules lifted off the elements, written in no layer so they outrank
/// the stylesheet.
fn markup_css(markup: &MarkupSheet) -> String {
    let mut out = String::new();
    for (class, rules) in markup.iter() {
        write_decls(&mut out, &format!(".{class}"), &rules.base);
        for (pseudo, decls) in &rules.states {
            write_decls(&mut out, &format!(".{class}{pseudo}"), decls);
        }
    }
    out
}

/// One rule, or nothing when it would declare nothing.
fn write_decls(out: &mut String, selector: &str, decls: &[WebDecl]) {
    if decls.is_empty() {
        return;
    }
    out.push_str(selector);
    out.push_str(" {\n");
    for decl in decls {
        out.push_str("  ");
        out.push_str(&decl.name);
        out.push_str(": ");
        out.push_str(&decl.value);
        out.push_str(";\n");
    }
    out.push_str("}\n");
}

/// What a sheet's own custom properties leave the emitter unable to write
/// out correctly.
///
/// A token the sheet uses as a length in one place and as a plain number in
/// another has no one value that reads right in both, so it is written as it
/// was authored and the length uses are the ones that break.
#[must_use]
pub fn token_warnings(sheet: Option<&Stylesheet>, mode: CssMode) -> Vec<String> {
    if mode == CssMode::Computed {
        return Vec::new();
    }
    let Some(sheet) = sheet else {
        return Vec::new();
    };
    Tokens::of(sheet)
        .ambiguous
        .iter()
        .map(|name| {
            format!(
                "`{name}` is used both where a bare number means pixels and where it means a \
                 plain number, so it is written out as authored; give the two uses their own \
                 tokens, or write this one with a unit"
            )
        })
        .collect()
}

/// Every rule of `sheet`, in cascade order.
pub fn rules_css(sheet: &Stylesheet) -> String {
    let tokens = Tokens::of(sheet);
    let mut blocks: Vec<Block> = sheet
        .rules
        .iter()
        .flat_map(|rule| blocks_for(rule, &tokens))
        .collect();
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

/// The custom properties of one sheet that hold a length.
///
/// Lumen reads a bare number in a length as pixels, so an app writes
/// `--radius: 16` and means 16 pixels. A browser reads the same declaration
/// as the number 16, and drops `border-radius: var(--radius)` as invalid.
/// The unit has to go on somewhere, and the definition is the only place it
/// can go: a use site is `var(--radius)` whatever the token holds, and the
/// same token reaches inline styles and the nodes the browser runtime builds,
/// none of which the stylesheet can reach back into.
///
/// A token counts as a length when the sheet uses it in a property whose
/// bare numbers are pixels, and never in one whose are not. A token used
/// both ways cannot be both, so it is left alone and reported: whichever
/// unit went on would be wrong somewhere.
#[derive(Debug, Default)]
pub struct Tokens {
    lengths: BTreeSet<String>,
    /// Tokens the sheet uses as a length in one place and as a plain number
    /// in another.
    pub ambiguous: BTreeSet<String>,
}

impl Tokens {
    /// Read `sheet` for the custom properties that hold a length.
    #[must_use]
    pub fn of(sheet: &Stylesheet) -> Self {
        let mut lengths = BTreeSet::new();
        let mut others = BTreeSet::new();
        // Only a token written as a bare number is in question at all: a
        // colour or a value that already carries its unit reads the same
        // wherever it lands.
        let mut bare: BTreeSet<String> = BTreeSet::new();
        let mut united: BTreeSet<String> = BTreeSet::new();
        // `--a: var(--b)` passes whatever `--a` is on to `--b`, and the use
        // that decides `--a` may be read after this definition, so the two
        // sets are grown to a fixed point rather than in one pass.
        let mut aliases: Vec<(String, Vec<String>)> = Vec::new();
        for rule in &sheet.rules {
            for declaration in &rule.declarations {
                let referenced = var_names(&declaration.value);
                if declaration.name.starts_with("--") {
                    if !referenced.is_empty() {
                        aliases.push((declaration.name.clone(), referenced));
                    }
                    let set = if is_bare_number(&declaration.value) {
                        &mut bare
                    } else {
                        &mut united
                    };
                    set.insert(declaration.name.clone());
                    continue;
                }
                let set = if is_length_property(&declaration.name) {
                    &mut lengths
                } else {
                    &mut others
                };
                set.extend(referenced);
            }
        }
        let mut changed = true;
        while changed {
            changed = false;
            for (name, referenced) in &aliases {
                for set in [&mut lengths, &mut others] {
                    if !set.contains(name) {
                        continue;
                    }
                    for target in referenced {
                        changed |= set.insert(target.clone());
                    }
                }
            }
        }
        // A token defined twice, once bare and once with a unit, is already
        // written the way its author meant it in one of the two places; only
        // one written bare everywhere is missing anything.
        let candidates: BTreeSet<String> = bare.difference(&united).cloned().collect();
        let ambiguous: BTreeSet<String> = lengths
            .intersection(&others)
            .filter(|name| candidates.contains(*name))
            .cloned()
            .collect();
        Self {
            lengths: lengths
                .difference(&others)
                .filter(|name| candidates.contains(*name))
                .cloned()
                .collect(),
            ambiguous,
        }
    }

    /// The value `declaration` is written with: a token that holds a length
    /// gains the unit its numbers were written without.
    fn value_of(&self, name: &str, value: &str) -> String {
        if self.lengths.contains(name) {
            lengths(value)
        } else {
            value.to_string()
        }
    }
}

/// The custom properties `value` reads, in the order they appear.
fn var_names(value: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find("var(") {
        rest = &rest[at + "var(".len()..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(rest.len());
        let (name, tail) = rest.split_at(end);
        if name.starts_with("--") {
            names.push(name.to_string());
        }
        rest = tail;
    }
    names
}

/// The blocks one source rule becomes.
///
/// A rule whose selectors do not all weigh the same is split, one block
/// per weight, because a single position in the file cannot stand for two
/// places in the cascade. Selectors that do weigh the same stay together.
fn blocks_for(rule: &Rule, tokens: &Tokens) -> Vec<Block> {
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
    let (plain, states) = split_declarations(rule, tokens);
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

fn split_declarations(
    rule: &Rule,
    tokens: &Tokens,
) -> (Declarations, Vec<(&'static str, Declarations)>) {
    let mut plain: Declarations = Vec::new();
    let mut states: Vec<(&'static str, Declarations)> = Vec::new();
    for decl in &rule.declarations {
        match rewrite_property(&decl.name, &decl.value) {
            Emission::Plain(written) => {
                plain.extend(written.into_iter().map(|d| (d, decl.important)));
            }
            Emission::CustomProp(mut written) => {
                written.value = tokens.value_of(&written.name, &written.value);
                plain.push((written, decl.important));
            }
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
        let emitted = styles_css(None, &MarkupSheet::default(), CssMode::Computed);
        assert_eq!(
            emitted,
            format!("{LAYER_ORDER}@layer lumen.reset {{\n{RESET_CSS}}}\n")
        );
    }

    #[test]
    fn a_site_with_no_stylesheet_still_gets_the_reset() {
        let emitted = styles_css(None, &MarkupSheet::default(), CssMode::Sheet);
        assert!(emitted.contains("box-sizing: border-box"), "{emitted}");
    }

    #[test]
    fn the_file_names_its_layers_before_it_fills_them() {
        let emitted = styles_css(None, &MarkupSheet::default(), CssMode::Sheet);
        let order = emitted
            .find("@layer lumen.reset,")
            .expect("the layer order");
        let reset = emitted
            .find("@layer lumen.reset {")
            .expect("the reset layer");
        assert!(
            order < reset,
            "a layer used before it is named takes the order it was used in:\n{emitted}"
        );
    }
}
