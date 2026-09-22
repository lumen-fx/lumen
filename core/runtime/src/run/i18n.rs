//! Locale wiring: locale resolution, catalogue loading, the app's base
//! writing direction, and the script-side translator and formatter hooks.
//!
//! An app's translations live in `<app_dir>/locale/<lang>.ftl`, one Fluent
//! catalogue per locale, which `lumenc i18n extract` writes and translators
//! edit. At startup every catalogue is loaded and the locale the app
//! starts in comes from `[app] locale`, else the locale the OS reports,
//! else `en-US`. A script changes it later with `set_locale`, and every
//! catalogue is already in memory, so the switch needs no file read. A key
//! the active locale lacks falls through to the locale the
//! app's source strings are written in, which is `[app] fallback_locale`,
//! else `en-US`; naming the active locale there leaves the miss resolving
//! to the text the author wrote.
//!
//! Two readers share one registry: markup (`translatable="key"`, resolved
//! in `crate::spawn`) reads [`lumen_core::i18n::AppI18n`] off the world,
//! and scripts (`t("key")`) reach the same catalogue through the
//! process-wide [`lumen_core::i18n`] hook installed here. Both see a
//! catalogue reload the moment it lands.
//!
//! Locale-aware formatting is split the same way and over the same pair of
//! seams: markup (`format="currency:EUR"`) reads the world resource and
//! scripts (`format_currency(...)`) reach the app's ICU4X formatters
//! through the formatting hook installed here. Reading the per-app
//! resource for markup is what keeps a process hosting two Lumen apps from
//! rendering one app's text in the other's locale.
//!
//! The resolved locale also sets the app's base writing direction, through
//! [`lumen_core::components::DefaultLayoutDirection`]: an app running in
//! Arabic or Hebrew is mirrored with nothing in its markup saying so, which
//! is what the web target already does from the `<html>` tag. A `dir`
//! attribute overrides it for the element it is on and everything under it.
//! Switching locale moves the direction with the catalogue, so picking
//! Arabic from a language menu mirrors the tree that is already on screen.

use super::*;

use lumen_core::i18n::AppI18n;
use lumen_i18n::{I18nPlugin, SharedI18n};

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

/// Resolve the locale the app starts in, install [`SharedI18n`] +
/// [`AppI18n`], set the base writing direction from the locale, load every
/// catalogue under `<dir>/locale`, and publish the translator every script
/// host's `t()` builtin calls, the formatter its `format_*` builtins call,
/// and the tag its `locale()` builtin reads.
pub(crate) fn register_i18n(
    app: &mut App,
    dir: &Path,
    cfg: &crate::config::LumenToml,
) -> Result<(), RunError> {
    let mut plugin = I18nPlugin::default();
    if let Some(locale) = &cfg.app.locale {
        plugin = plugin.with_locale(locale.clone());
    }
    if let Some(fallback) = &cfg.app.fallback_locale {
        plugin = plugin.with_fallback_locale(fallback.clone());
    }
    let current = plugin.install(&mut app.world);
    // The locale carries the base writing direction, the same way the web
    // target reads it off the tag: an app running in Arabic is mirrored
    // without the author asking. A `dir` in the markup still wins, on the
    // root and on any subtree, through the existing cascade.
    let direction = if lumen_i18n::is_rtl(&current) {
        LayoutDirection::Rtl
    } else {
        LayoutDirection::Ltr
    };
    app.world.insert_resource(DefaultLayoutDirection(direction));
    let shared = app.world.resource::<SharedI18n>().clone();
    let fallback = shared
        .read()
        .fallback_chain
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let loaded = shared
        .write()
        .load_dir(&locale_dir(dir), catalogue_reader(&app.world))
        .map_err(|e| RunError::I18n(e.to_string()))?;
    tracing::debug!(locale = %current, fallback = %fallback, catalogues = loaded.len(), "i18n ready");

    let for_scripts = app.world.resource::<AppI18n>().clone();
    let formatting = for_scripts.clone();
    lumen_core::i18n::set_translator(move |key| for_scripts.try_translate(key));
    lumen_core::i18n::set_formatter(move |spec, value| formatting.format(spec, value));
    // What a script's `locale()` answers before anything switches. The
    // starting locale can come from the OS, so no script knows it otherwise,
    // and a language menu needs it to mark the entry it is already on.
    lumen_core::i18n::set_active_locale(&current.to_string());
    Ok(())
}

/// Load the catalogues a compiled app carries for every locale that has no
/// loose file under `locale/`.
///
/// A loose file wins: [`register_i18n`] has already loaded every one, and a
/// locale it loaded is passed over here. That keeps the loose-files rule for
/// an app's data, so a translator edits the file beside an app and the app
/// reads the edit, while an app shipped with no `locale/` directory still
/// reads in every language it was built with. The same goes for the fallback
/// chain: `[app] fallback_locale` in `lumen.toml` wins over the one the
/// artifact recorded.
pub(crate) fn add_compiled_catalogues(
    world: &mut World,
    cfg: &crate::config::LumenToml,
    compiled: &lumen_ir::artifact::CompiledI18n,
) -> Result<(), RunError> {
    if compiled.catalogues.is_empty() && compiled.fallback.is_empty() {
        return Ok(());
    }
    let Some(shared) = world.get_resource::<SharedI18n>().cloned() else {
        return Ok(());
    };
    let mut i18n = shared.write();
    for (tag, source) in &compiled.catalogues {
        let lang: lumen_i18n::LanguageIdentifier = lumen_i18n::Lang::try_from(tag.as_str())
            .map_err(|e| RunError::I18n(e.to_string()))?
            .into();
        if i18n.bundles.contains_key(&lang) {
            continue;
        }
        i18n.load_ftl(lang, source)
            .map_err(|e| RunError::I18n(format!("the compiled `{tag}` catalogue: {e}")))?;
    }
    if cfg.app.fallback_locale.is_none() && !compiled.fallback.is_empty() {
        i18n.fallback_chain = compiled
            .fallback
            .iter()
            .map(|tag| lumen_i18n::Lang::try_from(tag.as_str()).map(Into::into))
            .collect::<Result<_, _>>()
            .map_err(|e| RunError::I18n(e.to_string()))?;
    }
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
