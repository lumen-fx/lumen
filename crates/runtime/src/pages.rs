//! File-based pages - multi-`.lmn` discovery, registration, and the
//! runtime navigation resolver.
//!
//! ## The model (Next.js / SvelteKit file-based routing, real-HTML `<a>`)
//!
//! Every `.lmn` file in the app's `src/` directory is a page, keyed by its
//! filename stem. The home page is `index.lmn` (falling back to the `[app] entry`
//! stem, then `main.lmn` for single-file compat). All pages load up front,
//! and the fragments declared in ANY of the app's files are merged into one
//! table every page parses against, so a shared `layout.lmn` template (with
//! a `<slot>`) is usable from every page.
//!
//! Rendering reuses the existing `<if>` reconciler with ZERO new machinery:
//! the assembled tree is the entry `<root>` holding one synthetic
//! `<if signal="route.path" eq="<page-key>">` gate per page, wrapping that
//! page's content. Navigation is a reserved-signal write (`route.path`); the
//! reconciler mounts the matching gate and unmounts the rest. This keeps
//! navigation reactive-only (no per-frame tick) and candela-neutral (the
//! navigation command rides the shared bus in [`lumen_core::nav`], not a
//! Rhai builtin).
//!
//! ## Page resolution
//!
//! The framework does not pattern-match `:id` segments. A requested path is
//! resolved by longest existing `.lmn` prefix ([`lumen_core::nav::resolve_path`]):
//! `/settings` -> `settings.lmn`; `/user/7` (no `user/7.lmn`) -> `user.lmn`
//! with `/7` exposed on the reserved `route.segment` signal for the page's
//! own code to parse.
//!
//! ## Deferred seams
//!
//! - **Web transpile:** [`RouteHistory`] is an in-memory back/forward stack.
//!   The web target binds the same navigation surface to the real
//!   `history.pushState` / `popstate` API and `<a href>` to a real DOM anchor
//!   + URL. The reserved-signal surface is unchanged; only the history
//!   backend swaps.
//! - **AOT packaging:** `lumenc build` bakes ONE `LayoutIR`; the assembled
//!   multi-page IR (this module's output) already is one `LayoutIR`, so the
//!   AOT path folds in by running [`assemble`] at build time and serialising
//!   the combined IR. Page keys travel on the IR; a `PageRegistry` is rebuilt
//!   from the discovered set at load. (Not wired here - noted as a follow-up.)

use bevy_ecs::prelude::*;
use lumen_core::nav;
use std::path::{Path, PathBuf};

use crate::config::LumenToml;
#[cfg(feature = "runtime-parse")]
use lumen_ir::fragment::FragmentTable;
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec, LayoutIR};

// The resolver, the history and the anchor are host-neutral and live in
// `lumen-scene` beside the reconciler they drive. Discovery is what keeps
// this module here: it reads a directory.
pub use lumen_scene::routing::{
    Anchor, Location, PageRegistry, RouteHistory, apply_navigation, install_routing,
    navigate_on_anchor_click,
};

/// One discovered page file.
#[derive(Clone, Debug)]
pub struct PageFile {
    /// Page key - the filename stem (`settings` for `settings.lmn`).
    pub key: String,
    /// Absolute (or app-relative) path to the `.lmn` file.
    pub path: PathBuf,
}

/// The static plan produced by [`discover`], read by the loader and the
/// runtime. Mirrors what `lumen.toml [pages]` describes when explicit.
#[derive(Clone, Debug, Resource)]
pub struct PagePlan {
    /// `true` when more than one page participates (or `[pages] enabled`
    /// forces it). `false` keeps the legacy single-file load path untouched.
    pub multipage: bool,
    /// Home page key.
    pub entry_key: String,
    /// The `.lmn` file handed to the loader as the entry (`html_path`).
    pub entry_file: PathBuf,
    /// All navigable pages, entry first. Excludes the shared `layout.lmn`.
    pub pages: Vec<PageFile>,
    /// Every `.lmn` file that contributes fragments to the app-wide table:
    /// all pages plus the shared `layout.lmn`. Read by
    /// [`collect_fragments`]; wider than [`Self::pages`] so `layout.lmn`
    /// (which is not itself a page) still shares its fragments app-wide.
    pub fragment_files: Vec<PathBuf>,
    /// Directory the page files live in: the app's `src/`.
    pub dir: PathBuf,
}

impl PagePlan {
    /// Page keys, longest-first (so the longest existing prefix wins in
    /// resolution).
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.pages.iter().map(|p| p.key.clone()).collect();
        keys.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        keys
    }
}

// -- discovery ---------------------------------------------------------------

/// Discover the pages in `src_dir` (the app's `src/`, from
/// [`crate::app_layout::AppLayout`]), honouring `[pages]` / `[app]` config.
///
/// Single-file apps (a lone `main.lmn` or `index.lmn`, and no `[pages]
/// enabled = true`) come back with `multipage = false` and the existing
/// single-file load path runs unchanged - the compat guarantee.
pub fn discover(src_dir: &Path, cfg: &LumenToml) -> PagePlan {
    // 1. Scan the directory once for every `.lmn` file. This set feeds the
    //    app-wide fragment table (so a shared `layout.lmn` is usable from
    //    every page) and, absent an explicit `[pages] include`, the navigable
    //    page set.
    let mut all_lmn: Vec<PageFile> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(src_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("lmn") {
                all_lmn.push(PageFile {
                    key: stem(&path.file_name().unwrap().to_string_lossy()),
                    path,
                });
            }
        }
    }
    all_lmn.sort_by(|a, b| a.key.cmp(&b.key));

    // 2. Navigable pages. `layout.lmn` is the shared layout, not a page: it
    //    contributes its fragments to the table but never gets its own
    //    `<if>` gate or a navigable key. An explicit `[pages] include` lists
    //    the navigable set verbatim; otherwise every non-layout `.lmn` is a
    //    page.
    let mut files: Vec<PageFile> = if let Some(list) = &cfg.pages.include {
        list.iter()
            .map(|name| PageFile {
                key: stem(name),
                path: src_dir.join(name),
            })
            .collect()
    } else {
        all_lmn
            .iter()
            .filter(|f| f.key != LAYOUT_STEM)
            .cloned()
            .collect()
    };

    // 3. Pick the entry key.
    let entry_stem = cfg.app.entry.as_deref().map(stem);
    let entry_key = cfg
        .pages
        .entry
        .clone()
        .filter(|k| files.iter().any(|f| &f.key == k))
        .or_else(|| {
            files
                .iter()
                .find(|f| f.key == "index")
                .map(|f| f.key.clone())
        })
        .or_else(|| {
            entry_stem
                .as_ref()
                .filter(|s| files.iter().any(|f| &f.key == *s))
                .cloned()
        })
        .or_else(|| {
            files
                .iter()
                .find(|f| f.key == "main")
                .map(|f| f.key.clone())
        })
        .or_else(|| files.first().map(|f| f.key.clone()))
        .unwrap_or_else(|| "main".to_string());

    // 4. Order pages entry-first.
    files.sort_by_key(|f| f.key != entry_key);

    let entry_file = files
        .iter()
        .find(|f| f.key == entry_key)
        .map(|f| f.path.clone())
        // Nothing discovered (empty dir / in-memory source): keep the legacy
        // default so the caller still resolves `main.lmn`.
        .unwrap_or_else(|| src_dir.join(cfg.app.entry.as_deref().unwrap_or("main.lmn")));

    // Every `.lmn` in the dir contributes fragments (pages plus `layout.lmn`).
    let fragment_files: Vec<PathBuf> = all_lmn.iter().map(|f| f.path.clone()).collect();

    // The multi-page pipeline runs whenever more than one page participates.
    // `files` already reflects an explicit `[pages] include`, so an include
    // naming more than one page (even ones living in a subfolder the
    // directory scan never sees) flips the default on its own; a single-entry
    // include does not. A directory scan finding more than one `.lmn` file
    // also flips it: a lone `index.lmn` beside a shared `layout.lmn` counts,
    // so the page still picks up the shared fragments; a single-file app does
    // not and takes the untouched legacy path. An explicit `[pages] enabled`
    // always wins over either count.
    let multipage = cfg
        .pages
        .enabled
        .unwrap_or(files.len() > 1 || all_lmn.len() > 1);

    PagePlan {
        multipage,
        entry_key,
        entry_file,
        pages: files,
        fragment_files,
        dir: src_dir.to_path_buf(),
    }
}

/// Reserved filename stem for the shared layout. `layout.lmn` contributes its
/// fragments to every page but is not itself a navigable page.
const LAYOUT_STEM: &str = "layout";

fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

// -- IR assembly (graft pages under `<if>` gates) ----------------------------

/// Graft every page in `plan` into `ir` (parsed from the entry file) as
/// sibling `<if signal="route.path" eq="<key>">` gates, parsing each page
/// against the app-wide `fragments` table and merging every page's scripts.
/// Returns the extra page-file paths to add to the hot-reload watch set (all
/// non-entry pages).
///
/// Must run BEFORE asset-path resolution / the CSS cascade so the assembled
/// tree flows through the existing pipeline uniformly.
#[cfg(feature = "runtime-parse")]
pub fn assemble(
    ir: &mut LayoutIR,
    plan: &PagePlan,
    fragments: &FragmentTable,
    parser: &dyn crate::source_parser::SourceParser,
    entry_markup: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut gates: Vec<Element> = Vec::new();
    let mut script_source = String::new();
    let mut external_scripts: Vec<String> = Vec::new();
    let mut watch: Vec<PathBuf> = Vec::new();

    for page in &plan.pages {
        // The entry page parses from the text the loader already carries -
        // compiler plugins may have rewritten it, and a disk re-read here
        // would silently discard their markup transform for multi-page apps.
        let raw = if page.path == plan.entry_file {
            entry_markup.to_string()
        } else {
            std::fs::read_to_string(&page.path)
                .map_err(|e| format!("read page {}: {e}", page.path.display()))?
        };
        let pir = parser
            .parse_html_with_loader(&raw, &page.path, fragments)
            .map_err(|e| format!("parse page {}: {e}", page.path.display()))?;

        // A synthetic `<if>` gate keyed on the reserved active-page signal.
        //
        // The gate is the box the page mounts inside, so a page's own
        // `<root>` attributes ride on it: `<root bg="..." padding="...">`
        // in `settings.lmn` paints and insets the settings page. The entry
        // is the exception: its `<root>` is itself the app's root element,
        // spawned with those attributes already, so repeating them on the
        // gate would paint and inset the home page twice. `skin` and
        // `frameless` are document metadata rather than layout attributes
        // (they live on the `LayoutIR`, not in `Attributes`), so a page
        // cannot smuggle a window setting through here.
        let is_entry = page.key == plan.entry_key;
        let mut attrs = if is_entry {
            Attributes::default()
        } else {
            pir.root.attrs.clone()
        };
        attrs.if_signal = Some(nav::PATH_SIGNAL.to_string());
        attrs.if_eq = Some(page.key.clone());
        attrs.if_mode = IfModeSpec::Render;
        let gate = Element {
            tag: "if".to_string(),
            attrs,
            // The placeholder catalog travels with the attributes it
            // describes, or a `bg="{theme}"` on a page root resolves
            // against nothing.
            interpolations: if is_entry {
                Vec::new()
            } else {
                pir.root.interpolations.clone()
            },
            children: pir.root.children.clone(),
            ..Element::default()
        };
        gates.push(gate);

        if !pir.script_source.trim().is_empty() {
            if !script_source.is_empty() {
                script_source.push('\n');
            }
            script_source.push_str(&pir.script_source);
        }
        for ext in pir.external_scripts {
            if !external_scripts.contains(&ext) {
                external_scripts.push(ext);
            }
        }
        if page.key != plan.entry_key {
            watch.push(page.path.clone());
        }
    }

    // Watch every fragment-only file too (the shared `layout.lmn`), so editing
    // the layout hot-reloads even though it is not a navigable page.
    for tf in &plan.fragment_files {
        if tf != &plan.entry_file && !plan.pages.iter().any(|p| &p.path == tf) {
            watch.push(tf.clone());
        }
    }

    // Replace the entry's own children/scripts with the assembled set. Root
    // attrs (skin, class, window flags) stay as the entry parsed them.
    ir.root.children = gates;
    ir.script_source = script_source;
    ir.external_scripts = external_scripts;

    Ok(watch)
}

/// Merge the fragments every one of the app's `.lmn` files declares into the
/// one table each page parses against. This is what makes a `<template>` in
/// `layout.lmn` reachable from every page.
///
/// Two files declaring the same name with different bodies is an error: the
/// table is app-wide, so either answer would silently change what half the
/// use sites render.
#[cfg(feature = "runtime-parse")]
pub fn collect_fragments(
    plan: &PagePlan,
    parser: &dyn crate::source_parser::SourceParser,
    entry_markup: Option<&str>,
) -> Result<FragmentTable, String> {
    let mut table = FragmentTable::new();
    for path in &plan.fragment_files {
        // Same substitution as `assemble`: the entry's declarations come
        // from the (possibly plugin-transformed) text the loader holds, so
        // a `<template>` a plugin emits into the entry is instantiable.
        let src = match entry_markup {
            Some(entry) if path == &plan.entry_file => entry.to_string(),
            _ => std::fs::read_to_string(path)
                .map_err(|e| format!("read {}: {e}", path.display()))?,
        };
        let declared = parser
            .collect_fragments(&src, path)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        table
            .merge(declared)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(table)
}

// -- runtime wiring ----------------------------------------------------------

/// Install the page registry, in-memory history, the reserved-signal seeds,
/// and the navigation systems onto a built app. Called once at boot when
/// `plan.multipage` is set.
///
/// The [`PagePlan`] goes in as a resource alongside the routing itself,
/// because a from-source run reloads pages from the files it names. A compiled
/// app has no files to reload and installs through [`install_routing`].
pub fn install(app: &mut lumen_core::app::App, plan: &PagePlan) {
    install_routing(app, plan.entry_key.clone(), plan.keys());
    app.world.insert_resource(plan.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LumenToml;

    /// Unique scratch dir per call, holding just `index.lmn`: the shared
    /// fixture for the `discover` default-flip tests below.
    fn temp_app_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen_pages_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.lmn"), "<root></root>").unwrap();
        dir
    }

    #[test]
    fn include_naming_several_subfolder_pages_enables_multipage() {
        // The app dir itself holds only `index.lmn` (the directory scan sees
        // one file), but `include` names two more pages in a subfolder the
        // scan never looks into. That still states multi-page intent, so the
        // default must flip on.
        let dir = temp_app_dir("include_many");
        let mut cfg = LumenToml::default();
        cfg.pages.include = Some(vec![
            "index.lmn".into(),
            "pages/settings.lmn".into(),
            "pages/about.lmn".into(),
        ]);

        let plan = discover(&dir, &cfg);
        assert!(plan.multipage, "a multi-page include must enable multipage");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_entry_include_stays_single_page() {
        // An `include` naming exactly one page (even one living in a
        // subfolder) states no more intent than the plain single-file case,
        // so the default must stay off.
        let dir = temp_app_dir("include_one");
        let mut cfg = LumenToml::default();
        cfg.pages.include = Some(vec!["pages/settings.lmn".into()]);

        let plan = discover(&dir, &cfg);
        assert!(
            !plan.multipage,
            "a single-entry include must not enable multipage"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_enabled_false_wins_over_a_multipage_include() {
        // `[pages] enabled = false` is an explicit override and must win even
        // when `include` alone would flip the default on.
        let dir = temp_app_dir("include_disabled");
        let mut cfg = LumenToml::default();
        cfg.pages.enabled = Some(false);
        cfg.pages.include = Some(vec!["index.lmn".into(), "pages/settings.lmn".into()]);

        let plan = discover(&dir, &cfg);
        assert!(
            !plan.multipage,
            "explicit enabled = false must override the include-derived default"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_longest_first() {
        let plan = PagePlan {
            multipage: true,
            entry_key: "index".into(),
            entry_file: PathBuf::new(),
            pages: vec![
                PageFile {
                    key: "index".into(),
                    path: PathBuf::new(),
                },
                PageFile {
                    key: "user".into(),
                    path: PathBuf::new(),
                },
                PageFile {
                    key: "settings".into(),
                    path: PathBuf::new(),
                },
            ],
            fragment_files: Vec::new(),
            dir: PathBuf::new(),
        };
        let keys = plan.keys();
        assert_eq!(keys[0], "settings"); // longest first
    }
}
