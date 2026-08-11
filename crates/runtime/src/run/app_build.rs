use super::*;

/// Construct the fully-configured [`App`] and the [`WinitOptions`] the
/// windowed path would run it with - everything [`run_app`] does short
/// of entering the event loop. Split out so [`run_app_headless`] can
/// reuse the identical build without duplicating the plugin / system
/// wiring.
pub fn build_app(mut opts: RunOptions) -> Result<(App, WinitOptions), RunError> {
    let mut rhai_extensions = std::mem::take(&mut opts.rhai_extensions);
    let app_hooks = std::mem::take(&mut opts.app_hooks);
    // The injected markup/CSS front-end (the runtime links no parser itself).
    // Shared as an `Arc` so the initial load, the hot-reload resource, and the
    // devtools-overlay parse can all reach the same impl.
    let parser: Option<std::sync::Arc<dyn SourceParser>> =
        opts.parser.take().map(std::sync::Arc::from);
    let dir = opts.dir.clone();
    let cfg = crate::config::LumenToml::load_or_default(&dir).map_err(RunError::Config)?;
    // File-based pages: discover the page set up front. The entry file is
    // `index.lmn` (else the `[app] entry` stem, else `main.lmn`). Single-file
    // apps come back `multipage = false` and take the untouched legacy path.
    // In-memory (`markup`) sources bypass discovery entirely.
    let page_plan = if opts.markup.is_some() {
        None
    } else {
        Some(crate::pages::discover(&dir, &cfg))
    };
    let html_path = match &page_plan {
        Some(plan) => plan.entry_file.clone(),
        None => dir.join(cfg.app.entry.as_deref().unwrap_or("main.lmn")),
    };
    let css_path = dir.join("main.css");
    let asset_roots = cfg.resolved_asset_roots(&dir);
    let skin_override = cfg.skin.name.clone();

    // Subsystem gating (measured startup quick-wins): resolve, from one bounded
    // source scan + `lumen.toml` + run-mode flags, which optional subsystems
    // this app actually uses so `build_app` can skip initialising the ones it
    // does not (audio device + ticker thread; the X11 global-hotkey manager).
    // See [`SubsystemUsage`] for the conservative, err-toward-ON contract. The
    // image / SVG decode worker pool needs no gate - `lumen-assets` spawns it
    // lazily on the first decode - and the HTTP surface likewise starts a
    // thread only per outbound request, so neither costs an idle app.
    let usage = SubsystemUsage::detect(&opts, &dir, &cfg, !app_hooks.is_empty());

    let mut app = App::new();
    // `[runtime] threads` overrides the `min(cores, 4)` default budget
    // (the `LUMEN_THREADS` env var still wins over this at first tick).
    if let Some(n) = cfg.runtime.threads.filter(|n| *n > 0) {
        app.desired_threads = n;
    }
    // -- Subsystem register units (see `run/subsystems.rs`) -----------------
    // `build_app` installs the default stack as a sequence of per-subsystem
    // `register_*` calls. The core visual stack is unconditional; the gated
    // units (hotkey / audio / MCP) are skipped when `usage` / run-mode proves
    // them unused. The OS host-resource units (filedialog / notify / tray /
    // clipboard / launcher / power) are cheap constructors left default-on
    // (see their `TODO(tree-shake)` notes).
    register_core(&mut app);
    // Global hotkeys - GATED on `usage.hotkey` (the `register_hotkey` marker);
    // skipping it avoids opening the X11 hotkey manager for a hotkey-free app.
    if usage.hotkey {
        register_os_hotkey(&mut app);
    }
    // File dialogs - the service is default-on, its tokio runtime GATED on
    // `usage.file_dialog`.
    register_os_filedialog(&mut app, usage.file_dialog);
    register_os_notify(&mut app, &cfg);
    register_os_tray(&mut app);
    register_os_misc(&mut app, &cfg);
    // Audio - GATED on `usage.audio`; a no-audio app gets the inert
    // `AudioService::disabled()` (no device, no ticker thread).
    register_audio(&mut app, usage.audio);
    // MCP introspection server - GATED on run-mode + `[mcp]` config.
    register_mcp(&mut app, opts.bounded, &cfg);
    // Reactive bindings, reconcilers, dialog lifecycle, error overlay - the
    // always-on reactive core.
    register_reactive(&mut app);
    // Translation. Runs before the tree is spawned so a
    // `translatable="key"` element resolves its text on the first frame,
    // and before the script host loads so `t("key")` works from `on_start`.
    register_i18n(&mut app, &dir, &cfg)?;

    // Initial load runs before the window exists, so there's no real
    // OS theme / viewport yet. Apply with the best-guess default
    // context; `detect_media_change` re-applies with the live context on
    // the first tick after the window seeds `StyleManager` / `Viewport`.
    //
    // Either deserialize a precompiled AOT artifact (parser-free path) or
    // parse `main.lmn` + `main.css` from source (`runtime-parse`).
    let loaded = load_inputs(
        &opts,
        parser.as_deref(),
        &html_path,
        &css_path,
        &dir,
        &asset_roots,
        skin_override.as_deref(),
        page_plan.as_ref(),
    )?;
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
    } = loaded;
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
    // Command-bus drain, the FFI typed-read mirror, and the
    // `set_color_scheme` `Command::Typed` handler. Always-on reactive plumbing.
    register_commands(&mut app);
    // Script host selection. Each script file picks its host from its own
    // extension, so an app that ships two languages runs two hosts side by
    // side; `[script] engine` collapses everything onto one. Every host
    // re-exports the same generic tick / dispatch / derivation systems from
    // `lumen-script`, and hosts reach each other only through the shared
    // `PropertyStore` signal bus.
    //
    // `register_script_common` installs the host-neutral half once, ordering
    // it against `lumen_script::ScriptSet` so its RC-critical edges cover
    // every active host; `register_script_host_systems::<H>` then installs
    // the per-host half once per language. The `set_color_scheme` / `page`
    // engine extensions are engine-typed (rhai::Engine vs mlua::Lua) so they
    // live in the arms below, but push byte-identical commands.
    //
    // A precompiled artifact carries the split the AOT compiler recorded, and
    // it is the only source of it: the app's `.lua` / `.rhai` files are not
    // shipped beside a compiled app for the directory scan to read. An
    // explicit `[script] engine` still collapses everything onto one host.
    let resolve_here = compiled_scripts.is_empty() || cfg.script.engine.is_some();
    let grouped = remap_trimmed_hosts(if resolve_here {
        grouped_script_sources(&ir, &dir, &cfg)?
    } else {
        compiled_scripts
    });
    let has_script = !grouped.is_empty();
    let mut reloaders = ScriptReloaders::default();
    let multi_host = grouped.len() > 1;
    register_script_common(&mut app, has_script);
    for (engine, combined) in grouped {
        match engine {
            crate::config::ScriptEngine::Rhai => {
                let mut plugin = ScriptRhaiPlugin::new(combined);
                // Built-in `set_color_scheme(name)` extension. Installed before
                // user-supplied extensions so apps can override (they'd
                // shadow the registration on the same name).
                let sender = app
                    .world
                    .resource::<lumen_core::command::CommandQueue>()
                    .sender()
                    .clone();
                plugin = plugin.with_extension(move |engine: &mut rhai::Engine| {
                    engine.register_fn("set_color_scheme", move |name: rhai::ImmutableString| {
                        let Some(scheme) =
                            lumen_core::components::ColorScheme::from_name(name.as_str())
                        else {
                            tracing::warn!(
                                "set_color_scheme: unknown name {:?}; expected one of \
                                 \"default\"/\"force-light\"/\"force-dark\"/\
                                 \"prefer-light\"/\"prefer-dark\"",
                                name.as_str()
                            );
                            return;
                        };
                        let cmd = lumen_core::command::Command::Typed {
                            type_id: std::any::TypeId::of::<ColorSchemeIntent>(),
                            payload: Box::new(ColorSchemeIntent(scheme)),
                        };
                        if sender.try_send(cmd).is_err() {
                            tracing::warn!(
                                "set_color_scheme: CommandQueue full; dropping scheme update"
                            );
                        }
                    });
                });
                // File-based-pages navigation from script. Not a Rhai-only builtin:
                // both arities route through the shared, host-neutral
                // `lumen_core::nav` bus (`page("x")` navigates; no-arg `page()` reads
                // the current page), so candela reaches the same command surface. The
                // C-ABI (the root `lumen` crate) and the Rust SDK call the same `nav` functions.
                plugin = plugin.with_extension(move |engine: &mut rhai::Engine| {
                    engine.register_fn("page", move |path: rhai::ImmutableString| {
                        lumen_core::nav::navigate(path.to_string());
                    });
                    engine.register_fn("page", move || -> rhai::ImmutableString {
                        lumen_core::nav::current().into()
                    });
                    // History back/forward (in-memory stack on desktop).
                    engine.register_fn("page_back", lumen_core::nav::back);
                    engine.register_fn("page_forward", lumen_core::nav::forward);
                });
                // Native extensions (RunOptions / Rust SDK hooks) are
                // `rhai::Engine`-typed, so they only bind to the Rhai host and
                // are inapplicable to an app with no Rhai in it.
                for ext in std::mem::take(&mut rhai_extensions) {
                    plugin = plugin.with_extension(ext);
                }
                app.add_plugin(plugin);
                register_script_host_systems::<RhaiHost>(&mut app, multi_host);
                reloaders.push(engine, reload_script::<RhaiHost>);
            }
            #[cfg(feature = "host-lua")]
            crate::config::ScriptEngine::Lua => {
                let mut plugin = ScriptLuaPlugin::new(combined);
                // Host-specific ports of the Rhai `set_color_scheme` + `page`
                // extensions. Different engine type, identical semantics:
                // both push the same `Command::Typed` / `lumen_core::nav`
                // effects the Rhai closures above do.
                let sender = app
                    .world
                    .resource::<lumen_core::command::CommandQueue>()
                    .sender()
                    .clone();
                plugin = plugin.with_extension(move |lua: &mut mlua::Lua| {
                    let register = |lua: &mut mlua::Lua| -> mlua::Result<()> {
                        let g = lua.globals();
                        // `set_color_scheme(name)` - same unknown-name warn +
                        // `Command::Typed(ColorSchemeIntent)` push as Rhai.
                        g.set(
                            "set_color_scheme",
                            lua.create_function(move |_, name: String| {
                                let Some(scheme) =
                                    lumen_core::components::ColorScheme::from_name(&name)
                                else {
                                    tracing::warn!(
                                        "set_color_scheme: unknown name {:?}; expected one of \
                                         \"default\"/\"force-light\"/\"force-dark\"/\
                                         \"prefer-light\"/\"prefer-dark\"",
                                        name
                                    );
                                    return Ok(());
                                };
                                let cmd = lumen_core::command::Command::Typed {
                                    type_id: std::any::TypeId::of::<ColorSchemeIntent>(),
                                    payload: Box::new(ColorSchemeIntent(scheme)),
                                };
                                if sender.try_send(cmd).is_err() {
                                    tracing::warn!(
                                        "set_color_scheme: CommandQueue full; dropping scheme update"
                                    );
                                }
                                Ok(())
                            })?,
                        )?;
                        // `page(path)` navigates; no-arg `page()` reads the
                        // current page - same host-neutral `lumen_core::nav`
                        // bus the Rhai arity overloads route through.
                        g.set(
                            "page",
                            lua.create_function(
                                |lua, path: Option<String>| -> mlua::Result<mlua::Value> {
                                    match path {
                                        Some(p) => {
                                            lumen_core::nav::navigate(p);
                                            Ok(mlua::Value::Nil)
                                        }
                                        None => Ok(mlua::Value::String(
                                            lua.create_string(lumen_core::nav::current())?,
                                        )),
                                    }
                                },
                            )?,
                        )?;
                        g.set(
                            "page_back",
                            lua.create_function(|_, ()| Ok(lumen_core::nav::back()))?,
                        )?;
                        g.set(
                            "page_forward",
                            lua.create_function(|_, ()| Ok(lumen_core::nav::forward()))?,
                        )?;
                        Ok(())
                    };
                    if let Err(e) = register(lua) {
                        tracing::warn!(
                            "lua engine extension (set_color_scheme/page) registration failed: {e}"
                        );
                    }
                });
                app.add_plugin(plugin);
                register_script_host_systems::<LuaHost>(&mut app, multi_host);
                reloaders.push(engine, reload_script::<LuaHost>);
            }
            #[cfg(feature = "host-candela")]
            crate::config::ScriptEngine::Candela => {
                // Pass the entry path so a `dylib "..."` import in the app
                // resolves its library next to the app under `lumenc run`,
                // matching how `lumenc check` resolves it.
                // Color scheme and page navigation register inside the host's
                // own engine build (`lumen-script-candela`), beside the rest
                // of the prelude surface, so no extension hooks are needed.
                let plugin =
                    ScriptCandelaPlugin::new(combined).with_uri(html_path.display().to_string());
                app.add_plugin(plugin);
                register_script_host_systems::<CandelaHost>(&mut app, multi_host);
                reloaders.push(engine, reload_script::<CandelaHost>);
            }
            // A trimmed-out host is remapped onto Rhai by `remap_trimmed_hosts`
            // above, so its `ScriptEngine` variant can never reach here.
            #[cfg(not(all(feature = "host-lua", feature = "host-candela")))]
            _ => unreachable!("a trimmed script host must be remapped to Rhai before this match"),
        }
    }
    let _ = &rhai_extensions;
    app.world.insert_resource(reloaders);
    use crate::spawn::SpawnIntoWorld;
    let root = ir.spawn_into(&mut app.world);

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
            crate::pages::install_routing(&mut app, pages.entry.clone(), pages.keys.clone());
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
        app.add_systems(TickStage::Systems, hot_reload);
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
    let mut on_close_state: Option<Box<dyn FnOnce(lumen_window_winit::WindowGeometry) + Send>> =
        None;
    if cfg.window.remember_state.unwrap_or(false) {
        let app_id = cfg
            .app
            .id
            .clone()
            .unwrap_or_else(|| derive_app_id(&opts.dir));
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
    let menubar = ir
        .menubar
        .as_ref()
        .map(|spec| lumen_window_winit::MenuBarOptions {
            menus: spec
                .menus
                .iter()
                .map(|m| lumen_window_winit::MenuOptions {
                    label: m.label.clone(),
                    items: m
                        .items
                        .iter()
                        .map(|entry| match entry {
                            lumen_ir::layout_ir::MenuEntrySpec::Item {
                                id,
                                label,
                                accelerator,
                            } => lumen_window_winit::MenuEntryOptions::Item {
                                id: id.clone(),
                                label: label.clone(),
                                accelerator: accelerator.clone(),
                            },
                            lumen_ir::layout_ir::MenuEntrySpec::Separator => {
                                lumen_window_winit::MenuEntryOptions::Separator
                            }
                        })
                        .collect(),
                })
                .collect(),
        });
    // Render-side shaper: share the layout shaper's already-scanned font
    // database (TaffyLayoutPlugin::build inserted it above) instead of
    // re-walking every system font directory - the double `fontdb` scan
    // cost ~12-19 ms of every startup. Identical db contents keep glyph
    // output byte-for-byte identical to two independent scans.
    let render_shaper = match app.world.get_non_send::<CosmicShaper>() {
        Some(layout_shaper) => CosmicShaper::new_sharing_db(layout_shaper),
        None => CosmicShaper::new(),
    };
    // `--lumen-window-bg` resolved from the fully-combined (UA + skin +
    // app) stylesheet paints the GPU clear behind the very first frame -
    // what a user sees before the root element itself paints, and behind
    // any pixel the tree doesn't cover. Only a plain solid color parses
    // (the clear is a single RGBA, not a gradient); an app whose active
    // layers don't define the token - or define it as something else -
    // falls back to `opts.clear` (`lumen_window_winit::DEFAULT_CLEAR`
    // unless the caller overrode it), preserving today's behavior exactly.
    let clear = ir
        .combined_stylesheet
        .as_ref()
        .and_then(|sheet| sheet.resolve_root_var("lumen-window-bg"))
        .and_then(|value| lumen_ir::values::parse_color("<root>", "lumen-window-bg", &value).ok())
        .map(Into::into)
        .unwrap_or(opts.clear);
    let winit_opts = WinitOptions {
        size,
        title,
        clear,
        text_shaper: Some(Box::new(render_shaper)),
        maximized,
        frameless: ir.frameless,
        start_position,
        on_close_state,
        menubar,
    };
    // Dev-only in-window devtools overlay (F12). Gated behind the off-by-
    // default `devtools` feature; absent from release / bundle builds. The
    // overlay markup is parsed through the injected front-end, so it mounts
    // only when a parser was supplied (every dev / from-source run).
    #[cfg(feature = "devtools")]
    if let Some(parser) = parser.as_deref() {
        crate::devtools_mount::install(&mut app, parser);
    }

    // Embedder hooks run last so they can order their systems against
    // everything the default stack registered above (script dispatch,
    // binding readers, reconcilers).
    for hook in app_hooks {
        hook(&mut app);
    }
    Ok((app, winit_opts))
}

/// Fold any group whose host this build trimmed out into the Rhai group.
///
/// A static `--bundle` compiles only the hosts the app needs (Rhai is always
/// present; `host-lua` / `host-candela` add the others). `lumenc` derives that
/// feature list from the same app directory, so a missing host only happens on
/// a hand-edited misconfig; folding the source onto Rhai keeps the app running
/// with a warning instead of dropping its script, and makes the trimmed match
/// arms in [`build_app`] provably unreachable.
fn remap_trimmed_hosts(grouped: GroupedScripts) -> GroupedScripts {
    #[cfg(all(feature = "host-lua", feature = "host-candela"))]
    {
        grouped
    }
    #[cfg(not(all(feature = "host-lua", feature = "host-candela")))]
    {
        let mut kept: GroupedScripts = Vec::new();
        for (engine, source) in grouped {
            let available = match engine {
                crate::config::ScriptEngine::Rhai => true,
                crate::config::ScriptEngine::Lua => cfg!(feature = "host-lua"),
                crate::config::ScriptEngine::Candela => cfg!(feature = "host-candela"),
            };
            if !available {
                tracing::warn!(
                    "this build was compiled without the {} host; running its \
                     script under the rhai host instead",
                    engine.name()
                );
            }
            let engine = if available {
                engine
            } else {
                crate::config::ScriptEngine::Rhai
            };
            match kept.iter_mut().find(|(e, _)| *e == engine) {
                Some((_, acc)) => {
                    acc.push('\n');
                    acc.push_str(&source);
                }
                None => kept.push((engine, source)),
            }
        }
        kept
    }
}

/// Lowercase + dash-only fallback `app-id` when `lumen.toml [app] id`
/// is unset. Mirrors the convention used by other crates that need
/// per-app state dirs.
fn derive_app_id(dir: &std::path::Path) -> String {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(|s| {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "lumen-app".to_string())
}
