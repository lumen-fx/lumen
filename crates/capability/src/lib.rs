//! Optional runtime subsystems, and the list they put themselves on.
//!
//! The run loop installs a fixed core (layout, input, text, the reactive
//! bindings) and then whatever optional subsystems the binary carries: a
//! tray icon host, the HTTP client, the introspection server, the devtools
//! overlay. It does not name any of them. Each one is a [`Capability`] that
//! its own crate registers before `main` through [`lumen_capability!`], and
//! the run loop reads the list at the [`Phase`] each entry asked for.
//!
//! That shape is what lets a link decide which subsystems an app carries.
//! The macro exports one symbol per capability, and a link that names the
//! symbol pulls the subsystem in while a link that does not leaves it out,
//! with no compiler in the loop; see the macro crate's docs for the
//! mechanics. A plain `cargo build` links every capability its graph holds,
//! so the default engine behaves as it always did.
//!
//! ```
//! use lumen_capability::{CapabilityEnv, Phase, lumen_capability};
//! use lumen_core::app::App;
//! use lumen_core::tick::TickStage;
//!
//! fn poll_beeper() {}
//!
//! fn install(app: &mut App, env: &CapabilityEnv) {
//!     // Skip the whole subsystem for an app that provably never beeps.
//!     if env.sources_mention(&["beep("]) {
//!         app.add_systems(TickStage::Systems, poll_beeper);
//!     }
//! }
//!
//! lumen_capability!("beeper", Phase::Platform, install);
//! ```
//!
//! An entry may also carry a preflight, run before the app is built, for a
//! subsystem that has to answer before any other work starts (a second
//! instance of a single-instance app exits from there).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lumen_core::app::App;
use serde::de::DeserializeOwned;

pub use lumen_capability_macros::lumen_capability;

/// The prefix of the register symbol every capability exports; the declared
/// name completes it. Naming it on a link line is what pulls a capability
/// out of an archive that nothing else references.
pub const REGISTER_PREFIX: &str = "lumen_capability_register_";

/// The register symbol of the capability called `name`: the prefix, then the
/// name with every character a symbol cannot carry replaced by `_`.
///
/// `lumen-capability-macros` spells the same name at the capability's
/// compile time; the two spellings are the contract between a capability and
/// a link that selects it.
pub fn register_symbol(name: &str) -> String {
    let mut symbol = String::with_capacity(REGISTER_PREFIX.len() + name.len());
    symbol.push_str(REGISTER_PREFIX);
    symbol.extend(
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }),
    );
    symbol
}

/// Where in the app build a capability is installed.
///
/// The variants are in build order. Within one phase, capabilities install
/// in name order, so the sequence is the same in every process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// After the core visual stack (layout, input, text editing, the
    /// interaction primitives) and before the reactive bindings. Host
    /// services live here: OS integration, the introspection server.
    Platform,
    /// After the command bus, before the app's runtime modules and script
    /// hosts load. A service a script host binds to at construction, such as
    /// the HTTP client behind `fetch()`, has to be in place by now.
    BeforeScripts,
    /// After the document is spawned, styled, and watched, and before the
    /// embedder's hooks run. An overlay that mounts into the built document
    /// belongs here.
    AfterBuild,
}

/// What a preflight decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// Build and run the app.
    Continue,
    /// Stop now, having done nothing else. The process exits successfully.
    Exit,
}

/// What a capability is installed with: the app's location, identity, and
/// configuration, the run mode, and a bounded view of its sources.
pub struct CapabilityEnv {
    /// The app directory.
    pub app_dir: PathBuf,
    /// The app's identity: `[app] id`, or the one derived from the
    /// directory name when the file leaves it unset.
    pub app_id: String,
    /// `[app] id` exactly as declared, `None` when the file leaves it unset.
    /// A subsystem whose platform keys state off an id the author chose
    /// (notification attribution, for one) reads this rather than the
    /// derived form.
    pub declared_app_id: Option<String>,
    /// The whole `lumen.toml`, parsed but untyped. A capability reads its own
    /// section with [`Self::section`]; nothing here knows which sections
    /// exist.
    pub config: toml::Table,
    /// No interactive session: a headless or bounded run, an automation
    /// driver, a test. A subsystem that exists for a person at a window
    /// stays idle.
    pub headless: bool,
    sources: String,
    opaque: bool,
    provided: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl CapabilityEnv {
    /// An environment for the app at `app_dir`.
    ///
    /// `sources` is a bounded concatenation of the app's markup, script and
    /// style files, which [`Self::sources_mention`] scans. `opaque` says
    /// that scan cannot see everything the app may do (a precompiled artifact
    /// with no readable source, an embedder's Rust hooks), in which case
    /// every mention query answers yes.
    pub fn new(
        app_dir: impl Into<PathBuf>,
        config: toml::Table,
        sources: String,
        opaque: bool,
    ) -> Self {
        let app_dir = app_dir.into();
        let declared_app_id = config
            .get("app")
            .and_then(|app| app.get("id"))
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let app_id = declared_app_id
            .clone()
            .unwrap_or_else(|| derive_app_id(&app_dir));
        Self {
            app_dir,
            app_id,
            declared_app_id,
            config,
            headless: false,
            sources,
            opaque,
            provided: HashMap::new(),
        }
    }

    /// Builder: mark the run as headless. See [`Self::headless`].
    pub fn headless(mut self, headless: bool) -> Self {
        self.headless = headless;
        self
    }

    /// Builder: declare the sources opaque, so every [`Self::sources_mention`]
    /// query answers yes. For a run the scan cannot see all of: an embedder's
    /// Rust hooks may drive any subsystem.
    pub fn opaque(mut self) -> Self {
        self.opaque = true;
        self
    }

    /// Hand a subsystem something the run has and this crate does not name:
    /// the markup front end a run from source was given, for one. Keyed by
    /// type; a second value of the same type replaces the first.
    pub fn provide<T: Any + Send + Sync>(&mut self, value: T) {
        self.provided.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// What [`Self::provide`] was handed under `T`, when anything was.
    pub fn provided<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.provided
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref::<T>())
    }

    /// Whether the app's sources contain any of `needles`.
    ///
    /// Errs toward yes: when the sources cannot be read in full, every
    /// query answers `true`, so a subsystem gated on this is installed
    /// whenever it might be used and skipped only when it provably is not.
    pub fn sources_mention(&self, needles: &[&str]) -> bool {
        self.opaque || needles.iter().any(|needle| self.sources.contains(needle))
    }

    /// The `[name]` table of `lumen.toml`, read into `T`.
    ///
    /// A missing table is `T::default()`. A table that does not fit `T` is
    /// reported and also read as the default, so one bad key disables the
    /// section rather than the app.
    pub fn section<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        let Some(table) = self.config.get(name) else {
            return T::default();
        };
        match table.clone().try_into() {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!("lumen.toml [{name}]: {e}; using the defaults");
                T::default()
            }
        }
    }
}

/// The app id a directory name gives: lowercased, every character that is
/// not a letter or digit replaced by `-`, `lumen-app` when nothing is left.
/// The same rule the runtime applies, spelled here so an environment built
/// with no runtime in the process derives the same id.
pub fn derive_app_id(dir: &Path) -> String {
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

/// One optional subsystem linked into the running binary.
#[derive(Debug, Clone, Copy)]
pub struct Capability {
    /// The name a link selects it by and a report lists it under.
    pub name: &'static str,
    /// When the run loop installs it.
    pub phase: Phase,
    /// Install it: insert resources, add plugins and systems.
    pub install: fn(&mut App, &CapabilityEnv),
    /// Run before the app is built, on an interactive launch only.
    pub preflight: Option<fn(&CapabilityEnv) -> Preflight>,
}

/// Every capability registered so far.
///
/// A `Mutex` rather than a lock-free list because registration happens once
/// per capability, before `main`, where a blocking lock has nobody to block
/// against; the cost lands nowhere a frame can see it.
static REGISTERED: Mutex<Vec<Capability>> = Mutex::new(Vec::new());

/// Add a capability to the list.
///
/// Called from a capability's pre-main constructor, so it must not panic on
/// a poisoned lock: a poisoned lock here would mean an earlier registration
/// panicked, and taking the list as it stands is better than aborting the
/// process before it starts. A name already on the list is left as it was.
pub fn register(capability: Capability) {
    let mut list = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    if list.iter().any(|c| c.name == capability.name) {
        return;
    }
    list.push(capability);
}

/// The registered capabilities, in install order: by phase, then by name.
/// Copied out so the caller holds no lock while it installs them.
pub fn registered() -> Vec<Capability> {
    let mut list = REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    list.sort_by(|a, b| a.phase.cmp(&b.phase).then_with(|| a.name.cmp(b.name)));
    list
}

/// Install every capability registered for `phase`, in name order.
pub fn install_phase(app: &mut App, env: &CapabilityEnv, phase: Phase) {
    for capability in registered().into_iter().filter(|c| c.phase == phase) {
        (capability.install)(app, env);
    }
}

/// Run every preflight, in install order. The first one that asks to exit
/// ends the walk.
pub fn preflight(env: &CapabilityEnv) -> Preflight {
    for capability in registered() {
        if let Some(preflight) = capability.preflight
            && preflight(env) == Preflight::Exit
        {
            return Preflight::Exit;
        }
    }
    Preflight::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing(_app: &mut App, _env: &CapabilityEnv) {}

    fn env(config: &str, sources: &str, opaque: bool) -> CapabilityEnv {
        CapabilityEnv::new(
            "/tmp/My App",
            toml::from_str(config).expect("valid toml"),
            sources.to_string(),
            opaque,
        )
    }

    #[test]
    fn the_list_is_ordered_by_phase_then_name() {
        register(Capability {
            name: "order-test-b",
            phase: Phase::Platform,
            install: nothing,
            preflight: None,
        });
        register(Capability {
            name: "order-test-a",
            phase: Phase::AfterBuild,
            install: nothing,
            preflight: None,
        });
        register(Capability {
            name: "order-test-0",
            phase: Phase::Platform,
            install: nothing,
            preflight: None,
        });
        let names: Vec<&str> = registered()
            .into_iter()
            .filter(|c| c.name.starts_with("order-test-"))
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["order-test-0", "order-test-b", "order-test-a"]);
    }

    #[test]
    fn a_name_registers_once() {
        register(Capability {
            name: "registry-test-twice",
            phase: Phase::Platform,
            install: nothing,
            preflight: None,
        });
        register(Capability {
            name: "registry-test-twice",
            phase: Phase::AfterBuild,
            install: nothing,
            preflight: None,
        });
        let entries: Vec<Capability> = registered()
            .into_iter()
            .filter(|c| c.name == "registry-test-twice")
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].phase, Phase::Platform);
    }

    #[test]
    fn the_register_symbol_matches_the_macro_spelling() {
        assert_eq!(
            register_symbol("os-tray"),
            "lumen_capability_register_os_tray"
        );
        assert_eq!(
            register_symbol("http.fetch+2"),
            "lumen_capability_register_http_fetch_2"
        );
    }

    #[test]
    fn the_app_id_comes_from_the_file_then_the_directory() {
        let declared = env("[app]\nid = \"com.acme.notes\"\n", "", false);
        assert_eq!(declared.app_id, "com.acme.notes");
        assert_eq!(declared.declared_app_id.as_deref(), Some("com.acme.notes"));
        let derived = env("", "", false);
        assert_eq!(derived.app_id, "my-app");
        assert_eq!(derived.declared_app_id, None);
    }

    #[test]
    fn a_mention_is_found_in_the_sources_or_assumed_when_they_are_opaque() {
        let plain = env(
            "",
            "fn f() { register_hotkey(\"Ctrl+S\", \"save\"); }",
            false,
        );
        assert!(plain.sources_mention(&["register_hotkey"]));
        assert!(!plain.sources_mention(&["pick_file"]));
        let opaque = env("", "", true);
        assert!(opaque.sources_mention(&["pick_file"]));
    }

    #[test]
    fn a_provided_value_is_read_back_by_type() {
        let mut env = env("", "", false);
        assert!(env.provided::<String>().is_none());
        env.provide(String::from("front end"));
        assert_eq!(
            env.provided::<String>().map(String::as_str),
            Some("front end")
        );
        assert!(env.provided::<u32>().is_none());
    }

    #[test]
    fn a_section_reads_its_table_and_defaults_when_absent_or_wrong() {
        #[derive(Default, serde::Deserialize, PartialEq, Debug)]
        struct Mcp {
            port: Option<u16>,
            simulate: Option<bool>,
        }
        let present = env("[mcp]\nport = 7000\nsimulate = true\n", "", false);
        assert_eq!(
            present.section::<Mcp>("mcp"),
            Mcp {
                port: Some(7000),
                simulate: Some(true)
            }
        );
        let absent = env("", "", false);
        assert_eq!(absent.section::<Mcp>("mcp"), Mcp::default());
        let wrong = env("[mcp]\nport = \"seven\"\n", "", false);
        assert_eq!(wrong.section::<Mcp>("mcp"), Mcp::default());
    }
}
