//! Locale wiring: locale resolution, catalogue loading, and the
//! script-side translator and formatter hooks.
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
//!
//! Locale-aware formatting is split the same way and over the same pair of
//! seams. [`lumen_i18n::SharedFormatter`] holds the app's ICU4X formatters;
//! markup (`format="currency:EUR"`) reads the resource and scripts
//! (`format_currency(...)`) reach it through the formatting hook installed
//! here. Reading the per-app resource for markup is what keeps a process
//! hosting two Lumen apps from formatting one app's text in the other's
//! locale.

use super::*;

use lumen_i18n::{I18nPlugin, SharedFormatter, SharedI18n};

/// Directory holding an app's `.ftl` catalogues.
pub fn locale_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("locale")
}

/// The byte-read seam catalogue loads go through: the app's asset source
/// chain when the asset server is up (so a source overlaying a catalogue
/// path wins, like every other asset read), else the plain filesystem.
fn catalogue_reader(world: &World) -> impl Fn(&Path) -> std::io::Result<Vec<u8>> + use<> {
    let reader = world
        .get_resource::<lumen_assets::AssetServer>()
        .map(|s| s.source_reader());
    move |path: &Path| match &reader {
        Some(reader) => reader.read(path),
        None => std::fs::read(path),
    }
}

/// Resolve the locale, install [`SharedI18n`] + [`SharedFormatter`], load
/// every catalogue under `<dir>/locale`, and publish the translator every
/// script host's `t()` builtin calls and the formatter its `format_*`
/// builtins call.
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
        .load_dir(&locale_dir(dir), catalogue_reader(&app.world))
        .map_err(|e| RunError::I18n(e.to_string()))?;
    tracing::debug!(locale = %current, catalogues = loaded.len(), "i18n ready");

    let for_scripts = shared.clone();
    lumen_core::i18n::set_translator(move |key| for_scripts.try_t(key));

    let formatter = app.world.resource::<SharedFormatter>().clone();
    lumen_core::i18n::set_formatter(move |spec, value| {
        lumen_i18n::format_spec(formatter.get(), spec, value)
    });
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
    if let Err(e) = shared
        .write()
        .load_dir(&locale_dir(dir), catalogue_reader(world))
    {
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
