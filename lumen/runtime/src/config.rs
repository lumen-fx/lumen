//! Per-app `lumen.toml` config loader.
//!
//! - Reads an optional file at `<app-dir>/lumen.toml`.
//! - Sets runtime defaults; CLI flags supplied on top take precedence.
//! - Unknown top-level keys are rejected; section fields default when absent.
//!
//! ```toml
//! [app]
//! entry = "main.lmn"          # default
//!
//! [window]
//! title = "My App"            # default: app directory name
//! size  = [1280, 720]         # default: [960, 720]
//!
//! [skin]
//! name = "default"            # default: none (bare framework)
//!
//! [mcp]
//! port = 7878                 # default: env LUMEN_MCP_PORT or off
//!
//! [profile]
//! mode = "off"                # off | chrome | stderr
//!
//! [asset_roots]
//! paths = ["icons", "../shared"]   # extra dirs scanned for relative src=
//! ```
//!
//! Parse failures surface as [`ConfigError`]; the caller decides whether to abort or fall back to defaults.

use serde::Deserialize;
use std::collections::HashMap;
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
    /// `[runtime]` section - subsystem init overrides (audio / MCP /
    /// hot-reload / thread budget).
    pub runtime: RuntimeCfg,
    /// `[capabilities]` section - per-app COMPILE-TIME subsystem trim toggles
    /// for the static `--bundle` build (Part B tree-shaking).
    pub capabilities: CapabilitiesCfg,
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
    /// `"python"`). OPTIONAL and used ONLY to override auto-detection: when
    /// absent, `crate::app_kind::detect` inspects the directory; when present,
    /// it wins. Lets an ambiguous directory (e.g. a Rust workspace member that
    /// should still run as pure markup) pin its build/run route.
    pub kind: Option<crate::app_kind::AppKind>,
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
///                                   #   on when >1 .lmn file is present)
/// include = ["index.lmn", "settings.lmn", "user.lmn"]  # explicit page set,
///                                   #   overriding directory auto-discovery
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PagesCfg {
    /// Home/entry page key (filename stem, no `.lmn`). Defaults to `index`
    /// when an `index.lmn` exists, else the `[app] entry` stem, else `main`.
    pub entry: Option<String>,
    /// Force multi-page mode on (`true`) or off (`false`). When absent,
    /// multi-page activates automatically iff more than one page file is
    /// present - so single-page apps (`main.lmn` / a lone `index.lmn`) keep
    /// the exact legacy single-file load path.
    pub enabled: Option<bool>,
    /// Explicit ordered page-file list (relative filenames). When set, this
    /// overrides directory auto-discovery - only these files are pages.
    pub include: Option<Vec<String>>,
}

/// `[window]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowCfg {
    /// Window title override.
    pub title: Option<String>,
    /// `[w, h]` in physical pixels.
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
    /// TCP port for the introspection server; `None` disables the server.
    pub port: Option<u16>,
    /// When `true`, the MCP plugin drains the `SimulateQueue` each tick and injects pointer, key, and scroll events. Defaults to off.
    pub simulate: Option<bool>,
}

/// `[profile]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileCfg {
    /// `off` | `chrome` | `stderr`. Default `off`.
    pub mode: Option<String>,
}

/// `[asset_roots]` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssetRootsCfg {
    /// Extra directories (relative to the app dir, or absolute).
    pub paths: Option<Vec<String>>,
}

/// `[script]` block - selects which `ScriptHost` engine runs the app's
/// scripts. Rhai is the default/compat host; `lua` selects the
/// `lumen-script-lua` host (mlua / Lua 5.4), which exposes the same
/// engine-function surface.
///
/// ```toml
/// [script]
/// engine = "lua"   # "rhai" (default) | "lua" | "candela"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptCfg {
    /// Engine name: `"rhai"` (default), `"lua"`, or `"candela"`. Unknown values
    /// fall back to Rhai via [`ScriptCfg::engine_kind`].
    pub engine: Option<String>,
}

/// The resolved script engine for an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptEngine {
    /// Rhai (`lumen-script-rhai`) - the default/compat host.
    #[default]
    Rhai,
    /// Lua 5.4 (`lumen-script-lua`).
    Lua,
    /// candela (`lumen-script-candela`) - the intended default Lumen language.
    Candela,
}

impl ScriptCfg {
    /// Resolve the declared engine name, defaulting to [`ScriptEngine::Rhai`]
    /// when absent or unrecognised (case-insensitive match on `rhai` /
    /// `lua` / `candela`).
    pub fn engine_kind(&self) -> ScriptEngine {
        match self.engine.as_deref().map(str::trim) {
            Some(e) if e.eq_ignore_ascii_case("lua") => ScriptEngine::Lua,
            Some(e) if e.eq_ignore_ascii_case("candela") => ScriptEngine::Candela,
            _ => ScriptEngine::Rhai,
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
/// automatic behaviour (usage-detection for audio; run-mode gating for MCP /
/// hot-reload; `min(cores, 4)` for threads), while `Some(v)` forces it.
///
/// ```toml
/// [runtime]
/// audio = true        # force the audio subsystem on (or false = off)
/// mcp = false         # force the MCP server off (or true = on)
/// hot_reload = false  # force the source watcher off (or true = on)
/// threads = 2         # bevy_ecs worker-thread budget
/// ```
///
/// Full compile-time crate exclusion (dropping the rodio / asset-decode
/// code from the binary entirely) is deferred until the lumenc/runtime
/// crate split; these knobs gate *initialization*, not linkage.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeCfg {
    /// Force the audio subsystem (rodio `OutputStream` + position ticker
    /// thread) on/off. `None` = auto-detect from app usage.
    pub audio: Option<bool>,
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

/// `[capabilities]` block -- per-app COMPILE-TIME subsystem trim toggles for
/// the static `--bundle` build (Part B tree-shaking). Unlike `[runtime]` (which
/// gates *initialization* in the always-full shared runtime), these select the
/// cargo FEATURE set lumenc compiles the per-app static seam with, so an unused
/// subsystem's crate is dropped from the binary entirely.
///
/// Every field is `Option<bool>`: `None` lets lumenc INFER the capability from a
/// bounded source scan (err toward ON -- see [`BundleCapabilities::resolve`]);
/// `Some(v)` forces it. Ignored by the shared dlopen'd cdylib and the dev
/// `lumenc run` path, which always ship every subsystem.
///
/// ```toml
/// [capabilities]
/// audio = false           # default inferred; explicit wins
/// http-fetch = false
/// mcp = false
/// async = false
/// ```
///
/// The one compiled script host is selected by `[script] engine` (or inferred
/// from the app's script file extensions), not here.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilitiesCfg {
    /// Force the audio subsystem (rodio/cpal/symphonia) into/out of the bundle.
    /// `None` = infer from `audio_*` builtins / audio-file markers.
    pub audio: Option<bool>,
    /// Force the MCP introspection server into/out of the bundle. `None` = OFF
    /// (MCP is a dev/introspection capability, never inferred into a release
    /// bundle).
    pub mcp: Option<bool>,
    /// Force the async (tokio) bridge into/out of the bundle. `None` = OFF (the
    /// markup runtime never installs `AsyncTokioPlugin` itself).
    #[serde(rename = "async")]
    pub async_rt: Option<bool>,
    /// Force the scripts' HTTP `fetch()` builtin (ureq + rustls + ring)
    /// into/out of the bundle. `None` = infer from a `fetch(` marker.
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
    /// Audio subsystem present.
    pub audio: bool,
    /// MCP introspection server present.
    pub mcp: bool,
    /// Async (tokio) bridge present.
    pub async_rt: bool,
    /// Scripts' HTTP `fetch()` builtin present.
    pub http_fetch: bool,
    /// The single compiled script host.
    pub host: ScriptEngine,
}

impl BundleCapabilities {
    /// Resolve the capability set for the app rooted at `dir` from `cfg` plus a
    /// bounded source scan. Explicit `[capabilities]` wins, then legacy
    /// `[runtime] audio`, then inference.
    pub fn resolve(dir: &Path, cfg: &LumenToml) -> Self {
        let hay = crate::run::subsystems::scan_app_sources(dir);

        // Audio: explicit [capabilities] audio, else legacy [runtime] audio,
        // else the same marker scan the runtime startup gate uses.
        let audio = cfg
            .capabilities
            .audio
            .or(cfg.runtime.audio)
            .unwrap_or_else(|| crate::run::subsystems::audio_markers_present(&hay));

        // MCP + async: dev/embedder-only capabilities. Never inferred ON for a
        // release bundle; only an explicit toggle pulls them in.
        let mcp = cfg.capabilities.mcp.unwrap_or(false);
        let async_rt = cfg.capabilities.async_rt.unwrap_or(false);

        // HTTP fetch: explicit, else infer from a `fetch(` builtin marker.
        let http_fetch = cfg
            .capabilities
            .http_fetch
            .unwrap_or_else(|| hay.contains("fetch("));

        let host = infer_script_host(dir, cfg);

        Self {
            audio,
            mcp,
            async_rt,
            http_fetch,
            host,
        }
    }

    /// The cargo `--features` list to compile the trimmed runtime seam with,
    /// passed after `--no-default-features`. `runtime-parse` is intentionally
    /// omitted: a `--bundle` runs from a precompiled AOT artifact, so the
    /// source parser is dropped too. Rhai (the always-compiled default host)
    /// contributes no feature.
    pub fn to_features(&self) -> Vec<String> {
        let mut f: Vec<String> = Vec::new();
        if self.audio {
            f.push("audio".into());
        }
        if self.mcp {
            f.push("mcp".into());
        }
        if self.async_rt {
            f.push("async".into());
        }
        if self.http_fetch {
            f.push("http-fetch".into());
        }
        match self.host {
            ScriptEngine::Rhai => {}
            ScriptEngine::Lua => f.push("host-lua".into()),
            ScriptEngine::Candela => f.push("host-candela".into()),
        }
        f
    }
}

/// Select the single script host to compile into a bundle: explicit
/// `[script] engine` wins; otherwise infer from the app's script file
/// extensions (a `.lua` file -> Lua, a `.cdl` file -> Candela), defaulting to
/// the always-compiled Rhai host.
fn infer_script_host(dir: &Path, cfg: &LumenToml) -> ScriptEngine {
    if cfg.script.engine.is_some() {
        return cfg.script.engine_kind();
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten().take(512) {
            match entry.path().extension().and_then(|e| e.to_str()) {
                Some("lua") => return ScriptEngine::Lua,
                Some("cdl") => return ScriptEngine::Candela,
                _ => {}
            }
        }
    }
    ScriptEngine::Rhai
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
    /// Free-form string (legacy untyped writes).
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
        // Absent -> Rhai default.
        let cfg: LumenToml = toml::from_str("").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Rhai);

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

        // Unknown -> falls back to Rhai.
        let cfg: LumenToml = toml::from_str("[script]\nengine = \"python\"\n").unwrap();
        assert_eq!(cfg.script.engine_kind(), ScriptEngine::Rhai);
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
        std::fs::create_dir_all(&dir).unwrap();

        // Bare UI app: audio + http-fetch inferred OFF; mcp/async default OFF;
        // Rhai host. Feature list is empty (all always-on or off).
        std::fs::write(
            dir.join("main.lmn"),
            "<root><button on_click=\"inc\">+</button></root>",
        )
        .unwrap();
        let cfg = LumenToml::default();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(!caps.audio && !caps.http_fetch && !caps.mcp && !caps.async_rt);
        assert_eq!(caps.host, ScriptEngine::Rhai);
        assert!(caps.to_features().is_empty());

        // Audio + fetch markers flip inference ON.
        std::fs::write(
            dir.join("app.rhai"),
            "fn f(){ audio_play(\"x.wav\"); fetch(\"http://h\", \"t\"); }",
        )
        .unwrap();
        let caps = BundleCapabilities::resolve(&dir, &cfg);
        assert!(caps.audio && caps.http_fetch);
        let feats = caps.to_features();
        assert!(feats.contains(&"audio".to_string()));
        assert!(feats.contains(&"http-fetch".to_string()));

        // Explicit [capabilities] audio = false overrides the marker.
        let mut cfg2 = LumenToml::default();
        cfg2.capabilities.audio = Some(false);
        assert!(!BundleCapabilities::resolve(&dir, &cfg2).audio);

        // A .lua file infers the lua host -> host-lua feature.
        std::fs::write(dir.join("logic.lua"), "-- lua").unwrap();
        let caps = BundleCapabilities::resolve(&dir, &LumenToml::default());
        assert_eq!(caps.host, ScriptEngine::Lua);
        assert!(caps.to_features().contains(&"host-lua".to_string()));

        // Explicit [script] engine wins over extension inference.
        let mut cfg3 = LumenToml::default();
        cfg3.script.engine = Some("candela".into());
        assert_eq!(
            BundleCapabilities::resolve(&dir, &cfg3).host,
            ScriptEngine::Candela
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capabilities_table_parses() {
        let src = r#"
            [capabilities]
            audio = false
            http-fetch = true
            mcp = false
            async = true
        "#;
        let cfg: LumenToml = toml::from_str(src).unwrap();
        assert_eq!(cfg.capabilities.audio, Some(false));
        assert_eq!(cfg.capabilities.http_fetch, Some(true));
        assert_eq!(cfg.capabilities.mcp, Some(false));
        assert_eq!(cfg.capabilities.async_rt, Some(true));
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
