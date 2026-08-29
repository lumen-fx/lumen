//! `lumenc link-kit emit` - turn a recorded link into a shippable kit.
//!
//! The release workflow builds the static launcher with `tools/link-recorder`
//! in the linker's place. That leaves two things behind: a JSON Lines record
//! of every link the build ran, and a stage directory holding a copy of every
//! file those links read. This subcommand picks the launcher's link out of the
//! record, classifies its arguments, copies what the kit has to carry into one
//! directory, and writes `manifest.json` describing how to replay it.
//!
//! It is not in `lumenc --help`. Nobody runs it by hand: it is one step of the
//! release workflow, and the kit it writes is a release asset that a later
//! `lumenc` consumes.
//!
//! Two arguments of the recorded line are decided here rather than by the
//! consumer, because they are properties of the kit and not choices:
//!
//! - `symbols.o` is left out. rustc writes it to hold an undefined reference
//!   to every symbol the binary exports, which is how it keeps them alive
//!   through the link. Every module's registration symbol is in that list, so
//!   a line that kept it would link every module the kit carries and there
//!   would be nothing to select. What the launcher itself calls, it references
//!   directly, so the file is dead weight once selection is the point.
//! - The toolchain's own choice of linker is left off a line a `cc` driver
//!   replays. rustc points `cc` at the LLD inside the Rust installation with
//!   `-fuse-ld=lld` and a `-B` prefix, and that LLD is a wrapper around a
//!   binary that in turn loads the toolchain's shared LLVM - so carrying it
//!   would mean carrying most of a Rust installation. A machine that has a
//!   `cc` has a linker behind it, and the recorded line asks nothing of a
//!   linker that only LLD can do.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lumen_modules::link_kit::{
    Artifact, ArtifactKind, Driver, DriverKind, KitModule, LinkArg, Manifest, Record,
    SCHEMA_VERSION,
};

use crate::package_cli::Target;

const USAGE: &str = "lumenc link-kit emit - write a link kit from a recorded link

USAGE:
    lumenc link-kit emit --record <file> --stage <dir> --out <dir>
                         --target <name> --target-dir <dir>
                         [--binary <stem>] [--rustc <version>]
                         [--driver-path <file>]
                         [--module <name>=<lib>]...
                         [--module-libs <name>=<lib>,<lib>]...

    --record FILE     The recorder's JSON Lines output.
    --stage DIR       The directory the recorder staged link inputs into.
    --out DIR         Where to write the kit. Created if absent.
    --target NAME     Release-asset target name, e.g. linux-x86_64.
    --target-dir DIR  The build's cargo target directory, as an absolute
                      path. A library search path under it was produced by
                      the build and travels with the kit; one outside it
                      belongs to the machine and does not.
    --binary STEM     File stem of the recorded binary to build the kit from
                      (default: lumen_launcher).
    --rustc VERSION   `rustc --version` of the toolchain that built the
                      inputs, recorded for a replay that fails.
    --driver-path F   A linker to ship in the kit and replay through, for a
                      platform whose consumers have none.
    --module N=LIB    A runtime module the kit carries: the name an app
                      declares it under, and its cargo library name, which
                      is what its rlib is called. Repeatable.
    --module-libs N=A,B
                      Native libraries only that module's crate graph asked
                      for, so a replay without the module drops them too.
                      Repeatable.";

/// Entry: `lumenc link-kit <subcommand>`.
pub fn cmd_link_kit(args: impl Iterator<Item = String>) -> ExitCode {
    let mut args = args.peekable();
    match args.next().as_deref() {
        Some("emit") => match emit(args) {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("lumenc link-kit emit: {e}");
                ExitCode::from(1)
            }
        },
        Some(h) if crate::is_help_flag(h) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("lumenc link-kit: unknown subcommand `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// What the caller asked for, once the flags are read.
struct Options {
    record: PathBuf,
    stage: PathBuf,
    out: PathBuf,
    target: Target,
    target_dir: PathBuf,
    binary: String,
    rustc: String,
    driver_path: Option<PathBuf>,
    /// Declared module name -> the cargo library name its rlib carries.
    modules: BTreeMap<String, String>,
    /// Declared module name -> the native libraries only it asks for.
    module_libs: BTreeMap<String, Vec<String>>,
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut record = None;
    let mut stage = None;
    let mut out = None;
    let mut target = None;
    let mut target_dir = None;
    let mut binary = "lumen_launcher".to_string();
    let mut rustc = "unknown".to_string();
    let mut driver_path = None;
    let mut modules = BTreeMap::new();
    let mut module_libs: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--record" => record = Some(PathBuf::from(value()?)),
            "--stage" => stage = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--target" => {
                let name = value()?;
                target = Some(
                    Target::parse(&name)
                        .ok_or_else(|| format!("no release target is named `{name}`"))?,
                );
            }
            "--target-dir" => target_dir = Some(PathBuf::from(value()?)),
            "--binary" => binary = value()?,
            "--rustc" => rustc = value()?,
            "--driver-path" => driver_path = Some(PathBuf::from(value()?)),
            "--module" => {
                let entry = value()?;
                let (name, lib) = entry
                    .split_once('=')
                    .ok_or_else(|| format!("--module takes <name>=<lib>, not `{entry}`"))?;
                modules.insert(name.to_string(), lib.to_string());
            }
            "--module-libs" => {
                let entry = value()?;
                let (name, libs) = entry.split_once('=').ok_or_else(|| {
                    format!("--module-libs takes <name>=<lib>,<lib>, not `{entry}`")
                })?;
                module_libs.insert(
                    name.to_string(),
                    libs.split(',')
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect(),
                );
            }
            other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
        }
    }

    let missing = |what: &str| format!("{what} is required\n\n{USAGE}");
    Ok(Options {
        record: record.ok_or_else(|| missing("--record"))?,
        stage: stage.ok_or_else(|| missing("--stage"))?,
        out: out.ok_or_else(|| missing("--out"))?,
        target: target.ok_or_else(|| missing("--target"))?,
        target_dir: target_dir.ok_or_else(|| missing("--target-dir"))?,
        binary,
        rustc,
        driver_path,
        modules,
        module_libs,
    })
}

/// Read the record, classify the line, and write the kit.
fn emit(args: impl Iterator<Item = String>) -> Result<String, String> {
    let options = parse_options(args)?;
    let text = fs::read_to_string(&options.record)
        .map_err(|e| format!("cannot read {}: {e}", options.record.display()))?;
    let mut records = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str::<Record>(line)
                .map_err(|e| format!("{}:{}: {e}", options.record.display(), n + 1))?,
        );
    }
    let record = pick(&records, &options.binary).ok_or_else(|| {
        format!(
            "no link in {} produced a binary named `{}` ({} links recorded)",
            options.record.display(),
            options.binary,
            records.len()
        )
    })?;

    let kit = classify(record, &options)?;
    write_kit(&kit, &options)?;
    Ok(format!(
        "wrote {} for {}: {} arguments, {} staged files, {} modules",
        options.out.join("manifest.json").display(),
        options.target.linkkit_archive_name(),
        kit.args.len(),
        kit.staged.len(),
        kit.modules.len(),
    ))
}

/// The last link whose output file is named `stem`.
///
/// A build links several binaries and cargo writes each one under a hashed
/// name in `deps/`, so the stem is what identifies one; the last match is the
/// one the build finished with.
fn pick<'a>(records: &'a [Record], stem: &str) -> Option<&'a Record> {
    records.iter().rev().find(|r| {
        r.out.as_deref().is_some_and(|out| {
            Path::new(out)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| {
                    n == stem || n.starts_with(&format!("{stem}-")) || n == format!("{stem}.exe")
                })
        })
    })
}

/// The classified line plus everything the kit has to carry with it.
#[derive(Debug)]
struct Kit {
    args: Vec<LinkArg>,
    /// Staged file names this line reads, in the order they were met.
    staged: Vec<String>,
    /// Search directories copied into the kit, kit-relative path -> source.
    libdirs: BTreeMap<String, PathBuf>,
    modules: Vec<KitModule>,
}

/// Turn the recorded arguments into manifest entries.
fn classify(record: &Record, options: &Options) -> Result<Kit, String> {
    if record.argv.len() != record.staged_argv.len() {
        return Err("the record's two argument lists disagree in length".to_string());
    }
    let msvc = options.target.rust_triple().ends_with("-msvc");
    // The native libraries each module alone contributes, inverted so one
    // lookup answers "which module asked for this".
    let mut lib_owner = BTreeMap::new();
    for (module, libs) in &options.module_libs {
        for lib in libs {
            lib_owner.insert(lib.clone(), module.clone());
        }
    }

    let mut kit = Kit {
        args: Vec::with_capacity(record.argv.len()),
        staged: Vec::new(),
        libdirs: BTreeMap::new(),
        modules: options
            .modules
            .keys()
            .map(String::as_str)
            .map(KitModule::new)
            .collect(),
    };

    let mut i = 0;
    while i < record.argv.len() {
        let arg = &record.argv[i];
        let staged = &record.staged_argv[i];
        i += 1;
        let mut take_next = || {
            let next = record.argv.get(i).cloned();
            if next.is_some() {
                i += 1;
            }
            next
        };

        // The output, in both spellings. `-o` and its value are two
        // arguments; MSVC joins them into one.
        if arg == "-o" {
            if take_next().is_none() {
                return Err("the recorded line ends after -o".to_string());
            }
            kit.args.push(LinkArg::Lit { value: arg.clone() });
            kit.args.push(LinkArg::Out {
                prefix: String::new(),
            });
            continue;
        }
        if let Some(prefix) = out_prefix(arg) {
            kit.args.push(LinkArg::Out { prefix });
            continue;
        }

        // Search paths, in the three spellings a driver uses.
        if arg == "-L" || arg == "-B" {
            let Some(dir) = take_next() else {
                return Err(format!("the recorded line ends after {arg}"));
            };
            if arg == "-L" {
                kit.args.push(LinkArg::Lit { value: arg.clone() });
                let entry = search_dir(String::new(), &dir, options, &mut kit);
                kit.args.push(entry);
            } else if let Some(entry) = prefix_dir("-B", &dir, options, &mut kit) {
                kit.args.push(entry);
            }
            continue;
        }
        if let Some(dir) = arg.strip_prefix("-L") {
            let entry = search_dir("-L".to_string(), dir, options, &mut kit);
            kit.args.push(entry);
            continue;
        }
        if let Some(dir) = arg.strip_prefix("-B") {
            if let Some(entry) = prefix_dir("-B", dir, options, &mut kit) {
                kit.args.push(entry);
            }
            continue;
        }
        // The other half of the toolchain's linker choice, dropped with the
        // prefix that pointed at it.
        if arg.starts_with("-fuse-ld=") {
            continue;
        }
        // The MSVC linker takes either lead character on every flag.
        if msvc
            && let Some(dir) =
                strip_prefix_fold(arg, "/LIBPATH:").or_else(|| strip_prefix_fold(arg, "-LIBPATH:"))
        {
            let prefix = arg[..arg.len() - dir.len()].to_string();
            let entry = search_dir(prefix, dir, options, &mut kit);
            kit.args.push(entry);
            continue;
        }

        // A file the recorder staged. The two lists agree everywhere else.
        if staged != arg {
            // See the module docs: the line is replayed without it.
            if staged
                .rsplit_once('-')
                .is_some_and(|(_, n)| n == "symbols.o")
            {
                continue;
            }
            kit.staged.push(staged.clone());
            kit.args.push(LinkArg::File {
                path: staged.clone(),
                module: owning_module(staged, &options.modules),
            });
            continue;
        }

        // Native libraries.
        if let Some(name) = arg.strip_prefix("-l").filter(|n| !n.is_empty()) {
            kit.args.push(LinkArg::SysLib {
                prefix: "-l".to_string(),
                name: name.to_string(),
                module: lib_owner.get(name).cloned(),
            });
            continue;
        }
        if msvc && !arg.starts_with(['-', '/']) && arg.to_ascii_lowercase().ends_with(".lib") {
            kit.args.push(LinkArg::SysLib {
                prefix: String::new(),
                name: arg.clone(),
                module: lib_owner.get(arg.as_str()).cloned(),
            });
            continue;
        }

        kit.args.push(LinkArg::Lit { value: arg.clone() });
    }

    // A module the caller named but the line never read is a kit that
    // promises a symbol nothing in it defines, and the consumer only finds
    // out when the link fails. It means the recorded binary was built without
    // that module: the launcher's static shape has to carry every module a
    // kit offers.
    //
    // The message names the library each module was looked up by, quoted, and
    // the size of the line it was looked for on. Those two separate a launcher
    // built without the module from a library name that arrived carrying a
    // character nobody meant to send, and the two look identical otherwise.
    let unlinked: Vec<String> = kit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .filter(|name| {
            !kit.args
                .iter()
                .any(|arg| matches!(arg, LinkArg::File { module: Some(m), .. } if m == name))
        })
        .map(|name| match options.modules.get(name) {
            Some(lib) => format!("{name} ({lib:?})"),
            None => name.to_string(),
        })
        .collect();
    if !unlinked.is_empty() {
        return Err(format!(
            "the recorded link read nothing belonging to: {}, each named beside \
             the library its rlib was looked up by. The line carries {} \
             arguments and names {} files, and {} modules were declared. The \
             binary it produced was built without them, so a kit cannot offer \
             them",
            unlinked.join(", "),
            kit.args.len(),
            kit.staged.len(),
            kit.modules.len(),
        ));
    }

    Ok(kit)
}

/// `/OUT:` and `-out:`, the MSVC spellings of `-o`.
fn out_prefix(arg: &str) -> Option<String> {
    let head: String = arg.chars().take(5).collect();
    (head.eq_ignore_ascii_case("/out:") || head.eq_ignore_ascii_case("-out:")).then_some(head)
}

fn strip_prefix_fold<'a>(arg: &'a str, prefix: &str) -> Option<&'a str> {
    arg.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &arg[prefix.len()..])
}

/// Classify one library search path.
///
/// A directory under the build's target directory was produced by the build -
/// a `-sys` crate's own static library lives in one - so the kit carries it. A
/// directory that no longer exists was a temporary of the build and is
/// replaced by an empty one, which keeps the argument's position on the line
/// without inventing a path. Anything else belongs to the machine that built
/// it, and the consumer's copy of the same system directory is the right one.
fn search_dir(prefix: String, dir: &str, options: &Options, kit: &mut Kit) -> LinkArg {
    let path = Path::new(dir);
    if path.starts_with(&options.target_dir) && path.is_dir() {
        let next = kit.libdirs.len();
        let kit_path = kit
            .libdirs
            .iter()
            .find(|(_, source)| source.as_path() == path)
            .map(|(kit_path, _)| kit_path.clone())
            .unwrap_or_else(|| {
                let kit_path = format!("libdirs/{next}");
                kit.libdirs.insert(kit_path.clone(), path.to_path_buf());
                kit_path
            });
        return LinkArg::SysDir {
            prefix,
            path: kit_path,
            staged: true,
        };
    }
    if !path.is_dir() {
        return LinkArg::SysDir {
            prefix,
            path: "libdirs/empty".to_string(),
            staged: true,
        };
    }
    LinkArg::SysDir {
        prefix,
        path: dir.to_string(),
        staged: false,
    }
}

/// Classify a `-B` prefix, which is where a `cc` driver looks for the linker
/// itself. A prefix that holds the toolchain's LLD is dropped rather than
/// carried; see the module docs.
fn prefix_dir(prefix: &str, dir: &str, options: &Options, kit: &mut Kit) -> Option<LinkArg> {
    let path = Path::new(dir);
    if LLD_NAMES.iter().any(|name| path.join(name).is_file()) {
        return None;
    }
    Some(search_dir(prefix.to_string(), dir, options, kit))
}

/// What a `cc` driver calls the linker inside a `-B` prefix directory.
const LLD_NAMES: [&str; 3] = ["ld.lld", "ld64.lld", "ld.lld.exe"];

/// The module whose rlib this staged file is, if it is one.
///
/// The staged name is a content hash, a hyphen, and the file's own name.
/// rustc names an rlib after its library, with a metadata hash appended when
/// two builds of it could meet on one line - which a module crate, built once
/// per target, does not get. Both spellings are the same library.
fn owning_module(staged: &str, modules: &BTreeMap<String, String>) -> Option<String> {
    let stem = staged
        .split_once('-')
        .map(|(_, rest)| rest)?
        .strip_prefix("lib")?
        .strip_suffix(".rlib")?;
    modules
        .iter()
        .find(|(_, lib)| stem == lib.as_str() || stem.starts_with(&format!("{lib}-")))
        .map(|(name, _)| name.clone())
}

/// Copy everything the kit carries, then write the manifest.
fn write_kit(kit: &Kit, options: &Options) -> Result<(), String> {
    let out = &options.out;
    // Asked before anything is copied: a kit that has to ship its linker and
    // was not given one is unusable, and finding that out after staging a
    // target directory's worth of files wastes the whole step.
    let mut driver = driver_for(options.target);
    if driver.kind == DriverKind::Lld && options.driver_path.is_none() {
        return Err(format!(
            "the {} kit is replayed through a linker it has to ship; pass --driver-path",
            options.target.name()
        ));
    }

    let stage_out = out.join("stage");
    fs::create_dir_all(&stage_out)
        .map_err(|e| format!("cannot create {}: {e}", stage_out.display()))?;
    for name in &kit.staged {
        let from = options.stage.join(name);
        let to = stage_out.join(name);
        if to.exists() {
            continue;
        }
        fs::copy(&from, &to).map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
    }

    fs::create_dir_all(out.join("libdirs/empty"))
        .map_err(|e| format!("cannot create {}: {e}", out.join("libdirs/empty").display()))?;
    for (kit_path, source) in &kit.libdirs {
        copy_dir(source, &out.join(kit_path))?;
    }

    if let Some(source) = options.driver_path.as_deref() {
        let bin = out.join("bin");
        let name = source
            .file_name()
            .ok_or_else(|| format!("{} is not a file", source.display()))?;
        fs::create_dir_all(&bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
        fs::copy(source, bin.join(name))
            .map_err(|e| format!("cannot copy {}: {e}", source.display()))?;
        driver.path = Some(format!("bin/{}", name.to_string_lossy()));
    }

    let manifest = Manifest {
        schema: SCHEMA_VERSION,
        target: options.target.name().to_string(),
        rust_triple: options.target.rust_triple().to_string(),
        rustc: options.rustc.clone(),
        lumen_version: env!("CARGO_PKG_VERSION").to_string(),
        driver,
        args: kit.args.clone(),
        modules: kit.modules.clone(),
        artifact: Artifact {
            kind: artifact_kind(options.target),
        },
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("cannot encode the manifest: {e}"))?;
    fs::write(out.join("manifest.json"), json + "\n")
        .map_err(|e| format!("cannot write {}: {e}", out.join("manifest.json").display()))
}

/// What replays a kit for this target.
///
/// Unix replays through `cc`: it contributes the C runtime startup files and
/// the system library paths, and a machine that can package a Lumen app has
/// one. Windows has no such compiler to borrow, so its kit ships LLD and the
/// line is fed to it directly.
fn driver_for(target: Target) -> Driver {
    let triple = target.rust_triple();
    if triple.contains("-windows-") {
        return Driver {
            kind: DriverKind::Lld,
            flavor: "link".to_string(),
            path: None,
        };
    }
    Driver {
        kind: DriverKind::Cc,
        flavor: if triple.contains("-apple-") {
            "darwin".to_string()
        } else {
            "gnu".to_string()
        },
        path: None,
    }
}

/// How the app's artifact reaches the executable this kit links.
fn artifact_kind(target: Target) -> ArtifactKind {
    if target.rust_triple().contains("-apple-") {
        ArtifactKind::MachoSection
    } else {
        ArtifactKind::Append
    }
}

/// Copy a directory tree, which is what a `-sys` crate's output directory is.
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .map_err(|e| format!("cannot copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use lumen_modules::link_kit::{
        Artifact, ArtifactKind, DriverKind, KitModule, LinkArg, Manifest, Record, RecordEnv,
        SCHEMA_VERSION,
    };

    use super::{Kit, Options, classify, emit, pick};
    use crate::package_cli::Target;

    /// A scratch directory that removes itself when the test ends.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lumen-emit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        Scratch(dir)
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the parent directory");
        }
        std::fs::write(path, bytes).expect("write the file");
    }

    fn text(path: &Path) -> String {
        path.to_str().expect("utf-8 path").to_string()
    }

    /// One line of the recorder's output.
    fn line(out: &str, argv: &[String], staged: &[String]) -> String {
        serde_json::to_string(&Record {
            out: Some(out.to_string()),
            argv: argv.to_vec(),
            staged_argv: staged.to_vec(),
            cwd: "/b".to_string(),
            env: RecordEnv::default(),
        })
        .expect("the record encodes")
    }

    fn run_emit(args: &[String]) -> Result<String, String> {
        emit(args.iter().cloned())
    }

    fn manifest_at(out: &Path) -> Manifest {
        let text = std::fs::read_to_string(out.join("manifest.json")).expect("read the manifest");
        serde_json::from_str(&text).expect("the manifest is a manifest")
    }

    fn options(target: &str, target_dir: &str) -> Options {
        Options {
            record: PathBuf::new(),
            stage: PathBuf::new(),
            out: PathBuf::new(),
            target: Target::parse(target).expect("known target"),
            target_dir: PathBuf::from(target_dir),
            binary: "lumen_launcher".to_string(),
            rustc: "rustc 1.97.0".to_string(),
            driver_path: None,
            modules: BTreeMap::from([("lumen-fs".to_string(), "lumen_fs".to_string())]),
            module_libs: BTreeMap::from([("lumen-audio".to_string(), vec!["asound".to_string()])]),
        }
    }

    /// The same, for a line that carries no module: every module named has to
    /// appear on the line, so a test about anything else declares none.
    fn no_modules(target: &str) -> Options {
        let mut options = options(target, "/b/target");
        options.modules.clear();
        options
    }

    /// The declared name and library name of every module under `std/`, the
    /// pairs `.github/scripts/first-party-modules.sh` hands the release step.
    fn first_party() -> BTreeMap<String, String> {
        ["archive", "audio", "download", "fs", "process"]
            .into_iter()
            .map(|name| (format!("lumen-{name}"), format!("lumen_{name}")))
            .collect()
    }

    /// A Windows line reading every first-party module's rlib, in the shape a
    /// recorded MSVC link has them: absolute drive paths, staged under a
    /// content hash and the file's own name.
    fn msvc_line() -> (Vec<String>, Vec<String>) {
        let deps = "D:\\b\\target\\x86_64-pc-windows-msvc\\release\\deps";
        let mut argv = vec!["/NOLOGO".to_string()];
        let mut staged = vec!["/NOLOGO".to_string()];
        for (i, lib) in ["process", "fs", "download", "audio", "archive"]
            .into_iter()
            .enumerate()
        {
            argv.push(format!("{deps}\\liblumen_{lib}.rlib"));
            staged.push(format!("1122334{i}-liblumen_{lib}.rlib"));
        }
        argv.push("kernel32.lib".to_string());
        staged.push("kernel32.lib".to_string());
        argv.push(format!("/OUT:{deps}\\lumen_launcher.exe"));
        staged.push(format!("/OUT:{deps}\\lumen_launcher.exe"));
        (argv, staged)
    }

    /// The module every entry on the line was attributed to, in order.
    fn attributed(kit: &Kit) -> Vec<&str> {
        kit.args
            .iter()
            .filter_map(|arg| match arg {
                LinkArg::File {
                    module: Some(name), ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// A record whose staged list differs from the raw one exactly where an
    /// argument named a file, which is the marker the recorder leaves.
    fn record(argv: &[&str], staged: &[&str]) -> Record {
        Record {
            out: Some("/b/target/x86_64-unknown-linux-gnu/release/deps/lumen_launcher-9f".into()),
            argv: argv.iter().map(|a| (*a).to_string()).collect(),
            staged_argv: staged.iter().map(|a| (*a).to_string()).collect(),
            cwd: "/b".to_string(),
            env: lumen_modules::link_kit::RecordEnv::default(),
        }
    }

    #[test]
    fn each_kind_of_argument_lands_on_its_own_entry() {
        // The unstaged-SysDir arm needs a directory that exists on the host
        // running the test; a missing one is substituted with libdirs/empty.
        let sysdir = std::env::temp_dir().display().to_string();
        let argv = [
            "-m64",
            "/b/target/x86_64-unknown-linux-gnu/release/deps/lumen_launcher.rcgu.o",
            "/b/target/x86_64-unknown-linux-gnu/release/deps/liblumen_fs.rlib",
            "-lasound",
            "-lm",
            "-L",
            sysdir.as_str(),
            "-o",
            "/b/target/x86_64-unknown-linux-gnu/release/deps/lumen_launcher-9f",
        ];
        let staged = [
            "-m64",
            "11223344-lumen_launcher.rcgu.o",
            "55667788-liblumen_fs.rlib",
            "-lasound",
            "-lm",
            "-L",
            sysdir.as_str(),
            "-o",
            "/b/target/x86_64-unknown-linux-gnu/release/deps/lumen_launcher-9f",
        ];
        let options = options("linux-x86_64", "/b/target");
        let kit = classify(&record(&argv, &staged), &options).expect("the record classifies");

        assert_eq!(
            kit.args,
            vec![
                LinkArg::Lit {
                    value: "-m64".to_string()
                },
                LinkArg::File {
                    path: "11223344-lumen_launcher.rcgu.o".to_string(),
                    module: None,
                },
                LinkArg::File {
                    path: "55667788-liblumen_fs.rlib".to_string(),
                    module: Some("lumen-fs".to_string()),
                },
                LinkArg::SysLib {
                    prefix: "-l".to_string(),
                    name: "asound".to_string(),
                    module: Some("lumen-audio".to_string()),
                },
                LinkArg::SysLib {
                    prefix: "-l".to_string(),
                    name: "m".to_string(),
                    module: None,
                },
                LinkArg::Lit {
                    value: "-L".to_string()
                },
                LinkArg::SysDir {
                    prefix: String::new(),
                    path: sysdir,
                    staged: false,
                },
                LinkArg::Lit {
                    value: "-o".to_string()
                },
                LinkArg::Out {
                    prefix: String::new()
                },
            ]
        );
        assert_eq!(
            kit.staged,
            vec![
                "11223344-lumen_launcher.rcgu.o",
                "55667788-liblumen_fs.rlib"
            ]
        );
        assert_eq!(kit.modules.len(), 1, "one --module was declared");
        assert_eq!(kit.modules[0].name, "lumen-fs");
    }

    #[test]
    fn the_exported_symbol_list_is_left_off_the_line() {
        let argv = ["/tmp/rustcXX/symbols.o", "-lm"];
        let staged = ["aabbccdd-symbols.o", "-lm"];
        let kit = classify(&record(&argv, &staged), &no_modules("linux-x86_64"))
            .expect("the record classifies");
        assert_eq!(kit.staged, Vec::<String>::new());
        assert_eq!(
            kit.args,
            vec![LinkArg::SysLib {
                prefix: "-l".to_string(),
                name: "m".to_string(),
                module: None,
            }]
        );
    }

    #[test]
    fn a_search_path_the_build_produced_travels_and_a_vanished_one_empties() {
        let argv = [
            "-L",
            "/b/target/x86_64-unknown-linux-gnu/release/build/ring-aa/out",
            "-L",
            "/b/target/x86_64-unknown-linux-gnu/release/deps/rustcXX/raw-dylibs",
        ];
        let kit = classify(&record(&argv, &argv), &no_modules("linux-x86_64"))
            .expect("the record classifies");
        // Neither directory exists in the test's file system, so both take
        // the vanished-temporary arm; what the entry proves is that the
        // argument keeps its place and never names a path off this machine.
        assert!(kit.libdirs.is_empty());
        assert_eq!(
            kit.args[1],
            LinkArg::SysDir {
                prefix: String::new(),
                path: "libdirs/empty".to_string(),
                staged: true,
            }
        );
    }

    #[test]
    fn the_msvc_spellings_classify_the_same_way() {
        let argv = [
            "/LIBPATH:C:\\rust\\lib",
            "kernel32.lib",
            "/DEBUG",
            "/OUT:C:\\b\\lumen_launcher.exe",
        ];
        let kit = classify(&record(&argv, &argv), &no_modules("windows-x86_64"))
            .expect("the record classifies");
        assert_eq!(
            kit.args,
            vec![
                LinkArg::SysDir {
                    prefix: "/LIBPATH:".to_string(),
                    path: "libdirs/empty".to_string(),
                    staged: true,
                },
                LinkArg::SysLib {
                    prefix: String::new(),
                    name: "kernel32.lib".to_string(),
                    module: None,
                },
                LinkArg::Lit {
                    value: "/DEBUG".to_string()
                },
                LinkArg::Out {
                    prefix: "/OUT:".to_string()
                },
            ]
        );
    }

    /// The launcher's static shape has to carry every module a kit offers, so
    /// a module that contributed nothing to the recorded line is a build that
    /// forgot it rather than a kit with one module fewer.
    #[test]
    fn a_module_the_line_never_read_is_refused() {
        let argv = ["-lm"];
        let error = classify(&record(&argv, &argv), &options("linux-x86_64", "/b/target"))
            .expect_err("lumen-fs was declared and never linked");
        assert!(error.contains("lumen-fs"), "{error}");
    }

    /// Windows is the one target whose line is written in the MSVC spellings,
    /// and a module's rlib has to be found on it the same way it is on every
    /// other target.
    #[test]
    fn every_module_on_an_msvc_line_is_attributed_to_its_rlib() {
        let (argv, staged) = msvc_line();
        let mut options = options("windows-x86_64", "D:\\b\\target");
        options.modules = first_party();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let staged: Vec<&str> = staged.iter().map(String::as_str).collect();
        let kit = classify(&record(&argv, &staged), &options).expect("the record classifies");
        assert_eq!(
            attributed(&kit),
            [
                "lumen-process",
                "lumen-fs",
                "lumen-download",
                "lumen-audio",
                "lumen-archive"
            ]
        );
    }

    /// A library name is matched against the rlib's own name whole, so one
    /// that arrives carrying a line ending matches nothing. What the refusal
    /// owes its reader is the name it was given, quoted, because the
    /// difference between that and a launcher built without the module is
    /// invisible otherwise.
    #[test]
    fn a_library_name_carrying_a_line_ending_is_quoted_in_the_refusal() {
        let (argv, staged) = msvc_line();
        let mut options = options("windows-x86_64", "D:\\b\\target");
        options.modules = first_party()
            .into_iter()
            .map(|(name, lib)| (name, format!("{lib}\r")))
            .collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let staged: Vec<&str> = staged.iter().map(String::as_str).collect();
        let error = classify(&record(&argv, &staged), &options)
            .expect_err("no library name matches an rlib");
        assert!(error.contains(r#"lumen-fs ("lumen_fs\r")"#), "{error}");
        assert!(error.contains("5 modules"), "{error}");
    }

    #[test]
    fn the_launcher_link_is_picked_out_of_the_build() {
        let other = Record {
            out: Some("/b/target/release/deps/build_script_build-11".to_string()),
            ..Default::default()
        };
        let launcher = record(&[], &[]);
        let records = vec![other.clone(), launcher.clone()];
        assert_eq!(pick(&records, "lumen_launcher"), Some(&launcher));
        assert_eq!(pick(&records, "lumenc"), None);
    }

    /// A line that ends where a value was expected is a record this build
    /// cannot read, rather than one it guesses the rest of.
    #[test]
    fn a_line_that_stops_mid_argument_is_refused() {
        let options = no_modules("linux-x86_64");
        for (argv, expect) in [
            (vec!["-o"], "ends after -o"),
            (vec!["-L"], "ends after -L"),
            (vec!["-B"], "ends after -B"),
        ] {
            let error = classify(&record(&argv, &argv), &options).expect_err("the line is short");
            assert!(error.contains(expect), "{error}");
        }
        let error = classify(&record(&["-lm"], &[]), &options).expect_err("the lists disagree");
        assert!(error.contains("disagree in length"), "{error}");
    }

    /// The whole of an emit: pick the launcher's link out of the build, sort
    /// every argument, copy what the kit carries, and write the manifest a
    /// replay reads.
    #[test]
    fn a_recorded_line_becomes_a_kit_that_carries_what_it_reads() {
        let root = scratch("kit");
        let target_dir = root.join("target");
        // A `-sys` crate's own output directory: produced by the build, so the
        // kit carries it, subdirectories and all.
        let built = target_dir.join("release/build/ring-aa/out");
        write(&built.join("libring.a"), b"the static library");
        write(&built.join("nested/extra.a"), b"one more");
        // A `-B` prefix holding the toolchain's own LLD, which is dropped
        // rather than carried.
        let lld = root.join("lld-prefix");
        write(&lld.join("ld.lld"), b"the toolchain's linker");

        let stage = root.join("stage");
        write(&stage.join("11223344-lumen_launcher.rcgu.o"), b"an object");
        write(&stage.join("55667788-liblumen_fs.rlib"), b"an archive");
        write(&stage.join("aabbccdd-symbols.o"), b"the exported symbols");

        let argv: Vec<String> = [
            "-m64".to_string(),
            "/b/deps/lumen_launcher.rcgu.o".to_string(),
            "/b/deps/liblumen_fs.rlib".to_string(),
            "/tmp/rustcXX/symbols.o".to_string(),
            "-L".to_string(),
            text(&built),
            format!("-L{}", text(&built)),
            "-L/no/such/directory".to_string(),
            "-L".to_string(),
            text(&root),
            "-B".to_string(),
            text(&lld),
            "-B".to_string(),
            text(&built),
            format!("-B{}", text(&built)),
            "-fuse-ld=lld".to_string(),
            "-lasound".to_string(),
            "-lm".to_string(),
            "-o".to_string(),
            "/b/target/release/deps/lumen_launcher-9f".to_string(),
        ]
        .to_vec();
        let mut staged = argv.clone();
        staged[1] = "11223344-lumen_launcher.rcgu.o".to_string();
        staged[2] = "55667788-liblumen_fs.rlib".to_string();
        staged[3] = "aabbccdd-symbols.o".to_string();

        // Two links and a blank line, which is what a build leaves behind.
        let record_path = root.join("record.jsonl");
        write(
            &record_path,
            format!(
                "{}\n\n{}\n",
                line("/b/target/release/deps/build_script_build-11", &[], &[]),
                line("/b/target/release/deps/lumen_launcher-9f", &argv, &staged),
            )
            .as_bytes(),
        );

        let out = root.join("kit");
        let args: Vec<String> = [
            "--record",
            &text(&record_path),
            "--stage",
            &text(&stage),
            "--out",
            &text(&out),
            "--target",
            "linux-x86_64",
            "--target-dir",
            &text(&target_dir),
            "--rustc",
            "rustc 1.97.0",
            "--module",
            "lumen-fs=lumen_fs",
            "--module-libs",
            "lumen-audio=asound",
        ]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
        let message = run_emit(&args).expect("the record has a launcher link in it");
        assert!(
            message.contains("lumen-linkkit-linux-x86_64.tar.gz"),
            "{message}"
        );
        assert!(message.contains("2 staged files"), "{message}");
        assert!(message.contains("1 modules"), "{message}");

        let manifest = manifest_at(&out);
        assert_eq!(manifest.schema, SCHEMA_VERSION);
        assert_eq!(manifest.target, "linux-x86_64");
        assert_eq!(manifest.rust_triple, "x86_64-unknown-linux-gnu");
        assert_eq!(manifest.rustc, "rustc 1.97.0");
        assert_eq!(manifest.lumen_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.driver.kind, DriverKind::Cc);
        assert_eq!(manifest.driver.flavor, "gnu");
        assert_eq!(manifest.driver.path, None);
        assert_eq!(
            manifest.artifact,
            Artifact {
                kind: ArtifactKind::Append
            }
        );
        assert_eq!(manifest.modules, vec![KitModule::new("lumen-fs")]);

        let sysdir = |prefix: &str, path: &str, staged: bool| LinkArg::SysDir {
            prefix: prefix.to_string(),
            path: path.to_string(),
            staged,
        };
        assert_eq!(
            manifest.args,
            vec![
                LinkArg::Lit {
                    value: "-m64".to_string()
                },
                LinkArg::File {
                    path: "11223344-lumen_launcher.rcgu.o".to_string(),
                    module: None,
                },
                LinkArg::File {
                    path: "55667788-liblumen_fs.rlib".to_string(),
                    module: Some("lumen-fs".to_string()),
                },
                LinkArg::Lit {
                    value: "-L".to_string()
                },
                sysdir("", "libdirs/0", true),
                // The same directory a second time keeps the copy it already
                // has rather than staging it twice.
                sysdir("-L", "libdirs/0", true),
                // A directory the build deleted keeps its place on the line
                // without naming a path off the machine that recorded it.
                sysdir("-L", "libdirs/empty", true),
                LinkArg::Lit {
                    value: "-L".to_string()
                },
                // The machine's own directory, which the consumer has its own
                // copy of.
                sysdir("", &text(&root), false),
                // A `-B` prefix is one argument either way it was written.
                sysdir("-B", "libdirs/0", true),
                sysdir("-B", "libdirs/0", true),
                LinkArg::SysLib {
                    prefix: "-l".to_string(),
                    name: "asound".to_string(),
                    module: Some("lumen-audio".to_string()),
                },
                LinkArg::SysLib {
                    prefix: "-l".to_string(),
                    name: "m".to_string(),
                    module: None,
                },
                LinkArg::Lit {
                    value: "-o".to_string()
                },
                LinkArg::Out {
                    prefix: String::new()
                },
            ]
        );

        assert_eq!(
            std::fs::read(out.join("stage/11223344-lumen_launcher.rcgu.o")).expect("staged"),
            b"an object"
        );
        assert_eq!(
            std::fs::read(out.join("stage/55667788-liblumen_fs.rlib")).expect("staged"),
            b"an archive"
        );
        assert!(
            !out.join("stage/aabbccdd-symbols.o").exists(),
            "the exported symbol list is left off the line, so it is not carried"
        );
        assert_eq!(
            std::fs::read(out.join("libdirs/0/libring.a")).expect("the search path travels"),
            b"the static library"
        );
        assert_eq!(
            std::fs::read(out.join("libdirs/0/nested/extra.a")).expect("subdirectories too"),
            b"one more"
        );
        assert!(out.join("libdirs/empty").is_dir());
        assert!(!out.join("bin").exists(), "a `cc` kit ships no linker");

        // Emitting over a kit that is already there copies nothing twice.
        run_emit(&args).expect("the same kit writes again");
        assert_eq!(manifest_at(&out).args.len(), 15);
    }

    /// Windows has no `cc` to borrow, so its kit ships a linker and is refused
    /// outright when it was not given one to ship.
    #[test]
    fn a_windows_kit_is_refused_without_the_linker_it_has_to_ship() {
        let root = scratch("windows");
        let stage = root.join("stage");
        write(&stage.join("12345678-liblumen_fs.rlib"), b"an archive");

        let argv: Vec<String> = [
            "/LIBPATH:C:\\rust\\lib",
            "kernel32.lib",
            "asound.lib",
            "/b/deps/liblumen_fs.rlib",
            "/DEBUG",
            "/OUT:C:\\b\\lumen_launcher.exe",
        ]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
        let mut staged = argv.clone();
        staged[3] = "12345678-liblumen_fs.rlib".to_string();

        let record_path = root.join("record.jsonl");
        write(
            &record_path,
            line("/b/lumen_launcher.exe", &argv, &staged).as_bytes(),
        );

        let out = root.join("kit");
        let mut args: Vec<String> = [
            "--record",
            &text(&record_path),
            "--stage",
            &text(&stage),
            "--out",
            &text(&out),
            "--target",
            "windows-x86_64",
            "--target-dir",
            &text(&root.join("target")),
            "--module",
            "lumen-fs=lumen_fs",
            "--module-libs",
            "lumen-fs=asound.lib",
        ]
        .iter()
        .map(|a| (*a).to_string())
        .collect();

        let error = run_emit(&args).expect_err("no linker was named");
        assert!(error.contains("--driver-path"), "{error}");
        assert!(error.contains("windows-x86_64"), "{error}");
        assert!(
            !out.exists(),
            "the answer comes before a target directory's worth of files is copied"
        );

        let linker = root.join("rust-lld.exe");
        write(&linker, b"the linker the kit ships");
        args.push("--driver-path".to_string());
        args.push(text(&linker));
        run_emit(&args).expect("a kit that ships its linker");

        let manifest = manifest_at(&out);
        assert_eq!(manifest.driver.kind, DriverKind::Lld);
        assert_eq!(manifest.driver.flavor, "link");
        assert_eq!(manifest.driver.path.as_deref(), Some("bin/rust-lld.exe"));
        assert_eq!(
            std::fs::read(out.join("bin/rust-lld.exe")).expect("the linker travels"),
            b"the linker the kit ships"
        );
        assert_eq!(
            manifest.args,
            vec![
                LinkArg::SysDir {
                    prefix: "/LIBPATH:".to_string(),
                    path: "libdirs/empty".to_string(),
                    staged: true,
                },
                LinkArg::SysLib {
                    prefix: String::new(),
                    name: "kernel32.lib".to_string(),
                    module: None,
                },
                LinkArg::SysLib {
                    prefix: String::new(),
                    name: "asound.lib".to_string(),
                    module: Some("lumen-fs".to_string()),
                },
                LinkArg::File {
                    path: "12345678-liblumen_fs.rlib".to_string(),
                    module: Some("lumen-fs".to_string()),
                },
                LinkArg::Lit {
                    value: "/DEBUG".to_string()
                },
                LinkArg::Out {
                    prefix: "/OUT:".to_string()
                },
            ]
        );
    }

    /// macOS is replayed through `cc` like the other Unix targets, and its
    /// artifact goes into the executable as the link writes it, because a
    /// signature covers the whole file and nothing may follow it.
    #[test]
    fn a_macos_kit_is_replayed_through_cc_and_carries_its_artifact_on_the_line() {
        let root = scratch("macos");
        let stage = root.join("stage");
        write(&stage.join("13579bdf-launcher.o"), b"an object");

        let argv = vec![
            "/b/deps/launcher.o".to_string(),
            "-o".to_string(),
            "/b/target/release/deps/lumen_launcher".to_string(),
        ];
        let mut staged = argv.clone();
        staged[0] = "13579bdf-launcher.o".to_string();

        let record_path = root.join("record.jsonl");
        write(
            &record_path,
            line("/b/target/release/deps/lumen_launcher", &argv, &staged).as_bytes(),
        );

        let out = root.join("kit");
        let args: Vec<String> = [
            "--record",
            &text(&record_path),
            "--stage",
            &text(&stage),
            "--out",
            &text(&out),
            "--target",
            "macos-aarch64",
            "--target-dir",
            &text(&root.join("target")),
        ]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
        run_emit(&args).expect("the record has the launcher's link in it");

        let manifest = manifest_at(&out);
        assert_eq!(manifest.driver.kind, DriverKind::Cc);
        assert_eq!(manifest.driver.flavor, "darwin");
        assert_eq!(manifest.rust_triple, "aarch64-apple-darwin");
        assert_eq!(
            manifest.artifact,
            Artifact {
                kind: ArtifactKind::MachoSection
            }
        );
    }

    /// Every request an emit cannot answer, and what it says instead.
    #[test]
    fn the_requests_an_emit_cannot_answer_are_named() {
        let root = scratch("refused");
        let error = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            run_emit(&args).expect_err("the request cannot be answered")
        };

        assert!(error(&["--frobnicate"]).contains("unknown option `--frobnicate`"));
        assert!(error(&["--record"]).contains("--record needs a value"));
        assert!(error(&["--module", "lumen-fs"]).contains("--module takes <name>=<lib>"));
        assert!(error(&["--module-libs", "lumen-fs"]).contains("--module-libs takes"));
        assert!(error(&["--target", "plan9-x86_64"]).contains("no release target is named"));
        assert!(error(&[]).contains("--record is required"));
        assert!(error(&["--record", "r"]).contains("--stage is required"));
        assert!(error(&["--record", "r", "--stage", "s"]).contains("--out is required"));

        let complete = |record: &Path| {
            [
                "--record".to_string(),
                text(record),
                "--stage".to_string(),
                text(&root),
                "--out".to_string(),
                text(&root.join("kit")),
                "--target".to_string(),
                "linux-x86_64".to_string(),
                "--target-dir".to_string(),
                text(&root.join("target")),
            ]
            .to_vec()
        };

        let missing = root.join("absent.jsonl");
        let error = run_emit(&complete(&missing)).expect_err("there is no record");
        assert!(error.contains("cannot read"), "{error}");

        let malformed = root.join("malformed.jsonl");
        write(&malformed, b"not a record\n");
        let error = run_emit(&complete(&malformed)).expect_err("the line is not a record");
        assert!(error.contains("malformed.jsonl:1"), "{error}");

        let other = root.join("other.jsonl");
        write(
            &other,
            line("/b/target/release/deps/build_script_build-11", &[], &[]).as_bytes(),
        );
        let error = run_emit(&complete(&other)).expect_err("no link produced the launcher");
        assert!(error.contains("`lumen_launcher`"), "{error}");
        assert!(error.contains("(1 links recorded)"), "{error}");

        // A module the caller named and the line never read: the binary the
        // record produced was built without it.
        let launcher = root.join("launcher.jsonl");
        write(
            &launcher,
            line(
                "/b/target/release/deps/lumen_launcher-9f",
                &["-lm".to_string()],
                &["-lm".to_string()],
            )
            .as_bytes(),
        );
        let mut args = complete(&launcher);
        args.push("--module".to_string());
        args.push("lumen-fs=lumen_fs".to_string());
        let error = run_emit(&args).expect_err("the line read nothing of lumen-fs");
        assert!(error.contains("lumen-fs"), "{error}");
    }
}
