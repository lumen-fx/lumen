//! The link kit: one target's link line, its inputs, and the modules on it.
//!
//! A link kit is what lets a machine with no Rust toolchain produce an
//! executable with the engine and a chosen set of runtime modules compiled
//! in. The release workflow builds the static launcher once per target with a
//! recorder in the linker's place (`tools/link-recorder`), keeps every file
//! that link read, and writes a [`Manifest`] describing the command that
//! produced the binary. Replaying that command with a module's object files
//! left out, or its register symbol forced in, is how one prebuilt kit turns
//! into any of the executables its modules can spell.
//!
//! A second kind of kit records the link of the engine's shared library
//! instead ([`ArtifactKind::SharedEngine`]): `liblumen_engine` on Linux and
//! macOS, `lumen.dll` on Windows. Replaying it with only the capabilities an
//! app uses, and with the export list cut down to what the files shipped
//! beside it resolve, gives each packaged app an engine of its own.
//!
//! Two things about the recorded line are not obvious and are why the
//! manifest is typed rather than a list of strings:
//!
//! - Some arguments name files that must travel with the kit (the rlibs and
//!   the temporary objects), some name directories that must not (the host's
//!   `/usr/lib`), and some name neither. A replay has to tell them apart to
//!   re-root the first kind and leave the third alone.
//! - A module's contribution to the line is a subset of it: its rlib, and the
//!   native libraries its own crate graph asked for. Dropping a module means
//!   dropping exactly that subset, which is what the `module` attribution on
//!   [`LinkArg::File`] and [`LinkArg::SysLib`] records.
//!
//! Everything here is data. Nothing in this module opens a file, and nothing
//! in the engine reads it: the producer is `lumenc link-kit emit` and the
//! consumer is a `lumenc` running on someone else's machine.
//!
//! The schema is versioned and pre-1.0, so it changes whenever a better shape
//! turns up; a kit whose [`Manifest::schema`] is not [`SCHEMA_VERSION`] is
//! refused rather than guessed at.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{REGISTER_PREFIX, entry_symbol};

/// The manifest version this build writes and accepts.
pub const SCHEMA_VERSION: u32 = 4;

/// One target's link kit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version, checked against [`SCHEMA_VERSION`] before anything
    /// else is read.
    pub schema: u32,
    /// Release-asset target name, for example `linux-x86_64`.
    pub target: String,
    /// The Rust target triple the recorded link was for.
    pub rust_triple: String,
    /// `rustc --version` of the toolchain that produced the inputs, for a
    /// report when a replay fails.
    pub rustc: String,
    /// The Lumen version the kit was built from. A kit and the app artifact
    /// it links are only one build together.
    pub lumen_version: String,
    /// The program that replays [`Manifest::args`].
    pub driver: Driver,
    /// The recorded link line, one entry per argument.
    pub args: Vec<LinkArg>,
    /// Every runtime module the kit can link in.
    pub modules: Vec<KitModule>,
    /// Every optional subsystem the kit can link in, and when an app gets
    /// each one.
    pub capabilities: Vec<KitCapability>,
    /// How the app's compiled artifact reaches the executable.
    pub artifact: Artifact,
    /// The engine build the recorded inputs are, as `lumen_engine_build_id`
    /// reports it. A shared-engine kit records it so a replay can check it is
    /// relinking the engine the rest of the toolchain was built against;
    /// absent where the recorded binary carries no engine build id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
}

/// What replays the line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Driver {
    /// The kind of program the arguments are written for.
    pub kind: DriverKind,
    /// The object format's dialect, named after LLD's flavors: `gnu`,
    /// `darwin`, or `link`. It is what says how a symbol is forced onto the
    /// line - `-u` for `gnu` and `darwin`, `/INCLUDE:` for `link`.
    pub flavor: String,
    /// Kit-relative path of the driver the kit ships, when it ships one.
    /// Absent means the driver is the consumer's own (`cc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The kind of program a replay runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// The platform's C compiler, which drives the system linker and
    /// contributes the C runtime startup files. The consumer's own.
    Cc,
    /// LLD, run directly. Shipped in the kit, because a Windows machine with
    /// no toolchain has no linker to borrow.
    Lld,
}

/// One argument of the recorded link line.
///
/// Every variant renders to exactly one argument. Where a flag and its value
/// share a token, the flag is the entry's `prefix` and the value is what the
/// consumer resolves; where they are two tokens, the flag is a
/// [`LinkArg::Lit`] of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkArg {
    /// Passed through as it was recorded.
    Lit {
        /// The argument.
        value: String,
    },
    /// Where the output path goes. The consumer substitutes its own.
    Out {
        /// Flag this value is joined to, empty when the flag is a separate
        /// argument.
        prefix: String,
    },
    /// A file the kit carries, at `path` under the kit's `stage` directory.
    File {
        /// Path relative to the kit's `stage` directory.
        path: String,
        /// The module this file belongs to, when it belongs to one. A replay
        /// that leaves that module out leaves this file out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        module: Option<String>,
    },
    /// A directory the linker searches.
    #[serde(rename = "sysdir")]
    SysDir {
        /// Flag this value is joined to, empty when the flag is a separate
        /// argument.
        prefix: String,
        /// Kit-relative when `staged`, otherwise a path on the machine that
        /// recorded the line, passed through for the consumer's own copy of
        /// the same system directory.
        path: String,
        /// Whether the kit carries this directory.
        staged: bool,
    },
    /// The list of symbols a library exports, which rustc writes for every
    /// library link: a version script for the GNU linkers, an exported-symbols
    /// list for ld64, a module-definition file for MSVC. The kit carries the
    /// recorded list; a replay either passes it on or puts its own in its
    /// place, which is what decides what the relinked library keeps.
    ExportList {
        /// The flag the path is joined to, such as `-Wl,--version-script=`.
        prefix: String,
        /// The recorded list, relative to the kit's `stage` directory.
        path: String,
    },
    /// A native library the linker resolves by name.
    #[serde(rename = "syslib")]
    SysLib {
        /// Flag this name is joined to: `-l` for the Unix drivers, empty for
        /// the MSVC one, which names libraries outright.
        prefix: String,
        /// The library's name, as the driver spells it.
        name: String,
        /// The module whose crate graph asked for this library, when the
        /// producer could attribute it. A replay that leaves that module out
        /// leaves this entry out, so the executable does not depend on a
        /// system library it makes no calls into.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        module: Option<String>,
    },
}

/// One runtime module a kit can link in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KitModule {
    /// The name an app declares the module under in `lumen.toml`.
    pub name: String,
    /// The symbol that forces the module onto the line. It is a module's
    /// registration entry, and the pre-main constructor that calls it sits in
    /// the same object file, so naming it is what pulls both out of the rlib.
    /// It is the only symbol a replay names: the module installs itself
    /// through the registry its constructor reaches, so the install entry is
    /// never called across this boundary.
    pub register_symbol: String,
}

impl KitModule {
    /// The entry for a module declared under `name`, spelled the way
    /// `lumen_module!` spelled it when the module was compiled.
    pub fn new(name: &str) -> KitModule {
        KitModule {
            name: name.to_string(),
            register_symbol: entry_symbol(REGISTER_PREFIX, name),
        }
    }
}

/// One optional subsystem a kit can link in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KitCapability {
    /// The name the app's `[capabilities]` table keys it by.
    pub name: String,
    /// The symbol that forces it onto the line: its registration entry, whose
    /// object also holds the pre-main constructor, exactly as a module's does.
    pub register_symbol: String,
    /// When an app that does not name it gets it anyway.
    pub select: KitSelect,
}

/// When a package carries a capability the app did not name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KitSelect {
    /// Every app.
    Always,
    /// An app whose sources mention any of these.
    OnUse(Vec<String>),
    /// No app; only a `[capabilities]` entry brings it in.
    OnRequest,
    /// An app whose `lumen.toml` sets `key` (a dotted path) to one of
    /// `any_of`, reading `default` when the key is absent.
    OnConfig {
        /// Dotted path of the key.
        key: String,
        /// The values that select the capability.
        any_of: Vec<String>,
        /// The value an absent key reads as.
        default: String,
    },
}

impl KitSelect {
    /// Whether an app with these sources and this `lumen.toml` gets the
    /// capability. `opaque` says the sources are not all the app runs, so
    /// every use rule answers yes.
    fn selects(&self, sources: &str, config: &toml::Table, opaque: bool) -> bool {
        match self {
            KitSelect::Always => true,
            KitSelect::OnUse(_) if opaque => true,
            KitSelect::OnUse(markers) => markers.iter().any(|m| sources.contains(m.as_str())),
            KitSelect::OnRequest => false,
            KitSelect::OnConfig {
                key,
                any_of,
                default,
            } => {
                let value = config_value(config, key).unwrap_or(default.as_str());
                any_of.iter().any(|v| v == value)
            }
        }
    }
}

/// The string at dotted path `key` in `config`, when there is one.
fn config_value<'a>(config: &'a toml::Table, key: &str) -> Option<&'a str> {
    let mut parts = key.split('.');
    let mut value = config.get(parts.next()?)?;
    for part in parts {
        value = value.as_table()?.get(part)?;
    }
    value.as_str()
}

impl From<&lumen_capability::Capability> for KitCapability {
    fn from(capability: &lumen_capability::Capability) -> Self {
        KitCapability {
            name: capability.name.to_string(),
            register_symbol: lumen_capability::register_symbol(capability.name),
            select: match capability.select {
                lumen_capability::Select::Always => KitSelect::Always,
                lumen_capability::Select::OnUse(markers) => {
                    KitSelect::OnUse(markers.iter().map(|m| m.to_string()).collect())
                }
                lumen_capability::Select::OnRequest => KitSelect::OnRequest,
                lumen_capability::Select::OnConfig {
                    key,
                    any_of,
                    default,
                } => KitSelect::OnConfig {
                    key: key.to_string(),
                    any_of: any_of.iter().map(|v| v.to_string()).collect(),
                    default: default.to_string(),
                },
            },
        }
    }
}

impl KitCapability {
    /// The markers its use rule scans sources for; empty for any other rule.
    pub fn markers(&self) -> &[String] {
        match &self.select {
            KitSelect::OnUse(markers) => markers,
            _ => &[],
        }
    }
}

/// The capabilities a package of one app carries, out of the ones a kit
/// offers.
///
/// `requested` is the app's `[capabilities]` table, and an entry there
/// settles that capability outright. Every other one follows its own rule
/// against `sources`, the app's markup, scripts, styles and config read into
/// one haystack, and `config`, the app's parsed `lumen.toml`. `opaque` says
/// the app runs code the scan cannot read (a C++ or Python program driving
/// the engine), so every use rule answers yes and only the table, a
/// request-only rule, or a config rule leaves a capability out. A requested
/// name the kit does not carry is an error naming what it does, so a
/// misspelling is caught rather than ignored.
pub fn select_capabilities<'a>(
    kit: &'a [KitCapability],
    sources: &str,
    config: &toml::Table,
    requested: &BTreeMap<String, bool>,
    opaque: bool,
) -> Result<Vec<&'a KitCapability>, String> {
    if let Some(unknown) = requested
        .keys()
        .find(|name| !kit.iter().any(|c| &c.name == *name))
    {
        let offered: Vec<&str> = kit.iter().map(|c| c.name.as_str()).collect();
        return Err(format!(
            "[capabilities] names '{unknown}', and this link kit carries no capability by \
             that name. It offers: {}.",
            if offered.is_empty() {
                "nothing".to_string()
            } else {
                offered.join(", ")
            }
        ));
    }
    Ok(kit
        .iter()
        .filter(|capability| match requested.get(&capability.name) {
            Some(wanted) => *wanted,
            None => capability.select.selects(sources, config, opaque),
        })
        .collect())
}

/// The capabilities `requested` turns off although `sources` mention their
/// builtins, each with the markers found. An app that says `os-tray = false`
/// and calls `tray_icon` packages without the tray, and the call does
/// nothing; this is what the package step warns about.
pub fn dropped_but_used<'a>(
    kit: &'a [KitCapability],
    sources: &str,
    requested: &BTreeMap<String, bool>,
) -> Vec<(&'a KitCapability, Vec<&'a str>)> {
    kit.iter()
        .filter(|capability| requested.get(&capability.name) == Some(&false))
        .filter_map(|capability| {
            let found: Vec<&str> = capability
                .markers()
                .iter()
                .map(String::as_str)
                .filter(|m| sources.contains(m))
                .collect();
            (!found.is_empty()).then_some((capability, found))
        })
        .collect()
}

/// How the app's compiled artifact reaches the executable a replay produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The mechanism this platform uses.
    pub kind: ArtifactKind,
}

/// How a replay's output carries the app: the two ways a launcher finds the
/// artifact it runs, and the shared engine, which carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Appended to the executable after the link, behind the launcher's
    /// footer magic.
    Append,
    /// Written into the executable by the link itself, as a Mach-O section.
    /// Appending is not available there: a signature has to cover the whole
    /// file, so anything added after the link invalidates it. The launcher
    /// names the segment and section it reads, so the manifest does not.
    MachoSection,
    /// The kit links the engine's shared library rather than an executable,
    /// and the app's artifact goes into the launcher beside it the way an
    /// unlinked package does. Nothing is appended and nothing is put on the
    /// line.
    SharedEngine,
}

/// One line of the link recorder's JSON Lines output.
///
/// `tools/link-recorder` writes these; the fields are the whole of what a
/// kit is built from. A build runs several links, so a reader picks the one
/// whose [`Record::out`] is the binary it wants.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The `-o` / `/OUT:` value of this link.
    #[serde(default)]
    pub out: Option<String>,
    /// The link's arguments, with every response file expanded.
    pub argv: Vec<String>,
    /// The same list with every argument that named a file replaced by the
    /// name it was staged under. An index where the two lists differ is a
    /// file the kit has to carry; an index where they agree is not.
    pub staged_argv: Vec<String>,
    /// The directory the link ran in, which is what a relative argument in it
    /// is relative to.
    #[serde(default)]
    pub cwd: String,
    /// The environment entries the line depends on.
    #[serde(default)]
    pub env: RecordEnv,
}

/// Environment the recorded line reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordEnv {
    /// `LIB`, the MSVC linker's library search path. Windows records need it:
    /// the line names the C runtime and the Windows SDK libraries by bare
    /// file name and resolves them through this variable.
    #[serde(rename = "LIB", default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        ArtifactKind, KitCapability, KitModule, KitSelect, LinkArg, Record, select_capabilities,
    };

    fn capability(name: &str, select: KitSelect) -> KitCapability {
        KitCapability {
            name: name.to_string(),
            register_symbol: lumen_capability::register_symbol(name),
            select,
        }
    }

    fn names(selected: &[&KitCapability]) -> Vec<String> {
        selected.iter().map(|c| c.name.clone()).collect()
    }

    /// Each rule against the sources: always in, in when mentioned, in only
    /// when asked.
    #[test]
    fn a_capability_follows_its_own_rule_when_the_app_does_not_name_it() {
        let kit = vec![
            capability("os-tray", KitSelect::OnUse(vec!["tray_icon".to_string()])),
            capability(
                "os-hotkey",
                KitSelect::OnUse(vec!["register_hotkey".to_string()]),
            ),
            capability("mcp", KitSelect::OnRequest),
            capability("core-ish", KitSelect::Always),
        ];
        let selected = select_capabilities(
            &kit,
            "fn on_start() { tray_icon(\"app\", \"icon.png\", \"\"); }",
            &toml::Table::new(),
            &BTreeMap::new(),
            false,
        )
        .expect("nothing was requested");
        assert_eq!(names(&selected), ["os-tray", "core-ish"]);
    }

    /// A `[capabilities]` entry wins over the rule, either way.
    #[test]
    fn a_requested_capability_is_settled_by_the_request() {
        let kit = vec![
            capability("os-tray", KitSelect::OnUse(vec!["tray_icon".to_string()])),
            capability("mcp", KitSelect::OnRequest),
        ];
        let requested = BTreeMap::from([("os-tray".to_string(), false), ("mcp".to_string(), true)]);
        let selected =
            select_capabilities(&kit, "tray_icon(", &toml::Table::new(), &requested, false)
                .expect("both are known");
        assert_eq!(names(&selected), ["mcp"]);
    }

    /// Sources the scan cannot read in full answer yes to every use rule; a
    /// request-only rule and the app's own table still decide.
    #[test]
    fn opaque_sources_keep_every_capability_a_use_rule_could_want() {
        let kit = vec![
            capability("os-tray", KitSelect::OnUse(vec!["tray_icon".to_string()])),
            capability("os-power", KitSelect::OnUse(vec!["keep_awake".to_string()])),
            capability("mcp", KitSelect::OnRequest),
        ];
        let requested = BTreeMap::from([("os-power".to_string(), false)]);
        let selected = select_capabilities(&kit, "", &toml::Table::new(), &requested, true)
            .expect("all known");
        assert_eq!(names(&selected), ["os-tray"]);
        let scanned = select_capabilities(&kit, "", &toml::Table::new(), &requested, false)
            .expect("all known");
        assert!(
            scanned.is_empty(),
            "nothing is mentioned: {:?}",
            names(&scanned)
        );
    }

    /// A capability the table turns off while the sources call into it is
    /// reported with the markers found, and only that one.
    #[test]
    fn a_capability_turned_off_while_used_is_reported_with_its_markers() {
        let kit = vec![
            capability(
                "os-tray",
                KitSelect::OnUse(vec!["tray_icon".to_string(), "tray_menu".to_string()]),
            ),
            capability("os-power", KitSelect::OnUse(vec!["keep_awake".to_string()])),
            capability("mcp", KitSelect::OnRequest),
        ];
        let requested = BTreeMap::from([
            ("os-tray".to_string(), false),
            ("os-power".to_string(), false),
            ("mcp".to_string(), false),
        ]);
        let reported =
            super::dropped_but_used(&kit, "tray_icon(\"id\", \"a.png\", \"\")", &requested);
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].0.name, "os-tray");
        assert_eq!(reported[0].1, ["tray_icon"]);
    }

    /// A name the kit does not carry is refused, naming what it does carry.
    #[test]
    fn a_requested_capability_the_kit_lacks_is_an_error_naming_the_offer() {
        let kit = vec![capability("os-tray", KitSelect::Always)];
        let requested = BTreeMap::from([("os-trey".to_string(), true)]);
        let error = select_capabilities(&kit, "", &toml::Table::new(), &requested, false)
            .expect_err("a misspelling");
        assert!(error.contains("'os-trey'"), "{error}");
        assert!(error.contains("os-tray"), "{error}");
    }

    /// Two interchangeable capabilities keyed on one config value: the value
    /// picks one, a value both answer to picks both, an absent key reads as
    /// the default, and a `[capabilities]` entry still wins.
    #[test]
    fn a_config_value_picks_between_capabilities() {
        let on = |any_of: &[&str]| KitSelect::OnConfig {
            key: "render.backend".to_string(),
            any_of: any_of.iter().map(|v| v.to_string()).collect(),
            default: "auto".to_string(),
        };
        let kit = vec![
            capability("render-gpu", on(&["gpu", "auto"])),
            capability("render-cpu", on(&["cpu", "auto"])),
        ];
        let pick = |toml: &str, requested: &[(&str, bool)]| {
            let config: toml::Table = toml::from_str(toml).expect("valid toml");
            let requested = requested
                .iter()
                .map(|(n, w)| (n.to_string(), *w))
                .collect::<BTreeMap<_, _>>();
            names(&select_capabilities(&kit, "", &config, &requested, false).expect("known"))
        };
        assert_eq!(pick("[render]\nbackend = \"cpu\"\n", &[]), ["render-cpu"]);
        assert_eq!(pick("[render]\nbackend = \"gpu\"\n", &[]), ["render-gpu"]);
        assert_eq!(pick("", &[]), ["render-gpu", "render-cpu"]);
        assert_eq!(
            pick("[render]\nbackend = \"auto\"\n", &[("render-gpu", false)]),
            ["render-cpu"]
        );
        // A key that is not a string reads as absent rather than matching.
        assert_eq!(
            pick("[render]\nbackend = 3\n", &[]),
            ["render-gpu", "render-cpu"]
        );
    }

    /// The registry's entry becomes the kit's, rule included.
    #[test]
    fn a_registered_capability_becomes_a_kit_entry() {
        fn nothing(_: &mut lumen_core::app::App, _: &lumen_capability::CapabilityEnv) {}
        let entry = KitCapability::from(&lumen_capability::Capability {
            name: "os-tray",
            phase: lumen_capability::Phase::Platform,
            install: nothing,
            preflight: None,
            select: lumen_capability::Select::OnUse(&["tray_icon"]),
            crate_name: "lumen-os-tray-capability",
        });
        assert_eq!(entry.register_symbol, "lumen_capability_register_os_tray");
        assert_eq!(
            entry.select,
            KitSelect::OnUse(vec!["tray_icon".to_string()])
        );
    }

    #[test]
    fn a_module_entry_spells_the_name_spaced_register_symbol() {
        let module = KitModule::new("lumen-audio");
        assert_eq!(module.register_symbol, "lumen_module_register_lumen_audio");
    }

    #[test]
    fn an_argument_round_trips_through_its_tag() {
        let args = vec![
            LinkArg::Lit {
                value: "-pie".to_string(),
            },
            LinkArg::Out {
                prefix: String::new(),
            },
            LinkArg::File {
                path: "aabbccdd-liblumen_fs.rlib".to_string(),
                module: Some("lumen-fs".to_string()),
            },
            LinkArg::SysDir {
                prefix: "-B".to_string(),
                path: "bin".to_string(),
                staged: true,
            },
            LinkArg::SysLib {
                prefix: "-l".to_string(),
                name: "asound".to_string(),
                module: None,
            },
            LinkArg::ExportList {
                prefix: "-Wl,--version-script=".to_string(),
                path: "eeff0011-list".to_string(),
            },
        ];
        let json = serde_json::to_string(&args).expect("the arguments encode");
        assert!(json.contains(r#"{"kind":"out","prefix":""}"#), "{json}");
        assert!(json.contains(r#""kind":"sysdir""#), "{json}");
        assert!(json.contains(r#""kind":"syslib""#), "{json}");
        assert!(json.contains(r#""kind":"export_list""#), "{json}");
        // An unattributed entry writes no `module` key at all, so a manifest
        // reads as the short list of what is attributed.
        assert!(!json.contains(r#""module":null"#), "{json}");
        assert_eq!(
            serde_json::from_str::<Vec<LinkArg>>(&json).expect("and decode"),
            args
        );
    }

    #[test]
    fn an_artifact_kind_is_spelled_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ArtifactKind::MachoSection).expect("encodes"),
            r#""macho_section""#
        );
        assert_eq!(
            serde_json::to_string(&ArtifactKind::SharedEngine).expect("encodes"),
            r#""shared_engine""#
        );
    }

    #[test]
    fn a_record_reads_without_the_windows_only_fields() {
        let record: Record = serde_json::from_str(
            r#"{"out":"app","argv":["-o","app"],"staged_argv":["-o","app"],"cwd":"/tmp"}"#,
        )
        .expect("the Unix recorder writes no LIB");
        assert_eq!(record.out.as_deref(), Some("app"));
        assert_eq!(record.env.lib, None);
    }
}
