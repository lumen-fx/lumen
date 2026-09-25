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
//!
//! A sheet also carries text the cascade never read: the at-rules Lumen
//! does not implement and a browser does. Those are written back out as
//! authored, and the files one of them names travel with the site, so the
//! `url()` rewriting the build needs lives here too.

use std::collections::{BTreeMap, BTreeSet};

use lumen_core::palette::Palette;
use lumen_html::style::{
    Emission, WebDecl, is_bare_number, is_length_property, lengths, rewrite_property,
};
use lumen_html::web_names;
use lumen_ir::css::{
    Origin, Rule, Specificity, Stylesheet, canonical_property_name, media_query_to_css,
    palette_root_css, selector_to_web,
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

/// The custom properties `bg-fit` writes, registered as not inherited. On
/// the desktop `bg-fit` applies to the element it is written on and nothing
/// else, and an unregistered custom property would pass a parent's value
/// down to every background image under it.
const BG_FIT_PROPERTIES: &str = "@property --lm-bg-size { syntax: \"*\"; inherits: false; }\n\
@property --lm-bg-position { syntax: \"*\"; inherits: false; }\n";

/// The whole `styles.css` for a site.
///
/// In [`CssMode::Computed`] the file is the reset and the at-rules a
/// resolved style still needs beside it, such as the `@font-face` naming
/// the family an element carries. The rules themselves are left out: the
/// elements carry what the cascade resolved as inline styles instead, and a
/// second copy of the rules would only argue with them.
pub fn styles_css(sheet: Option<&Stylesheet>, markup: &MarkupSheet, mode: CssMode) -> String {
    let mut out = String::from(LAYER_ORDER);
    out.push_str(BG_FIT_PROPERTIES);
    if let Some(sheet) = sheet {
        out.push_str(&at_rules_css(sheet, mode));
    }
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

/// At-rule names the web target writes out, each with whether a file
/// carrying the resolved cascade instead of the rules ([`CssMode::Computed`])
/// still needs it. A name a stylesheet carries and this list does not is
/// left out of the file rather than written blind.
///
/// A font is needed in either mode: an element carries the family name the
/// cascade resolved, and the `@font-face` block beside it is the only thing
/// that says where that family's file is. A keyframe block is not: what
/// starts an animation is a rule, and the computed file has no rules.
const EMITTED_AT_RULES: &[(&str, bool)] = &[("keyframes", false), ("font-face", true)];

/// The at-rules the sheet carried, back in the form they were authored in.
///
/// A carried block is browser CSS, not Lumen's dialect: the emitter writes
/// the body through untouched, so a keyframe declares `background` and
/// `transform` rather than `bg`, and a `@font-face` names its file the way
/// the build left it.
///
/// They land at top level, outside every layer. Layer order is what decides
/// which of two same-named blocks wins, and an app has one source of them,
/// so a layer would add a rule with nothing to settle.
fn at_rules_css(sheet: &Stylesheet, mode: CssMode) -> String {
    let mut out = String::new();
    let mut open: Option<String> = None;
    for at_rule in &sheet.at_rules {
        let emitted = EMITTED_AT_RULES
            .iter()
            .find(|(name, _)| *name == at_rule.name)
            .is_some_and(|(_, computed)| mode == CssMode::Sheet || *computed);
        if !emitted {
            continue;
        }
        // Neighbours under the same query share one wrapper, the way
        // `rules_css` groups its blocks.
        let media = at_rule.media.as_ref().map(media_query_to_css);
        if media != open {
            if open.is_some() {
                out.push_str("}\n");
            }
            if let Some(query) = &media {
                out.push_str("@media ");
                out.push_str(query);
                out.push_str(" {\n");
            }
            open = media;
        }
        out.push('@');
        out.push_str(&at_rule.name);
        if !at_rule.prelude.is_empty() {
            out.push(' ');
            out.push_str(&at_rule.prelude);
        }
        out.push_str(" {");
        out.push_str(&at_rule.body);
        out.push_str("}\n");
    }
    if open.is_some() {
        out.push_str("}\n");
    }
    out
}

/// Rewrite every `url()` in `css`, leaving one `resolve` answers `None` for
/// as authored.
///
/// A replacement is written double-quoted whatever form the author used:
/// one form is always valid, and a name a build chose is not one anybody
/// has to quote by hand. A `local()` source names a font installed on the
/// reader's machine rather than a file, so nothing here touches it.
pub fn rewrite_css_urls(css: &str, mut resolve: impl FnMut(&str) -> Option<String>) -> String {
    // Lowercasing ASCII leaves every byte offset where it was, so a match
    // found here indexes the text the author wrote.
    let lower = css.to_ascii_lowercase();
    let mut out = String::new();
    let mut at = 0usize;
    while let Some(found) = lower[at..].find("url(") {
        let start = at + found;
        let open = start + "url(".len();
        let Some(close) = closing_paren(&css[open..]).map(|i| open + i) else {
            break;
        };
        // A longer name ending in the same four characters, such as a
        // custom `burl(`, is not the function this rewrites.
        if css[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            out.push_str(&css[at..=close]);
            at = close + 1;
            continue;
        }
        let inner = css[open..close].trim();
        let named = inner
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .or_else(|| {
                inner
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
            })
            .unwrap_or(inner);
        out.push_str(&css[at..start]);
        match resolve(named) {
            Some(target) => {
                out.push_str("url(\"");
                out.push_str(&target);
                out.push_str("\")");
            }
            None => out.push_str(&css[start..=close]),
        }
        at = close + 1;
    }
    out.push_str(&css[at..]);
    out
}

/// The byte index of the `)` closing an already-opened `(`, ignoring one
/// written inside a string.
fn closing_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b')' => return Some(i),
                _ => {}
            },
        }
    }
    None
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
    /// Tokens that hold a `url()`, directly or through another token. A
    /// `bg` reading one of these is an image and says how it is placed; a
    /// `bg` reading any other token is a colour or a gradient and is written
    /// as one declaration.
    images: BTreeSet<String>,
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
        let mut images: BTreeSet<String> = BTreeSet::new();
        for rule in &sheet.rules {
            for declaration in &rule.declarations {
                let referenced = var_names(&declaration.value);
                if declaration.name.starts_with("--") {
                    if declaration.value.to_ascii_lowercase().contains("url(") {
                        images.insert(declaration.name.clone());
                    }
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
        // An image flows the other way along an alias: `--a: var(--b)` holds
        // an image when `--b` does.
        let mut changed = true;
        while changed {
            changed = false;
            for (name, referenced) in &aliases {
                if !images.contains(name) && referenced.iter().any(|r| images.contains(r)) {
                    images.insert(name.clone());
                    changed = true;
                }
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
            images,
            ambiguous,
        }
    }

    /// The declarations `bg: value` becomes in this sheet. The rewrite on its
    /// own cannot see what a token holds, so it places every `var()` as if it
    /// were an image; here, a token the sheet never points at an image stays
    /// one plain `background`.
    fn background(&self, value: &str) -> Emission {
        let value = value.trim();
        let image = value.to_ascii_lowercase().contains("url(")
            || var_names(value)
                .iter()
                .any(|name| self.images.contains(name));
        if image {
            rewrite_property("bg", value)
        } else {
            Emission::Plain(vec![WebDecl::new("background", value)])
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
        let emission = if canonical_property_name(&decl.name) == "bg" {
            tokens.background(&decl.value)
        } else {
            rewrite_property(&decl.name, &decl.value)
        };
        match emission {
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
            format!("{LAYER_ORDER}{BG_FIT_PROPERTIES}@layer lumen.reset {{\n{RESET_CSS}}}\n")
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
