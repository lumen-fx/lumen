//! Resolving a tree's translatable strings into one language.
//!
//! A page reads in its language with nothing running, so the translation
//! happens to the tree before a document is written from it rather than to
//! the document afterwards. A build does this once per locale; a server
//! embedding the emitter does it once per locale it holds a tree for.
//! [`locale_trees`] is where both build those trees.

use std::sync::Arc;

use lumen_core::components::AuthoredStrings;
use lumen_i18n::{Catalogues, LanguageIdentifier, SharedI18n};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_ir::translate::translate;

use crate::server::PageHead;
use crate::spec::{LocaleSpec, PageSpec, SiteSpec};

/// What a site's trees are built from: the app, its pages, the languages it
/// is written in and what every tree shares.
#[derive(Debug, Clone, Copy)]
pub struct SiteLocales<'a> {
    /// The app's tree, in the text it was authored with.
    pub ir: &'a LayoutIR,
    /// Every page key, in the order the pages are written.
    pub keys: &'a [String],
    /// What each page says about itself. A page with no head keeps the site's
    /// title and description.
    pub heads: &'a [PageHead],
    /// Every locale the site is written in, the one at the site root first.
    pub locales: &'a [String],
    /// The catalogues each tree's text is resolved through.
    pub catalogues: &'a Catalogues,
    /// What every tree shares: the site settings, the assets and the lifted
    /// markup rules. Its pages and locale are ignored.
    pub shared: &'a SiteSpec,
}

/// One tree of a site: the app in one language.
#[derive(Clone)]
pub struct LocaleTree {
    /// Every page of the tree, sharing one translated tree, with its head on.
    pub spec: SiteSpec,
    /// The registry the tree's text was resolved through, for resolving
    /// markup that reaches the tree later, such as a row's component body.
    /// `None` for a locale that is not a language tag, whose tree is in the
    /// text the author wrote.
    pub i18n: Option<SharedI18n>,
}

/// One tree per locale in `site.locales`, the root's first.
///
/// This is how a build writes its locale trees and how a server holding the
/// build's files builds the same ones, so the two cannot drift. Each tree is
/// the app with every `translatable` element resolved for its locale, one
/// [`PageSpec`] per key sharing it, each with its [`PageHead`] applied, and a
/// [`LocaleSpec`] naming the root and every other locale as an alternate.
///
/// A locale that is not a language tag is said in `warnings`, and its tree is
/// written in the text the author wrote.
pub fn locale_trees(site: SiteLocales<'_>, warnings: &mut Vec<String>) -> Vec<LocaleTree> {
    let Some(root) = site.locales.first() else {
        return Vec::new();
    };
    site.locales
        .iter()
        .map(|locale| {
            let i18n = match locale.parse::<LanguageIdentifier>() {
                Ok(_) => site.catalogues.i18n(locale).ok().map(SharedI18n::new),
                Err(e) => {
                    warnings.push(format!("locale `{locale}` is not a valid BCP-47 tag: {e}"));
                    None
                }
            };
            let ir = Arc::new(match &i18n {
                Some(i18n) => translate_ir(site.ir, i18n),
                None => site.ir.clone(),
            });
            let pages = site
                .keys
                .iter()
                .map(|key| {
                    let mut page = PageSpec::new(key.clone(), Arc::clone(&ir));
                    if let Some(head) = site.heads.iter().find(|head| &head.key == key) {
                        head.apply(&mut page);
                    }
                    page
                })
                .collect();
            let spec = SiteSpec {
                pages,
                locale: LocaleSpec {
                    alternates: site
                        .locales
                        .iter()
                        .filter(|other| *other != locale)
                        .cloned()
                        .collect(),
                    default_locale: root.clone(),
                    ..LocaleSpec::new(locale.clone())
                },
                ..site.shared.clone()
            };
            LocaleTree { spec, i18n }
        })
        .collect()
}

/// `ir` with every `translatable` element's strings resolved through `i18n`.
///
/// This is the same no-argument lookup markup gets at run time, following the
/// same rule the spawner does ([`lumen_ir::translate::translate`]), so a
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
    let authored = AuthoredStrings::from(&element.attrs);
    let strings = translate(&authored, &|key| i18n.try_t(key));
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

    /// The arm a selector picks is emitted wrapped in the Unicode isolation
    /// marks, the same string the desktop spawns from the same catalogue. A
    /// page prerendered here and the tree it hydrates into have to agree
    /// byte for byte.
    #[test]
    fn a_selected_arm_is_emitted_bidi_isolated() {
        let mut root = Element {
            tag: "root".to_string(),
            children: vec![label("inbox", "You have messages")],
            ..Element::default()
        };
        translate_element(
            &mut root,
            &german(
                "inbox = Du hast { $count ->\n    [one] Nachricht\n   *[other] Nachrichten\n}\n",
            ),
        );
        assert_eq!(
            root.children[0].attrs.text.as_deref(),
            Some("Du hast \u{2068}Nachrichten\u{2069}")
        );
    }

    #[test]
    fn every_locale_gets_a_translated_tree_naming_the_others() {
        let ir = LayoutIR {
            root: Element {
                tag: "root".to_string(),
                children: vec![label("greeting", "Hello")],
                ..Element::default()
            },
            ..LayoutIR::default()
        };
        let catalogues = Catalogues::parse(
            &[("de-DE".to_string(), "greeting = Hallo\n".to_string())],
            &[],
        )
        .expect("a valid catalogue");
        let keys = ["index".to_string(), "settings".to_string()];
        let heads = [PageHead {
            key: "settings".to_string(),
            title: Some("Settings".to_string()),
            description: None,
            index: false,
        }];
        let locales = [
            "en-US".to_string(),
            "de-DE".to_string(),
            "not a tag".to_string(),
        ];
        let mut warnings = Vec::new();
        let trees = locale_trees(
            SiteLocales {
                ir: &ir,
                keys: &keys,
                heads: &heads,
                locales: &locales,
                catalogues: &catalogues,
                shared: &SiteSpec::default(),
            },
            &mut warnings,
        );

        assert_eq!(trees.len(), 3);
        let german = &trees[1].spec;
        assert_eq!(german.locale.locale, "de-DE");
        assert_eq!(german.locale.default_locale, "en-US");
        assert_eq!(german.locale.alternates, ["en-US", "not a tag"]);
        assert!(!german.locale.is_root());
        assert!(trees[0].spec.locale.is_root());
        // One translated tree, shared by every page of the locale.
        assert!(Arc::ptr_eq(&german.pages[0].ir, &german.pages[1].ir));
        assert_eq!(
            german.pages[0].ir.root.children[0].attrs.text.as_deref(),
            Some("Hallo")
        );
        // The head goes on the page it names, and only there.
        assert_eq!(german.pages[1].title.as_deref(), Some("Settings"));
        assert!(!german.pages[1].index);
        assert_eq!(german.pages[0].title, None);
        // A locale that is no tag is said, and written as authored.
        assert!(trees[2].i18n.is_none());
        assert_eq!(
            trees[2].spec.pages[0].ir.root.children[0]
                .attrs
                .text
                .as_deref(),
            Some("Hello")
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not a tag"), "{warnings:?}");
    }
}
