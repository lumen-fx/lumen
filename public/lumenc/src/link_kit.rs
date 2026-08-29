//! Linking an app into one executable from a prebuilt link kit.
//!
//! `lumenc package --static` writes a folder holding a single executable: the
//! engine, the launcher, and the runtime modules the app declares, all in one
//! file with nothing beside it to find. Producing that normally means a Rust
//! toolchain and a from-source build. It does not have to: the release
//! published the link line that built the static launcher and every file that
//! link read (see [`lumen_modules::link_kit`]), so the same executable can be
//! produced by replaying that line with the modules the app did not ask for
//! left out.
//!
//! The three steps are [`locate`] (find or fetch the kit for a target),
//! [`plan`] (turn the manifest plus the app's `[dependencies]` into a command),
//! and [`link`] (run it). What a replay changes about the recorded line is
//! only this:
//!
//! - The output path becomes the app's executable.
//! - Every argument naming a file the kit carries is re-rooted into the kit.
//! - A module the app did not declare loses its objects and the native
//!   libraries only it asked for, so the executable neither carries the code
//!   nor depends on the system libraries behind it.
//! - Each module the app did declare gains one forced symbol. Its rlib is an
//!   archive nothing else references, and its registration entry is what pulls
//!   the object holding the pre-main constructor out of it.
//!
//! The producer already left `symbols.o` off the line, so nothing here has to
//! drop it: keeping it would have linked every module the kit carries, which
//! is the one thing selection cannot survive.
//!
//! The app's own compiled artifact reaches the executable the way the target
//! does it: appended past the end on Linux and Windows, and written into a
//! Mach-O section by the link itself on macOS, where a signature covers the
//! whole file and nothing may follow it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use lumen_modules::link_kit::{
    ArtifactKind, Driver, DriverKind, LinkArg, Manifest, SCHEMA_VERSION,
};
use lumen_runtime::modules::DependenciesCfg;

use crate::package_cli::{
    EVERY_MEMBER, Members, Target, Unpack, append_artifact, cannot_fetch, component_cache,
    fetch_release_files, first_dir_with, search_dirs, set_executable,
};

/// The manifest at the root of a kit, and the only member [`locate`] insists
/// on: a directory holding it is a kit, and one that does not is not.
const MANIFEST: &str = "manifest.json";

/// Directory to take the kit from instead of looking one up. What CI and the
/// static-packaging tests point at the kit they just built.
const KIT_DIR_ENV: &str = "LUMEN_LINK_KIT_DIR";

/// Link `exe` from the kit for `target`, with the modules `deps` declares
/// compiled in and `artifact` inside the file.
pub(crate) fn link_app(
    exe: &Path,
    artifact: &[u8],
    target: Target,
    lib_dir: Option<&Path>,
    deps: &DependenciesCfg,
) -> Result<Vec<String>, String> {
    let kit = locate(target, lib_dir)?;
    let manifest = read_manifest(&kit, target)?;

    // Written before the line is planned, because macOS puts it on the line.
    let scratch = exe.with_extension("lmna-staging");
    std::fs::write(&scratch, artifact).map_err(|e| format!("write {}: {e}", scratch.display()))?;
    let planned = plan(&kit, &manifest, deps, exe, &scratch);
    let result = planned.and_then(|plan| {
        link(&plan)?;
        finish(&plan, artifact)?;
        Ok(plan.modules)
    });
    let _ = std::fs::remove_file(&scratch);
    result
}

/// The kit for `target` on this machine, fetched from the release channel
/// when it is not there yet.
///
/// [`KIT_DIR_ENV`] wins outright and is never fetched over: a caller that
/// names a directory has already decided which kit it wants. Otherwise the
/// search is the one the toolchain files take - `--lib-dir`, the directory
/// holding this `lumenc`, `LUMEN_LIB_DIR` - and then the cache for the
/// release [`crate::release::resolve`] names.
pub(crate) fn locate(target: Target, lib_dir: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os(KIT_DIR_ENV).filter(|v| !v.is_empty()) {
        return named_kit(&PathBuf::from(dir));
    }

    let wanted = [MANIFEST.to_string()];
    let dirs = search_dirs(lib_dir, target == Target::host());
    if let Some(dir) = first_dir_with(&dirs, &wanted) {
        return Ok(dir);
    }

    let component = format!("linkkit-{}", target.name());
    let (version, dir) = component_cache(&component)
        .map_err(|why| cannot_fetch(&wanted, Some(target.name()), &dirs, &why))?;
    if !dir.join(MANIFEST).is_file() {
        fetch_release_files(
            &version,
            &target.linkkit_archive_name(),
            &Members {
                wanted: &wanted,
                optional: &[EVERY_MEMBER.to_string()],
                trees: &[],
                layout: Unpack::Tree,
            },
            &dir,
            &format!(
                "A release older than static packaging ships no link kit; build the {} kit \
                 yourself and point {KIT_DIR_ENV} at it instead.",
                target.name()
            ),
        )?;
    }
    Ok(dir)
}

/// The kit a caller named outright, checked for being one at all. A directory
/// handed in by name is used as it is: a caller that says where the kit is has
/// already decided which one it wants, so nothing is fetched over it.
fn named_kit(dir: &Path) -> Result<PathBuf, String> {
    if !dir.join(MANIFEST).is_file() {
        return Err(format!(
            "{KIT_DIR_ENV} points at {}, which holds no {MANIFEST}, so it is not a link kit",
            dir.display()
        ));
    }
    Ok(dir.to_path_buf())
}

/// Read and check the manifest at the root of `kit`.
///
/// The schema is checked before anything else is read, and the target after
/// it: a kit is one platform's link line, and replaying another platform's
/// would fail deep inside a linker rather than here.
fn read_manifest(kit: &Path, target: Target) -> Result<Manifest, String> {
    let path = kit.join(MANIFEST);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if manifest.schema != SCHEMA_VERSION {
        return Err(format!(
            "the link kit in {} was written against schema {} and this lumenc reads schema \
             {SCHEMA_VERSION}. A kit and the lumenc that replays it come from one release; \
             use the kit published with this one.",
            kit.display(),
            manifest.schema
        ));
    }
    if manifest.target != target.name() {
        return Err(format!(
            "the link kit in {} is for {}, and this package is for {}",
            kit.display(),
            manifest.target,
            target.name()
        ));
    }
    Ok(manifest)
}

/// A replay: the program to run, its arguments, and what still has to happen
/// to the file the link writes.
#[derive(Debug)]
pub(crate) struct Plan {
    /// The program the replay runs.
    program: PathBuf,
    /// Its arguments, in order.
    args: Vec<OsString>,
    /// Which of the two the program is, for a failure that reads differently
    /// depending on whose tools are missing.
    kind: DriverKind,
    /// How the app's artifact reaches the executable.
    artifact: ArtifactKind,
    /// The executable being written.
    exe: PathBuf,
    /// The modules linked in, in declaration order.
    modules: Vec<String>,
}

/// Turn the recorded line into the one that produces this app's executable.
///
/// `scratch` is the app's compiled artifact on disk, which only the Mach-O
/// arm puts on the line; every other target appends it afterwards.
pub(crate) fn plan(
    kit: &Path,
    manifest: &Manifest,
    deps: &DependenciesCfg,
    exe: &Path,
    scratch: &Path,
) -> Result<Plan, String> {
    let mut modules = Vec::with_capacity(deps.0.len());
    for dep in &deps.0 {
        let module = manifest
            .modules
            .iter()
            .find(|module| module.name == dep.name)
            .ok_or_else(|| missing_from_kit(&dep.name, manifest))?;
        modules.push(module);
    }

    let mut args = Vec::with_capacity(manifest.args.len() + modules.len() + 1);
    // Ahead of the archives: a forced symbol is what makes the linker read a
    // module's rlib at all, and GNU ld only looks for one in an archive it
    // has not passed yet.
    for module in &modules {
        args.push(OsString::from(force_include(
            &manifest.driver,
            &module.register_symbol,
        )?));
    }

    let declared = |module: &Option<String>| match module {
        Some(name) => modules.iter().any(|kept| &kept.name == name),
        None => true,
    };
    let stage = kit.join("stage");
    for arg in &manifest.args {
        match arg {
            LinkArg::Lit { value } => args.push(OsString::from(value)),
            LinkArg::Out { prefix } => args.push(joined(prefix, exe)),
            LinkArg::File { path, module } => {
                if declared(module) {
                    args.push(stage.join(path).into_os_string());
                }
            }
            LinkArg::SysDir {
                prefix,
                path,
                staged,
            } => {
                let dir = if *staged {
                    kit.join(path)
                } else {
                    PathBuf::from(path)
                };
                args.push(joined(prefix, &dir));
            }
            LinkArg::SysLib {
                prefix,
                name,
                module,
            } => {
                if declared(module) {
                    args.push(OsString::from(format!("{prefix}{name}")));
                }
            }
        }
    }

    if manifest.artifact.kind == ArtifactKind::MachoSection {
        args.push(sectcreate(&manifest.driver, scratch)?);
    }

    let (program, lead) = program(kit, &manifest.driver)?;
    args.splice(0..0, lead);
    Ok(Plan {
        program,
        args,
        kind: manifest.driver.kind,
        artifact: manifest.artifact.kind,
        exe: exe.to_path_buf(),
        modules: modules.iter().map(|m| m.name.clone()).collect(),
    })
}

/// A module the app declared and the kit does not carry. Naming what the kit
/// offers separates a misspelled name from a kit built without the module.
fn missing_from_kit(name: &str, manifest: &Manifest) -> String {
    let offered: Vec<&str> = manifest
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    format!(
        "dependency '{name}': the {} link kit carries no module by that name, so a static \
         package cannot compile it in. The kit offers: {}. A module from outside the \
         toolchain is not in any kit; package without --static and ship it beside the app.",
        manifest.target,
        if offered.is_empty() {
            "nothing".to_string()
        } else {
            offered.join(", ")
        }
    )
}

/// The program a replay runs, and the arguments that go before the recorded
/// line. LLD is one binary carrying every object format, so the flavor the
/// manifest records is what selects the one this kit was recorded for.
fn program(kit: &Path, driver: &Driver) -> Result<(PathBuf, Vec<OsString>), String> {
    match driver.kind {
        DriverKind::Cc => Ok((PathBuf::from("cc"), Vec::new())),
        DriverKind::Lld => {
            let path = driver.path.as_deref().ok_or_else(|| {
                "this link kit is replayed through a linker it ships and names none".to_string()
            })?;
            let program = kit.join(path);
            if !program.is_file() {
                return Err(format!(
                    "the link kit names {} as its linker and the file is not there",
                    program.display()
                ));
            }
            set_executable(&program)?;
            Ok((
                program,
                vec![OsString::from("-flavor"), OsString::from(&driver.flavor)],
            ))
        }
    }
}

/// How this driver spells "keep this symbol whether or not anything references
/// it", which is what pulls a module's object out of its archive.
fn force_include(driver: &Driver, symbol: &str) -> Result<String, String> {
    match (driver.kind, driver.flavor.as_str()) {
        (DriverKind::Cc, "gnu") => Ok(format!("-Wl,-u,{symbol}")),
        // Mach-O writes a C symbol with a leading underscore, and the linker
        // is asked for the name the object file carries.
        (DriverKind::Cc, "darwin") => Ok(format!("-Wl,-u,_{symbol}")),
        (DriverKind::Lld, "link") => Ok(format!("/INCLUDE:{symbol}")),
        (kind, flavor) => Err(unknown_driver(kind, flavor)),
    }
}

/// The argument that writes the app's artifact into the executable as it is
/// linked, for the target whose signature leaves no room to append one.
fn sectcreate(driver: &Driver, artifact: &Path) -> Result<OsString, String> {
    match (driver.kind, driver.flavor.as_str()) {
        (DriverKind::Cc, "darwin") => Ok(joined("-Wl,-sectcreate,__LUMEN,__lmna,", artifact)),
        (kind, flavor) => Err(unknown_driver(kind, flavor)),
    }
}

fn unknown_driver(kind: DriverKind, flavor: &str) -> String {
    format!(
        "this link kit is replayed through a {kind:?} driver in the {flavor} flavor, which \
         this lumenc does not know how to drive"
    )
}

/// A flag and the path it is joined to, as one argument.
fn joined(prefix: &str, value: &Path) -> OsString {
    let mut arg = OsString::from(prefix);
    arg.push(value);
    arg
}

/// Run the replay.
pub(crate) fn link(plan: &Plan) -> Result<(), String> {
    let output = Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| cannot_run(plan, &e.to_string()))?;
    if !output.status.success() {
        return Err(link_failed(plan, &String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

/// What a driver that could not be started means. It is the one failure whose
/// answer is a piece of software to install rather than anything about the
/// app, so the message names it.
fn cannot_run(plan: &Plan, why: &str) -> String {
    if plan.kind == DriverKind::Lld {
        return format!(
            "cannot run {}: {why}. It is the linker the link kit ships; re-run to fetch the \
             kit again.",
            plan.program.display()
        );
    }
    let toolchain = if cfg!(target_os = "macos") {
        "the Xcode Command Line Tools (xcode-select --install)"
    } else {
        "a C toolchain (build-essential on Debian and Ubuntu, base-devel on Arch, \
         gcc + glibc-devel elsewhere)"
    };
    format!(
        "cannot run cc: {why}. A static package is linked on this machine, through the C \
         compiler that drives the system linker, so it needs {toolchain}."
    )
}

/// What a replay that ran and failed means.
fn link_failed(plan: &Plan, stderr: &str) -> String {
    // The MSVC C runtime and the Windows SDK import libraries are not
    // redistributable, so no kit can carry them and the linker resolving them
    // is what says whether they are installed.
    if plan.kind == DriverKind::Lld && stderr.to_ascii_lowercase().contains("kernel32.lib") {
        return format!(
            "the link kit's linker could not find the Windows SDK import libraries. A static \
             package links against the Microsoft C runtime and the Windows SDK, which Microsoft \
             does not allow anyone else to ship, so they have to be installed: the Visual Studio \
             Build Tools with the \"Desktop development with C++\" workload. The linker said:\n{}",
            stderr.trim_end()
        );
    }
    format!(
        "linking {} failed:\n{}",
        plan.exe.display(),
        stderr.trim_end()
    )
}

/// Everything the executable still needs once the link has written it.
fn finish(plan: &Plan, artifact: &[u8]) -> Result<(), String> {
    match plan.artifact {
        ArtifactKind::Append => append_artifact(&plan.exe, artifact)?,
        // The link already wrote it into a section; what is left is the
        // signature, which has to be the last thing done to the file.
        ArtifactKind::MachoSection => sign(&plan.exe)?,
    }
    set_executable(&plan.exe)
}

/// Strip and ad-hoc sign a Mach-O executable.
///
/// A replayed link keeps every symbol the build produced, where the release's
/// own binaries are stripped by their profile. Signing comes after, because
/// the signature covers the file as it ends up, and on Apple silicon an
/// executable without one does not start.
fn sign(exe: &Path) -> Result<(), String> {
    let stripped = Command::new("strip").arg("-x").arg(exe).output();
    if !matches!(&stripped, Ok(output) if output.status.success()) {
        eprintln!(
            "lumenc package: warning: strip did not run, so {} keeps the symbols the link \
             left in it",
            exe.display()
        );
    }
    let signed = Command::new("codesign")
        .args(["-s", "-", "-f"])
        .arg(exe)
        .output()
        .map_err(|e| {
            format!(
                "cannot run codesign: {e}. A macOS executable is signed before it will start, \
                 so packaging needs the Xcode Command Line Tools (xcode-select --install)."
            )
        })?;
    if !signed.status.success() {
        return Err(format!(
            "codesign could not sign {}:\n{}",
            exe.display(),
            String::from_utf8_lossy(&signed.stderr).trim_end()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use lumen_modules::link_kit::{Artifact, KitModule};

    use super::*;

    fn manifest(driver: Driver, artifact: ArtifactKind) -> Manifest {
        Manifest {
            schema: SCHEMA_VERSION,
            target: "linux-x86_64".to_string(),
            rust_triple: "x86_64-unknown-linux-gnu".to_string(),
            rustc: "rustc 1.97.0".to_string(),
            lumen_version: "0.0.9".to_string(),
            driver,
            args: vec![
                LinkArg::Lit {
                    value: "-m64".to_string(),
                },
                LinkArg::File {
                    path: "aa-launcher.o".to_string(),
                    module: None,
                },
                LinkArg::File {
                    path: "bb-liblumen_fs.rlib".to_string(),
                    module: Some("lumen-fs".to_string()),
                },
                LinkArg::File {
                    path: "cc-liblumen_audio.rlib".to_string(),
                    module: Some("lumen-audio".to_string()),
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
                LinkArg::SysDir {
                    prefix: "-L".to_string(),
                    path: "libdirs/0".to_string(),
                    staged: true,
                },
                LinkArg::SysDir {
                    prefix: "-L".to_string(),
                    path: "/usr/lib".to_string(),
                    staged: false,
                },
                LinkArg::Lit {
                    value: "-o".to_string(),
                },
                LinkArg::Out {
                    prefix: String::new(),
                },
            ],
            modules: vec![KitModule::new("lumen-audio"), KitModule::new("lumen-fs")],
            artifact: Artifact { kind: artifact },
        }
    }

    fn unix() -> Driver {
        Driver {
            kind: DriverKind::Cc,
            flavor: "gnu".to_string(),
            path: None,
        }
    }

    fn deps(names: &[&str]) -> DependenciesCfg {
        DependenciesCfg(
            names
                .iter()
                .map(|name| lumen_runtime::modules::DepCfg {
                    name: (*name).to_string(),
                    source: lumen_runtime::modules::ModuleSource::Bundled,
                    config: toml::Table::new(),
                    tags: Vec::new(),
                })
                .collect(),
        )
    }

    fn args(plan: &Plan) -> Vec<String> {
        plan.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The whole selection rule in one line: the declared module's rlib and
    /// its forced symbol are on it, and the undeclared module's rlib and the
    /// system library only it asked for are not.
    #[test]
    fn an_undeclared_module_leaves_with_its_native_library() {
        let manifest = manifest(unix(), ArtifactKind::Append);
        let plan = plan(
            Path::new("/kit"),
            &manifest,
            &deps(&["lumen-fs"]),
            Path::new("/out/Demo"),
            Path::new("/out/Demo.lmna-staging"),
        )
        .expect("the app declares a module the kit carries");

        // Kit-relative entries render through Path::join, so the expected
        // strings are built the same way to hold on every host separator.
        let kit = Path::new("/kit");
        let staged = |name: &str| kit.join("stage").join(name).display().to_string();
        assert_eq!(
            args(&plan),
            vec![
                "-Wl,-u,lumen_module_register_lumen_fs".to_string(),
                "-m64".to_string(),
                staged("aa-launcher.o"),
                staged("bb-liblumen_fs.rlib"),
                "-lm".to_string(),
                format!("-L{}", kit.join("libdirs/0").display()),
                "-L/usr/lib".to_string(),
                "-o".to_string(),
                "/out/Demo".to_string(),
            ]
        );
        assert_eq!(plan.program, Path::new("cc"));
        assert_eq!(plan.modules, vec!["lumen-fs".to_string()]);
    }

    /// An app declaring nothing links the launcher and the engine alone.
    #[test]
    fn declaring_no_module_forces_no_symbol() {
        let manifest = manifest(unix(), ArtifactKind::Append);
        let plan = plan(
            Path::new("/kit"),
            &manifest,
            &deps(&[]),
            Path::new("/out/Demo"),
            Path::new("/out/scratch"),
        )
        .expect("an app may declare nothing");
        let args = args(&plan);
        assert!(
            !args.iter().any(|a| a.contains("register")),
            "no symbol is forced: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("rlib") || a == "-lasound"),
            "neither module travels: {args:?}"
        );
    }

    #[test]
    fn a_module_the_kit_does_not_carry_names_what_it_carries() {
        let manifest = manifest(unix(), ArtifactKind::Append);
        let error = plan(
            Path::new("/kit"),
            &manifest,
            &deps(&["shape-tools"]),
            Path::new("/out/Demo"),
            Path::new("/out/scratch"),
        )
        .expect_err("the kit carries no shape-tools");
        assert!(error.contains("shape-tools"), "{error}");
        assert!(error.contains("lumen-audio, lumen-fs"), "{error}");
        assert!(error.contains("without --static"), "{error}");
    }

    /// macOS spells the symbol the way the object file does and puts the
    /// artifact on the line, because a signature leaves no room after it.
    #[test]
    fn the_mach_o_line_carries_the_artifact_and_the_underscored_symbol() {
        let driver = Driver {
            kind: DriverKind::Cc,
            flavor: "darwin".to_string(),
            path: None,
        };
        let mut manifest = manifest(driver, ArtifactKind::MachoSection);
        manifest.target = "macos-aarch64".to_string();
        let plan = plan(
            Path::new("/kit"),
            &manifest,
            &deps(&["lumen-fs"]),
            Path::new("/out/Demo"),
            Path::new("/out/app.lmna"),
        )
        .expect("the app declares a module the kit carries");
        let args = args(&plan);
        assert_eq!(args[0], "-Wl,-u,_lumen_module_register_lumen_fs");
        assert_eq!(
            args.last().map(String::as_str),
            Some("-Wl,-sectcreate,__LUMEN,__lmna,/out/app.lmna")
        );
        assert_eq!(plan.artifact, ArtifactKind::MachoSection);
    }

    /// Windows has no `cc` to borrow, so the kit's own LLD runs the line and
    /// the flavor selects the object format it was recorded for.
    #[test]
    fn the_windows_line_runs_through_the_linker_the_kit_ships() {
        let kit = std::env::temp_dir().join(format!("lumen-kit-plan-{}", std::process::id()));
        let bin = kit.join("bin");
        std::fs::create_dir_all(&bin).expect("kit dir");
        std::fs::write(bin.join("rust-lld.exe"), b"linker").expect("write linker");

        let driver = Driver {
            kind: DriverKind::Lld,
            flavor: "link".to_string(),
            path: Some("bin/rust-lld.exe".to_string()),
        };
        let mut manifest = manifest(driver, ArtifactKind::Append);
        manifest.target = "windows-x86_64".to_string();
        let plan = plan(
            &kit,
            &manifest,
            &deps(&["lumen-audio"]),
            Path::new("C:/out/Demo.exe"),
            Path::new("C:/out/scratch"),
        )
        .expect("the app declares a module the kit carries");

        assert_eq!(plan.program, bin.join("rust-lld.exe"));
        let args = args(&plan);
        assert_eq!(args[0], "-flavor");
        assert_eq!(args[1], "link");
        assert_eq!(args[2], "/INCLUDE:lumen_module_register_lumen_audio");
        assert!(args.iter().any(|a| a == "-lasound"), "{args:?}");

        let _ = std::fs::remove_dir_all(&kit);
    }

    /// A kit from another release is refused rather than replayed: its line
    /// names files by fields this build may read differently.
    #[test]
    fn a_kit_of_another_schema_or_another_target_is_refused() {
        let kit = std::env::temp_dir().join(format!("lumen-kit-read-{}", std::process::id()));
        std::fs::create_dir_all(&kit).expect("kit dir");
        let target = Target::parse("linux-x86_64").expect("known target");

        let mut manifest = manifest(unix(), ArtifactKind::Append);
        manifest.schema = SCHEMA_VERSION + 1;
        write_manifest(&kit, &manifest);
        let error = read_manifest(&kit, target).expect_err("a newer schema is refused");
        assert!(error.contains("schema"), "{error}");

        manifest.schema = SCHEMA_VERSION;
        manifest.target = "macos-aarch64".to_string();
        write_manifest(&kit, &manifest);
        let error = read_manifest(&kit, target).expect_err("another platform's kit is refused");
        assert!(error.contains("macos-aarch64"), "{error}");
        assert!(error.contains("linux-x86_64"), "{error}");

        manifest.target = "linux-x86_64".to_string();
        write_manifest(&kit, &manifest);
        assert_eq!(
            read_manifest(&kit, target)
                .expect("its own target reads")
                .target,
            "linux-x86_64"
        );

        std::fs::write(kit.join(MANIFEST), b"{ not a manifest").expect("write");
        let error = read_manifest(&kit, target).expect_err("the manifest does not parse");
        assert!(error.contains(MANIFEST), "{error}");

        let _ = std::fs::remove_dir_all(&kit);
        let error = read_manifest(&kit, target).expect_err("there is no manifest");
        assert!(error.contains("read "), "{error}");
    }

    fn write_manifest(kit: &Path, manifest: &Manifest) {
        std::fs::write(
            kit.join(MANIFEST),
            serde_json::to_string(manifest).expect("encodes"),
        )
        .expect("write manifest");
    }

    /// The named-directory override is what CI and the static-packaging tests
    /// point at a kit they built rather than one a release published.
    #[test]
    fn a_named_directory_is_a_kit_only_when_it_holds_a_manifest() {
        let kit = std::env::temp_dir().join(format!("lumen-kit-env-{}", std::process::id()));
        std::fs::create_dir_all(&kit).expect("kit dir");

        let error = named_kit(&kit).expect_err("the directory holds no manifest");
        assert!(error.contains(MANIFEST), "{error}");
        assert!(error.contains(KIT_DIR_ENV), "{error}");

        write_manifest(&kit, &manifest(unix(), ArtifactKind::Append));
        assert_eq!(named_kit(&kit).expect("the kit is there"), kit);

        let _ = std::fs::remove_dir_all(&kit);
    }

    #[test]
    fn a_driver_this_build_cannot_spell_is_named_rather_than_guessed() {
        let driver = Driver {
            kind: DriverKind::Cc,
            flavor: "wasm".to_string(),
            path: None,
        };
        let error = force_include(&driver, "sym").expect_err("no such flavor here");
        assert!(error.contains("wasm"), "{error}");

        // The same answer where a replay meets it: forcing a module's symbol
        // is the first thing a planned line does.
        let error = plan(
            Path::new("/kit"),
            &manifest(driver, ArtifactKind::Append),
            &deps(&["lumen-fs"]),
            Path::new("/out/Demo"),
            Path::new("/out/scratch"),
        )
        .expect_err("no such flavor here either");
        assert!(error.contains("wasm"), "{error}");
    }

    /// A kit built without any module at all still says what it offers, so a
    /// misspelled name and a kit that carries nothing read differently.
    #[test]
    fn a_kit_that_carries_no_module_says_so() {
        let mut manifest = manifest(unix(), ArtifactKind::Append);
        manifest.modules.clear();
        let error = plan(
            Path::new("/kit"),
            &manifest,
            &deps(&["lumen-fs"]),
            Path::new("/out/Demo"),
            Path::new("/out/scratch"),
        )
        .expect_err("the kit carries nothing");
        assert!(error.contains("offers: nothing"), "{error}");
    }

    #[test]
    fn a_prefix_and_its_path_are_one_argument() {
        assert_eq!(
            joined("-L", Path::new("/usr/lib")),
            OsStr::new("-L/usr/lib")
        );
        assert_eq!(joined("", Path::new("/out/Demo")), OsStr::new("/out/Demo"));
    }

    /// A kit that ships its own linker is checked for carrying it:
    /// the alternative is a driver failure deep in a replay.
    #[test]
    fn a_kit_that_ships_a_linker_is_checked_for_carrying_it() {
        let kit = std::env::temp_dir().join(format!("lumen-kit-driver-{}", std::process::id()));
        let mut driver = Driver {
            kind: DriverKind::Lld,
            flavor: "link".to_string(),
            path: None,
        };
        let error = program(&kit, &driver).expect_err("the kit names no linker");
        assert!(error.contains("names none"), "{error}");

        driver.path = Some("bin/rust-lld.exe".to_string());
        let error = program(&kit, &driver).expect_err("the file is not there");
        assert!(error.contains("is not there"), "{error}");
        assert!(error.contains("rust-lld.exe"), "{error}");
    }

    #[test]
    fn an_artifact_section_is_only_spelled_for_the_driver_that_takes_one() {
        let error =
            sectcreate(&unix(), Path::new("/out/app.lmna")).expect_err("gnu appends instead");
        assert!(error.contains("gnu"), "{error}");
    }

    fn a_plan(program: PathBuf, args: Vec<OsString>, kind: DriverKind) -> Plan {
        Plan {
            program,
            args,
            kind,
            artifact: ArtifactKind::Append,
            exe: PathBuf::from("/out/Demo"),
            modules: Vec::new(),
        }
    }

    /// A program that says `message` and fails, which is what a linker that
    /// could not resolve something does. Spelling it is the platform-specific
    /// part; what the test is about is what the failure reads as.
    #[cfg(unix)]
    fn failing_driver(message: &str) -> (PathBuf, Vec<OsString>) {
        (
            PathBuf::from("/bin/sh"),
            vec![
                OsString::from("-c"),
                OsString::from(format!("echo '{message}' >&2; exit 1")),
            ],
        )
    }

    #[cfg(windows)]
    fn failing_driver(message: &str) -> (PathBuf, Vec<OsString>) {
        (
            PathBuf::from("cmd"),
            vec![
                OsString::from("/c"),
                OsString::from(format!("echo {message} 1>&2 & exit 1")),
            ],
        )
    }

    /// A driver that cannot be started at all is the one failure whose answer
    /// is a piece of software to install, so the message names it.
    #[test]
    fn a_driver_that_cannot_be_started_names_what_to_install() {
        let absent = PathBuf::from("lumen-no-such-driver-on-this-machine");
        let error = link(&a_plan(absent.clone(), Vec::new(), DriverKind::Cc))
            .expect_err("there is no such program");
        assert!(error.contains("cannot run cc"), "{error}");
        let toolchain = if cfg!(target_os = "macos") {
            "Xcode"
        } else {
            "C toolchain"
        };
        assert!(error.contains(toolchain), "{error}");

        let error = link(&a_plan(absent, Vec::new(), DriverKind::Lld))
            .expect_err("there is no such program");
        assert!(error.contains("the linker the link kit ships"), "{error}");
    }

    /// A replay that ran and failed reports the driver's own words. The one
    /// failure that means the Windows SDK is not installed says so, because no
    /// kit is allowed to carry those libraries.
    #[test]
    fn a_replay_that_fails_reports_what_the_driver_said() {
        let (program, args) = failing_driver("could not open kernel32.lib");
        let error = link(&a_plan(program, args, DriverKind::Lld)).expect_err("the driver failed");
        assert!(error.contains("Windows SDK"), "{error}");
        assert!(error.contains("kernel32.lib"), "{error}");

        let (program, args) = failing_driver("undefined reference to main");
        let error = link(&a_plan(program, args, DriverKind::Cc)).expect_err("the driver failed");
        assert!(error.contains("linking"), "{error}");
        assert!(error.contains("Demo"), "{error}");
        assert!(error.contains("undefined reference to main"), "{error}");
    }
}
