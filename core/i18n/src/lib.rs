//! Translation + locale-aware formatters.
//!
//! Two halves:
//!
//! - **Translation** - [`I18n`] wraps per-locale [`FluentBundle`]s
//!   keyed by [`LanguageIdentifier`]. `load_ftl` parses `.ftl` source
//!   strings; `t` / `t_with_lang` resolve keys with optional
//!   [`FluentArgs`]. A key of the form `message.attribute` resolves that
//!   Fluent attribute rather than the message value, which is how one
//!   markup key reaches an element's placeholder and its alternative
//!   text. Falls through `fallback_chain` in order on a miss, which
//!   ends with the locale the app's source strings are written in.
//!   [`read_catalogues`] is the one walker of an app's `locale/`
//!   directory, and [`Catalogues`] parses a set of catalogues once for
//!   a caller that builds a registry per locale or per render.
//! - **Formatting** - [`LocaleFormatter`] wraps ICU4X's decimal,
//!   date-time, currency and relative-time formatters for the active
//!   locale. `format_number`, `format_date`, `format_time`,
//!   `format_datetime`, `format_currency` and `format_relative` return
//!   localized `String`s, and [`formatter::format_spec`] reaches all of
//!   them from one spec string, which is what markup's `format`
//!   attribute and the scripts' `format_*` builtins carry.
//!
//! ECS integration: [`I18nPlugin`] installs [`SharedI18n`] and
//! [`SharedFormatter`] (shared handles to the registry and the
//! formatters, for a catalogue reload, for a locale switch, and for the
//! build-time tools that translate an IR tree) and
//! [`lumen_core::i18n::AppI18n`] (the app's half of core's opaque seam,
//! which is how markup reaches all of it as it spawns), for the locale
//! the caller pins or the one `sys-locale` reports. The [`t!`] macro
//! takes any `I18n` binding, including a [`SharedI18n::read`] guard.
//!
//! An app's locale is not fixed for the life of the process:
//! [`switch_locale`] moves the catalogue, the formatters and the writing
//! direction together, and every reader holding a shared handle sees the
//! move at once.
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
pub use formatter::{FormatterError, LocaleFormatter, format_spec};

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
    /// A catalogue, or the directory holding them, could not be read.
    #[error("read {0}")]
    Read(String),
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
/// Bundles are built with `FluentBundle::new_concurrent` rather than
/// `FluentBundle::new`, which returns the non-`Sync` variant, so the
/// resource can be read from parallel bevy_ecs systems.
#[derive(Resource)]
pub struct I18n {
    /// One [`FluentBundle`] per loaded locale.
    pub bundles: HashMap<LanguageIdentifier, FluentBundle<Arc<FluentResource>>>,
    /// Active locale used by [`I18n::t`].
    pub current: LanguageIdentifier,
    /// Fallback search order applied when `current` does not resolve a key.
    /// Walked after `current`; ends with the locale the app was authored in,
    /// which an app names with `[app] fallback_locale` in `lumen.toml`.
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
    ///
    /// A value a placeable substitutes - an argument, the arm a selector
    /// picks - is wrapped in Unicode isolation marks (U+2068 and U+2069),
    /// so it cannot reorder the text around it when the two run in
    /// opposite directions. A term or message reference is inlined
    /// without them, since it is catalogue text in the catalogue's own
    /// language. [`Catalogues`] builds its bundles the same way, so a
    /// catalogue resolves to the same bytes on the desktop, in a browser
    /// and on a server.
    pub fn load_ftl(
        &mut self,
        lang: LanguageIdentifier,
        ftl_source: &str,
    ) -> Result<(), I18nError> {
        let bundle = bundle(lang.clone(), parse(ftl_source)?)?;
        self.bundles.insert(lang, bundle);
        Ok(())
    }

    /// Switch the active locale every later lookup resolves against.
    ///
    /// This moves the catalogue and nothing else. An app's locale is more
    /// than its catalogue: the number and date formatters and the base
    /// writing direction read it too, and [`switch_locale`] is the call
    /// that moves all three together.
    ///
    /// A locale with no bundle loaded is allowed. Every message falls
    /// through the chain then, which ends at the locale the app's source
    /// strings are written in.
    pub fn set_current(&mut self, lang: LanguageIdentifier) {
        self.current = lang;
    }

    /// Load every `<dir>/*.ftl` file, keying each bundle by the file
    /// stem (`de-DE.ftl` becomes the `de-DE` bundle). Returns the
    /// locales it loaded, ordered by tag. A missing directory is not an
    /// error; it just loads nothing.
    ///
    /// The files are found and read by [`read_catalogues`], so `read` is
    /// the seam every byte comes through: the runtime hands in the app's
    /// asset source chain, so a catalogue an asset source overlays loads
    /// from there.
    ///
    /// Re-running replaces the bundles it touches, so this doubles as
    /// the catalogue-reload entry point.
    pub fn load_dir(
        &mut self,
        dir: &std::path::Path,
        read: impl Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
    ) -> Result<Vec<LanguageIdentifier>, I18nError> {
        let mut loaded = Vec::new();
        for (tag, source) in read_catalogues(dir, read)? {
            let lang: LanguageIdentifier = Lang::try_from(tag.as_str())?.into();
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
    ///
    /// A chain entry equal to the active locale is skipped, so a miss never
    /// probes one bundle twice for the answer it already gave. The skip is
    /// here rather than in the chain itself because the active locale
    /// changes while the app runs: pruning the chain would throw away the
    /// entry the locale switched away from needs on the way back.
    pub fn try_t<'a>(&'a self, key: &'a str, args: &'a FluentArgs) -> Option<Cow<'a, str>> {
        self.lookup(&self.current, key, args).or_else(|| {
            self.fallback_chain
                .iter()
                .filter(|l| **l != self.current)
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
        // A key names a message, optionally followed by `.attribute`. A
        // Fluent identifier cannot contain a dot, so splitting on the first
        // one can never shadow a message a catalogue declares. This
        // is where the dotted spelling is defined: an element's `placeholder`
        // reaches the catalogue as `<key>.placeholder`, and a script asking
        // for `t("search.placeholder")` resolves the same string.
        let (name, attribute) = match key.split_once('.') {
            Some((name, attribute)) => (name, Some(attribute)),
            None => (key, None),
        };
        let msg = bundle.get_message(name)?;
        let pattern = match attribute {
            Some(attribute) => msg.get_attribute(attribute)?.value(),
            None => msg.value()?,
        };
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

/// Parse one catalogue's Fluent source.
fn parse(source: &str) -> Result<Arc<FluentResource>, I18nError> {
    FluentResource::try_new(source.to_string())
        .map(Arc::new)
        .map_err(|(_, errs)| I18nError::Parse(format!("{errs:?}")))
}

/// A bundle for `lang` holding `resource`.
///
/// A value a placeable substitutes is wrapped in Unicode isolation marks
/// (the bundle's default), which [`I18n::load_ftl`] describes.
fn bundle(
    lang: LanguageIdentifier,
    resource: Arc<FluentResource>,
) -> Result<FluentBundle<Arc<FluentResource>>, I18nError> {
    let mut bundle = FluentBundle::new_concurrent(vec![lang]);
    bundle
        .add_resource(resource)
        .map_err(|errs| I18nError::AddResource(format!("{errs:?}")))?;
    Ok(bundle)
}

/// Every `<dir>/<tag>.ftl` catalogue, as its tag and its Fluent source,
/// ordered by tag.
///
/// This is the one place an app's `locale/` directory is walked. The
/// listing comes from the filesystem; each file's bytes come through
/// `read`, the seam the caller reads its other app data through. The
/// runtime hands in the app's asset source chain; a compiler reading the
/// author's loose files passes `std::fs::read`. This crate takes the
/// function rather than naming an asset type to stay independent of the
/// asset stack.
///
/// A missing directory holds no catalogues, which is an app with no
/// translations. The tag is the file stem as written, and a file whose
/// stem is not a BCP-47 tag is an error rather than a file passed over.
/// The sources are not parsed here; [`Catalogues::parse`] does that.
///
/// # Errors
///
/// The directory or a catalogue in it cannot be read, a catalogue is not
/// UTF-8, or a stem is not a language tag.
pub fn read_catalogues(
    dir: &std::path::Path,
    read: impl Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
) -> Result<Vec<(String, String)>, I18nError> {
    let failed = |path: &std::path::Path, why: &dyn std::fmt::Display| {
        I18nError::Read(format!("{}: {why}", path.display()))
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(failed(dir, &e)),
    };
    let mut catalogues = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| failed(dir, &e))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ftl") {
            continue;
        }
        let tag = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| I18nError::BadLocale(path.display().to_string()))?;
        Lang::try_from(tag)?;
        let bytes = read(&path).map_err(|e| failed(&path, &e))?;
        let source = String::from_utf8(bytes).map_err(|e| failed(&path, &e))?;
        catalogues.push((tag.to_string(), source));
    }
    catalogues.sort();
    Ok(catalogues)
}

/// An app's catalogues, parsed once, and the chain a miss falls through.
///
/// Parsing a catalogue is the expensive half of building an [`I18n`], and
/// it does not depend on the locale an app runs in. This holds the parsed
/// resources so a caller that builds many registries from the same
/// catalogues, one per locale tree or one per render, parses each file
/// once. [`Self::i18n`] builds a registry of its own each time it is
/// called, so a locale switch in one never reaches another.
#[derive(Clone)]
pub struct Catalogues {
    resources: Vec<(LanguageIdentifier, Arc<FluentResource>)>,
    fallback: Vec<LanguageIdentifier>,
}

impl Catalogues {
    /// Parse `sources`, each a BCP-47 tag and that locale's Fluent source,
    /// with a miss falling through `fallback`.
    ///
    /// Empty `fallback` takes the chain [`I18nPlugin`] starts with, which
    /// is the one a desktop app gets when `[app] fallback_locale` is unset.
    ///
    /// # Errors
    ///
    /// A tag is not BCP-47, or a catalogue is not Fluent or declares a
    /// message twice. Every catalogue is checked, so a registry built from
    /// the result cannot fail on one.
    pub fn parse(sources: &[(String, String)], fallback: &[String]) -> Result<Self, I18nError> {
        let fallback = if fallback.is_empty() {
            I18nPlugin::default().fallback_chain
        } else {
            fallback
                .iter()
                .map(|tag| Lang::try_from(tag.as_str()).map(LanguageIdentifier::from))
                .collect::<Result<_, _>>()?
        };
        let mut resources = Vec::with_capacity(sources.len());
        for (tag, source) in sources {
            let lang: LanguageIdentifier = Lang::try_from(tag.as_str())?.into();
            let resource = parse(source)?;
            bundle(lang.clone(), Arc::clone(&resource))?;
            resources.push((lang, resource));
        }
        Ok(Self {
            resources,
            fallback,
        })
    }

    /// True when there is no catalogue.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// A registry with `locale` active and every catalogue loaded.
    ///
    /// # Errors
    ///
    /// `locale` is not a BCP-47 tag.
    pub fn i18n(&self, locale: &str) -> Result<I18n, I18nError> {
        let current: LanguageIdentifier = Lang::try_from(locale)?.into();
        let mut i18n = I18n::new(current, self.fallback.clone());
        for (lang, resource) in &self.resources {
            let bundle = bundle(lang.clone(), Arc::clone(resource))?;
            i18n.bundles.insert(lang.clone(), bundle);
        }
        Ok(i18n)
    }
}

impl Default for Catalogues {
    /// No catalogue, with the default chain.
    fn default() -> Self {
        Self {
            resources: Vec::new(),
            fallback: I18nPlugin::default().fallback_chain,
        }
    }
}

impl std::fmt::Debug for Catalogues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalogues")
            .field(
                "locales",
                &self
                    .resources
                    .iter()
                    .map(|(lang, _)| lang.to_string())
                    .collect::<Vec<_>>(),
            )
            .field("fallback", &self.fallback)
            .finish()
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

/// Shared handle to the app's [`LocaleFormatter`], installed as a resource
/// by [`I18nPlugin::install`].
///
/// A formatter is built for one locale rather than edited in place, so a
/// locale switch puts a new one behind this handle. Everything already
/// holding the handle - the markup path's `format` attribute and the
/// scripts' `format_*` builtins - formats for the new locale from the next
/// call on, which is what [`SharedI18n`] already does for the catalogue.
#[derive(Resource, Clone)]
pub struct SharedFormatter(Arc<RwLock<Arc<LocaleFormatter>>>);

impl SharedFormatter {
    /// Wrap `formatter` in a shareable handle.
    pub fn new(formatter: LocaleFormatter) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(formatter))))
    }

    /// The formatter in force now. A poisoned lock is recovered rather
    /// than propagated, for the reason [`SharedI18n::read`] gives.
    pub fn get(&self) -> Arc<LocaleFormatter> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Build the formatters for `lang` and put them in force.
    pub fn set(&self, lang: &LanguageIdentifier) {
        let built = Arc::new(LocaleFormatter::new(lang.clone()));
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = built;
    }
}

impl From<LocaleFormatter> for SharedFormatter {
    fn from(formatter: LocaleFormatter) -> Self {
        Self::new(formatter)
    }
}

/// Move an app to `tag`: its catalogue, its formatters and the writing
/// direction its text reads in, together.
///
/// `formatter` is optional because not every assembly links one. A browser
/// page that installs a catalogue and no formatters passes `None`, and a
/// `format` spec there leaves its text as it stands, before the switch and
/// after it.
///
/// Answers with the locale now in force and the direction to read it in, or
/// `None` when `tag` is not a BCP-47 tag, in which case nothing moved. A
/// tag that parses but names a locale with no catalogue loaded succeeds:
/// every message falls back to the text the author wrote, which is the rule
/// startup already follows for the same case.
pub fn switch_locale(
    catalogue: &SharedI18n,
    formatter: Option<&SharedFormatter>,
    tag: &str,
) -> Option<lumen_core::i18n::LocaleChange> {
    let lang: LanguageIdentifier = Lang::try_from(tag).ok()?.into();
    catalogue.write().set_current(lang.clone());
    if let Some(formatter) = formatter {
        formatter.set(&lang);
    }
    Some(lumen_core::i18n::LocaleChange {
        locale: lang.to_string(),
        direction: if is_rtl(&lang) {
            lumen_core::components::LayoutDirection::Rtl
        } else {
            lumen_core::components::LayoutDirection::Ltr
        },
    })
}

/// The set of right-to-left languages, in one place so every target
/// agrees on it. The web emitter (`LocaleSpec::new`) reads it to write
/// `<html dir>`, and the desktop runtime reads it to seed
/// `lumen_core::components::DefaultLayoutDirection`, which is what keeps
/// a tree rendered in the browser and the same tree rendered on the
/// desktop pointing the same way.
pub fn is_rtl(lang: &LanguageIdentifier) -> bool {
    matches!(
        lang.language.as_str(),
        "ar" | "fa" | "he" | "ur" | "yi" | "ps" | "sd" | "ckb"
    )
}

/// ECS plugin. Seeds `I18n` + [`LocaleFormatter`] for the system locale
/// (via [`sys_locale::get_locale`]) unless the app pins one. Both go into
/// the world as resources and behind core's opaque handle.
/// `fallback_chain` is consulted when the current locale lacks a key; an
/// entry equal to whichever locale is active is skipped at lookup, so a
/// miss never probes one bundle twice.
///
/// This crate does not depend on `lumen-core`'s `App` / `Plugin`
/// trait, to avoid pulling the whole render/runtime stack into
/// translation, and it does not implement `lumen_core::Plugin`.
/// Install it by calling [`I18nPlugin::install`] on a `World`, which
/// is what the runner does.
pub struct I18nPlugin {
    /// Locales to walk through (in order) when the active locale
    /// lacks a key. Ends with the locale the app was authored in, which
    /// defaults to `en-US` and which an app renames with
    /// `[app] fallback_locale` in `lumen.toml`.
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

    /// Builder: name the locale a key missing from the active catalogue
    /// falls through to. An app declaring `[app] fallback_locale` in
    /// `lumen.toml` takes this path.
    ///
    /// This replaces the chain rather than extending it, so the value the
    /// config can express is the value the plugin holds.
    pub fn with_fallback_locale(mut self, locale: LanguageIdentifier) -> Self {
        self.fallback_chain = vec![locale];
        self
    }

    /// Install [`SharedI18n`], [`SharedFormatter`] and
    /// [`lumen_core::i18n::AppI18n`] onto `world` for the locale the app
    /// starts in ([`Self::locale`], else the OS locale, else `en-US`).
    /// Returns that locale so callers can log it and load the matching
    /// catalogues.
    ///
    /// The catalogue and the formatters each go in twice: as the handle a
    /// reload or a locale switch writes to, and behind the opaque handles
    /// the spawner reads. Both are shared, so a switch through
    /// [`switch_locale`] reaches every reader at once. One install builds
    /// one [`LocaleFormatter`], which holds the locale and builds each ICU
    /// formatter the first time something formats with it, so an app that
    /// formats nothing loads no ICU data.
    pub fn install(self, world: &mut bevy_ecs::world::World) -> LanguageIdentifier {
        let current = self
            .locale
            .or_else(detect_system_locale)
            .unwrap_or_else(|| "en-US".parse().expect("en-US is valid"));
        let shared = SharedI18n::new(I18n::new(current.clone(), self.fallback_chain));
        let formatter = SharedFormatter::new(LocaleFormatter::new(current.clone()));
        let catalogue = shared.clone();
        let formatting = formatter.clone();
        let switch_catalogue = shared.clone();
        let switch_formatter = formatter.clone();
        world.insert_resource(lumen_core::i18n::AppI18n::new(
            Arc::new(move |key| catalogue.try_t(key)),
            Arc::new(move |spec, value| format_spec(&formatting.get(), spec, value)),
            Arc::new(move |tag| switch_locale(&switch_catalogue, Some(&switch_formatter), tag)),
        ));
        world.insert_resource(formatter);
        world.insert_resource(shared);
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
    fn a_registry_from_parsed_catalogues_falls_through_the_chain_it_was_given() {
        let catalogues = vec![
            ("en-US".to_string(), "hello = Hello!\n".to_string()),
            (
                "de-DE".to_string(),
                "hello = Hallo!\nbye = Tschuess\n".to_string(),
            ),
        ];
        let named = Catalogues::parse(&catalogues, &["de-DE".to_string()]).unwrap();
        let first = SharedI18n::new(named.i18n("fr-FR").unwrap());
        assert_eq!(first.t("hello"), "Hallo!");
        assert_eq!(first.read().fallback_chain, vec![lang("de-DE")]);
        // Each registry is its own: switching one leaves the next as built.
        first.write().set_current(lang("en-US"));
        let second = SharedI18n::new(named.i18n("fr-FR").unwrap());
        assert_eq!(second.read().current, lang("fr-FR"));
        // No chain named is the one a desktop app starts with.
        let default = SharedI18n::new(
            Catalogues::parse(&catalogues, &[])
                .unwrap()
                .i18n("fr-FR")
                .unwrap(),
        );
        assert_eq!(default.t("hello"), "Hello!");
        assert_eq!(default.t("bye"), "bye");
        assert!(named.i18n("not a tag").is_err());
        assert!(Catalogues::parse(&[("de-DE".into(), "= x".into())], &[]).is_err());
        // A message declared twice fails the parse, not a later registry.
        assert!(Catalogues::parse(&[("de-DE".into(), "a = 1\na = 2\n".into())], &[]).is_err());
        assert!(Catalogues::parse(&[("not a tag".into(), "a = 1\n".into())], &[]).is_err());
    }

    #[test]
    fn read_catalogues_orders_by_tag_and_refuses_a_stem_that_is_no_tag() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-i18n-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en-US.ftl"), "greet = Hello!\n").unwrap();
        std::fs::write(dir.join("de-DE.ftl"), "greet = Hallo!\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a catalogue").unwrap();
        let read = read_catalogues(&dir, |p| std::fs::read(p)).unwrap();
        assert_eq!(
            read,
            vec![
                ("de-DE".to_string(), "greet = Hallo!\n".to_string()),
                ("en-US".to_string(), "greet = Hello!\n".to_string()),
            ]
        );
        std::fs::write(dir.join("not a tag.ftl"), "greet = x\n").unwrap();
        assert!(read_catalogues(&dir, |p| std::fs::read(p)).is_err());
        let _ = std::fs::remove_dir_all(&dir);

        let missing = read_catalogues(std::path::Path::new("/definitely/not/here"), |p| {
            std::fs::read(p)
        });
        assert!(missing.unwrap().is_empty());
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
        assert_eq!(i.t("greet", &args), "Hello \u{2068}World\u{2069}!");
    }

    /// A substituted value carries the Unicode isolation marks; a message
    /// with no placeable comes back exactly as it was written, and a term
    /// reference is inlined without marks.
    #[test]
    fn placeables_are_bidi_isolated() {
        let mut i = I18n::new(lang("ar"), vec![]);
        i.load_ftl(
            lang("ar"),
            "-brand = Lumen\n\
             greet = \u{645}\u{631}\u{62d}\u{628}\u{627} { $name }\n\
             plain = \u{645}\u{631}\u{62d}\u{628}\u{627}\n\
             branded = \u{645}\u{631}\u{62d}\u{628}\u{627} { -brand }\n",
        )
        .unwrap();
        let mut args = FluentArgs::new();
        args.set("name", FluentValue::from("Alice"));
        assert_eq!(
            i.t("greet", &args),
            "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{2068}Alice\u{2069}"
        );
        let empty = FluentArgs::new();
        assert_eq!(i.t("plain", &empty), "\u{645}\u{631}\u{62d}\u{628}\u{627}");
        assert_eq!(
            i.t("branded", &empty),
            "\u{645}\u{631}\u{62d}\u{628}\u{627} Lumen"
        );
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
    fn a_dotted_key_resolves_a_message_attribute() {
        let mut i = I18n::new(lang("de-DE"), vec![]);
        i.load_ftl(
            lang("de-DE"),
            "search = Suche\n    .placeholder = Katalog durchsuchen\n",
        )
        .unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.t("search", &args), "Suche");
        assert_eq!(i.t("search.placeholder", &args), "Katalog durchsuchen");
        // An attribute the message does not declare is a miss, not the
        // message value.
        assert_eq!(i.try_t("search.alt", &args), None);
        assert_eq!(i.try_t("nothing.placeholder", &args), None);
    }

    #[test]
    fn a_message_can_carry_attributes_without_a_value() {
        let mut i = I18n::new(lang("de-DE"), vec![]);
        i.load_ftl(lang("de-DE"), "logo =\n    .alt = Das Lumen-Logo\n")
            .unwrap();
        let args = FluentArgs::new();
        assert_eq!(i.t("logo.alt", &args), "Das Lumen-Logo");
        // The image has no text of its own to translate.
        assert_eq!(i.try_t("logo", &args), None);
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
        let loaded = i.load_dir(&dir, |p| std::fs::read(p)).unwrap();
        assert_eq!(loaded.len(), 2);
        let args = FluentArgs::new();
        assert_eq!(i.t("greet", &args), "Hallo!");
        assert_eq!(i.t_with_lang(&lang("en-US"), "greet", &args), "Hello!");

        // The bytes come through the caller's read seam, not the raw
        // filesystem: a source that overlays a catalogue path wins over the
        // bytes on disk, exactly like every other asset read.
        let overlaid = i
            .load_dir(&dir, |p| {
                if p.file_name().and_then(|n| n.to_str()) == Some("de-DE.ftl") {
                    Ok(b"greet = Servus!\n".to_vec())
                } else {
                    std::fs::read(p)
                }
            })
            .unwrap();
        assert_eq!(overlaid.len(), 2);
        assert_eq!(i.t("greet", &args), "Servus!");

        // A stem that is not a BCP-47 tag is a load error, not a silent skip.
        std::fs::write(dir.join("not a tag.ftl"), "greet = x\n").unwrap();
        assert!(i.load_dir(&dir, |p| std::fs::read(p)).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dir_tolerates_a_missing_directory() {
        let mut i = I18n::default();
        let loaded = i
            .load_dir(std::path::Path::new("/definitely/not/here"), |p| {
                std::fs::read(p)
            })
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
        let app = world.resource::<lumen_core::i18n::AppI18n>().clone();
        assert_eq!(
            app.format("number", "1234.5").as_deref(),
            Some("1\u{202f}234,5")
        );
    }

    #[test]
    fn plugin_installs_the_fallback_locale_the_app_named() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("fr-FR"))
            .with_fallback_locale(lang("de-DE"))
            .install(&mut world);
        let shared = world.resource::<SharedI18n>().clone();
        assert_eq!(shared.read().fallback_chain, vec![lang("de-DE")]);
    }

    /// The chain keeps every locale it was given, including the one the app
    /// starts in. Pruning it at install would be right for that locale only;
    /// after a switch the entry the app came from is the one a miss needs.
    /// The duplicate probe is avoided at lookup instead.
    #[test]
    fn a_fallback_equal_to_the_active_locale_stays_in_the_chain() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("de-DE"))
            .with_fallback_locale(lang("de-DE"))
            .install(&mut world);
        let shared = world.resource::<SharedI18n>().clone();
        assert_eq!(shared.read().fallback_chain, vec![lang("de-DE")]);
    }

    #[test]
    fn a_switch_moves_the_catalogue_the_formatter_and_the_direction() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("en-US"))
            .install(&mut world);
        let shared = world.resource::<SharedI18n>().clone();
        shared
            .write()
            .load_ftl(lang("en-US"), "greet = Good morning")
            .unwrap();
        shared
            .write()
            .load_ftl(lang("de-DE"), "greet = Guten Morgen")
            .unwrap();
        let app = world.resource::<lumen_core::i18n::AppI18n>().clone();
        assert_eq!(app.try_translate("greet").as_deref(), Some("Good morning"));
        let english_number = app.format("number", "1234.5");

        let change = app.set_locale("de-DE").expect("de-DE is a locale tag");
        assert_eq!(change.locale, "de-DE");
        assert_eq!(
            change.direction,
            lumen_core::components::LayoutDirection::Ltr
        );
        assert_eq!(app.try_translate("greet").as_deref(), Some("Guten Morgen"));
        // The formatter moved with the catalogue: the two locales group and
        // point their decimals differently, so one number reads two ways.
        assert_ne!(app.format("number", "1234.5"), english_number);

        // And back: the locale the app started in still resolves, which is
        // what the chain would have lost had it been pruned at install.
        let change = app.set_locale("en-US").expect("en-US is a locale tag");
        assert_eq!(change.locale, "en-US");
        assert_eq!(app.try_translate("greet").as_deref(), Some("Good morning"));
    }

    #[test]
    fn a_right_to_left_locale_reports_its_direction() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("en-US"))
            .install(&mut world);
        let app = world.resource::<lumen_core::i18n::AppI18n>().clone();
        let change = app.set_locale("ar-EG").expect("ar-EG is a locale tag");
        assert_eq!(
            change.direction,
            lumen_core::components::LayoutDirection::Rtl
        );
    }

    #[test]
    fn a_tag_that_is_not_a_locale_changes_nothing() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("en-US"))
            .install(&mut world);
        let shared = world.resource::<SharedI18n>().clone();
        let app = world.resource::<lumen_core::i18n::AppI18n>().clone();
        assert_eq!(app.set_locale("not a tag at all"), None);
        assert_eq!(shared.read().current, lang("en-US"));
    }

    /// Naming a locale with no catalogue is legal, at startup and at a
    /// switch alike: every message falls through to the locale the app's
    /// source strings are written in.
    #[test]
    fn switching_to_a_locale_with_no_catalogue_falls_back() {
        let mut world = bevy_ecs::world::World::new();
        I18nPlugin::default()
            .with_locale(lang("en-US"))
            .with_fallback_locale(lang("en-US"))
            .install(&mut world);
        let shared = world.resource::<SharedI18n>().clone();
        shared
            .write()
            .load_ftl(lang("en-US"), "greet = Good morning")
            .unwrap();
        let app = world.resource::<lumen_core::i18n::AppI18n>().clone();
        app.set_locale("ja-JP").expect("ja-JP is a locale tag");
        assert_eq!(app.try_translate("greet").as_deref(), Some("Good morning"));
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
