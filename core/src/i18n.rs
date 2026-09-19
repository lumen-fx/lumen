//! Process-wide translation and formatting hooks - the surfaces a script
//! host reaches an app's locale through.
//!
//! Both capabilities live in `lumen-i18n` (Fluent bundles, locale fallback,
//! ICU4X formatters), which core does not depend on: core carries no
//! backend crates. What core owns is the seam. The runtime installs a
//! translator with [`set_translator`] once it has loaded the app's
//! catalogues, and a formatter with [`set_formatter`] once it knows the
//! locale; every script host calls [`translate`] from its `t()` / `tr()`
//! builtin and [`format`] from its `format_*` builtins, without linking
//! Fluent or ICU or reaching into the world.
//!
//! [`format`] takes two opaque strings, a spec and a value, and hands back
//! what the other side made of them. Core never learns that `currency:EUR`
//! names a currency, which is what lets a core-owned system apply a format
//! without core naming the capability.
//!
//! This mirrors [`crate::nav`]: one process-global bus, many producers and
//! consumers, no per-language plumbing. An app that never installs a
//! translator still resolves every key - [`translate`] returns the key
//! itself, which is exactly what an untranslated string should render as -
//! and an app with no formatter leaves every value as it stands.
//!
//! An app's locale is not fixed for the life of the process: [`AppI18n`]
//! carries the switch, and [`active_locale`] answers which locale is
//! running for a script that has no world to read the resource from.
//!
//! One of each is live at a time, so a host process running two Lumen apps
//! shares the second app's catalogue and locale with the first. Markup
//! avoids this by reading [`AppI18n`] off the world instead, which is the
//! same set of handles scoped to one app.
//!
//! Both shapes are opaque the whole way through, so a host links only
//! what it installs. A page whose text was already written for its locale
//! while the site was built installs nothing here, and carries neither a
//! catalogue nor a formatter.

use bevy_ecs::resource::Resource;
use std::sync::{Arc, RwLock};

/// Resolves a translation key against the active catalogue. Returns `None`
/// when the catalogue has no entry for the key, so callers can apply their
/// own fallback.
pub type Translator = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

static TRANSLATOR: RwLock<Option<Translator>> = RwLock::new(None);

/// Install the process-wide translator, replacing any previous one.
///
/// The runtime calls this after loading `<app_dir>/locale/*.ftl`. Reloading
/// a catalogue needs no second call: a translator that closes over shared
/// state sees the new bundles immediately.
pub fn set_translator<F>(f: F)
where
    F: Fn(&str) -> Option<String> + Send + Sync + 'static,
{
    let mut slot = TRANSLATOR.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(Arc::new(f));
}

/// Remove the installed translator. [`translate`] falls back to returning
/// keys verbatim.
pub fn clear_translator() {
    let mut slot = TRANSLATOR.write().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// Resolve `key` against the installed translator, or `None` when no
/// translator is installed or the catalogue lacks the key.
pub fn try_translate(key: &str) -> Option<String> {
    let f = {
        let slot = TRANSLATOR.read().unwrap_or_else(|e| e.into_inner());
        slot.clone()?
    };
    f(key)
}

/// Resolve `key`, falling back to the key itself. This is what a script's
/// `t("key")` returns.
pub fn translate(key: &str) -> String {
    try_translate(key).unwrap_or_else(|| key.to_string())
}

/// Formats one value the way one spec asks for. Both arguments are
/// opaque to core: the spec is whatever a `format` attribute or a
/// `format_*` builtin wrote, and what it means is the formatter's
/// business. Returns `None` when the spec is not one it knows or the
/// value is not what that spec expects, so callers can leave the text
/// alone.
pub type Formatter = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

static FORMATTER: RwLock<Option<Formatter>> = RwLock::new(None);

/// Install the process-wide formatter, replacing any previous one.
///
/// The runtime calls this once it has resolved the app's locale.
pub fn set_formatter<F>(f: F)
where
    F: Fn(&str, &str) -> Option<String> + Send + Sync + 'static,
{
    let mut slot = FORMATTER.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(Arc::new(f));
}

/// Remove the installed formatter. [`format`] answers `None` for
/// everything afterwards.
pub fn clear_formatter() {
    let mut slot = FORMATTER.write().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// Format `value` per `spec`, or `None` when no formatter is installed,
/// the spec is not one it knows, or the value is not what the spec
/// expects. A caller shows `value` unchanged then.
pub fn format(spec: &str, value: &str) -> Option<String> {
    let f = {
        let slot = FORMATTER.read().unwrap_or_else(|e| e.into_inner());
        slot.clone()?
    };
    f(spec, value)
}

/// The locale the app is running in, as a plain BCP-47 string.
///
/// Read by a script's `locale()` builtin, which runs inside a script
/// engine with no world to reach the per-app handle through. It carries
/// the same "one live at a time" caveat as the translator hook, and for
/// the same reason.
static ACTIVE_LOCALE: RwLock<String> = RwLock::new(String::new());

/// Publish the locale the app is running in. The runtime calls this once
/// the locale is resolved, and again every time it changes.
pub fn set_active_locale(tag: &str) {
    let mut slot = ACTIVE_LOCALE.write().unwrap_or_else(|e| e.into_inner());
    slot.clear();
    slot.push_str(tag);
}

/// The locale the app is running in, or an empty string when nothing has
/// published one.
pub fn active_locale() -> String {
    ACTIVE_LOCALE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// What changed when an app switched locale: the locale it is now in, and
/// the writing direction that locale reads in.
///
/// Both are things core already owns. The tag is an opaque string, and the
/// direction is the cascade's own [`crate::components::LayoutDirection`],
/// so core learns nothing about BCP-47 or Fluent by carrying this back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleChange {
    /// The locale now active.
    pub locale: String,
    /// The base writing direction that locale reads in.
    pub direction: crate::components::LayoutDirection,
}

/// Switches one app to another locale, answering with what changed, or
/// `None` when the tag is not a locale at all and nothing moved.
///
/// Naming a locale the app has no catalogue for is not a failure: every
/// message falls back, which is the rule startup already follows.
pub type LocaleSetter = Arc<dyn Fn(&str) -> Option<LocaleChange> + Send + Sync>;

/// One app's own translator, formatter and locale switch, held in its
/// world.
///
/// The hooks above are process-wide, which is what a script host needs
/// and all it can reach. Markup is spawned from a world, so it reads
/// this instead: a process hosting two Lumen apps renders each app's
/// text in its own locale, and switching one app's locale leaves the
/// other where it was.
///
/// The handles are the same opaque ones. Core forwards a key, or a spec
/// and a value, or a locale tag, and returns what came back; the locale
/// side of the seam is what the runtime installs here.
#[derive(Resource, Clone)]
pub struct AppI18n {
    translator: Translator,
    formatter: Formatter,
    set_locale: LocaleSetter,
}

impl AppI18n {
    /// Pair a translator, a formatter and a locale switch for one app.
    pub fn new(translator: Translator, formatter: Formatter, set_locale: LocaleSetter) -> Self {
        Self {
            translator,
            formatter,
            set_locale,
        }
    }

    /// Resolve `key` against this app's catalogue, or `None` on a miss.
    pub fn try_translate(&self, key: &str) -> Option<String> {
        (self.translator)(key)
    }

    /// Format `value` per `spec` for this app's locale, or `None` when
    /// the spec is not one it knows or the value is not what that spec
    /// expects. A caller shows `value` unchanged then.
    pub fn format(&self, spec: &str, value: &str) -> Option<String> {
        (self.formatter)(spec, value)
    }

    /// Switch this app to `tag`, answering with the locale it is now in
    /// and the writing direction to read it in. `None` means `tag` is
    /// not a locale tag and nothing changed.
    ///
    /// Reach this through a `ResMut<AppI18n>` borrow: the systems that
    /// rebuild an app's text wake on the resource being marked changed.
    pub fn set_locale(&self, tag: &str) -> Option<LocaleChange> {
        (self.set_locale)(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The translator and formatter slots are process-global, so these
    // run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn missing_translator_returns_key() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        clear_translator();
        assert_eq!(translate("app-title"), "app-title");
        assert_eq!(try_translate("app-title"), None);
    }

    #[test]
    fn installed_translator_resolves_and_falls_back() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_translator(|key| (key == "greet").then(|| "Hallo".to_string()));
        assert_eq!(translate("greet"), "Hallo");
        assert_eq!(translate("nope"), "nope");
        clear_translator();
    }

    #[test]
    fn set_replaces_previous() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_translator(|_| Some("first".to_string()));
        set_translator(|_| Some("second".to_string()));
        assert_eq!(translate("any"), "second");
        clear_translator();
    }

    #[test]
    fn missing_formatter_answers_nothing() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        clear_formatter();
        assert_eq!(format("number", "1234.5"), None);
    }

    #[test]
    fn installed_formatter_sees_the_spec_and_the_value() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_formatter(|spec, value| (spec == "number").then(|| format!("<{value}>")));
        assert_eq!(format("number", "1234.5").as_deref(), Some("<1234.5>"));
        assert_eq!(format("wat", "1234.5"), None);
        clear_formatter();
    }

    #[test]
    fn app_handles_answer_without_the_process_hooks() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        clear_translator();
        clear_formatter();
        let app = AppI18n::new(
            Arc::new(|key| (key == "greet").then(|| "Hallo".to_string())),
            Arc::new(|spec, value| (spec == "number").then(|| format!("<{value}>"))),
            Arc::new(|_| None),
        );
        assert_eq!(app.try_translate("greet").as_deref(), Some("Hallo"));
        assert_eq!(app.try_translate("nope"), None);
        assert_eq!(app.format("number", "1234.5").as_deref(), Some("<1234.5>"));
        assert_eq!(app.format("wat", "1234.5"), None);
    }

    #[test]
    fn the_locale_switch_answers_with_what_changed() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let app = AppI18n::new(
            Arc::new(|_| None),
            Arc::new(|_, _| None),
            Arc::new(|tag| {
                (tag == "ar-EG").then(|| LocaleChange {
                    locale: tag.to_string(),
                    direction: crate::components::LayoutDirection::Rtl,
                })
            }),
        );
        let change = app.set_locale("ar-EG").expect("a locale the switch takes");
        assert_eq!(change.locale, "ar-EG");
        assert_eq!(change.direction, crate::components::LayoutDirection::Rtl);
        assert_eq!(app.set_locale("not a tag"), None);
    }

    #[test]
    fn the_active_locale_starts_empty_and_holds_what_was_published() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_active_locale("");
        assert_eq!(active_locale(), "");
        set_active_locale("de-DE");
        assert_eq!(active_locale(), "de-DE");
        set_active_locale("");
    }

    #[test]
    fn set_formatter_replaces_previous() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_formatter(|_, _| Some("first".to_string()));
        set_formatter(|_, _| Some("second".to_string()));
        assert_eq!(format("number", "1").as_deref(), Some("second"));
        clear_formatter();
    }
}
