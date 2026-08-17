//! Lifting a style written on an element into a rule of its own.
//!
//! Lumen ranks a styling attribute above every rule that targets the element.
//! A browser ranks a rule above everything but an inline declaration, so the
//! obvious way to match Lumen is to write the attributes inline. That closes
//! two doors the author needs open: an inline declaration cannot carry
//! `:hover`, and a script setting one style replaces every inline declaration
//! on the node.
//!
//! So the styling an element carries becomes a class and a rule. The class
//! goes into the IR, which is what makes this work for a row the browser
//! builds after the page has loaded: the row is spawned from the same
//! template, carries the same class, and the rule is already in the file. A
//! rule keyed on the node's path could not reach it, because that path did
//! not exist when the site was built.
//!
//! Where the rules go is [`crate::css`]'s to say; what outranks what is the
//! whole point, and it is decided by layer rather than by `!important`.

use std::collections::BTreeMap;

use lumen_html::style::WebDecl;
use lumen_html::{MarkupRules, markup_rules};
use lumen_ir::layout_ir::Element;

/// Prefix of the class an element carries for the style written on it.
const STYLE_CLASS_PREFIX: &str = "lm-s";

/// Every rule lifted out of a tree, by the class that selects it.
///
/// Two elements written with the same styling share one entry, so a list of
/// a hundred identically styled rows costs one rule.
#[derive(Debug, Clone, Default)]
pub struct MarkupSheet {
    rules: BTreeMap<String, MarkupRules>,
}

impl MarkupSheet {
    /// True when no element in the tree was styled on itself.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every class and what it declares, in a fixed order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &MarkupRules)> {
        self.rules
            .iter()
            .map(|(class, rules)| (class.as_str(), rules))
    }
}

/// Move the styling written on each element of `root` into a class, and hand
/// back the rules those classes stand for.
///
/// The tree is left holding the classes, so an artifact written after this
/// carries them and the browser spawns rows already wearing their styling.
pub fn lift(root: &mut Element) -> MarkupSheet {
    let mut sheet = MarkupSheet::default();
    lift_element(root, &mut sheet);
    sheet
}

fn lift_element(element: &mut Element, sheet: &mut MarkupSheet) {
    let rules = markup_rules(&element.attrs);
    if !rules.is_empty() {
        let class = format!("{STYLE_CLASS_PREFIX}{:016x}", fingerprint(&rules));
        if !element.attrs.classes.contains(&class) {
            element.attrs.classes.push(class.clone());
        }
        sheet.rules.entry(class).or_insert(rules);
    }
    for child in &mut element.children {
        lift_element(child, sheet);
    }
}

/// A number standing for what a set of rules declares.
///
/// FNV-1a over the declarations as they will be written. Two elements agree
/// on a class exactly when they would have written the same rule, and the
/// value does not move between builds or between machines.
fn fingerprint(rules: &MarkupRules) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    eat_decls(&mut hash, &rules.base);
    for (pseudo, decls) in &rules.states {
        eat(&mut hash, pseudo.as_bytes());
        eat(&mut hash, b"{");
        eat_decls(&mut hash, decls);
        eat(&mut hash, b"}");
    }
    hash
}

fn eat_decls(hash: &mut u64, decls: &[WebDecl]) {
    for decl in decls {
        eat(hash, decl.name.as_bytes());
        eat(hash, b":");
        eat(hash, decl.value.as_bytes());
        eat(hash, b";");
    }
}

fn eat(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(tag: &str) -> Element {
        Element {
            tag: tag.to_string(),
            ..Element::default()
        }
    }

    fn styled(tag: &str, styles: &[(&str, &str)]) -> Element {
        let mut element = bare(tag);
        element.attrs.markup_styles = styles
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        element
    }

    #[test]
    fn a_styled_element_leaves_holding_a_class_the_sheet_declares() {
        let mut root = styled("root", &[("bg", "#101014")]);
        let sheet = lift(&mut root);

        let class = root
            .attrs
            .classes
            .first()
            .expect("the element kept the class it was given");
        assert!(class.starts_with(STYLE_CLASS_PREFIX), "{class}");
        let (named, rules) = sheet.iter().next().expect("one rule");
        assert_eq!(named, class);
        assert_eq!(rules.base, vec![WebDecl::new("background", "#101014")]);
    }

    #[test]
    fn two_elements_written_the_same_way_share_one_rule() {
        let mut root = bare("root");
        root.children = vec![
            styled("tile", &[("bg", "#101014")]),
            styled("tile", &[("bg", "#101014")]),
        ];
        let sheet = lift(&mut root);

        assert_eq!(sheet.iter().count(), 1);
        assert_eq!(
            root.children[0].attrs.classes,
            root.children[1].attrs.classes
        );
    }

    #[test]
    fn elements_written_differently_do_not() {
        let mut root = bare("root");
        root.children = vec![
            styled("tile", &[("bg", "#101014")]),
            styled("tile", &[("bg", "#202024")]),
        ];
        let sheet = lift(&mut root);

        assert_eq!(sheet.iter().count(), 2);
        assert_ne!(
            root.children[0].attrs.classes,
            root.children[1].attrs.classes
        );
    }

    #[test]
    fn a_state_written_on_an_element_reaches_the_sheet() {
        let mut root = styled("tile", &[("hover-bg", "#222")]);
        let sheet = lift(&mut root);

        let (_, rules) = sheet.iter().next().expect("one rule");
        assert!(rules.base.is_empty());
        assert_eq!(rules.states[0].0, ":hover");
    }

    #[test]
    fn an_unstyled_tree_lifts_nothing_and_keeps_its_classes() {
        let mut root = bare("root");
        root.attrs.classes = vec!["card".to_string()];
        let sheet = lift(&mut root);

        assert!(sheet.is_empty());
        assert_eq!(root.attrs.classes, vec!["card".to_string()]);
    }

    #[test]
    fn lifting_a_tree_twice_leaves_it_holding_the_class_once() {
        let mut root = styled("tile", &[("bg", "#101014")]);
        lift(&mut root);
        let classes = root.attrs.classes.clone();
        lift(&mut root);
        assert_eq!(root.attrs.classes, classes);
    }
}
