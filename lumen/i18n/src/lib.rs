//! Translation + locale-aware formatters (W5.7 + W5.8).
//!
//! Two halves:
//!
//! - **Translation** (W5.7) - [`I18n`] wraps per-locale [`FluentBundle`]s
//!   keyed by [`LanguageIdentifier`]. `load_ftl` parses `.ftl` source
//!   strings; `t` / `t_with_lang` resolve keys with optional
//!   [`FluentArgs`]. Falls through `fallback_chain` in order on a miss.
//! - **Formatting** (W5.8) - [`LocaleFormatter`] wraps ICU4X
//!   [`DecimalFormatter`] + [`DateTimeFormatter`] for the active
//!   locale. `format_number`, `format_date`, `format_time`,
//!   `format_datetime`, `format_currency` return localized
//!   `String`s. `format_relative` is a tiny "X units ago" stub -
//!   `icu_relativetime` is still at 0.1.x which doesn't compose with
//!   the 2.x `icu` line we pinned, so the precise CLDR-driven
//!   relative-time path is deferred.
//!
//! ECS integration: [`I18nPlugin`] installs `I18n` and
//! [`LocaleFormatter`] as resources, seeded from the system locale
//! (via `sys-locale`). The [`t!`] macro reads `Res<I18n>` inside a
//! bevy_ecs system.
//!
//! Conversions follow the project's `From`/`Into` convention - no
//! bespoke `parse_lang` or `convert_locale_to_langid` helpers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod formatter;
#[macro_use]
pub mod macros;

#[doc(hidden)]
pub use macros::reexports;

use bevy_ecs::resource::Resource;
// Concurrent variant - IntlLangMemoizer over std::sync::Mutex. Lets
// `I18n` sit in a bevy_ecs Resource (which requires Send + Sync) and
// be read from parallel systems.
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use std::borrow::Cow;
use std::collections::HashMap;
use thiserror::Error;
pub use unic_langid::LanguageIdentifier;

pub use fluent_bundle::FluentValue;
pub use formatter::{FormatterError, LocaleFormatter};

/// Errors surfaced by [`I18n`] when loading or resolving translations.
#[derive(Debug, Error)]
pub enum I18nError {
    /// `.ftl` source failed to parse.
    #[error("fluent parse error: {0}")]
    Parse(String),
    /// `.ftl` parsed but resource registration failed (key collision).
    #[error("fluent resource add error: {0}")]
    AddResource(String),
    /// BCP-47 tag failed to parse into a `LanguageIdentifier`.
    #[error("bad locale tag: {0}")]
    BadLocale(String),
}

impl From<I18nError> for std::fmt::Error {
    fn from(_: I18nError) -> Self {
        std::fmt::Error
    }
}

/// Wraps a parsed [`LanguageIdentifier`]. Distinct newtype so we can
/// hang `From<&str>` on it without orphan-rule problems.
///
/// Use [`Lang::from`] / `"en-US".into()` rather than a bespoke
/// `parse_lang` helper.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lang(pub LanguageIdentifier);

impl From<LanguageIdentifier> for Lang {
    fn from(id: LanguageIdentifier) -> Self {
        Self(id)
    }
}

impl From<Lang> for LanguageIdentifier {
    fn from(l: Lang) -> Self {
        l.0
    }
}

impl TryFrom<&str> for Lang {
    type Error = I18nError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse::<LanguageIdentifier>()
            .map(Lang)
            .map_err(|e| I18nError::BadLocale(format!("{s}: {e}")))
    }
}

impl std::str::FromStr for Lang {
    type Err = I18nError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

/// Translation registry. One [`FluentBundle`] per locale. Looks up
/// keys against `current`; falls through `fallback_chain` in order;
/// returns the key string itself when no bundle has it.
///
/// `FluentBundle` defaults are concurrent (`FluentBundle::new` returns
/// the non-Sync variant). We use the default concurrent variant so the
/// resource can be read from parallel bevy_ecs systems.
#[derive(Resource)]
pub struct I18n {
    /// One [`FluentBundle`] per loaded locale.
    pub bundles: HashMap<LanguageIdentifier, FluentBundle<FluentResource>>,
    /// Active locale used by [`I18n::t`].
    pub current: LanguageIdentifier,
    /// Fallback search order applied when `current` does not resolve a key.
    /// Walked after `current`; usually ends with the source locale (e.g. `en-US`).
    pub fallback_chain: Vec<LanguageIdentifier>,
}

impl I18n {
    /// Build an empty registry. Add bundles with [`Self::load_ftl`].
    pub fn new(current: LanguageIdentifier, fallback_chain: Vec<LanguageIdentifier>) -> Self {
        Self {
            bundles: HashMap::new(),
            current,
            fallback_chain,
        }
    }

    /// Parse + register `ftl_source` for `lang`. Idempotent: a second
    /// load for the same `lang` replaces the bundle (so hot-reload of a
    /// `.ftl` file just calls this again with the new bytes).
    pub fn load_ftl(
        &mut self,
        lang: LanguageIdentifier,
        ftl_source: &str,
    ) -> Result<(), I18nError> {
        let res = FluentResource::try_new(ftl_source.to_string())
            .map_err(|(_, errs)| I18nError::Parse(format!("{errs:?}")))?;
        let mut bundle = FluentBundle::new_concurrent(vec![lang.clone()]);
        // Disable Unicode isolation chars in test/CI output so round-trip
        // assertions stay readable. Authors can flip this if they want
        // them back for actual rendering.
        bundle.set_use_isolating(false);
        bundle
            .add_resource(res)
            .map_err(|errs| I18nError::AddResource(format!("{errs:?}")))?;
        self.bundles.insert(lang, bundle);
        Ok(())
    }

    /// Switch the active locale. Call before the next [`Self::t`].
    pub fn set_current(&mut self, lang: LanguageIdentifier) {
        self.current = lang;
    }

    /// Resolve `key` against the current locale, falling through
    /// `fallback_chain`. Returns the key string itself (as
    /// `Cow::Borrowed`) on a complete miss. `args` may carry
    /// [`FluentValue`] entries; `&FluentArgs::default()` is fine when
    /// the message takes no parameters.
    pub fn t<'a>(&'a self, key: &'a str, args: &'a FluentArgs) -> Cow<'a, str> {
        self.lookup(&self.current, key, args)
            .or_else(|| {
                self.fallback_chain
                    .iter()
                    .find_map(|l| self.lookup(l, key, args))
            })
            .unwrap_or(Cow::Borrowed(key))
    }

    /// Resolve `key` against an explicit locale (no fallback chain).
    /// Returns the key itself on a miss.
    pub fn t_with_lang<'a>(
        &'a self,
        lang: &LanguageIdentifier,
        key: &'a str,
        args: &'a FluentArgs,
    ) -> Cow<'a, str> {
        self.lookup(lang, key, args).unwrap_or(Cow::Borrowed(key))
    }

    fn lookup<'a>(
        &'a self,
        lang: &LanguageIdentifier,
        key: &'a str,
        args: &'a FluentArgs,
    ) -> Option<Cow<'a, str>> {
        let bundle = self.bundles.get(lang)?;
        let msg = bundle.get_message(key)?;
        let pattern = msg.value()?;
        let mut errors = Vec::new();
        let out = bundle
            .format_pattern(pattern, Some(args), &mut errors)
            .into_owned();
        if !errors.is_empty() {
            tracing::warn!(?errors, key, "fluent format_pattern errors");
        }
        Some(Cow::Owned(out))
    }
}

impl Default for I18n {
    fn default() -> Self {
        let en: LanguageIdentifier = "en-US".parse().expect("en-US is valid");
        Self::new(en.clone(), vec![en])
    }
}

/// RTL languages list. Lifted straight from the i18n audit spec
/// (`docs/audits/i18n.md` "Rewrite spec section 1") so the plugin agrees with
/// whatever `LayoutDirection::DefaultLayoutDirection` ends up doing in
/// `lumen-core`. Used only as a tiny helper for callers wanting to
/// short-circuit "is the system in RTL" without reaching into ICU4X.
pub fn is_rtl(lang: &LanguageIdentifier) -> bool {
    matches!(
        lang.language.as_str(),
        "ar" | "fa" | "he" | "ur" | "yi" | "ps" | "sd" | "ckb"
    )
}

/// ECS plugin. Seeds `I18n` + [`LocaleFormatter`] from the system
/// locale (via [`sys_locale::get_locale`]) and pushes them as
/// resources. `fallback_chain` is consulted when the current locale
/// lacks a key.
///
/// Note: this crate does not depend on `lumen-core`'s `App` /
/// `Plugin` trait to avoid pulling the entire render/runtime stack
/// into translation. The runner crate (`lumenc`) can register it as
/// a normal `lumen_core::Plugin` via the shim below - or call
/// [`I18nPlugin::install`] directly on a `World` for headless usage.
pub struct I18nPlugin {
    /// Locales to walk through (in order) when the active locale
    /// lacks a key. The active locale itself is detected via
    /// `sys-locale`; override it after install via
    /// [`I18n::set_current`] if the host wants a different default.
    pub fallback_chain: Vec<LanguageIdentifier>,
}

impl Default for I18nPlugin {
    fn default() -> Self {
        let en: LanguageIdentifier = "en-US".parse().expect("en-US is valid");
        Self {
            fallback_chain: vec![en],
        }
    }
}

impl I18nPlugin {
    /// Detect the system locale and install [`I18n`] + [`LocaleFormatter`]
    /// onto `world`. Returns the resolved active locale so callers can
    /// log it.
    pub fn install(self, world: &mut bevy_ecs::world::World) -> LanguageIdentifier {
        let current =
            detect_system_locale().unwrap_or_else(|| "en-US".parse().expect("en-US is valid"));
        let i18n = I18n::new(current.clone(), self.fallback_chain);
        let fmt = LocaleFormatter::new(current.clone());
        world.insert_resource(i18n);
        world.insert_resource(fmt);
        current
    }
}

/// Read `LANG` / OS locale APIs via `sys-locale` and parse the
/// result. Returns `None` when the OS reports nothing or the tag
/// fails to parse.
pub fn detect_system_locale() -> Option<LanguageIdentifier> {
    let raw = sys_locale::get_locale()?;
    // sys-locale returns POSIX-style strings like `en_US.UTF-8` on
    // Linux. Strip the codeset and normalize `_` -> `-` so the parser
    // accepts it.
    let cleaned = raw.split('.').next().unwrap_or(&raw).replace('_', "-");
    cleaned.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(s: &str) -> LanguageIdentifier {
        s.parse().expect("test lang parses")
    }

    #[test]
    fn lang_from_str_roundtrip() {
        let l: Lang = "de-DE".try_into().unwrap();
        let id: LanguageIdentifier = l.into();
        assert_eq!(id.language.as_str(), "de");
        assert_eq!(id.region.map(|r| r.as_str().to_string()), Some("DE".into()));
    }

    #[test]
    fn bad_locale_errors() {
        let r: Result<Lang, _> = "not a tag at all".try_into();
        assert!(r.is_err());
    }

    #[test]
    fn translate_hits_current_locale() {
        let mut i = I18n::new(lang("en-US"), vec![lang("en-US")]);
        i.load_ftl(lang("en-US"), "hello = Hello!").unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.t("hello", &args), "Hello!");
    }

    #[test]
    fn translate_falls_through_chain() {
        let mut i = I18n::new(lang("de-DE"), vec![lang("en-US")]);
        i.load_ftl(lang("en-US"), "hello = Hello!").unwrap();
        i.load_ftl(lang("de-DE"), "good-bye = Tsch\u{fc}ss!")
            .unwrap();
        let args = FluentArgs::new();
        // current (de-DE) lacks `hello` -> falls to en-US.
        assert_eq!(i.t("hello", &args), "Hello!");
        // current has `good-bye`.
        assert_eq!(i.t("good-bye", &args), "Tsch\u{fc}ss!");
    }

    #[test]
    fn missing_key_returns_key() {
        let i = I18n::new(lang("en-US"), vec![]);
        let args = FluentArgs::new();
        assert_eq!(i.t("nope", &args), "nope");
    }

    #[test]
    fn args_interpolate() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "greet = Hello { $name }!")
            .unwrap();
        let mut args = FluentArgs::new();
        args.set("name", FluentValue::from("World"));
        assert_eq!(i.t("greet", &args), "Hello World!");
    }

    #[test]
    fn de_de_uses_german_bundle() {
        let mut i = I18n::new(lang("de-DE"), vec![lang("en-US")]);
        i.load_ftl(lang("en-US"), "greet = Hello!").unwrap();
        i.load_ftl(lang("de-DE"), "greet = Hallo!").unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.t("greet", &args), "Hallo!");
        // Explicit en-US lookup ignores the de-DE current.
        assert_eq!(i.t_with_lang(&lang("en-US"), "greet", &args), "Hello!");
    }

    #[test]
    fn rtl_detection() {
        assert!(is_rtl(&lang("ar-EG")));
        assert!(is_rtl(&lang("he-IL")));
        assert!(!is_rtl(&lang("en-US")));
        assert!(!is_rtl(&lang("de-DE")));
    }

    #[test]
    fn reload_replaces_bundle() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "greet = First").unwrap();
        i.load_ftl(lang("en-US"), "greet = Second").unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.t("greet", &args), "Second");
    }
}
