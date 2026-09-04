//! Resolving a tree's translatable text into one language.
//!
//! A page reads in its language with nothing running, so the translation
//! happens to the tree before a document is written from it rather than to
//! the document afterwards. A build does this once per locale; a server
//! embedding the emitter does it once per locale it holds a tree for.

use lumen_i18n::{SharedI18n, translated_or_authored};
use lumen_ir::layout_ir::{Element, LayoutIR};

/// `ir` with every `translatable` element's text resolved through `i18n`.
///
/// This is the same no-argument lookup markup gets at run time: a key with no
/// message in the catalogue falls back to the element's authored text, and
/// then to the key.
pub fn translate_ir(ir: &LayoutIR, i18n: &SharedI18n) -> LayoutIR {
    let mut out = ir.clone();
    translate(&mut out.root, i18n);
    out
}

fn translate(element: &mut Element, i18n: &SharedI18n) {
    if let Some(key) = element.attrs.translatable.clone() {
        element.attrs.text = Some(translated_or_authored(
            i18n.try_t(&key),
            element.attrs.text.as_deref(),
            &key,
        ));
    }
    for child in &mut element.children {
        translate(child, i18n);
    }
}

#[cfg(test)]
mod tests {
    use lumen_i18n::{I18n, LanguageIdentifier};
    use lumen_ir::layout_ir::Attributes;

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
