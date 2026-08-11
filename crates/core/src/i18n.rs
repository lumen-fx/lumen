//! Process-wide translation hook - the one surface a script host reaches
//! translation through.
//!
//! Translation itself lives in `lumen-i18n` (Fluent bundles, locale
//! fallback, ICU4X formatters), which core does not depend on: core carries
//! no backend crates. What core owns is the seam. The runtime installs a
//! translator with [`set_translator`] once it has loaded the app's
//! catalogues; every script host calls [`translate`] from its `t()` / `tr()`
//! builtin without linking Fluent or reaching into the world.
//!
//! This mirrors [`crate::nav`]: one process-global bus, many producers and
//! consumers, no per-language plumbing. An app that never installs a
//! translator still resolves every key - [`translate`] returns the key
//! itself, which is exactly what an untranslated string should render as.
//!
//! One translator is live at a time, so a host process running two Lumen
//! apps shares the second app's catalogue with the first. Markup
//! translation avoids this by reading the per-app resource instead.

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

#[cfg(test)]
mod tests {
    use super::*;

    // The translator slot is process-global, so these run one at a time.
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
}
