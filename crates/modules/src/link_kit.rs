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

use serde::{Deserialize, Serialize};

use crate::{REGISTER_PREFIX, entry_symbol};

/// The manifest version this build writes and accepts.
pub const SCHEMA_VERSION: u32 = 1;

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
    /// How the app's compiled artifact reaches the executable.
    pub artifact: Artifact,
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

/// How the app's compiled artifact reaches the executable a replay produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The mechanism this platform uses.
    pub kind: ArtifactKind,
}

/// The two ways a launcher finds the artifact it runs.
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
    use super::{ArtifactKind, KitModule, LinkArg, Record};

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
        ];
        let json = serde_json::to_string(&args).expect("the arguments encode");
        assert!(json.contains(r#"{"kind":"out","prefix":""}"#), "{json}");
        assert!(json.contains(r#""kind":"sysdir""#), "{json}");
        assert!(json.contains(r#""kind":"syslib""#), "{json}");
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
