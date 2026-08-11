//! Translation wiring: locale resolution, catalogue loading, and the
//! script-side translator hook.
//!
//! An app's translations live in `<app_dir>/locale/<lang>.ftl`, one Fluent
//! catalogue per locale, which `lumenc i18n extract` writes and translators
//! edit. At startup every catalogue is loaded and the active locale is
//! chosen from `[app] locale`, else the locale the OS reports, else
//! `en-US`; a key the active locale lacks falls through to `en-US`.
//!
//! Two readers share one registry through [`lumen_i18n::SharedI18n`]:
//! markup (`translatable="key"`, resolved in `crate::spawn`) reads the
//! resource, and scripts (`t("key")`) reach it through the process-wide
//! [`lumen_core::i18n`] hook installed here. Both see a catalogue reload
//! the moment it lands.

use super::*;

use lumen_i18n::{I18nPlugin, SharedI18n};

/// Directory holding an app's `.ftl` catalogues.
pub(crate) fn locale_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("locale")
}

/// Resolve the locale, install [`SharedI18n`] + `LocaleFormatter`, load
/// every catalogue under `<dir>/locale`, and publish the translator every
/// script host's `t()` builtin calls.
pub(crate) fn register_i18n(
    app: &mut App,
    dir: &Path,
    cfg: &crate::config::LumenToml,
) -> Result<(), RunError> {
    let mut plugin = I18nPlugin::default();
    if let Some(locale) = &cfg.app.locale {
        plugin = plugin.with_locale(locale.clone());
    }
    let current = plugin.install(&mut app.world);
    let shared = app.world.resource::<SharedI18n>().clone();
    let loaded = shared
        .write()
        .load_dir(&locale_dir(dir))
        .map_err(|e| RunError::I18n(e.to_string()))?;
    tracing::debug!(locale = %current, catalogues = loaded.len(), "i18n ready");

    let for_scripts = shared.clone();
    lumen_core::i18n::set_translator(move |key| for_scripts.try_t(key));
    Ok(())
}

/// Re-read every catalogue into the live registry. Called by hot reload
/// after an edit under `locale/`; `load_ftl` replaces a locale's bundle, so
/// the running app picks the new strings up without a restart.
#[cfg(feature = "runtime-parse")]
pub(crate) fn reload_catalogues(world: &mut World, dir: &Path) {
    let Some(shared) = world.get_resource::<SharedI18n>().cloned() else {
        return;
    };
    if let Err(e) = shared.write().load_dir(&locale_dir(dir)) {
        eprintln!("lumenc hot-reload: locale catalogue failed to load: {e}");
    }
}

/// Path + mtime of every `.ftl` file under `<dir>/locale`, sorted by path.
/// The hot-reload sweep compares whole stamp lists rather than a fixed set
/// of paths, so adding or deleting a catalogue registers as a change too.
#[cfg(feature = "runtime-parse")]
pub(crate) fn locale_stamps(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(locale_dir(dir)) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Option<SystemTime>)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ftl"))
        .map(|p| {
            let m = mtime(&p);
            (p, m)
        })
        .collect();
    out.sort();
    out
}
