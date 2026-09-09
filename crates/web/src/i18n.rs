//! Resolving a tree's translatable strings into one language.
//!
//! A page reads in its language with nothing running, so the translation
//! happens to the tree before a document is written from it rather than to
//! the document afterwards. A build does this once per locale; a server
//! embedding the emitter does it once per locale it holds a tree for.

use lumen_i18n::SharedI18n;
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_ir::translate::translate_attrs;

/// `ir` with every `translatable` element's strings resolved through `i18n`.
///
/// This is the same no-argument lookup markup gets at run time, following the
/// same rule the spawner does ([`lumen_ir::translate::translate_attrs`]), so a
/// page built for a locale reads like the app run in it.
pub fn translate_ir(ir: &LayoutIR, i18n: &SharedI18n) -> LayoutIR {
    let mut out = ir.clone();
    translate_element(&mut out.root, i18n);
    out
}

/// Resolve every `translatable` element's strings in one subtree.
///
/// The tree a page is written from goes through [`translate_ir`]; a component
/// body a build read off a `<for>` row arrives on its own and goes through
/// this, so a row's card reads in the same language as the markup around it.
///
/// The resolved strings are written back onto the element, which is how a
/// translated `placeholder`, `title` and `alt` reach the document: the
/// emitter reads all three off the attributes it is handed.
pub fn translate_element(element: &mut Element, i18n: &SharedI18n) {
    let strings = translate_attrs(&element.attrs, &|key| i18n.try_t(key));
    element.attrs.text = strings.text;
    element.attrs.placeholder = strings.placeholder;
    element.attrs.alt = strings.alt;
    if let Some(spec) = &mut element.attrs.tooltip
        && let Some(text) = strings.tooltip
    {
        spec.text = text;
    }
    for child in &mut element.children {
        translate_element(child, i18n);
    }
}

#[cfg(test)]
mod tests {
    use lumen_i18n::{I18n, LanguageIdentifier};
    use lumen_ir::layout_ir::{Attributes, TooltipSpec};

    use super::*;

    fn label(key: &str, text: &str) -> Element {
        Element {
            tag: "label".to_string(),
            attrs: Attributes {
                translatable: Some(key.to_string()),
                text: Some(text.to_string()),
                ..Attributes::default()
            },
            ..Element::default()
        }
    }

    fn german(messages: &str) -> SharedI18n {
        let lang: LanguageIdentifier = "de-DE".parse().expect("a valid tag");
        let mut i18n = I18n::new(lang.clone(), Vec::new());
        i18n.load_ftl(lang, messages).expect("a valid catalogue");
        SharedI18n::new(i18n)
    }

    #[test]
    fn one_key_reaches_a_placeholder_an_alt_and_a_tooltip_body() {
        let mut root = Element {
            tag: "root".to_string(),
            children: vec![
                Element {
                    tag: "input".to_string(),
                    attrs: Attributes {
                        translatable: Some("search".to_string()),
                        placeholder: Some("Search".to_string()),
                        ..Attributes::default()
                    },
                    ..Element::default()
                },
                Element {
                    tag: "image".to_string(),
                    attrs: Attributes {
                        translatable: Some("logo".to_string()),
                        alt: Some("The Lumen logo".to_string()),
                        ..Attributes::default()
                    },
                    ..Element::default()
                },
                Element {
                    tag: "button".to_string(),
                    attrs: Attributes {
                        text: Some("Save".to_string()),
                        tooltip: Some(TooltipSpec {
                            text: "Save the file".to_string(),
                            translatable: Some("save-tip".to_string()),
                            delay_ms: None,
                            offset: None,
                        }),
                        ..Attributes::default()
                    },
                    ..Element::default()
                },
            ],
            ..Element::default()
        };
        translate_element(
            &mut root,
            &german(
                "search =\n    .placeholder = Katalog durchsuchen\n\
                 logo =\n    .alt = Das Lumen-Logo\n\
                 save-tip = Datei speichern\n",
            ),
        );
        assert_eq!(
            root.children[0].attrs.placeholder.as_deref(),
            Some("Katalog durchsuchen")
        );
        // The input names its placeholder, so the key does not become the
        // field's value.
        assert_eq!(root.children[0].attrs.text, None);
        assert_eq!(
            root.children[1].attrs.alt.as_deref(),
            Some("Das Lumen-Logo")
        );
        assert_eq!(
            root.children[2]
                .attrs
                .tooltip
                .as_ref()
                .map(|spec| spec.text.as_str()),
            Some("Datei speichern")
        );
    }

    #[test]
    fn a_key_with_a_message_reads_in_that_language_everywhere_in_the_tree() {
        let ir = LayoutIR {
            root: Element {
                tag: "root".to_string(),
                children: vec![label("greeting", "Hello"), label("missing", "Goodbye")],
                ..Element::default()
            },
            ..LayoutIR::default()
        };
        let out = translate_ir(&ir, &german("greeting = Hallo\n"));
        assert_eq!(out.root.children[0].attrs.text.as_deref(), Some("Hallo"));
        // No message, so the text the author wrote stands.
        assert_eq!(out.root.children[1].attrs.text.as_deref(), Some("Goodbye"));
        // The tree it was resolved from is left as it was.
        assert_eq!(ir.root.children[0].attrs.text.as_deref(), Some("Hello"));
    }
}
