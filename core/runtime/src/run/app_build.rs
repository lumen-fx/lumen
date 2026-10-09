use super::*;
use crate::app_layout::AppLayout;
use lumen_core::window::{Menu, MenuEntry, MenuModel, WindowGeometry};
use lumen_ir::layout_ir::MenuEntrySpec;

use lumen_capability::Phase;
use lumen_modules::language::{LanguageTable, ScriptGrouping};
use lumen_script::{ScriptFnAppExt, ScriptLanguages, ScriptProgram};

/// Construct the fully-configured [`App`] and the [`WindowSetup`] the
/// windowed path would run it with - everything [`run_app`] does short
/// of entering the event loop. Split out so [`run_app_headless`] can
/// reuse the identical build without duplicating the plugin / system
/// wiring.
pub fn build_app(mut opts: RunOptions) -> Result<(App, WindowSetup), RunError> {
    // Host-neutral native functions (the C-ABI's `lumen_app_expose`, the Rust
    // SDK). They go into the registry every host drains, so an exposed
    // function is callable from whichever languages the app ships.
    let native_fns = std::mem::take(&mut opts.native_fns);
    let app_hooks = std::mem::take(&mut opts.app_hooks);
    // The injected markup/CSS front-end (the runtime links no parser itself).
    // Shared as an `Arc` so the initial load, the hot-reload resource, and the
    // devtools-overlay parse can all reach the same impl.
    let parser: Option<std::sync::Arc<dyn SourceParser>> =
        opts.parser.take().map(std::sync::Arc::from);
    let dir = opts.dir.clone();
    let cfg = crate::config::LumenToml::load_or_default(&dir).map_err(RunError::Config)?;
    // The tags this app's modules bring, published before the markup is
    // parsed. A module registers its own tag when it installs, which covers
    // the run path; the declaration is what covers a compile, where nothing
    // is loaded at all.
    register_declared_tags(&cfg, &[lumen_modules::Target::Desktop], &[]);
    // The dependencies this desktop run loads: its own target table laid over
    // the shared one. Each is a library; a module's web half is a web
    // build's business.
    let dependencies = cfg.dependencies_for(lumen_modules::Target::Desktop);
    // Where this app lives and what it is called, published before anything
    // that resolves a path runs: every path a script names is resolved
    // against these, and a script's `on_start` fires on the first tick.
    let app_id = cfg
        .app
        .id
        .clone()
        .unwrap_or_else(|| lumen_capability::derive_app_id(&dir));
    lumen_core::app_paths::set_app(dir.clone(), app_id.clone());
    // Where this app's code is. In-memory markup has no source tree to check,
    // so it takes the paths without one.
    let layout = if opts.markup.is_some() {
        AppLayout::of(&dir, &cfg)
    } else {
        AppLayout::resolve(&dir, &cfg)?
    };
    // File-based pages: discover the page set up front. The entry file is
    // `index.lmn` (else the `[app] entry` stem, else `main.lmn`). Single-file
    // apps come back `multipage = false` and take the untouched legacy path.
    // In-memory (`markup`) sources bypass discovery entirely.
    let page_plan = if opts.markup.is_some() {
        None
    } else {
        Some(crate::pages::discover(&layout.src_dir, &cfg))
    };
    let html_path = match &page_plan {
        Some(plan) => plan.entry_file.clone(),
        None => layout.entry_path,
    };
    let css_path = layout.css_path;
    let lib_dir = layout.lib_dir;
    // The script libraries the app depends on, resolved by lumenc before the
    // build; the runtime resolves nothing itself.
    let import_roots = std::mem::take(&mut opts.import_roots);
    let asset_roots = cfg.resolved_asset_roots(&dir);
    let skin_override = cfg.skin.name.clone();
    // The injected compiler-plugin chain (loaded by lumenc, like the parser),
    // shared with hot reload as a resource; a reload pays only the hook calls.
    let compiler_plugins: std::sync::Arc<dyn crate::compiler_plugins::CompilerPlugins> = opts
        .compiler_plugins
        .take()
        .unwrap_or_else(|| std::sync::Arc::new(crate::compiler_plugins::NoCompilerPlugins));

    // What the optional subsystems are installed with: the app's location,
    // identity and config, the run mode, and a bounded view of its sources
    // for the ones that gate on use (see `capability_env`). The embedder's
    // hooks are Rust the scan cannot read, so with any present every
    // use query answers yes.
    let mut env = capability_env(&opts, &dir, &cfg);
    if !app_hooks.is_empty() {
        env = env.opaque();
    }
    if let Some(parser) = parser.clone() {
        env.provide(parser);
    }

    let mut app = App::new();
    // The run mode, before anything is installed: a plugin that only makes
    // sense for a person at a window reads this from `Plugin::build`. Same bit
    // the capability and portable-plugin environments carry, from the same
    // source.
    app.world.insert_resource(lumen_core::app::RunMode {
        headless: opts.bounded,
    });
    // `[runtime] threads` overrides the `min(cores, 4)` default budget
    // (the `LUMEN_THREADS` env var still wins over this at first tick).
    if let Some(n) = cfg.runtime.threads.filter(|n| *n > 0) {
        app.desired_threads = n;
    }
    // The core stack (see `run/subsystems.rs`), then the optional
    // subsystems this binary carries, at the phase each asked for. The core
    // is unconditional; an optional subsystem decides for itself whether an
    // app uses it. `build_app` names none of them.
    // The backends the stack is built on register into their registries
    // first, so the core stack finds the layout engine it installs.
    lumen_capability::install_phase(&mut app, &env, Phase::Backends);
    // Text shaping first: the layout engine measures through the shaper
    // installed here, and the renderer gets the sibling returned here.
    let render_shaper = register_text(&mut app);
    register_core(&mut app)?;
    // Host services: OS integration, the introspection server, the render
    // backends.
    lumen_capability::install_phase(&mut app, &env, Phase::Platform);
    // A rendered headless run starts its offscreen renderer now: the
    // backends registered above, and bringing one up (a GPU device and its
    // pipelines) needs nothing from the rest of the build it overlaps.
    if let Some((width, height)) = opts.offscreen_prestart.take() {
        let candidates = crate::run::render_backends(&app, &cfg)?;
        app.world
            .insert_resource(crate::run_headless::OffscreenPrestart::spawn(
                candidates, width, height,
            )?);
    }
    // Reactive bindings, reconcilers, dialog lifecycle, error overlay - the
    // always-on reactive core.
    register_reactive(&mut app);
    // Translation. Runs before the tree is spawned so a
    // `translatable="key"` element resolves its text on the first frame,
    // and before the script host loads so `t("key")` works from `on_start`.
    register_i18n(&mut app, &dir, &cfg)?;

    // Command-bus drain, the FFI typed-read mirror, and the
    // `set_color_scheme` `Command::Typed` handler. Always-on reactive plumbing.
    register_commands(&mut app);
    // What a script host binds to at construction, the HTTP client behind
    // `fetch()` for one: the first host to build installs a `FetchRegistry`
    // if none exists yet, so a client has to be in place before it. Ahead of
    // the module phase, which is where it has always been: a module that
    // installs a client of its own replaces this one, and moving the modules
    // would have silently reversed that.
    lumen_capability::install_phase(&mut app, &env, Phase::BeforeScripts);
    // The registration order is the shadowing order: a plugin's functions
    // first, then the embedder's. Every host drains this one registry as it
    // loads and seals it afterwards, so a registration that arrives too late
    // to bind says so instead of going quiet. The builtin table is not in it;
    // each host binds that itself when it is constructed, so a name registered
    // here shadows a builtin of the same name.
    // Runtime dependencies (`[dependencies]`): prebuilt dylibs of either
    // kind - engine-locked modules installed as ordinary Plugins, portable
    // plugins bound through the C-ABI loader - each told apart by its
    // exports. Before the embedder's plugin phase on purpose: registrations
    // shadow in arrival order, so a later embedder registration of the same
    // script-fn name wins over a module's. Any load failure is a stderr
    // banner plus a `LoadedModules` failure entry, and the app keeps booting
    // without that module.
    //
    // Before the markup is parsed, too: a module that brings an element
    // registers its tag from `Plugin::build`, and a parse that ran first
    // would have refused the element the module exists to provide. Everything
    // below this point needs the parse to have happened, so this is the last
    // moment a module can be installed and still be ahead of it.
    //
    // A compiled app is read before them instead: its tree was parsed when it
    // was compiled, so no module's tag waits on this order.
    //
    // Script hosts are modules too, loaded beside the declared ones: the
    // module each of the app's script languages runs on. A compiled app names
    // them; an app run from source has them picked from the language
    // descriptors in reach, by the `<script>` elements its markup names. The
    // descriptor table is only consulted on the source path: a shipped app
    // runs from the artifact and carries none.
    let table = if opts.artifact_bytes.is_some() || opts.artifact.is_some() {
        LanguageTable::default()
    } else {
        language_table()
    };
    let grouping = ScriptGrouping {
        table: &table,
        engine: cfg.script.engine(),
    };
    let no_languages = ScriptLanguages::default();
    let compiled_first = if opts.artifact_bytes.is_some() || opts.artifact.is_some() {
        Some(load_inputs(
            &opts,
            parser.as_deref(),
            &*compiler_plugins,
            &html_path,
            &css_path,
            &dir,
            &asset_roots,
            skin_override.as_deref(),
            page_plan.as_ref(),
            &Languages {
                grouping,
                registered: &no_languages,
            },
        )?)
    } else {
        None
    };
    let host_modules: Vec<String> = match &compiled_first {
        Some(loaded) => loaded
            .scripts
            .iter()
            .map(|script| script.module.clone())
            .filter(|module| !module.is_empty())
            .collect(),
        None => script_languages(
            &opts,
            parser.as_deref(),
            grouping,
            &html_path,
            page_plan.as_ref(),
        )
        .iter()
        .filter_map(|language| table.source_provider(language))
        .map(|provider| provider.module.clone())
        .collect(),
    };
    let dependencies = lumen_modules::language::with_implied(&dependencies, &host_modules);
    #[cfg(feature = "modules")]
    {
        let env = crate::modules::InitEnv {
            app_dir: dir.clone(),
            app_id: cfg
                .app
                .id
                .clone()
                .unwrap_or_else(|| lumen_capability::derive_app_id(&opts.dir)),
            // The same bit the `RunMode` resource above carries, for the
            // portable arm, which reads this environment rather than the
            // world.
            headless: opts.bounded,
            hot_reload: opts.hot_reload && !opts.bounded,
        };
        crate::modules::load_modules(&mut app, &dir, &dependencies, &opts.resolved_modules, &env);
    }
    #[cfg(not(feature = "modules"))]
    for dep in &dependencies.0 {
        eprintln!(
            "lumen-runtime: dependency '{}' is declared but this runtime was built without \
             the `modules` feature; the app runs without it",
            dep.name
        );
    }

    // Initial load runs before the window exists, so there's no real
    // OS theme / viewport yet. Apply with the best-guess default
    // context; `detect_media_change` re-applies with the live context on
    // the first tick after the window seeds `StyleManager` / `Viewport`.
    //
    // Either deserialize a precompiled AOT artifact (parser-free path) or
    // parse `main.lmn` + `main.css` from source (`runtime-parse`).
    let registered = app
        .world
        .get_resource::<ScriptLanguages>()
        .cloned()
        .unwrap_or_default();
    let loaded = match compiled_first {
        Some(loaded) => loaded,
        None => load_inputs(
            &opts,
            parser.as_deref(),
            &*compiler_plugins,
            &html_path,
            &css_path,
            &dir,
            &asset_roots,
            skin_override.as_deref(),
            page_plan.as_ref(),
            &Languages {
                grouping,
                registered: &registered,
            },
        )?,
    };
    let LoadResult {
        ir,
        html_mtime,
        css_mtime,
        script_paths,
        script_mtimes,
        include_paths,
        include_mtimes,
        css_import_paths,
        css_import_mtimes,
        scripts: compiled_scripts,
        pages: compiled_pages,
        fragments,
        i18n: compiled_i18n,
    } = loaded;
    // The catalogues a compiled app carries fill in every locale the app
    // directory has no loose file for, before the tree spawns and before a
    // script's `on_start` calls `t()`.
    add_compiled_catalogues(&mut app.world, &cfg, &compiled_i18n)?;
    // The app's declared fragments, reachable by key for the rest of the
    // run: a script instantiates one, and the applier builds it here.
    crate::fragments::install(&mut app.world, fragments);
    // Hot-reload watch fields are only consumed by the (feature-gated)
    // watcher below; in a parser-free build they are always empty.
    #[cfg(not(feature = "runtime-parse"))]
    let _ = (
        &html_mtime,
        &css_mtime,
        &script_paths,
        &script_mtimes,
        &include_paths,
        &include_mtimes,
        &css_import_paths,
        &css_import_mtimes,
    );
    // Script hosts. Each part of the program runs on the host its language's
    // module registered; an app that ships two languages runs two hosts side
    // by side, reaching each other only through the shared `PropertyStore`
    // signal bus, and `[script] engine` collapses everything onto one.
    //
    // `register_script_common` installs the host-neutral half once, ordering
    // it against `lumen_script::ScriptSet` so its RC-critical edges cover
    // every active host; each language's install adds the per-host half. The
    // native functions an app adds - a plugin's and the embedder's
    // `RunOptions::native_fns` - go into one `ScriptFnRegistry` below, which
    // each host drains as it loads; the shared builtin surface
    // (`lumen_script::builtin_script_fns`) is bound by each host itself.
    //
    // A precompiled artifact carries the split the AOT compiler recorded, and
    // it is the only source of it: a compiled app ships no script files for
    // the grouping to read, and a language with a bytecode form ships no
    // source at all.
    let programs: Vec<(String, ScriptProgram)> = if compiled_scripts.is_empty() {
        grouped_script_sources(&ir, &dir, grouping)?
            .into_iter()
            .map(|(language, source)| {
                (
                    language,
                    ScriptProgram {
                        source,
                        ..ScriptProgram::default()
                    },
                )
            })
            .collect()
    } else {
        compiled_scripts
            .into_iter()
            .map(|script| {
                (
                    script.engine,
                    ScriptProgram {
                        source: script.source,
                        bytecode: script.bytecode,
                        ..ScriptProgram::default()
                    },
                )
            })
            .collect()
    };
    let has_script = !programs.is_empty();
    let mut reloaders = ScriptReloaders::default();
    let multi_host = programs.len() > 1;
    // The host-neutral half of the script wiring. It needs the parse: whether
    // the app ships a script at all is read out of the document.
    register_script_common(&mut app, has_script);
    for install in std::mem::take(&mut opts.plugins) {
        install(&mut app);
    }
    app.add_script_fns(native_fns);
    for (language, mut program) in programs {
        let Some(entry) = registered.get(&language).copied() else {
            no_host_for(&mut app, &language, &table);
            continue;
        };
        // The entry path names the program in an error, `lib/` is where a
        // native library a script imports is looked for, and the script
        // libraries and compile-time flags are this desktop run's.
        program.uri = html_path.display().to_string();
        program.lib_dir = Some(lib_dir.clone());
        program.import_roots = import_roots.clone();
        program.cfg_flags = lumen_modules::Target::Desktop
            .cfg_flags()
            .iter()
            .map(|f| (*f).to_string())
            .collect();
        (entry.install)(&mut app, program, multi_host);
        if let Some(reload) = entry.reload {
            reloaders.push(language, reload);
        }
    }
    // RC6: a script that failed to load leaves `ScriptLoadFailure` behind.
    // Mirror it into the in-app error banner so the failure is visible in the
    // window itself, not only in the stderr banner the host printed.
    if let Some(fail) = app.world.get_resource::<lumen_script::ScriptLoadFailure>() {
        let msg = format!("script load failed: {}", fail.0);
        app.world.resource_mut::<ErrorBanner>().0 = Some(msg);
    }
    app.world.insert_resource(reloaders);
    app.world
        .insert_resource(ScriptLanguageTable(table.clone()));
    use crate::spawn::SpawnIntoWorld;
    let root = ir.spawn_into(&mut app.world);
    crate::run::restyle::install_root_class_list(&mut app.world, root);

    // Pages: install the page registry, in-memory history, the reserved
    // `route.*` signal seeds, and the navigation systems (`apply_navigation`
    // before the `<if>` reconciler; anchor-click -> navigate). Only when the
    // app has more than one page.
    //
    // A compiled app carries its page set in the artifact and is authoritative
    // about it: the `.lmn` files a directory scan would look for are compiled
    // in and not shipped, so the scan above always comes back single-page for
    // one. An app loaded from source uses the plan discovered from its files.
    match (&compiled_pages, &page_plan) {
        (Some(pages), _) => {
            crate::pages::install_routing(
                &mut app,
                pages.entry.clone(),
                pages.keys.clone(),
                crate::pages::Location::page(&pages.entry),
            );
        }
        (None, Some(plan)) if plan.multipage => crate::pages::install(&mut app, plan),
        _ => {}
    }

    // Seed K9's class cache so the first `set_root_class` call has a
    // baseline to compare against (avoids a respawn on the first tick
    // if a theme detector wrote `theme-light` before anyone set it).
    let initial_root_classes: Vec<String> = app
        .world
        .get::<lumen_core::components::LumenClasses>(root)
        .map(|c| c.0.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    app.world
        .insert_resource(RootClassesCache(initial_root_classes));

    // Style-invalidation cache, style version tracking, the live combined
    // stylesheet, the theme/media re-resolver systems, and the per-cache
    // memory budget. Always-on styling plumbing.
    register_styles(&mut app, &ir, &cfg);

    // Hot reload re-parses source on file change, so it exists only in
    // `runtime-parse` builds. In-memory sources (and artifact loads) have no
    // Stash the injected markup front-end so `set_inner_markup` (design 4.4)
    // can parse a fragment at runtime through the same seam hot reload uses.
    // Present on the from-source run path (dev / SDK / CLI `run`); the
    // precompiled-artifact path carries no parser, and `set_inner_markup` is a
    // no-op there. The hot-reload block below re-inserts the same resource for
    // its own re-parse; this makes it available regardless of hot-reload.
    if let Some(parser) = parser.clone() {
        app.world
            .insert_resource(crate::source_parser::RuntimeParser(parser));
    }
    app.world
        .insert_resource(crate::compiler_plugins::RuntimeCompilerPlugins(
            compiler_plugins,
        ));

    // file to watch - force hot reload off so the watcher never despawns the
    // tree in favour of stale disk state.
    // Hot-reload gating. `[runtime] hot_reload` forces the answer; otherwise
    // the watcher runs only for an interactive run from source - never for a
    // headless / bounded automation run (no editing session, and the `notify`
    // watcher would spawn a thread the bench pays for). Markup / artifact
    // runs have no source files to watch regardless.
    #[cfg(feature = "runtime-parse")]
    let hot_reload_enabled = match cfg.runtime.hot_reload {
        Some(v) => v,
        None => opts.hot_reload && !opts.bounded,
    };
    #[cfg(feature = "runtime-parse")]
    if hot_reload_enabled && opts.markup.is_none() && opts.artifact.is_none() {
        // Parent directories of every tracked source file (deduplicated).
        // Computed before the paths move into `HotReloadState`.
        let locale_dir = crate::run::i18n::locale_dir(&dir);
        let watch_dirs: std::collections::HashSet<PathBuf> = [&html_path, &css_path]
            .into_iter()
            .chain(&script_paths)
            .chain(&include_paths)
            .chain(&css_import_paths)
            .filter_map(|p| p.parent())
            .chain(locale_dir.is_dir().then_some(locale_dir.as_path()))
            .filter(|d| d.is_dir())
            .map(PathBuf::from)
            .collect();
        let authored_text = authored_texts(&mut app.world);
        app.world.insert_resource(HotReloadState {
            dir: dir.clone(),
            html_path: html_path.clone(),
            css_path: css_path.clone(),
            html_mtime,
            css_mtime,
            script_paths,
            script_mtimes,
            include_paths,
            include_mtimes,
            css_import_paths,
            css_import_mtimes,
            locale_stamps: crate::run::i18n::locale_stamps(&dir),
            asset_roots: asset_roots.clone(),
            skin_override: skin_override.clone(),
            root,
            authored_text,
        });
        // Change detection driver: notify watcher by default (idle apps
        // park with zero ticks; an fs event wakes the loop for one tick),
        // mtime polling behind `LUMEN_HOT_RELOAD_POLL` or on watcher
        // init failure.
        let driver = if std::env::var_os("LUMEN_HOT_RELOAD_POLL").is_some() {
            eprintln!("lumenc: hot reload using mtime polling (LUMEN_HOT_RELOAD_POLL set)");
            HotReloadDriver::Poll
        } else {
            let flag = std::sync::Arc::new(HotReloadFlag::default());
            match spawn_hot_reload_watcher(&watch_dirs, std::sync::Arc::clone(&flag)) {
                Ok(watcher) => HotReloadDriver::Watch {
                    flag,
                    _watcher: std::sync::Arc::new(std::sync::Mutex::new(watcher)),
                },
                Err(e) => {
                    eprintln!(
                        "lumenc: hot-reload file watcher init failed ({e}); \
                         falling back to mtime polling"
                    );
                    HotReloadDriver::Poll
                }
            }
        };
        app.world.insert_resource(driver);
        // Stash the injected parser so `hot_reload::<H>` (a `&mut World`
        // system that can't take it as a param) can re-parse on change. The
        // watcher is only wired for a from-source run, which always carries a
        // parser (`load_inputs` above would have returned `ParserDisabled`
        // otherwise), so this unwrap is unreachable in practice.
        if let Some(parser) = parser.clone() {
            app.world
                .insert_resource(crate::source_parser::RuntimeParser(parser));
        }
        // One watcher system for the whole app: it respawns the tree once and
        // then reloads each active host through the `ScriptReloaders` table.
        //
        // Ordered before the DOM index publish: the respawn replaces the root,
        // and `on_ready` re-fires on this same tick. A publish that ran first
        // would hand that dispatch the despawned root, so a script that mounts
        // into the document would attach to nothing.
        app.add_systems(
            TickStage::Systems,
            hot_reload.before(lumen_scene::dom::build_dom_index),
        );
        // An image or SVG file edited on disk reloads in place: the asset
        // watcher reports the path, and this strips what every element on
        // it shows so the next tick decodes the new bytes.
        app.add_systems(
            TickStage::Systems,
            lumen_assets::reload_changed_images.after(lumen_assets::process_watch_events),
        );
    }

    // RunOptions (set by the CLI / embedder) overrides lumen.toml,
    // which overrides built-in defaults.
    let title = opts
        .title
        .or_else(|| cfg.window.title.clone())
        .unwrap_or_else(|| derive_title(&opts.dir));
    let mut size = match cfg.window.size {
        Some([w, h]) if opts.size == RunOptions::DEFAULT_SIZE => (w, h),
        _ => opts.size,
    };
    let mut maximized = true;
    let mut start_position: Option<(i32, i32)> = None;
    let mut on_close_state: Option<Box<dyn FnOnce(WindowGeometry) + Send>> = None;
    if cfg.window.remember_state.unwrap_or(false) {
        let prev = crate::window_state::load(&app_id);
        if let Some([w, h]) = prev.size {
            size = (w, h);
        }
        start_position = prev.position.map(|[x, y]| (x, y));
        maximized = prev.maximized;
        on_close_state = Some(Box::new(move |g| {
            crate::window_state::save(
                &app_id,
                &crate::window_state::WindowState {
                    position: g.position.map(|(x, y)| [x, y]),
                    size: Some([g.size.0, g.size.1]),
                    maximized: g.maximized,
                },
            );
        }));
    }
    let menubar = ir.menubar.as_ref().map(|spec| MenuModel {
        menus: spec
            .menus
            .iter()
            .map(|m| Menu {
                label: m.label.clone(),
                items: m
                    .items
                    .iter()
                    .map(|entry| match entry {
                        MenuEntrySpec::Item {
                            id,
                            label,
                            accelerator,
                        } => MenuEntry::Item {
                            id: id.clone(),
                            label: label.clone(),
                            accelerator: accelerator.clone(),
                        },
                        MenuEntrySpec::Separator => MenuEntry::Separator,
                    })
                    .collect(),
            })
            .collect(),
    });
    // `--lumen-window-bg` resolved from the fully-combined (UA + skin +
    // app) stylesheet paints the GPU clear behind the very first frame -
    // what a user sees before the root element itself paints, and behind
    // any pixel the tree doesn't cover. Only a plain solid color parses
    // (the clear is a single RGBA, not a gradient); an app whose active
    // layers don't define the token - or define it as something else -
    // falls back to `opts.clear` (`lumen_core::window::DEFAULT_CLEAR`
    // unless the caller overrode it), preserving today's behavior exactly.
    let clear = ir
        .combined_stylesheet
        .as_ref()
        .and_then(|sheet| sheet.resolve_root_var("lumen-window-bg"))
        .and_then(|value| lumen_ir::values::parse_color("<root>", "lumen-window-bg", &value).ok())
        .map(Into::into)
        .unwrap_or(opts.clear);
    let window = WindowSetup {
        options: WindowOptions {
            size,
            title,
            clear,
            maximized,
            frameless: ir.frameless,
            start_position,
            on_close_state,
            menubar,
        },
        text_shaper: Some(render_shaper),
    };
    // What mounts into the built document: the devtools overlay, when the
    // binary carries it and the run has a front end to parse it with.
    lumen_capability::install_phase(&mut app, &env, Phase::AfterBuild);

    // Embedder hooks run last so they can order their systems against
    // everything the default stack registered above (script dispatch,
    // binding readers, reconcilers).
    for hook in app_hooks {
        hook(&mut app);
    }
    Ok((app, window))
}

/// The language descriptors in reach of this run. A set that disagrees with
/// itself (two default languages) is reported and read as empty: the app
/// runs, and every script says it has no host.
fn language_table() -> LanguageTable {
    LanguageTable::discover(None).unwrap_or_else(|e| {
        lumen_core::warn_line!("lumen-runtime: {e}");
        LanguageTable::default()
    })
}

/// The languages an app run from source needs a host for: read off the
/// `<script>` elements of its markup, before the parse. A run with no parser
/// has nothing to read them with, and the parse it cannot do fails on its own.
#[cfg(feature = "runtime-parse")]
fn script_languages(
    opts: &RunOptions,
    parser: Option<&dyn SourceParser>,
    grouping: ScriptGrouping<'_>,
    html_path: &Path,
    plan: Option<&crate::pages::PagePlan>,
) -> Vec<String> {
    match parser {
        Some(parser) => source_languages(parser, grouping, html_path, opts.markup.as_deref(), plan),
        None => Vec::new(),
    }
}

#[cfg(not(feature = "runtime-parse"))]
fn script_languages(
    _opts: &RunOptions,
    _parser: Option<&dyn SourceParser>,
    _grouping: ScriptGrouping<'_>,
    _html_path: &Path,
    _plan: Option<&crate::pages::PagePlan>,
) -> Vec<String> {
    Vec::new()
}

/// A part of the program whose language no loaded module registered: say so
/// once, on stderr and in the window, and run the app without it. The module
/// loader has already bannered a host module that failed to load; this names
/// the language, and the module that would run it when a descriptor says.
fn no_host_for(app: &mut App, language: &str, table: &LanguageTable) {
    let module = table
        .shipped_provider(language)
        .map(|p| format!("the `{}` module", p.module))
        .unwrap_or_else(|| "a module that provides it".to_string());
    let reason = format!(
        "no script host for the `{language}` language is loaded, so the app's `{language}` \
         script does not run. Install {module} beside the engine."
    );
    lumen_core::warn_line!(
        "\n\
         ================================================================\n\
         lumen-runtime: SCRIPT NOT RUN\n\
         \n\
           {reason}\n\
         \n\
         The window will still open, but every event handler, signal,\n\
         and derivation in that script is DISABLED.\n\
         ================================================================\n"
    );
    app.world
        .insert_resource(lumen_script::ScriptLoadFailure(reason));
}
