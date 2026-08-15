//! File-based pages - multi-`.lmn` discovery, registration, and the
//! runtime navigation resolver.
//!
//! ## The model (Next.js / SvelteKit file-based routing, real-HTML `<a>`)
//!
//! Every `.lmn` file in the app directory is a page, keyed by its filename
//! stem. The home page is `index.lmn` (falling back to the `[app] entry`
//! stem, then `main.lmn` for single-file compat). All pages load up front;
//! `<template>` blocks found in ANY page file are hoisted into a global
//! preamble so a shared `layout.lmn` template (with a `<slot>`) is usable
//! from every page.
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
use lumen_core::nav::{self, NavOp};
use lumen_core::property_store::PropertyStore;
use std::path::{Path, PathBuf};

use crate::config::LumenToml;
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec, LayoutIR};

/// Navigation target attached to a spawned `<a href="...">` element. A click
/// on the entity navigates the active page.
#[derive(Component, Clone, Debug)]
pub struct Anchor(pub String);

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
    /// Every `.lmn` file that contributes `<template>` blocks to the global
    /// preamble: all pages plus the shared `layout.lmn`. Read by
    /// [`collect_preamble`]; wider than [`Self::pages`] so `layout.lmn`
    /// (which is not itself a page) still shares its template app-wide.
    pub template_files: Vec<PathBuf>,
    /// App directory the pages live in.
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

/// Runtime page registry - the resolver's view of the loaded pages.
#[derive(Clone, Debug, Resource)]
pub struct PageRegistry {
    /// Home page key.
    pub entry: String,
    /// Page keys, longest-first.
    pub keys: Vec<String>,
}

/// One entry on the in-memory history stack.
#[derive(Clone, Debug)]
pub struct Location {
    /// Resolved page key.
    pub path: String,
    /// Leftover segment after the matched page prefix.
    pub segment: String,
}

/// In-memory back/forward history (desktop). The web target replaces this
/// with the real History API; the navigation surface is identical.
#[derive(Clone, Debug, Resource)]
pub struct RouteHistory {
    /// Visited locations, oldest first.
    pub stack: Vec<Location>,
    /// Index of the currently-active location within [`Self::stack`].
    pub cursor: usize,
}

impl RouteHistory {
    fn active(&self) -> Option<&Location> {
        self.stack.get(self.cursor)
    }
}

// -- discovery ---------------------------------------------------------------

/// Discover the pages for `dir`, honouring `[pages]` / `[app]` config.
///
/// Single-file apps (a lone `main.lmn` or `index.lmn`, and no `[pages]
/// enabled = true`) come back with `multipage = false` and the existing
/// single-file load path runs unchanged - the compat guarantee.
pub fn discover(dir: &Path, cfg: &LumenToml) -> PagePlan {
    // 1. Scan the directory once for every `.lmn` file. This set feeds the
    //    global `<template>` preamble (so a shared `layout.lmn` is usable from
    //    every page) and, absent an explicit `[pages] include`, the navigable
    //    page set.
    let mut all_lmn: Vec<PageFile> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
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
    //    contributes its template to the preamble but never gets its own
    //    `<if>` gate or a navigable key. An explicit `[pages] include` lists
    //    the navigable set verbatim; otherwise every non-layout `.lmn` is a
    //    page.
    let mut files: Vec<PageFile> = if let Some(list) = &cfg.pages.include {
        list.iter()
            .map(|name| PageFile {
                key: stem(name),
                path: dir.join(name),
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
        .unwrap_or_else(|| dir.join(cfg.app.entry.as_deref().unwrap_or("main.lmn")));

    // Every `.lmn` in the dir contributes templates (pages plus `layout.lmn`).
    let template_files: Vec<PathBuf> = all_lmn.iter().map(|f| f.path.clone()).collect();

    // The multi-page / template pipeline runs whenever the app has more than
    // one `.lmn` file. A lone `index.lmn` beside a `layout.lmn` counts, so the
    // page still picks up the shared template; a single-file app does not and
    // takes the untouched legacy path.
    let multipage = cfg.pages.enabled.unwrap_or(all_lmn.len() > 1);

    PagePlan {
        multipage,
        entry_key,
        entry_file,
        pages: files,
        template_files,
        dir: dir.to_path_buf(),
    }
}

/// Reserved filename stem for the shared layout. `layout.lmn` contributes its
/// `<template>` to every page but is not itself a navigable page.
const LAYOUT_STEM: &str = "layout";

fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

// -- IR assembly (graft pages under `<if>` gates) ----------------------------

/// Graft every page in `plan` into `ir` (parsed from the entry file) as
/// sibling `<if signal="route.path" eq="<key>">` gates, hoisting all
/// `<template>` blocks into a global preamble and merging every page's
/// scripts. Returns the extra page-file paths to add to the hot-reload watch
/// set (all non-entry pages).
///
/// Must run BEFORE asset-path resolution / the CSS cascade so the assembled
/// tree flows through the existing pipeline uniformly.
#[cfg(feature = "runtime-parse")]
pub fn assemble(
    ir: &mut LayoutIR,
    plan: &PagePlan,
    parser: &dyn crate::source_parser::SourceParser,
) -> Result<Vec<PathBuf>, String> {
    // Global template preamble: every `<template>` block from every page.
    let preamble = collect_preamble(plan);

    let mut gates: Vec<Element> = Vec::new();
    let mut script_source = String::new();
    let mut external_scripts: Vec<String> = Vec::new();
    let mut watch: Vec<PathBuf> = Vec::new();

    for page in &plan.pages {
        let raw = std::fs::read_to_string(&page.path)
            .map_err(|e| format!("read page {}: {e}", page.path.display()))?;
        let src = format!("{preamble}{raw}");
        let pir = parser
            .parse_html_with_loader(&src, &page.path)
            .map_err(|e| format!("parse page {}: {e}", page.path.display()))?;

        // A synthetic `<if>` gate keyed on the reserved active-page signal.
        let gate = Element {
            tag: "if".to_string(),
            attrs: Attributes {
                if_signal: Some(nav::PATH_SIGNAL.to_string()),
                if_eq: Some(page.key.clone()),
                if_mode: IfModeSpec::Render,
                ..Attributes::default()
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

    // Watch every template-only file too (the shared `layout.lmn`), so editing
    // the layout hot-reloads even though it is not a navigable page.
    for tf in &plan.template_files {
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

/// Collect the global `<template>` preamble: every `<template>` block from
/// every page file, concatenated. Prepended to each page (and the entry) at
/// parse time so a shared `layout.lmn` template is usable app-wide.
pub fn collect_preamble(plan: &PagePlan) -> String {
    let mut preamble = String::new();
    for path in &plan.template_files {
        if let Ok(src) = std::fs::read_to_string(path) {
            for block in extract_template_blocks(&src) {
                preamble.push_str(&block);
                preamble.push('\n');
            }
        }
    }
    preamble
}

/// Extract every top-level `<template ...>...</template>` block (inclusive) from
/// `src`, balancing nested opens so a template body containing the literal
/// text does not truncate the capture. Text inside XML comments is skipped, so
/// a comment that mentions the literal `<template>` (as `layout.lmn`'s own
/// docs do) never derails the scan.
fn extract_template_blocks(src: &str) -> Vec<String> {
    const OPEN: &[u8] = b"<template";
    const CLOSE: &[u8] = b"</template>";
    const COMMENT_OPEN: &[u8] = b"<!--";
    const COMMENT_CLOSE: &[u8] = b"-->";
    // Scan on ASCII-lowercased BYTES so a multibyte char (e.g. an em-dash) in a
    // template body never trips the byte cursor. The `<` / `>` tag delimiters
    // are ASCII, so the recorded start/end indices are always char
    // boundaries; `src[start..end]` slices cleanly.
    let lower = src.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lb.len() {
        // Skip whole comments before looking for the next `<template`, so a
        // commented-out or merely-mentioned template does not match.
        if lb[i..].starts_with(COMMENT_OPEN) {
            match find_sub(&lb[i + COMMENT_OPEN.len()..], COMMENT_CLOSE) {
                Some(rel) => {
                    i += COMMENT_OPEN.len() + rel + COMMENT_CLOSE.len();
                    continue;
                }
                None => break, // unterminated comment: nothing more to extract
            }
        }
        if !lb[i..].starts_with(OPEN) {
            i += 1;
            continue;
        }
        let start = i;
        let after = start + OPEN.len();
        // Confirm a tag boundary (`<template` followed by space / `>` / `/`).
        let ok = lb
            .get(after)
            .is_none_or(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/');
        if !ok {
            i = after;
            continue;
        }
        // Balance nested `<template ...>` opens up to the matching close.
        let mut depth = 0usize;
        let mut j = start;
        let mut end = None;
        while j < lb.len() {
            if lb[j..].starts_with(OPEN) {
                depth += 1;
                j += OPEN.len();
            } else if lb[j..].starts_with(CLOSE) {
                depth -= 1;
                j += CLOSE.len();
                if depth == 0 {
                    end = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        match end {
            Some(e) => {
                blocks.push(src[start..e].to_string());
                i = e;
            }
            None => break,
        }
    }
    blocks
}

/// First index of subslice `needle` within `haystack`, or `None`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&k| &haystack[k..k + needle.len()] == needle)
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

/// Install navigation for a known page set: the registry, the in-memory
/// history, the reserved-signal seeds, and the two navigation systems.
///
/// This is what both an app loaded from source and one loaded from a compiled
/// artifact end up calling; they differ only in where the page set came from,
/// a directory listing in one case and [`lumen_ir::artifact::CompiledPages`]
/// in the other.
pub fn install_routing(app: &mut lumen_core::app::App, entry: String, keys: Vec<String>) {
    use lumen_core::tick::TickStage;

    // Seed the reserved signals so the entry page's `<if>` gate mounts on the
    // first reconcile pass.
    {
        let mut store = app.world.resource_mut::<PropertyStore>();
        store.set_global_str(nav::PATH_SIGNAL, entry.as_str());
        store.set_global_str(nav::SEGMENT_SIGNAL, "");
    }
    nav::set_current(&entry);

    app.world.insert_resource(PageRegistry {
        entry: entry.clone(),
        keys,
    });
    app.world.insert_resource(RouteHistory {
        stack: vec![Location {
            path: entry,
            segment: String::new(),
        }],
        cursor: 0,
    });

    // Resolver runs before the `<if>` reconciler so a navigation this tick
    // swaps the mounted page this tick.
    app.add_systems(
        TickStage::Systems,
        apply_navigation.before(crate::spawn::reconcile_if_blocks),
    );
    app.add_systems(TickStage::Systems, navigate_on_anchor_click);
}

/// The single navigation resolver. Reads the reserved request signal (written
/// by every surface via [`lumen_core::nav::request`]), resolves the target by
/// longest existing-file prefix, updates the reserved `route.path` /
/// `route.segment` cells, and maintains the in-memory history stack.
pub fn apply_navigation(
    mut store: ResMut<PropertyStore>,
    registry: Option<Res<PageRegistry>>,
    mut history: ResMut<RouteHistory>,
    mut last: Local<Option<String>>,
) {
    let Some(registry) = registry else {
        return;
    };
    let Some(request) = store.get_global_str(nav::REQUEST_SIGNAL) else {
        return;
    };
    let request = request.to_string();
    if last.as_deref() == Some(request.as_str()) {
        return; // already processed this exact request
    }
    *last = Some(request.clone());

    let Some((_seq, op)) = nav::parse_request(&request) else {
        return;
    };

    let target: Option<Location> = match op {
        NavOp::Navigate(path) => {
            let (key, segment) = nav::resolve_path(&path, &registry.keys, &registry.entry);
            // Truncate any forward history, then push.
            let keep = history.cursor + 1;
            history.stack.truncate(keep);
            history.stack.push(Location {
                path: key.clone(),
                segment: segment.clone(),
            });
            history.cursor = history.stack.len() - 1;
            Some(Location { path: key, segment })
        }
        NavOp::Back => {
            if history.cursor > 0 {
                history.cursor -= 1;
            }
            history.active().cloned()
        }
        NavOp::Forward => {
            if history.cursor + 1 < history.stack.len() {
                history.cursor += 1;
            }
            history.active().cloned()
        }
    };

    if let Some(loc) = target {
        store.set_global_str(nav::PATH_SIGNAL, loc.path.as_str());
        store.set_global_str(nav::SEGMENT_SIGNAL, loc.segment.as_str());
        nav::set_current(&loc.path);
    }
}

/// Declarative navigation: a click on a spawned `<a href>` navigates the
/// active page. The anchor is a real element; on the future web target it
/// transpiles to a DOM `<a href>` and this system's effect becomes the
/// browser's own default anchor navigation.
pub fn navigate_on_anchor_click(
    mut clicks: bevy_ecs::message::MessageReader<lumen_core::input::ClickEvent>,
    anchors: Query<&Anchor>,
) {
    for click in clicks.read() {
        if let Ok(anchor) = anchors.get(click.entity) {
            // Honor `event.prevent_default()` from a phase-4 click handler:
            // link navigation is the click default action, so a prevented
            // click does not navigate.
            let handle = lumen_core::node::NodeHandle::new(click.entity).pack();
            if lumen_script::event::is_click_default_prevented(handle) {
                continue;
            }
            nav::navigate(anchor.0.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_balanced_template_blocks() {
        let src = "<root/>\n<template name=\"a\"><column><slot/></column></template>\n\
                   <template name=\"b\"><label/></template>";
        let blocks = extract_template_blocks(src);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("name=\"a\""));
        assert!(blocks[1].contains("name=\"b\""));
    }

    #[test]
    fn multibyte_chars_do_not_break_the_scanner() {
        // An em dash (`\u{2014}`, 3 UTF-8 bytes) inside a template body must
        // not trip the byte cursor onto a non-char-boundary.
        let src = "<template name=\"x\"><label text=\"a \u{2014} b \u{2014} c\"/></template>";
        let blocks = extract_template_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("\u{2014} b \u{2014}"));
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
            template_files: Vec::new(),
            dir: PathBuf::new(),
        };
        let keys = plan.keys();
        assert_eq!(keys[0], "settings"); // longest first
    }

    #[test]
    fn comment_mentioning_template_is_skipped() {
        // A comment that mentions the literal `<template>` (as `layout.lmn`'s
        // own docs do) must not derail the scan or swallow the real block.
        let src = "<root>\n<!-- this `<template>` is documented here -->\n\
                   <template name=\"layout\"><column><slot/></column></template>\n</root>";
        let blocks = extract_template_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("name=\"layout\""));
        assert!(blocks[0].contains("<slot/>"));
    }
}
