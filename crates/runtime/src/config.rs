//! Per-app `lumen.toml` config loader.
//!
//! - Reads an optional file at `<app-dir>/lumen.toml`.
//! - Sets runtime defaults; CLI flags supplied on top take precedence.
//! - Unknown top-level keys are rejected; section fields default when absent.
//!
//! ```toml
//! [app]
//! entry = "main.lmn"          # default
//! locale = "de-DE"            # default: the OS locale, else en-US
//!
//! [window]
//! title = "My App"            # default: app directory name
//! size  = [1280, 720]         # default: [960, 720]
//!
//! [skin]
//! name = "default"            # default: none (bare framework)
//!
//! [mcp]
//! port = 7878                 # default when absent; 0 disables the server
//!
//! [profile]
//! mode = "off"                # off | chrome | tracy | stderr
//!
//! [asset_roots]
//! paths = ["icons", "../shared"]   # extra dirs scanned for relative src=
//!
//! [dependencies]                   # runtime modules, loaded in sorted-name order
//! lumen-audio = { bundled = true }
//! shape-tools = { path = "modules/shape-tools", config = { units = "mm" } }
//!
//! [[hooks]]                        # project build/setup commands; see `crate::hooks`
//! when    = "prebuild"             # "prebuild" | "prerun"
//! os      = "linux"                # optional: "linux" | "macos" | "windows"
//! run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
//! inputs  = ["md.c"]
//! outputs = ["libmd.so"]
//!
//! [[plugins]]                      # compiler plugins, run in declaration order
//! name    = "markdown"             # must match what the cdylib reports
//! path    = "plugins/markdown"     # built cdylib; extensionless probes lib*.so/.dylib/.dll
//! config  = { flavor = "gfm" }     # optional, handed to the plugin verbatim
//! ```
//!
//! Parse failures surface as [`ConfigError`]; the caller decides whether to abort or fall back to defaults.

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Parsed `lumen.toml` config, all fields optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LumenToml {
    /// `[app]` section.
    pub app: AppCfg,
    /// `[pages]` section - file-based multi-page navigation config.
    pub pages: PagesCfg,
    /// `[window]` section.
    pub window: WindowCfg,
    /// `[skin]` section.
    pub skin: SkinCfg,
    /// `[mcp]` section.
    pub mcp: McpCfg,
    /// `[profile]` section.
    pub profile: ProfileCfg,
    /// `[asset_roots]` section.
    pub asset_roots: AssetRootsCfg,
    /// `[script]` section - script-engine selection.
    pub script: ScriptCfg,
    /// `[perf]` section - per-cache memory budgets.
    pub perf: PerfCfg,
    /// `[runtime]` section - subsystem init overrides (MCP /
    /// hot-reload / thread budget).
    pub runtime: RuntimeCfg,
    /// `[capabilities]` section - per-app COMPILE-TIME subsystem trim toggles
    /// for the static `--bundle` build (Part B tree-shaking).
    pub capabilities: CapabilitiesCfg,
    /// `[web]` section - how `lumenc web` emits the app as a static site.
    pub web: WebCfg,
    /// `[[hooks]]` array - project-declared build/setup commands, run at the
    /// `prebuild` / `prerun` trigger points by `lumenc run` / `build` /
    /// `bundle`. See [`crate::hooks`] for the execution semantics (ordering,
    /// OS filtering, staleness skip) and [`HookCfg`] for the field reference.
    pub hooks: Vec<HookCfg>,
    /// `[[plugins]]` array - compiler plugins, run in declaration order by
    /// every compile path, `check` included. This field only accepts the
    /// array (`deny_unknown_fields` would otherwise reject any app declaring
    /// one); nothing reads it. The schema, the loader, and the validation
    /// live in `lumenc-plugin` (see its `PluginCfg`), lumenc reads the file
    /// itself, and the chain reaches the pipeline through the injected
    /// [`crate::compiler_plugins::CompilerPlugins`] boundary, the same
    /// inversion the parser uses.
    pub plugins: Vec<toml::Value>,
    /// `[dependencies]` table - prebuilt dylibs loaded at startup in
    /// sorted-name order: engine-linked runtime modules and portable C-ABI
    /// plugins, told apart by their exports. The schema, the loader, and
    /// the failure policy live in `lumen-modules` (re-exported as
    /// [`crate::modules`]); `build_app` runs the table through the loader
    /// when the `modules` feature is on.
    pub dependencies: lumen_modules::DependenciesCfg,
    /// `[signals]` table - optional typed schema. Each key declares
    /// the expected `SignalType`. Used by `lumenc lint --signals`
    /// to flag untyped writes / schema mismatches. Schema entries
    /// are optional: missing keys just downgrade the lint severity.
    #[serde(default)]
    pub signals: SignalsCfg,
}

/// `[app]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppCfg {
    /// Markup entry filename relative to the app dir. Defaults to `main.lmn`.
    pub entry: Option<String>,
    /// Stable identifier used for per-app state directories (window state, plugin caches). Falls back to the app directory name when absent.
    pub id: Option<String>,
    /// Optional app-kind override (`"markup"` | `"rust"` | `"cpp"` |
    /// `"python"`). Optional and used only to override auto-detection: when
    /// absent, `crate::app_kind::detect` inspects the directory; when present,
    /// it wins. Lets an ambiguous directory (e.g. a Rust workspace member that
    /// should still run as pure markup) pin its build/run route.
    pub kind: Option<crate::app_kind::AppKind>,
    /// BCP-47 tag naming the locale the app starts in, e.g. `"de-DE"`.
    /// Absent means "follow the OS", falling back to `en-US`. The tag
    /// selects which `locale/<tag>.ftl` catalogue `translatable="..."`
    /// markup and the scripts' `t()` builtin resolve against; every
    /// catalogue in the directory is loaded regardless. A tag that is not
    /// valid BCP-47 is a `lumen.toml` error.
    #[serde(default, deserialize_with = "de_locale")]
    pub locale: Option<lumen_i18n::LanguageIdentifier>,
}

/// Parse `[app] locale` into a validated language identifier, so a typo
/// fails at config-load time naming the offending tag rather than silently
/// leaving the app in its default locale.
fn de_locale<'de, D>(deserializer: D) -> Result<Option<lumen_i18n::LanguageIdentifier>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    lumen_i18n::Lang::try_from(raw.trim())
        .map(|l| Some(l.into()))
        .map_err(|e| {
            serde::de::Error::custom(format!("app: `locale` is not a valid BCP-47 tag: {e}"))
        })
}

/// `[pages]` block - file-based multi-page navigation.
///
/// Static app structure belongs in `lumen.toml` (the single source of
/// truth). Auto-discovery loads every `.lmn` file in the app dir as a page
/// keyed by its filename stem; these fields let the config pin the static
/// bits the AOT build and the runtime both read.
///
/// ```toml
/// [pages]
/// entry = "index"                   # home page key (default: "index", else the
///                                   #   [app] entry stem, else "main")
/// enabled = true                    # force multi-page on/off (default: auto -
///                                   #   on when >1 .lmn file is present, or
///                                   #   `include` names >1 page)
/// include = ["index.lmn", "settings.lmn", "user.lmn"]  # explicit page set,
///                                   #   overriding directory auto-discovery
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PagesCfg {
    /// Home/entry page key (filename stem, no `.lmn`). Defaults to `index`
    /// when an `index.lmn` exists, else the `[app] entry` stem, else `main`.
    pub entry: Option<String>,
    /// Force multi-page mode on (`true`) or off (`false`) and win over the
    /// auto default either way. When absent, multi-page activates
    /// automatically when more than one `.lmn` file sits in the app
    /// directory, or when `include` names more than one page, including
    /// pages living in a subfolder the directory scan never sees. So
    /// single-page apps (`main.lmn` / a lone `index.lmn`, or a single-entry
    /// `include`) keep the exact legacy single-file load path.
    pub enabled: Option<bool>,
    /// Explicit ordered page-file list (relative filenames, may name a
    /// subfolder). When set, this overrides directory auto-discovery: only
    /// these files are pages.
    pub include: Option<Vec<String>>,
}

/// `[window]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowCfg {
    /// Window title override.
    pub title: Option<String>,
    /// `[w, h]` in logical pixels.
    pub size: Option<[u32; 2]>,
    /// When `true`, persists window position, size, and maximised state to `<state_dir>/<app-id>/window-state.toml` on close and restores from it on the next launch. Defaults to `false`.
    pub remember_state: Option<bool>,
}

/// `[skin]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkinCfg {
    /// Embedded skin name; equivalent to `<root skin="...">`.
    pub name: Option<String>,
}

/// `[mcp]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpCfg {
    /// TCP port for the introspection server. Absent means port 7878; `0`
    /// disables the server.
    pub port: Option<u16>,
    /// When `true`, the MCP plugin drains the `SimulateQueue` each tick and injects pointer, key, and scroll events. Defaults to off.
    pub simulate: Option<bool>,
}

/// `[profile]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileCfg {
    /// `off` | `chrome` | `tracy` | `stderr`. Default `off`.
    pub mode: Option<String>,
}

/// `[asset_roots]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssetRootsCfg {
    /// Extra directories (relative to the app dir, or absolute).
    pub paths: Option<Vec<String>>,
}

/// `[script]` block - an override that forces every script in the app onto one
/// engine. Without it each script file picks its host from its own extension
/// (`.cdl` -> candela, `.rhai` -> rhai, `.lua` -> lua) and an app that ships
/// more than one language runs one host per language. Set `engine` when the
/// per-file answer is not the one you want, most often because the app keeps
/// its script inline in the markup, where there is no extension to read.
///
/// ```toml
/// [script]
/// engine = "lua"   # "candela" (default) | "rhai" | "lua"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptCfg {
    /// Engine name: `"candela"` (default), `"rhai"`, or `"lua"`. Unknown values
    /// fall back to candela via [`ScriptCfg::engine_kind`].
    pub engine: Option<String>,
}

/// One script engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ScriptEngine {
    /// candela (`lumen-script-candela`), the default Lumen language.
    #[default]
    Candela,
    /// Lua 5.4 (`lumen-script-lua`).
    Lua,
    /// Rhai (`lumen-script-rhai`) - compat host.
    Rhai,
}

impl ScriptEngine {
    /// Every engine, in the fixed order active hosts are built and their
    /// systems registered. Declaration order is the ordering key, so a
    /// two-language app wires its hosts the same way on every run.
    pub const ALL: [ScriptEngine; 3] =
        [ScriptEngine::Candela, ScriptEngine::Lua, ScriptEngine::Rhai];

    /// The engine that owns a script file with this extension, or `None` for an
    /// extension no host claims.
    pub fn from_extension(ext: &str) -> Option<ScriptEngine> {
        match ext {
            "cdl" => Some(ScriptEngine::Candela),
            "lua" => Some(ScriptEngine::Lua),
            "rhai" => Some(ScriptEngine::Rhai),
            _ => None,
        }
    }

    /// The engine that owns a script path, read from its file extension.
    pub fn from_path(path: &Path) -> Option<ScriptEngine> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(ScriptEngine::from_extension)
    }

    /// The engine an engine name selects, defaulting to
    /// [`ScriptEngine::Candela`] for a name no host claims. The inverse of
    /// [`Self::name`], with the same fallback [`ScriptCfg::engine_kind`] uses.
    pub fn from_name(name: &str) -> ScriptEngine {
        let name = name.trim();
        if name.eq_ignore_ascii_case("lua") {
            ScriptEngine::Lua
        } else if name.eq_ignore_ascii_case("rhai") {
            ScriptEngine::Rhai
        } else {
            ScriptEngine::Candela
        }
    }

    /// The `[script] engine` name for this engine.
    pub fn name(self) -> &'static str {
        match self {
            ScriptEngine::Candela => "candela",
            ScriptEngine::Lua => "lua",
            ScriptEngine::Rhai => "rhai",
        }
    }
}

impl ScriptCfg {
    /// Resolve the declared engine name, defaulting to
    /// [`ScriptEngine::Candela`] when absent or unrecognised
    /// (case-insensitive match on `candela` / `rhai` / `lua`).
    pub fn engine_kind(&self) -> ScriptEngine {
        match self.engine.as_deref().map(str::trim) {
            Some(e) if e.eq_ignore_ascii_case("lua") => ScriptEngine::Lua,
            Some(e) if e.eq_ignore_ascii_case("rhai") => ScriptEngine::Rhai,
            _ => ScriptEngine::Candela,
        }
    }
}

/// `[perf]` block carrying per-cache memory caps.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerfCfg {
    /// Image content cache cap in MB; defaults to 64.
    pub images_mb: Option<u32>,
    /// Text shape-cache cap in entries; defaults to 512.
    pub shape_entries: Option<u32>,
    /// Vello scene-fragment cache cap in entries; defaults to 256.
    pub scene_fragments: Option<u32>,
}

/// `[runtime]` block - per-app overrides for the startup subsystem-gating
/// (measured startup quick-wins). Every field is `Option`: `None` keeps the
/// automatic behaviour (run-mode gating for MCP /
/// hot-reload; `min(cores, 4)` for threads), while `Some(v)` forces it.
///
/// ```toml
/// [runtime]
/// mcp = false         # force the MCP server off (or true = on)
/// hot_reload = false  # force the source watcher off (or true = on)
/// threads = 2         # bevy_ecs worker-thread budget
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeCfg {
    /// Force the MCP introspection server on/off. `None` = on for an
    /// interactive windowed run, off for a headless / bounded run (unless
    /// `[mcp] simulate = true`). Interacts with `[mcp] port` (a `port = 0`
    /// still hard-disables).
    pub mcp: Option<bool>,
    /// Force the hot-reload source watcher on/off. `None` = on only for an
    /// interactive run from source (off for headless / bounded / artifact /
    /// in-memory).
    pub hot_reload: Option<bool>,
    /// bevy_ecs worker-thread budget. `None` = `min(available_parallelism, 4)`.
    /// Also overridable at runtime by the `LUMEN_THREADS` env var, which
    /// wins over this value.
    pub threads: Option<usize>,
}

/// `[capabilities]` block: per-app compile-time subsystem trim toggles for
/// the static `--bundle` build (Part B tree-shaking). Unlike `[runtime]` (which
/// gates *initialization* in the always-full shared runtime), these select the
/// cargo feature set lumenc compiles the per-app static seam with, so an unused
/// subsystem's crate is dropped from the binary entirely.
///
/// Every field is `Option<bool>`: `None` lets lumenc infer the capability from a
/// bounded source scan (err toward on, see [`BundleCapabilities::resolve`]);
/// `Some(v)` forces it. Ignored by the shared dlopen'd library and the dev
/// `lumenc run` path, which always ship every subsystem.
///
/// ```toml
/// [capabilities]
/// http-fetch = false
/// mcp = false
/// async = false
/// ```
///
/// Which script hosts get compiled in follows from the app's script file
/// extensions (or `[script] engine`), not from this block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilitiesCfg {
    /// Force the MCP introspection server into/out of the bundle. `None` = OFF
    /// (MCP is a dev/introspection capability, never inferred into a release
    /// bundle).
    pub mcp: Option<bool>,
    /// Force the async (tokio) bridge into/out of the bundle. `None` = infer
    /// from the file-dialog builtins, which resolve on that runtime.
    #[serde(rename = "async")]
    pub async_rt: Option<bool>,
    /// Force the HTTP client behind the scripts' `fetch()` / `http()` builtins
    /// (ureq + rustls + ring) into/out of the bundle. `None` = infer from a
    /// `fetch(` marker.
    #[serde(rename = "http-fetch")]
    pub http_fetch: Option<bool>,
}

/// The resolved per-app capability set for a static `--bundle`, produced by
/// merging explicit `[capabilities]` / `[runtime]` config with a conservative
/// source scan, then mapped to the cargo `--features` list the trimmed runtime
/// seam is built with.
///
/// CONSERVATIVE CONTRACT (mirrors [`crate::run::subsystems::SubsystemUsage`]):
/// a capability is inferred OFF only on a reliable *unused* signal; anything
/// ambiguous (a marker present, an unreadable dir) forces it ON. Explicit
/// config always overrides inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleCapabilities {
    /// MCP introspection server present.
    pub mcp: bool,
    /// Async (tokio) bridge present.
    pub async_rt: bool,
    /// Scripts' HTTP `fetch()` builtin present.
    pub http_fetch: bool,
    /// Runtime-module loader present. Follows `[dependencies]`: any declared
    /// module keeps it, because a build without the loader would drop the
    /// modules in silence rather than with the load banner.
    pub modules: bool,
    /// The script hosts compiled into the bundle, one per language the app
    /// ships, in [`ScriptEngine::ALL`] order.
    pub hosts: Vec<ScriptEngine>,
}

impl BundleCapabilities {
    /// Resolve the capability set for the app rooted at `dir` from `cfg` plus a
    /// bounded source scan. Explicit `[capabilities]` wins, then inference.
    pub fn resolve(dir: &Path, cfg: &LumenToml) -> Self {
        let hay = crate::run::subsystems::scan_app_sources(dir);

        // MCP: a dev/introspection capability, never inferred ON for a release
        // bundle; only an explicit toggle pulls it in.
        let mcp = cfg.capabilities.mcp.unwrap_or(false);

        // Async: an app that opens dialogs must keep the capability, since
        // that is what they resolve on. Same marker scan the runtime startup
        // gate uses.
        let async_rt = cfg
            .capabilities
            .async_rt
            .unwrap_or_else(|| crate::run::subsystems::file_dialog_markers_present(&hay));

        // HTTP fetch: explicit, else infer from a `fetch(` builtin marker.
        let http_fetch = cfg
            .capabilities
            .http_fetch
            .unwrap_or_else(|| hay.contains("fetch("));

        // Modules: any declared `[dependencies]` entry keeps the loader in,
        // so a missing module banners at startup instead of vanishing.
        let modules = !cfg.dependencies.0.is_empty();

        let hosts = infer_script_hosts(dir, cfg);

        Self {
            mcp,
            async_rt,
            http_fetch,
            modules,
            hosts,
        }
    }

    /// The cargo `--features` list to compile the trimmed runtime seam with,
    /// passed after `--no-default-features`. `runtime-parse` is intentionally
    /// omitted: a `--bundle` runs from a precompiled AOT artifact, so the
    /// source parser is dropped too. Every script host contributes its own
    /// feature, so a bundle links only the languages the app ships.
    pub fn to_features(&self) -> Vec<String> {
        let mut f: Vec<String> = Vec::new();
        if self.mcp {
            f.push("mcp".into());
        }
        if self.async_rt {
            f.push("async".into());
        }
        if self.http_fetch {
            f.push("http-fetch".into());
        }
        if self.modules {
            f.push("modules".into());
        }
        for host in &self.hosts {
            match host {
                ScriptEngine::Rhai => f.push("host-rhai".into()),
                ScriptEngine::Lua => f.push("host-lua".into()),
                ScriptEngine::Candela => f.push("host-candela".into()),
            }
        }
        f
    }
}

/// The script engines the app rooted at `dir` needs, in [`ScriptEngine::ALL`]
/// order.
///
/// `[script] engine` forces the answer to that one engine. Otherwise every
/// script file in the app's `src/` contributes its extension's engine (`.cdl`
/// -> candela, `.lua` -> Lua, `.rhai` -> Rhai), so an app holding two
/// languages comes back with two engines. An app with no script at all comes
/// back with candela, the language an inline `<script>` block is read as.
///
/// This is the directory-scan answer, used before the markup is parsed: by
/// `lumenc build` to pick which hosts to compile into a bundle, and by the
/// startup subsystem gate. Once the markup is available, the authoritative
/// grouping comes from the `<script src>` set the app references.
pub fn infer_script_hosts(dir: &Path, cfg: &LumenToml) -> Vec<ScriptEngine> {
    if cfg.script.engine.is_some() {
        return vec![cfg.script.engine_kind()];
    }
    let mut found: Vec<ScriptEngine> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(crate::app_layout::src_dir(dir)) {
        for entry in rd.flatten().take(512) {
            if let Some(engine) = ScriptEngine::from_path(&entry.path())
                && !found.contains(&engine)
            {
                found.push(engine);
            }
        }
    }
    if found.is_empty() {
        return vec![ScriptEngine::default()];
    }
    found.sort();
    found
}

/// `[web]` block - how `lumenc web` emits the app as a static site.
///
/// The rest of `lumen.toml` still describes the app: `[window] title` is the
/// documents' title, `[app] id` / `entry` / `locale`, `[pages]`,
/// `[asset_roots]` and `[script] engine` all apply. This block covers what
/// only a site has: where it is served from, what a crawler is told, and
/// which locales it is emitted in.
///
/// `[capabilities]` does not apply. A desktop build compiles a runtime per
/// app; a site loads one prebuilt runtime that ships with the toolchain, so
/// there is nothing per-app to trim.
///
/// ```toml
/// [web]
/// out_dir   = "dist/web"       # where the site is written
/// base_path = "/"              # URL prefix the site is served under
/// url = "https://example.com"  # absolute site URL; canonical + sitemap need it
/// description = "A Lumen app"  # used by any page without its own
/// locales = ["en-US", "de-DE"] # one tree per locale
/// host = "netlify"             # also write that host's deep-path rewrite file
/// render = "ssr"               # a document per request
/// runtime = false              # and no browser runtime in it
///
/// [web.seed]
/// count = 3                    # signal values the pages are rendered with
///
/// [web.pages.settings]
/// title = "Settings"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebCfg {
    /// Directory the site is written to, relative to the app dir unless
    /// absolute. Defaults to `dist/web`.
    pub out_dir: Option<String>,
    /// URL prefix the site is served under, such as `/` or `/docs/`. Every
    /// link and asset reference in the documents hangs off it.
    pub base_path: Option<String>,
    /// Absolute site URL, such as `https://example.com`. The canonical link,
    /// the social metadata and the sitemap need it; without it they are left
    /// out.
    pub url: Option<String>,
    /// Description used by any page that does not set its own.
    pub description: Option<String>,
    /// Image for social previews, relative to the site root or absolute.
    pub og_image: Option<String>,
    /// Absolute URL the pages declare as canonical, for a site published at
    /// more than one address. Defaults to [`Self::url`].
    pub canonical: Option<String>,
    /// Every locale the site is emitted in, one document tree each. Absent
    /// emits the default locale alone.
    pub locales: Option<Vec<String>>,
    /// The locale whose tree sits at the site root; the others sit under
    /// `/<tag>/`. Defaults to `[app] locale`, then `en-US`.
    pub default_locale: Option<String>,
    /// Skin the site is styled with. Defaults to `[skin] name`, then
    /// `default`. `auto` is not read here: it means "the machine's OS", and
    /// a site is served to every OS.
    pub skin: Option<String>,
    /// How the pages are styled.
    pub css: WebCssMode,
    /// Which shape a widget the parser built out of smaller elements is
    /// emitted as.
    pub widgets: WebWidgets,
    /// Where a page's document comes from.
    pub render: WebRender,
    /// Whether the documents carry the browser runtime. Unset takes what
    /// [`Self::render`] implies, which is the only thing `static` and `csr`
    /// differ about; `render = "ssr"` is the one that leaves it open.
    pub runtime: Option<bool>,
    /// Where the state the pages are rendered with comes from.
    pub prerender: WebPrerender,
    /// Add a content hash to asset file names, so a cache can hold them
    /// forever. A build does not apply this yet.
    pub hash_assets: Option<bool>,
    /// Write the extra `data-lm-*` attributes that name what an element came
    /// from. A build does not write them yet.
    pub debug_attrs: Option<bool>,
    /// What an app menu bar becomes in a document.
    pub menubar: WebMenubar,
    /// Write `sitemap.xml`. Needs [`Self::url`]. Defaults to on when a URL is
    /// configured.
    pub sitemap: Option<bool>,
    /// Host the site is deployed to, which decides the rewrite file that
    /// makes a deep path serve the app instead of the host's own 404.
    pub host: WebHost,
    /// How a link to another page of the same site is followed.
    pub navigation: WebNavigation,
    /// `[web.seed]` - signal values every page is rendered with.
    pub seed: BTreeMap<String, WebSeedValue>,
    /// `[web.pages.<key>]` - per-page title and description.
    pub pages: BTreeMap<String, WebPageCfg>,
}

/// `[web.pages.<key>]` block - what one page says about itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebPageCfg {
    /// Title for this page. Falls back to `[window] title`.
    pub title: Option<String>,
    /// Description for this page. Falls back to `[web] description`.
    pub description: Option<String>,
}

/// One `[web.seed]` value.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum WebSeedValue {
    /// A boolean. Ahead of the number arms: TOML tells the two apart, and
    /// serde tries untagged arms in order.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A number with a fraction.
    Float(f64),
    /// Text.
    Str(String),
    /// The rows of an array signal, written as an array of tables. This is
    /// what puts a list in a page that nothing has run yet: a `<for>` over
    /// this name is emitted with these rows in it.
    Rows(Vec<BTreeMap<String, String>>),
}

/// `[web] css` - how a page's styling reaches the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebCssMode {
    /// As a stylesheet, with the selectors, states and media queries the app
    /// was written with.
    #[default]
    Sheet,
    /// As the values Lumen's own cascade resolved, written onto each element
    /// as an inline style. Nothing is left to match on, which is what makes
    /// it the thing to compare the stylesheet against.
    Computed,
}

/// `[web] widgets` - which shape a composed widget is emitted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebWidgets {
    /// As the HTML element that carries the same meaning, so a screen reader
    /// and a crawler read a dropdown as a dropdown.
    #[default]
    Semantic,
    /// As the elements the parser built the widget out of.
    Verbatim,
}

/// `[web] render` - where a page's document comes from.
///
/// Every mode writes the whole markup tree, so a reader and a crawler get the
/// same document whichever one is set. What changes is where that document is
/// produced and what runs once it is open. [`WebPrerender`] is the other
/// half, and says which state the markup is written with.
///
/// `static` and `csr` differ only in whether the document carries the runtime,
/// which is why [`WebCfg::runtime`] is refused alongside either of them when
/// it says the opposite. `ssr` leaves that question open, so it is the one
/// that key answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebRender {
    /// Files and nothing else: no runtime, no scripts, links followed as the
    /// browser follows any link. A directory to upload and leave alone.
    Static,
    /// The pages load the runtime, which adopts the markup they arrived with
    /// and runs the app from there.
    #[default]
    Csr,
    /// Each document is produced for the request that asks for it, by running
    /// the app for that request. The build writes what a server needs instead
    /// of the pages: the compiled app, the stylesheet, the assets, and the
    /// runtime when the pages carry one.
    Ssr,
}

/// `[web] prerender` - where the state a page is rendered with comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebPrerender {
    /// The values `[web.seed]` and the markup itself declare.
    #[default]
    Seeds,
    /// Run the app at build time and write each page with the state it
    /// settles into, on top of the declared values.
    Run,
    /// None: a branch is not taken and a list has no rows until the browser
    /// runs the app.
    None,
}

/// `[web] menubar` - what an app menu bar becomes in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebMenubar {
    /// Left out. A desktop menu bar is the window's, and a page has no
    /// window.
    #[default]
    Omit,
    /// A `<nav>` holding the top-level items.
    Nav,
}

/// `[web] host` - where the site is deployed, and so which rewrite file lets
/// a deep path reach the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebHost {
    /// A plain file server, which serves `404.html` for a path it has no
    /// file for. That is what the emitted `404.html` is there for.
    #[default]
    Static,
    /// Netlify: also write `_redirects`.
    Netlify,
    /// Vercel: also write `vercel.json`.
    Vercel,
    /// Apache: also write `.htaccess`.
    Apache,
    /// nginx: also write `nginx.conf`, to include from a server block.
    Nginx,
}

/// `[web] navigation` - how a link to another page of the same site is
/// followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebNavigation {
    /// The runtime intercepts the click and swaps the page in place.
    #[default]
    Soft,
    /// The browser loads the target document.
    Hard,
}

/// One `[[hooks]]` entry: an app-declared build/setup command.
///
/// ```toml
/// [[hooks]]
/// when    = "prebuild"
/// os      = "linux"
/// run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
/// inputs  = ["md.c"]
/// outputs = ["libmd.so"]
/// ```
///
/// `when` and `run` are required; `os`, `inputs`, and `outputs` are optional
/// and default empty/absent. Validated at parse time by the custom
/// [`Deserialize`] impl below, so a bad `when` / `os` value or an empty `run`
/// surfaces as a `lumen.toml` parse error naming the offending value and the
/// accepted set, rather than failing later at hook-run time.
///
/// Hooks execute arbitrary shell commands read from a file inside the app
/// directory - the same trust model as a Cargo build script. `lumenc run
/// <dir>` on an app from an untrusted source runs that app's hooks; pass
/// `--no-hooks` to skip them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCfg {
    /// Trigger point: `prebuild` or `prerun`.
    pub when: HookWhen,
    /// Restrict the hook to one OS. `None` runs on every platform.
    pub os: Option<HookOs>,
    /// The command line, run via `sh -c` (unix) or `cmd /C` (windows) with
    /// the app directory as the child's cwd.
    pub run: String,
    /// Files the command reads, relative to the app directory unless
    /// absolute. Used only for the staleness check; never passed to `run`.
    pub inputs: Vec<String>,
    /// Files the command produces, relative to the app directory unless
    /// absolute. Used only for the staleness check.
    pub outputs: Vec<String>,
}

/// `[[hooks]]` `when` value: the trigger point a hook fires at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookWhen {
    /// Fires for `lumenc build`, `lumenc bundle`, and `lumenc run` (before
    /// `prerun`). The place to produce native artifacts a build or run needs.
    Prebuild,
    /// Fires for `lumenc run` only, after every `prebuild` hook has run.
    Prerun,
}

/// Bare-string `"prebuild"` / `"prerun"` decoder for [`HookWhen`].
impl TryFrom<&str> for HookWhen {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "prebuild" => Ok(HookWhen::Prebuild),
            "prerun" => Ok(HookWhen::Prerun),
            other => Err(format!(
                "unknown hook `when` value `{other}` (expected one of: prebuild, prerun)"
            )),
        }
    }
}

/// `[[hooks]]` `os` value: restricts a hook to one platform, matched against
/// [`std::env::consts::OS`] (`"linux"`, `"macos"`, `"windows"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOs {
    /// Matches `std::env::consts::OS == "linux"`.
    Linux,
    /// Matches `std::env::consts::OS == "macos"`.
    Macos,
    /// Matches `std::env::consts::OS == "windows"`.
    Windows,
}

/// Bare-string `"linux"` / `"macos"` / `"windows"` decoder for [`HookOs`].
impl TryFrom<&str> for HookOs {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "linux" => Ok(HookOs::Linux),
            "macos" => Ok(HookOs::Macos),
            "windows" => Ok(HookOs::Windows),
            other => Err(format!(
                "unknown hook `os` value `{other}` (expected one of: linux, macos, windows)"
            )),
        }
    }
}

/// Raw `[[hooks]]` table shape, deserialized field-by-field so [`HookCfg`]
/// can validate `when` / `os` / `run` and produce a clear error naming the
/// offending value.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHookCfg {
    when: String,
    os: Option<String>,
    run: String,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
}

impl<'de> Deserialize<'de> for HookCfg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawHookCfg::deserialize(deserializer)?;
        let when = HookWhen::try_from(raw.when.as_str()).map_err(serde::de::Error::custom)?;
        let os = raw
            .os
            .as_deref()
            .map(HookOs::try_from)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        if raw.run.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "hooks: `run` must not be empty or whitespace-only",
            ));
        }
        Ok(HookCfg {
            when,
            os,
            run: raw.run,
            inputs: raw.inputs,
            outputs: raw.outputs,
        })
    }
}

/// `[signals]` table - typed signal schema for the `lumenc lint
/// --signals` mode. Each entry maps a signal name to its expected
/// [`SignalType`]. Authors hand-write this; the lint subcommand
/// uses it to flag untyped writes and schema mismatches.
///
/// ```toml
/// [signals]
/// count = "i64"
/// theme = "string"
/// user = { name = "string", email = "string" }
///
/// [signals.users]
/// type = "array"
/// fields = { id = "i64", name = "string", email = "string" }
/// ```
#[derive(Debug, Clone, Default)]
pub struct SignalsCfg {
    /// Declared signal name -> type. Missing entries are not errors.
    pub fields: HashMap<String, SignalType>,
}

/// Custom deserializer - accept a `[signals]` table whose values are
/// either bare-string type tokens (`"i64"`), nested inline tables
/// (`{ name = "string" }` -> struct), or explicit `{ type = "array",
/// fields = { ... } }` records.
impl<'de> Deserialize<'de> for SignalsCfg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw: HashMap<String, toml::Value> = HashMap::deserialize(deserializer)?;
        let mut fields = HashMap::with_capacity(raw.len());
        for (k, v) in raw {
            let ty = SignalType::try_from(&v).map_err(serde::de::Error::custom)?;
            fields.insert(k, ty);
        }
        Ok(SignalsCfg { fields })
    }
}

/// Declared signal type for a [`SignalsCfg`] entry.
///
/// The scalar variants (`I64`, `F64`, `Bool`, `Str`, `Color`, `Vec2`)
/// mirror the four PropertyValue variants Lumen's PropertyStore models
/// natively plus `string` for unrestricted strings. `Object` and
/// `Array` carry their own field schemas so nested map/array signals
/// can be typed at the leaf.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalType {
    /// `signal_set_int` target - `PropertyValue::I64`.
    I64,
    /// `signal_set_float` target - `PropertyValue::F64`.
    F64,
    /// `signal_set_bool` target - `PropertyValue::Bool`.
    Bool,
    /// `signal_set` target - `PropertyValue::Str`. `signal_set` has no
    /// dedicated `signal_set_string` variant; it is already the typed
    /// sink for a `string`-declared signal.
    Str,
    /// `signal_set_color` target - `#rrggbb` / `#rrggbbaa`.
    Color,
    /// `vec2(x, y)` typed signal.
    Vec2,
    /// Array of typed records.
    Array {
        /// Per-field types of each array element record.
        fields: HashMap<String, SignalType>,
    },
    /// Inline object / map record.
    Object {
        /// Per-field types of the record.
        fields: HashMap<String, SignalType>,
    },
}

/// Bare-string `"i64"` / `"f64"` / `"bool"` / `"string"` / `"color"` /
/// `"vec2"` decoder for [`SignalType`].
impl TryFrom<&str> for SignalType {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "i64" | "int" | "integer" => Ok(SignalType::I64),
            "f64" | "float" | "number" => Ok(SignalType::F64),
            "bool" | "boolean" => Ok(SignalType::Bool),
            "string" | "str" | "text" => Ok(SignalType::Str),
            "color" => Ok(SignalType::Color),
            "vec2" => Ok(SignalType::Vec2),
            "array" => Ok(SignalType::Array {
                fields: HashMap::new(),
            }),
            "object" | "map" => Ok(SignalType::Object {
                fields: HashMap::new(),
            }),
            other => Err(format!(
                "unknown signal type `{other}` (expected one of: i64, f64, bool, string, color, vec2, array, object)"
            )),
        }
    }
}

/// Parse a `toml::Value` into a [`SignalType`]. Accepts:
/// - bare strings -> scalar variant
/// - inline tables with `type = "array"` + `fields = { ... }` -> `Array`
/// - inline tables with `type = "object"` + `fields = { ... }` -> `Object`
/// - inline tables without a `type` key -> `Object` (treat the whole
///   table as the field map).
impl TryFrom<&toml::Value> for SignalType {
    type Error = String;

    fn try_from(v: &toml::Value) -> Result<Self, Self::Error> {
        match v {
            toml::Value::String(s) => SignalType::try_from(s.as_str()),
            toml::Value::Table(t) => {
                let ty = t.get("type").and_then(|x| x.as_str()).unwrap_or("object");
                let fields_map = t.get("fields").and_then(|x| x.as_table());
                let parse_fields =
                    |table: &toml::value::Table| -> Result<HashMap<String, SignalType>, String> {
                        let mut out = HashMap::with_capacity(table.len());
                        for (k, vv) in table {
                            out.insert(k.clone(), SignalType::try_from(vv)?);
                        }
                        Ok(out)
                    };
                match ty {
                    "array" => {
                        let fields = if let Some(f) = fields_map {
                            parse_fields(f)?
                        } else {
                            HashMap::new()
                        };
                        Ok(SignalType::Array { fields })
                    }
                    "object" | "map" => {
                        let fields = if let Some(f) = fields_map {
                            parse_fields(f)?
                        } else {
                            // No nested `fields` key -> treat all
                            // top-level entries (minus `type`) as the
                            // field map.
                            let mut tmp = toml::value::Table::new();
                            for (k, vv) in t {
                                if k == "type" {
                                    continue;
                                }
                                tmp.insert(k.clone(), vv.clone());
                            }
                            parse_fields(&tmp)?
                        };
                        Ok(SignalType::Object { fields })
                    }
                    other => Err(format!(
                        "unknown signal table type `{other}` (expected `array` or `object`)"
                    )),
                }
            }
            _ => Err(format!(
                "signal entry must be a type string or table, got {v:?}"
            )),
        }
    }
}

/// Errors returned by [`LumenToml::load_or_default`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `lumen.toml` exists but could not be read.
    #[error("read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    /// `lumen.toml` failed to parse as TOML.
    #[error("parse {0}: {1}")]
    Parse(PathBuf, toml::de::Error),
}

impl LumenToml {
    /// Loads `<dir>/lumen.toml` when present and returns the parsed config; returns [`Self::default`] when the file is absent.
    pub fn load_or_default(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join("lumen.toml");
        match std::fs::read_to_string(&path) {
            Ok(src) => toml::from_str::<Self>(&src).map_err(|e| ConfigError::Parse(path, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Read(path, e)),
        }
    }

    /// Returns `[asset_roots].paths` resolved against `dir`, leaving absolute entries unchanged and joining relative entries onto `dir`.
    pub fn resolved_asset_roots(&self, dir: &Path) -> Vec<PathBuf> {
        self.asset_roots
            .paths
            .as_ref()
            .map(|v| {
                v.iter()
                    .map(|p| {
                        let pp = Path::new(p);
                        if pp.is_absolute() {
                            pp.to_path_buf()
                        } else {
                            dir.join(pp)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default() {
        let tmp = std::env::temp_dir().join("lumenc-cfg-missing");
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg = LumenToml::load_or_default(&tmp).unwrap();
        assert!(cfg.app.entry.is_none());
        assert!(cfg.window.size.is_none());
    }

    #[test]
    fn signals_table_parses_scalars_and_nested() {
        let src = r#"
            [signals]
            count = "i64"
            theme = "string"
            ratio = "f64"
            dark  = "bool"
            tint  = "color"
            user  = { name = "string", email = "string" }

            [signals.users]
            type = "array"
            fields = { id = "i64", name = "string", email = "string" }
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        let sigs = &cfg.signals.fields;
        assert_eq!(sigs.get("count"), Some(&SignalType::I64));
        assert_eq!(sigs.get("theme"), Some(&SignalType::Str));
        assert_eq!(sigs.get("ratio"), Some(&SignalType::F64));
        assert_eq!(sigs.get("dark"), Some(&SignalType::Bool));
        assert_eq!(sigs.get("tint"), Some(&SignalType::Color));
        match sigs.get("user") {
            Some(SignalType::Object { fields }) => {
                assert_eq!(fields.get("name"), Some(&SignalType::Str));
                assert_eq!(fields.get("email"), Some(&SignalType::Str));
            }
            other => panic!("expected user=Object, got {other:?}"),
        }
        match sigs.get("users") {
            Some(SignalType::Array { fields }) => {
                assert_eq!(fields.get("id"), Some(&SignalType::I64));
                assert_eq!(fields.get("name"), Some(&SignalType::Str));
            }
            other => panic!("expected users=Array, got {other:?}"),
        }
    }

    #[test]
    fn signals_table_rejects_unknown_scalar() {
        let src = r#"
            [signals]
            count = "nope"
        "#;
        let res: Result<LumenToml, _> = toml::from_str(src);
        assert!(res.is_err());
    }

    #[test]
    fn script_engine_selection() {
        // Absent -> candela default.
        let cfg: LumenToml = toml::from_str("").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Candela);

        // Explicit lua.
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"lua\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Lua);

        // Explicit candela (case-insensitive).
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"candela\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Candela);
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"CANDELA\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Candela);

        // Case-insensitive; explicit rhai.
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"RHAI\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Rhai);

        // Unknown -> falls back to candela.
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"python\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Candela);
    }

    #[test]
    fn capabilities_infer_and_override() {
        let dir = std::env::temp_dir().join(format!(
            "lumen_caps_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The app's code, and so every marker the scan reads, is under `src/`.
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();

        // Bare UI app: http-fetch inferred OFF; mcp/async default OFF;
        // candela host (the default when no script file names a language), so
        // the feature list carries only that host.
        std::fs::write(
            src.join("main.lmn"),
            "<root><button id=\"inc\">+</button></root>",
        )
        .unwrap();
        let cfg = LumenToml::default();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(!caps.http_fetch && !caps.mcp && !caps.async_rt);
        assert_eq!(caps.hosts, vec![ScriptEngine::Candela]);
        assert_eq!(caps.to_features(), vec!["host-candela".to_string()]);

        // A fetch marker flips inference ON.
        std::fs::write(
            src.join("app.rhai"),
            "fn f(){ fetch(\"http://h\", \"t\"); }",
        )
        .unwrap();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(caps.http_fetch);
        let feats = caps.to_features();
        assert!(feats.contains(&"http-fetch".to_string()));

        // Explicit [capabilities] http-fetch = false overrides the marker.
        let mut cfg2 = LumenToml::default();
        cfg2.capabilities.http_fetch = Some(false);
        assert!(!BundleCapabilities::resolve(&dir, &cfg2).http_fetch);

        // A file-dialog builtin keeps the async runtime in the bundle: on
        // macOS it is the only path that opens a dialog at all.
        std::fs::write(src.join("dialogs.rhai"), "fn f(){ pick_file(\"import\"); }").unwrap();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(caps.async_rt);
        assert!(caps.to_features().contains(&"async".to_string()));

        // Explicit [capabilities] async = false still overrides the marker.
        let mut cfg_no_async = LumenToml::default();
        cfg_no_async.capabilities.async_rt = Some(false);
        assert!(!BundleCapabilities::resolve(&dir, &cfg_no_async).async_rt);
        std::fs::remove_file(src.join("dialogs.rhai")).unwrap();

        // A .lua file alongside the .rhai one needs both hosts compiled in,
        // and each host names its own feature.
        std::fs::write(src.join("logic.lua"), "-- lua").unwrap();
        let caps = BundleCapabilities::resolve(&dir, &LumenToml::default());
        assert_eq!(caps.hosts, vec![ScriptEngine::Lua, ScriptEngine::Rhai]);
        let feats = caps.to_features();
        assert!(feats.contains(&"host-lua".to_string()));
        assert!(feats.contains(&"host-rhai".to_string()));

        // Explicit [script] engine collapses the app onto one host.
        let mut cfg3 = LumenToml::default();
        cfg3.script.engine = Some("candela".into());
        assert_eq!(
            BundleCapabilities::resolve(&dir, &cfg3).hosts,
            vec![ScriptEngine::Candela]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependencies_table_parses_and_keeps_the_modules_capability() {
        use lumen_modules::ModuleSource;
        let src = r#"
            [dependencies]
            zeta = "1.2"
            alpha = { path = "modules/alpha", config = { units = "mm" } }
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        // Sorted by name: the table is unordered, so sorted order is the
        // documented load order.
        assert_eq!(cfg.dependencies.0.len(), 2);
        assert_eq!(cfg.dependencies.0[0].name, "alpha");
        assert_eq!(
            cfg.dependencies.0[0].source,
            ModuleSource::Path("modules/alpha".into())
        );
        assert_eq!(cfg.dependencies.0[1].name, "zeta");

        // Declared dependencies keep the loader in a bundle.
        let dir = std::env::temp_dir().join("lumen-deps-caps-test");
        std::fs::create_dir_all(&dir).unwrap();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(caps.modules);
        assert!(caps.to_features().contains(&"modules".to_string()));

        // No dependencies, no loader.
        let caps = BundleCapabilities::resolve(&dir, &LumenToml::default());
        assert!(!caps.modules);
        assert!(!caps.to_features().contains(&"modules".to_string()));
    }

    #[test]
    fn capabilities_table_parses() {
        let src = r#"
            [capabilities]
            http-fetch = true
            mcp = false
            async = true
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        assert_eq!(cfg.capabilities.http_fetch, Some(true));
        assert_eq!(cfg.capabilities.mcp, Some(false));
        assert_eq!(cfg.capabilities.async_rt, Some(true));
    }

    #[test]
    fn hooks_table_parses() {
        let src = r#"
            [[hooks]]
            when    = "prebuild"
            os      = "linux"
            run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
            inputs  = ["md.c"]
            outputs = ["libmd.so"]

            [[hooks]]
            when = "prerun"
            run  = "echo starting"
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        assert_eq!(cfg.hooks.len(), 2);
        let first = &cfg.hooks[0];
        assert_eq!(first.when, HookWhen::Prebuild);
        assert_eq!(first.os, Some(HookOs::Linux));
        assert_eq!(first.run, "cc -shared -fPIC -O2 -o libmd.so md.c");
        assert_eq!(first.inputs, vec!["md.c".to_string()]);
        assert_eq!(first.outputs, vec!["libmd.so".to_string()]);
        let second = &cfg.hooks[1];
        assert_eq!(second.when, HookWhen::Prerun);
        assert_eq!(second.os, None);
        assert!(second.inputs.is_empty());
        assert!(second.outputs.is_empty());
    }

    #[test]
    fn hooks_reject_unknown_when() {
        let src = r#"
            [[hooks]]
            when = "postbuild"
            run  = "true"
        "#;
        let err = toml::from_str::<LumenToml>(src).unwrap_err();
        assert!(err.to_string().contains("postbuild"));
    }

    #[test]
    fn hooks_reject_unknown_os() {
        let src = r#"
            [[hooks]]
            when = "prebuild"
            os   = "plan9"
            run  = "true"
        "#;
        let err = toml::from_str::<LumenToml>(src).unwrap_err();
        assert!(err.to_string().contains("plan9"));
    }

    #[test]
    fn hooks_reject_empty_run() {
        let src = r#"
            [[hooks]]
            when = "prebuild"
            run  = "   "
        "#;
        assert!(toml::from_str::<LumenToml>(src).is_err());
    }

    #[test]
    fn app_kind_override_parses() {
        use crate::app_kind::AppKind;
        // Absent -> None (auto-detect decides at runtime).
        let cfg: LumenToml = toml::from_str("[app]\nentry = \"main.lmn\"\n").unwrap();
        assert_eq!(cfg.app.kind, None);
        // Explicit overrides, one per variant (serde lowercase rename).
        let cfg: LumenToml = toml::from_str("[app]\nkind = \"rust\"\n").unwrap();
        assert_eq!(cfg.app.kind, Some(AppKind::Rust));
        let cfg: LumenToml = toml::from_str("[app]\nkind = \"cpp\"\n").unwrap();
        assert_eq!(cfg.app.kind, Some(AppKind::Cpp));
        let cfg: LumenToml = toml::from_str("[app]\nkind = \"python\"\n").unwrap();
        assert_eq!(cfg.app.kind, Some(AppKind::Python));
        let cfg: LumenToml = toml::from_str("[app]\nkind = \"markup\"\n").unwrap();
        assert_eq!(cfg.app.kind, Some(AppKind::Markup));
        // Unknown value is rejected (deny_unknown_fields-style strictness on
        // the enum discriminant).
        assert!(toml::from_str::<LumenToml>("[app]\nkind = \"go\"\n").is_err());
    }

    #[test]
    fn app_locale_parses_and_rejects_garbage() {
        // Absent -> follow the OS.
        let cfg: LumenToml = toml::from_str("[app]\nentry = \"main.lmn\"\n").unwrap();
        assert!(cfg.app.locale.is_none());
        // Valid BCP-47 lands parsed.
        let cfg: LumenToml = toml::from_str("[app]\nlocale = \"de-DE\"\n").unwrap();
        let locale = cfg.app.locale.expect("locale parsed");
        assert_eq!(locale.language.as_str(), "de");
        assert_eq!(
            locale.region.map(|r| r.as_str().to_string()),
            Some("DE".into())
        );
        // A typo names itself in the error.
        let err = toml::from_str::<LumenToml>("[app]\nlocale = \"not a tag\"\n").unwrap_err();
        assert!(err.to_string().contains("locale"), "{err}");
    }

    #[test]
    fn full_config_round_trips() {
        let src = r#"
            [app]
            entry = "alt.lmn"

            [window]
            title = "Demo"
            size = [1280, 720]

            [skin]
            name = "default"

            [mcp]
            port = 9090

            [profile]
            mode = "chrome"

            [asset_roots]
            paths = ["icons", "../shared"]
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        assert_eq!(cfg.app.entry.as_deref(), Some("alt.lmn"));
        assert_eq!(cfg.window.size, Some([1280, 720]));
        assert_eq!(cfg.skin.name.as_deref(), Some("default"));
        assert_eq!(cfg.mcp.port, Some(9090));
        assert_eq!(cfg.profile.mode.as_deref(), Some("chrome"));
        assert_eq!(cfg.asset_roots.paths.as_ref().unwrap().len(), 2);
    }
}
