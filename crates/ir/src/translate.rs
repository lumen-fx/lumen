//! Resolving the strings an element shows into one language.
//!
//! `translatable="key"` names one catalogue message, and the element's every
//! visible string comes off it: the text from the message value, and each
//! further string from the Fluent attribute of the same name. So a search box
//! written as
//!
//! ```text
//! <input placeholder="Search catalogue" translatable="search"/>
//! ```
//!
//! reads its placeholder from `search.placeholder`, and an author names one
//! key per widget rather than one per string.
//!
//! This lives here, below both consumers, because the desktop spawner and the
//! web emitter resolve the same element and must reach the same strings: an
//! app run in a locale and a page built for it read alike. The rule has to
//! know which attributes the markup authored, which the catalogue cannot see,
//! so it belongs on this side of the seam. `lumen-ir` itself stays free of a
//! translation dependency: the caller brings the lookup.

use crate::layout_ir::Attributes;

/// What an element shows once its catalogue key has been resolved.
///
/// Each field is `None` exactly when the element has no such string at all;
/// a string the markup authored is never dropped, only replaced.
pub struct TranslatedStrings {
    /// The element's text: [`Attributes::text`] after translation.
    pub text: Option<String>,
    /// A text entry's prompt: [`Attributes::placeholder`] after translation.
    pub placeholder: Option<String>,
    /// An image's alternative text: [`Attributes::alt`] after translation.
    pub alt: Option<String>,
    /// The popup body of the wrapping `<tooltip>`, which carries its own key.
    pub tooltip: Option<String>,
}

/// The catalogue key the `name` string of a `translatable="key"` element
/// resolves through.
///
/// `lumen_i18n::I18n`'s lookup is what splits this back apart, and defines
/// the dotted spelling both sides agree on.
pub fn attribute_key(key: &str, name: &str) -> String {
    format!("{key}.{name}")
}

/// Every string `attrs` shows, resolved through `lookup`.
///
/// `lookup` answers a catalogue key with the message it holds, or `None` when
/// it holds none - `lumen_i18n::SharedI18n::try_t` and
/// `lumen_core::i18n::AppI18n::try_translate` are the two that do.
///
/// Only a string the markup authored is translated. A catalogue entry for
/// `key.placeholder` on an element with no `placeholder` does nothing, which
/// is the same rule `lumenc i18n extract` applies from the other side, so
/// what the extractor writes is exactly what resolves.
pub fn translate_attrs(
    attrs: &Attributes,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> TranslatedStrings {
    let mut out = TranslatedStrings {
        text: attrs.text.clone(),
        placeholder: attrs.placeholder.clone(),
        alt: attrs.alt.clone(),
        tooltip: attrs.tooltip.as_ref().map(|spec| spec.text.clone()),
    };
    if let Some(key) = &attrs.translatable {
        if attrs.placeholder.is_some() {
            out.placeholder = lookup(&attribute_key(key, "placeholder")).or(out.placeholder);
        }
        if attrs.alt.is_some() {
            out.alt = lookup(&attribute_key(key, "alt")).or(out.alt);
        }
        // The catalogue's string wins, then the authored text, then the key
        // itself, so an element whose translations are missing still shows
        // its source string and one with no text at all shows the key that
        // failed to resolve. The key stops standing in once the element
        // names another translated string: an `<input translatable="search"
        // placeholder="Search"/>` has no text, and writing `search` into the
        // field as its value is a wrong value rather than a diagnostic.
        out.text = lookup(key)
            .or(out.text)
            .or_else(|| (attrs.placeholder.is_none() && attrs.alt.is_none()).then(|| key.clone()));
    }
    // A `<tooltip>` is its own element with its own text, so it takes a plain
    // key of its own rather than an attribute on the trigger's. That also
    // survives the desugars: a `<checkbox>` moves its key onto the caption
    // child it synthesizes, leaving the trigger nothing to hang one on.
    if let Some(spec) = &attrs.tooltip
        && let Some(key) = &spec.translatable
    {
        out.tooltip = lookup(key).or(out.tooltip);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::layout_ir::TooltipSpec;

    fn catalogue(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn resolve(attrs: &Attributes, entries: &[(&str, &str)]) -> TranslatedStrings {
        let map = catalogue(entries);
        translate_attrs(attrs, &|key| map.get(key).cloned())
    }

    #[test]
    fn an_untranslatable_element_keeps_every_authored_string() {
        let attrs = Attributes {
            text: Some("Search".into()),
            placeholder: Some("Search catalogue".into()),
            alt: Some("A logo".into()),
            ..Attributes::default()
        };
        let out = resolve(&attrs, &[("search", "Suche")]);
        assert_eq!(out.text.as_deref(), Some("Search"));
        assert_eq!(out.placeholder.as_deref(), Some("Search catalogue"));
        assert_eq!(out.alt.as_deref(), Some("A logo"));
    }

    #[test]
    fn one_key_reaches_the_text_the_placeholder_and_the_alt() {
        let attrs = Attributes {
            text: Some("Search".into()),
            placeholder: Some("Search catalogue".into()),
            alt: Some("A logo".into()),
            translatable: Some("search".into()),
            ..Attributes::default()
        };
        let out = resolve(
            &attrs,
            &[
                ("search", "Suche"),
                ("search.placeholder", "Katalog durchsuchen"),
                ("search.alt", "Ein Logo"),
            ],
        );
        assert_eq!(out.text.as_deref(), Some("Suche"));
        assert_eq!(out.placeholder.as_deref(), Some("Katalog durchsuchen"));
        assert_eq!(out.alt.as_deref(), Some("Ein Logo"));
    }

    #[test]
    fn a_string_the_markup_does_not_write_is_not_translated_into_existence() {
        let attrs = Attributes {
            text: Some("Search".into()),
            translatable: Some("search".into()),
            ..Attributes::default()
        };
        let out = resolve(&attrs, &[("search.placeholder", "Katalog durchsuchen")]);
        assert_eq!(out.placeholder, None);
        assert_eq!(out.alt, None);
    }

    #[test]
    fn a_missing_message_leaves_the_authored_string() {
        let attrs = Attributes {
            text: Some("Search".into()),
            placeholder: Some("Search catalogue".into()),
            translatable: Some("search".into()),
            ..Attributes::default()
        };
        let out = resolve(&attrs, &[]);
        assert_eq!(out.text.as_deref(), Some("Search"));
        assert_eq!(out.placeholder.as_deref(), Some("Search catalogue"));
    }

    #[test]
    fn a_key_with_no_text_anywhere_shows_itself() {
        let attrs = Attributes {
            translatable: Some("app-title".into()),
            ..Attributes::default()
        };
        assert_eq!(resolve(&attrs, &[]).text.as_deref(), Some("app-title"));
    }

    #[test]
    fn a_key_naming_another_string_does_not_stand_in_for_the_text() {
        let attrs = Attributes {
            placeholder: Some("Search catalogue".into()),
            translatable: Some("search".into()),
            ..Attributes::default()
        };
        let out = resolve(&attrs, &[("search.placeholder", "Katalog durchsuchen")]);
        assert_eq!(out.text, None);
        assert_eq!(out.placeholder.as_deref(), Some("Katalog durchsuchen"));
    }

    #[test]
    fn a_tooltip_carries_its_own_key() {
        let attrs = Attributes {
            text: Some("Save".into()),
            translatable: Some("save".into()),
            tooltip: Some(TooltipSpec {
                text: "Save the file".into(),
                translatable: Some("save-tip".into()),
                delay_ms: None,
                offset: None,
            }),
            ..Attributes::default()
        };
        let out = resolve(
            &attrs,
            &[("save", "Speichern"), ("save-tip", "Datei speichern")],
        );
        assert_eq!(out.text.as_deref(), Some("Speichern"));
        assert_eq!(out.tooltip.as_deref(), Some("Datei speichern"));
    }

    #[test]
    fn an_unmarked_tooltip_keeps_its_authored_body() {
        let attrs = Attributes {
            tooltip: Some(TooltipSpec {
                text: "Save the file".into(),
                translatable: None,
                delay_ms: None,
                offset: None,
            }),
            ..Attributes::default()
        };
        let out = resolve(&attrs, &[("save-tip", "Datei speichern")]);
        assert_eq!(out.tooltip.as_deref(), Some("Save the file"));
    }
}
