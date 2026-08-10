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
//! ECS integration: [`I18nPlugin`] installs [`SharedI18n`] (a shared
//! handle to the registry) and [`LocaleFormatter`] as resources, for the
//! locale the caller pins or the one `sys-locale` reports. The [`t!`]
//! macro takes any `I18n` binding, including a [`SharedI18n::read`]
//! guard.
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
use std::sync::{Arc, RwLock};
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

    /// Load every `<dir>/*.ftl` file, keying each bundle by the file
    /// stem (`de-DE.ftl` becomes the `de-DE` bundle). Returns the
    /// locales it loaded, in filesystem order. A missing directory is
    /// not an error; it just loads nothing.
    ///
    /// Re-running replaces the bundles it touches, so this doubles as
    /// the catalogue-reload entry point.
    pub fn load_dir(
        &mut self,
        dir: &std::path::Path,
    ) -> Result<Vec<LanguageIdentifier>, I18nError> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        let mut loaded = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ftl") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| I18nError::BadLocale(path.display().to_string()))?;
            let lang: LanguageIdentifier = Lang::try_from(stem)?.into();
            let source = std::fs::read_to_string(&path)
                .map_err(|e| I18nError::Parse(format!("{}: {e}", path.display())))?;
            self.load_ftl(lang.clone(), &source)?;
            loaded.push(lang);
        }
        Ok(loaded)
    }

    /// Resolve `key` against the current locale, falling through
    /// `fallback_chain`. Returns the key string itself (as
    /// `Cow::Borrowed`) on a complete miss. `args` may carry
    /// [`FluentValue`] entries; `&FluentArgs::default()` is fine when
    /// the message takes no parameters.
    pub fn t<'a>(&'a self, key: &'a str, args: &'a FluentArgs) -> Cow<'a, str> {
        self.try_t(key, args).unwrap_or(Cow::Borrowed(key))
    }

    /// Like [`Self::t`], but reports a miss as `None` instead of
    /// echoing the key. Callers with their own fallback (markup that
    /// carries authored text alongside its `translatable` key) need to
    /// tell "translated to the key" from "no entry".
    pub fn try_t<'a>(&'a self, key: &'a str, args: &'a FluentArgs) -> Option<Cow<'a, str>> {
        self.lookup(&self.current, key, args).or_else(|| {
            self.fallback_chain
                .iter()
                .find_map(|l| self.lookup(l, key, args))
        })
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

/// Shared handle to the app's [`I18n`] registry, installed as a resource
/// by [`I18nPlugin::install`].
///
/// Translation is read from two places that cannot both hold an ECS
/// resource borrow: the spawn path (which has the world) and the script
/// hosts' `t()` builtin (which runs inside a script engine with no world
/// access). Both share one registry through this handle, so a catalogue
/// reload is visible everywhere at once.
#[derive(Resource, Clone)]
pub struct SharedI18n(Arc<RwLock<I18n>>);

impl SharedI18n {
    /// Wrap `i18n` in a shareable handle.
    pub fn new(i18n: I18n) -> Self {
        Self(Arc::new(RwLock::new(i18n)))
    }

    /// Borrow the registry for reading. A poisoned lock is recovered
    /// rather than propagated: a panic mid-translation must not take the
    /// whole UI down.
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, I18n> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Borrow the registry for writing (locale switch, catalogue reload).
    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, I18n> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Resolve `key` for the current locale, returning the key itself on
    /// a miss. The no-argument form scripts and markup use.
    pub fn t(&self, key: &str) -> String {
        self.try_t(key).unwrap_or_else(|| key.to_string())
    }

    /// Resolve `key`, reporting a miss as `None`. See [`I18n::try_t`].
    pub fn try_t(&self, key: &str) -> Option<String> {
        let args = FluentArgs::new();
        self.read().try_t(key, &args).map(Cow::into_owned)
    }
}

impl From<I18n> for SharedI18n {
    fn from(i18n: I18n) -> Self {
        Self::new(i18n)
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
    /// lacks a key. Usually ends with the locale the app was authored
    /// in (`en-US`).
    pub fallback_chain: Vec<LanguageIdentifier>,
    /// Active locale. `None` detects it from the OS via `sys-locale`,
    /// falling back to `en-US`.
    pub locale: Option<LanguageIdentifier>,
}

impl Default for I18nPlugin {
    fn default() -> Self {
        let en: LanguageIdentifier = "en-US".parse().expect("en-US is valid");
        Self {
            fallback_chain: vec![en],
            locale: None,
        }
    }
}

impl I18nPlugin {
    /// Builder: pin the active locale instead of detecting it. An app
    /// declaring `[app] locale` in `lumen.toml` takes this path.
    pub fn with_locale(mut self, locale: LanguageIdentifier) -> Self {
        self.locale = Some(locale);
        self
    }

    /// Install [`SharedI18n`] + [`LocaleFormatter`] onto `world` for the
    /// resolved locale ([`Self::locale`], else the OS locale, else
    /// `en-US`). Returns that locale so callers can log it and load the
    /// matching catalogues.
    pub fn install(self, world: &mut bevy_ecs::world::World) -> LanguageIdentifier {
        let current = self
            .locale
            .or_else(detect_system_locale)
            .unwrap_or_else(|| "en-US".parse().expect("en-US is valid"));
        let i18n = I18n::new(current.clone(), self.fallback_chain);
        let fmt = LocaleFormatter::new(current.clone());
        world.insert_resource(SharedI18n::new(i18n));
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
    fn try_t_reports_a_miss() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "hit = Hit!").unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.try_t("hit", &args).as_deref(), Some("Hit!"));
        assert_eq!(i.try_t("miss", &args), None);
        // `t` still echoes the key so untranslated UI renders something.
        assert_eq!(i.t("miss", &args), "miss");
    }

    #[test]
    fn load_dir_keys_bundles_by_file_stem() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-i18n-load-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en-US.ftl"), "greet = Hello!\n").unwrap();
        std::fs::write(dir.join("de-DE.ftl"), "greet = Hallo!\n").unwrap();
        // Non-FTL files are ignored.
        std::fs::write(dir.join("notes.txt"), "not a catalogue").unwrap();

        let mut i = I18n::new(lang("de-DE"), vec![lang("en-US")]);
        let loaded = i.load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        let args = FluentArgs::new();
        assert_eq!(i.t("greet", &args), "Hallo!");
        assert_eq!(i.t_with_lang(&lang("en-US"), "greet", &args), "Hello!");

        // A stem that is not a BCP-47 tag is a load error, not a silent skip.
        std::fs::write(dir.join("not a tag.ftl"), "greet = x\n").unwrap();
        assert!(i.load_dir(&dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dir_tolerates_a_missing_directory() {
        let mut i = I18n::default();
        let loaded = i
            .load_dir(std::path::Path::new("/definitely/not/here"))
            .unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn shared_handle_translates_and_reloads() {
        let mut i = I18n::new(lang("de-DE"), vec![lang("en-US")]);
        i.load_ftl(lang("de-DE"), "greet = Hallo!").unwrap();
        let shared = SharedI18n::new(i);
        assert_eq!(shared.t("greet"), "Hallo!");
        assert_eq!(shared.t("missing"), "missing");
        assert_eq!(shared.try_t("missing"), None);

        shared
            .write()
            .load_ftl(lang("de-DE"), "greet = Servus!")
            .unwrap();
        assert_eq!(shared.t("greet"), "Servus!");
    }

    #[test]
    fn plugin_installs_shared_resources_for_the_pinned_locale() {
        let mut world = bevy_ecs::world::World::new();
        let current = I18nPlugin::default()
            .with_locale(lang("fr-FR"))
            .install(&mut world);
        assert_eq!(current, lang("fr-FR"));
        let shared = world.resource::<SharedI18n>().clone();
        assert_eq!(shared.read().current, lang("fr-FR"));
        assert!(world.get_resource::<LocaleFormatter>().is_some());
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
