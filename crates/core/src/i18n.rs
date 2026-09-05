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
//! One of each is live at a time, so a host process running two Lumen apps
//! shares the second app's catalogue and locale with the first. Markup
//! avoids this by reading the per-app resources instead.

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
    fn set_formatter_replaces_previous() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_formatter(|_, _| Some("first".to_string()));
        set_formatter(|_, _| Some("second".to_string()));
        assert_eq!(format("number", "1").as_deref(), Some("second"));
        clear_formatter();
    }
}
