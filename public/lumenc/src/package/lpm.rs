//! `lpm`, the client of the package registry at `reg.lumenfx.dev`.
//!
//! A `version` source in `[dependencies]` or `[[plugins]]` names a package in
//! the registry. `lpm` is what resolves those requirements, downloads what
//! they resolve to, and keeps `lumen.lock`. `lumenc` resolves nothing itself:
//! it reads the requirements out of `lumen.toml`, hands them to `lpm` on the
//! command line, and reads the JSON that comes back.
//!
//! `lpm` never reads `lumen.toml`. Every requirement crosses as a `--req`
//! argument, so the two programs share a command line rather than a file
//! format, and a change to `lumen.toml` is `lumenc`'s business alone.
//!
//! One `lpm` serves every app on the machine, at `~/.local/bin/lpm`
//! (`%LOCALAPPDATA%\Programs\lpm\lpm.exe` on Windows). `LPM_BIN` names another
//! copy, PATH is searched next, and that shared path answers last; when
//! nothing answers, or the copy that does is older than [`MIN_VERSION`], the
//! newest release is downloaded there and verified against the checksums
//! published beside it.
//!
//! What comes back:
//!
//! - A `lumen`-platform package from `[dependencies]` is a runtime module or
//!   a portable plugin; the runtime's loader tells the two apart by the
//!   symbols the library exports, so both arrive here as one library file.
//! - A `lumen`-platform package from `[[plugins]]` is a compiler plugin,
//!   which `lumenc` opens while it compiles.
//! - A `candela`-platform package is a script library, and becomes an import
//!   root the candela host compiles against.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

/// The repository `lpm` is published from, unless `LPM_GH_REPO` names another.
const DEFAULT_REGISTRY_REPO: &str = "lumen-fx/registry";

/// The oldest `lpm` that speaks the protocol this module drives. A copy on
/// this machine older than this is replaced with the newest release. 0.1.0
/// had no `install`, so it cannot answer for anything here.
pub const MIN_VERSION: &str = "0.2.0";

/// What to run when `lpm` is needed and cannot be installed.
pub const INSTALL_HINT: &str = "curl -fsSL https://reg.lumenfx.dev/install.sh | sh";

/// The checksum list published beside the `lpm` archives.
const CHECKSUMS: &str = "checksums.txt";

/// The JSON schema this `lumenc` reads. A newer `lpm` that changed the shape
/// says so in its own `schema` field, and the mismatch is an error rather
/// than a silent misread.
const SCHEMA: u32 = 1;

/// Whether `--offline` was passed on this invocation.
///
/// The compile paths resolve automatically, several calls below argument
/// parsing, and the flag is a property of the invocation rather than of any
/// one of them. The CLI records it here once, before it dispatches.
static OFFLINE: AtomicBool = AtomicBool::new(false);

/// Record that `--offline` was passed.
pub fn set_offline(offline: bool) {
    OFFLINE.store(offline, Ordering::Relaxed);
}

/// Whether this invocation runs offline.
pub fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

/// Which table declared a requirement. It is what says the kind of the
/// `lumen`-platform package the requirement resolves to; the registry knows
/// the platform, and the app says what it wants the package for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// `[dependencies]`: loaded into the running app.
    Dependencies,
    /// `[[plugins]]`: loaded into `lumenc` while it compiles.
    Plugins,
}

/// One `version` requirement, and where it was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Registry package name, which is the table key or the `name` field.
    pub name: String,
    /// The requirement text as written, in cargo semantics.
    pub req: String,
    /// The table it came from.
    pub table: Table,
}

/// How a resolution is allowed to reach the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mode {
    /// Fail rather than let the resolution change `lumen.lock`.
    pub locked: bool,
    /// Use what is already downloaded and never reach the network.
    pub offline: bool,
}

impl Mode {
    /// The mode the compile paths resolve in: the lock may grow, and
    /// `--offline` is honoured.
    pub fn of_invocation() -> Mode {
        Mode {
            locked: false,
            offline: offline(),
        }
    }
}

/// One package `lpm` resolved, as it reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    /// Registry package name.
    pub name: String,
    /// The exact version the requirement resolved to.
    pub version: String,
    /// `lumen` for a native library, `candela` for a script library.
    pub platform: String,
    /// The target this copy was built for, or `any`.
    pub target: String,
    /// The package root on disk.
    pub dir: PathBuf,
    /// The files at that root, as `lpm` unpacked them.
    #[serde(default)]
    pub files: Vec<String>,
    /// What this package itself depends on, name to exact version.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

impl Package {
    /// The library file a `lumen`-platform package carries at its root,
    /// probed under the spellings every by-name lookup in Lumen shares.
    pub fn library(&self) -> Result<PathBuf, String> {
        lumen_modules::library_spellings(&self.name)
            .iter()
            .map(|file| self.dir.join(file))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                format!(
                    "package '{}' {} holds no library for {} at {}; the package carries: {}",
                    self.name,
                    self.version,
                    self.target,
                    self.dir.display(),
                    if self.files.is_empty() {
                        "nothing".to_string()
                    } else {
                        self.files.join(", ")
                    }
                )
            })
    }
}

/// What an app's `version` sources resolved to, sorted into the three things
/// `lumenc` does with a package.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// `[dependencies]` entries on the `lumen` platform: the library the
    /// running app loads, keyed by the declared name.
    pub modules: BTreeMap<String, PathBuf>,
    /// `[[plugins]]` entries on the `lumen` platform: the cdylib `lumenc`
    /// opens while it compiles, keyed by the declared name.
    pub compiler_plugins: BTreeMap<String, PathBuf>,
    /// `candela`-platform packages: the name a script imports under, and the
    /// directory its `.cdl` files sit in.
    pub candela_roots: Vec<(String, PathBuf)>,
    /// `[dependencies]` entries on the `lumen` platform: the module's root,
    /// which a web build reads the module's web half from, keyed by the
    /// declared name.
    pub roots: BTreeMap<String, PathBuf>,
    /// The exact version every resolved package settled on, transitive ones
    /// included. `lumenc add` writes the answer back into `lumen.toml` when
    /// the author named no requirement.
    pub versions: BTreeMap<String, String>,
}

/// The lock `lpm` writes, read back for the JSON it prints.
#[derive(Debug, Deserialize)]
struct Resolution {
    schema: u32,
    #[serde(default)]
    packages: Vec<Package>,
}

/// This machine's target, spelled the way the release assets are.
pub fn host_target() -> &'static str {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    match (os, arch) {
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", _) => "windows-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", _) => "macos-x86_64",
        (_, "aarch64") => "linux-aarch64",
        _ => "linux-x86_64",
    }
}

/// Resolve `reqs` for `target`, downloading whatever the app is missing, and
/// sort the answer by what each package is for.
///
/// An empty requirement list runs nothing: an app with no `version` source
/// never looks for `lpm`, never reaches the network, and gets no lock.
pub fn resolve(
    app_dir: &Path,
    target: &str,
    reqs: &[Requirement],
    mode: Mode,
) -> Result<Resolved, String> {
    sort(reqs, ask(Ask::Install, app_dir, target, reqs, &[], mode)?)
}

/// Re-resolve the app's requirements, letting the lock move to newer
/// versions. An empty `names` list moves every package the app declares.
pub fn update(
    app_dir: &Path,
    target: &str,
    reqs: &[Requirement],
    names: &[String],
    mode: Mode,
) -> Result<Resolved, String> {
    sort(reqs, ask(Ask::Update, app_dir, target, reqs, names, mode)?)
}

/// Which of the two resolving subcommands is being run. They take the same
/// arguments and answer in the same JSON; they differ in whether a pinned
/// version is allowed to move.
#[derive(Debug, Clone, Copy)]
enum Ask {
    /// Honour the lock and download whatever it pins.
    Install,
    /// Let the lock move to the newest version each requirement allows.
    Update,
}

/// Run one resolving subcommand and hand back the packages it reports.
fn ask(
    what: Ask,
    app_dir: &Path,
    target: &str,
    reqs: &[Requirement],
    names: &[String],
    mode: Mode,
) -> Result<Vec<Package>, String> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<String> = vec![
        match what {
            Ask::Install => "install".to_string(),
            Ask::Update => "update".to_string(),
        },
        "--lock".to_string(),
        app_dir.join(LOCK_FILE).display().to_string(),
        "--target".to_string(),
        target.to_string(),
        "--host".to_string(),
        format!("lumen@{}", env!("CARGO_PKG_VERSION")),
    ];
    for req in reqs {
        args.push("--req".to_string());
        args.push(format!("{}@{}", req.name, req.req));
    }
    if mode.locked {
        args.push("--locked".to_string());
    }
    if mode.offline {
        args.push("--offline".to_string());
    }
    args.push("--json".to_string());
    args.extend(names.iter().cloned());

    let stdout = run(&args)?;
    let resolution: Resolution = serde_json::from_str(&stdout)
        .map_err(|e| format!("lpm printed JSON this lumenc cannot read: {e}"))?;
    if resolution.schema != SCHEMA {
        return Err(format!(
            "lpm speaks resolution schema {} and this lumenc reads {SCHEMA}; update lumenc",
            resolution.schema
        ));
    }
    Ok(resolution.packages)
}

/// The lock file `lpm` writes, beside `lumen.toml`.
pub const LOCK_FILE: &str = "lumen.lock";

/// Sort resolved packages by what the app asked each one for.
///
/// A package the app declared is keyed by the table that declared it. A
/// package it did not declare is a dependency of one that it did: a
/// `candela` one still becomes an import root, because a script library
/// imports its own dependencies, and a `lumen` one belongs to whatever
/// declared it and is not loaded by name here.
fn sort(reqs: &[Requirement], packages: Vec<Package>) -> Result<Resolved, String> {
    let mut resolved = Resolved::default();
    for package in packages {
        resolved
            .versions
            .insert(package.name.clone(), package.version.clone());
        let declared = reqs.iter().find(|r| r.name == package.name);
        match package.platform.as_str() {
            "candela" => resolved.candela_roots.push((package.name, package.dir)),
            "lumen" => match declared.map(|r| r.table) {
                // A module with a web half and no library is for pages alone;
                // one with neither is a package that cannot have been meant.
                Some(Table::Dependencies) => {
                    match package.library() {
                        Ok(library) => {
                            resolved.modules.insert(package.name.clone(), library);
                        }
                        Err(_) if lumen_modules::addon::web_half(&package.dir).is_some() => {}
                        Err(e) => return Err(e),
                    }
                    resolved
                        .roots
                        .insert(package.name.clone(), package.dir.clone());
                }
                Some(Table::Plugins) => {
                    resolved
                        .compiler_plugins
                        .insert(package.name.clone(), package.library()?);
                }
                None => {}
            },
            other => {
                return Err(format!(
                    "package '{}' is for the {other} platform, which this lumenc has nothing to \
                     do with; a Lumen app takes `lumen` packages (modules, portable plugins, \
                     compiler plugins) and `candela` packages (script libraries)",
                    package.name
                ));
            }
        }
    }
    resolved.candela_roots.sort();
    resolved.candela_roots.dedup();
    Ok(resolved)
}

// ============================================================
// Running lpm
// ============================================================

/// Run `lpm` with `args` and hand back its stdout.
///
/// `lpm` prints one line on stderr when it fails, and the exit code says what
/// kind of failure it was. Both cross into the message, so a `lumenc` user
/// reads the registry's own words rather than a wrapper's paraphrase.
fn run(args: &[String]) -> Result<String, String> {
    let bin = binary()?;
    let out = std::process::Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let why = match out.status.code() {
        Some(2) => "lumenc asked lpm for something it does not understand; update lpm",
        Some(3) => {
            "the lock would have to change, and --locked says it must not; run `lumenc update`"
        }
        Some(4) => "--offline, and something the app needs has not been downloaded yet",
        _ => "",
    };
    Err(if why.is_empty() {
        format!("lpm: {said}")
    } else if said.is_empty() {
        format!("lpm: {why}")
    } else {
        format!("lpm: {said} ({why})")
    })
}

/// The `lpm` to run: `$LPM_BIN`, then PATH, then the one shared path. A copy
/// older than [`MIN_VERSION`], or none at all, is downloaded to the shared
/// path first.
pub fn binary() -> Result<PathBuf, String> {
    if let Some(named) = std::env::var_os("LPM_BIN").filter(|v| !v.is_empty()) {
        let named = PathBuf::from(named);
        if !named.is_file() {
            return Err(format!(
                "LPM_BIN names {}, which is not a file",
                named.display()
            ));
        }
        return Ok(named);
    }
    for candidate in [on_path(), shared_path()].into_iter().flatten() {
        if version_of(&candidate).is_some_and(|v| !older_than_min(&v)) {
            return Ok(candidate);
        }
    }
    let shared = shared_path().ok_or_else(|| {
        format!(
            "lpm is needed to resolve this app's registry dependencies, and there is nowhere \
             to install it on this machine. Install it by hand: {INSTALL_HINT}"
        )
    })?;
    download(&shared).map_err(|why| {
        format!("lpm could not be installed: {why}. Install it by hand: {INSTALL_HINT}")
    })?;
    Ok(shared)
}

/// `lpm` on PATH, if one is there.
fn on_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "lpm.exe" } else { "lpm" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
}

/// The one path every app on this machine shares: `~/.local/bin/lpm`, or
/// `%LOCALAPPDATA%\Programs\lpm\lpm.exe` on Windows.
pub fn shared_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|d| {
                PathBuf::from(d)
                    .join("Programs")
                    .join("lpm")
                    .join("lpm.exe")
            })
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(|d| PathBuf::from(d).join(".local").join("bin").join("lpm"))
    }
}

/// What `lpm --version` says, as `X.Y.Z`. `None` when the file does not run
/// or answers with something else.
fn version_of(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version_line(&String::from_utf8_lossy(&out.stdout))
}

/// The version in `lpm version X.Y.Z (...)`.
fn parse_version_line(line: &str) -> Option<String> {
    let word = line.split_whitespace().nth(2)?;
    (!word.is_empty() && word.split('.').count() == 3).then(|| word.to_string())
}

/// Whether `version` predates [`MIN_VERSION`], comparing the three numbers.
fn older_than_min(version: &str) -> bool {
    let read = |v: &str| -> (u64, u64, u64) {
        let mut parts = v
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    };
    read(version) < read(MIN_VERSION)
}

// ============================================================
// Installing lpm
// ============================================================

/// Download the newest `lpm` release and put the executable at `to`.
///
/// The archive is verified against the checksums published beside it, the
/// same rule the toolchain installer follows: nothing is installed that the
/// publisher did not vouch for.
fn download(to: &Path) -> Result<(), String> {
    let (base, version) = match asset_base_override() {
        // An override names a directory of archives rather than a release, so
        // the version in the file name comes from the archives themselves.
        Some(base) => {
            let version = probe_version(&base)?;
            (base, version)
        }
        None => {
            let repo = registry_repo();
            let tag = crate::package::release::latest_tag(&repo)
                .ok_or_else(|| format!("could not read the newest release of {repo}"))?;
            let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
            (
                format!("https://github.com/{repo}/releases/download/{tag}"),
                version,
            )
        }
    };
    install_from(&base, &version, to)
}

/// Fetch `lpm <version>` from `base`, check it against the checksums
/// published beside it, and leave the executable at `to`.
///
/// Split from the choice of release above so the download and the
/// verification are the same code whether the archives came from a release or
/// from a mirror.
fn install_from(base: &str, version: &str, to: &Path) -> Result<(), String> {
    let archive = archive_name(version);
    let dir = to
        .parent()
        .ok_or_else(|| format!("{} has no directory to install into", to.display()))?;
    let exe = [if cfg!(windows) {
        "lpm.exe".to_string()
    } else {
        "lpm".to_string()
    }];
    crate::package::cli::fetch_verified_archive(
        &crate::package::cli::Publisher {
            base: base.to_string(),
            sums: CHECKSUMS.to_string(),
            name: format!("lpm {version}"),
            hint: "Install lpm by hand instead.".to_string(),
        },
        &archive,
        &crate::package::cli::Members::flat(&exe),
        dir,
    )?;
    make_executable(&dir.join(&exe[0]))
}

/// The archive `lpm` publishes for this machine: `lpm_<version>_<os>_<arch>`
/// with `.zip` on Windows and `.tar.gz` everywhere else. The spelling is Go's,
/// because `lpm` is a Go program released by GoReleaser.
fn archive_name(version: &str) -> String {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let ext = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("lpm_{version}_{os}_{arch}.{ext}")
}

/// A directory of `lpm` archives to install from instead of the newest
/// release, named by `LPM_ASSET_BASE`. A mirror uses it; so does a test with
/// a local server.
fn asset_base_override() -> Option<String> {
    std::env::var("LPM_ASSET_BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string())
}

/// The version an overriding asset directory publishes, read off the
/// checksum list: every line names an archive, and the archives carry the
/// version in their names.
fn probe_version(base: &str) -> Result<String, String> {
    let url = format!("{base}/{CHECKSUMS}");
    let bytes =
        crate::package::cli::http_get(&url).map_err(|e| format!("cannot download {url}: {e}"))?;
    let sums = String::from_utf8_lossy(&bytes);
    sums.lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter_map(|name| name.strip_prefix("lpm_"))
        .filter_map(|rest| rest.split('_').next())
        .map(str::to_string)
        .next()
        .ok_or_else(|| format!("{url} names no lpm archive"))
}

/// Give the installed executable its executable bit. Windows needs none.
fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// The repository `lpm` is published from.
fn registry_repo() -> String {
    std::env::var("LPM_GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY_REPO.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_gives_up_its_number() {
        assert_eq!(
            parse_version_line("lpm version 1.2.3 (abcdef, 2026-09-01)\n").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            parse_version_line("lpm version 0.1.0").as_deref(),
            Some("0.1.0")
        );
    }

    /// A line in another shape is not read as a version: an unrecognised
    /// answer means the copy is replaced, which is safe, rather than trusted.
    #[test]
    fn anything_else_is_not_a_version() {
        assert_eq!(parse_version_line(""), None);
        assert_eq!(parse_version_line("lpm 1.2.3"), None);
        assert_eq!(parse_version_line("lpm version next"), None);
    }

    #[test]
    fn older_copies_are_the_ones_replaced() {
        assert!(older_than_min("0.0.1"));
        assert!(!older_than_min(MIN_VERSION));
        assert!(!older_than_min("99.0.0"));
    }

    #[test]
    fn the_archive_name_is_gos_spelling() {
        let name = archive_name("1.4.0");
        assert!(name.starts_with("lpm_1.4.0_"), "{name}");
        for word in ["linux", "darwin", "windows"] {
            if name.contains(word) {
                return;
            }
        }
        panic!("{name} names no operating system");
    }

    /// Every target `lpm` publishes for is one the release assets name, so a
    /// package resolved for the host lands under a directory a package can be
    /// published to.
    #[test]
    fn the_host_target_is_a_published_spelling() {
        let target = host_target();
        let (os, arch) = target.split_once('-').expect("<os>-<arch>");
        assert!(["linux", "macos", "windows"].contains(&os), "{target}");
        assert!(["x86_64", "aarch64"].contains(&arch), "{target}");
    }

    fn req(name: &str, table: Table) -> Requirement {
        Requirement {
            name: name.to_string(),
            req: "1".to_string(),
            table,
        }
    }

    fn package(name: &str, platform: &str, dir: &Path) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            platform: platform.to_string(),
            target: host_target().to_string(),
            dir: dir.to_path_buf(),
            files: Vec::new(),
            dependencies: BTreeMap::new(),
        }
    }

    /// A `lumen` package holding a web half and no library is a module for
    /// pages alone: nothing to load, and its root is what a web build reads
    /// the web half from.
    #[test]
    fn a_lumen_package_with_only_a_web_half_has_a_root_and_no_library() {
        let dir = std::env::temp_dir().join(format!("lumenc-lpm-web-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let web = dir.join(lumen_modules::addon::WEB_DIR);
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join(lumen_modules::addon::ADDON_MANIFEST), b"").unwrap();

        let resolved = sort(
            &[req("chart", Table::Dependencies)],
            vec![package("chart", "lumen", &dir)],
        )
        .unwrap();
        assert_eq!(resolved.roots.get("chart"), Some(&dir));
        assert!(resolved.modules.is_empty());

        // Neither a library nor a web half: nothing a build could use.
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = sort(
            &[req("none", Table::Dependencies)],
            vec![package("none", "lumen", &empty)],
        )
        .unwrap_err();
        assert!(err.contains("holds no library"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The table a requirement came from is what tells a module from a
    /// compiler plugin: the registry says only that both are `lumen`.
    #[test]
    fn the_declaring_table_settles_what_a_lumen_package_is() {
        let dir = std::env::temp_dir().join(format!("lumenc-lpm-sort-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join(&lumen_modules::library_spellings("shape")[0]);
        std::fs::write(&lib, b"").unwrap();
        let plugin = dir.join(&lumen_modules::library_spellings("md")[0]);
        std::fs::write(&plugin, b"").unwrap();

        let reqs = [req("shape", Table::Dependencies), req("md", Table::Plugins)];
        let resolved = sort(
            &reqs,
            vec![
                package("shape", "lumen", &dir),
                package("md", "lumen", &dir),
                package("fmt", "candela", &dir),
            ],
        )
        .unwrap();
        assert_eq!(resolved.modules.get("shape"), Some(&lib));
        assert_eq!(resolved.roots.get("shape"), Some(&dir));
        assert_eq!(resolved.compiler_plugins.get("md"), Some(&plugin));
        assert_eq!(
            resolved.candela_roots,
            vec![("fmt".to_string(), dir.clone())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_platform_lumenc_has_no_use_for_names_itself() {
        let reqs = [req("wat", Table::Dependencies)];
        let err = sort(&reqs, vec![package("wat", "zig", Path::new("/tmp"))]).unwrap_err();
        assert!(err.contains("'wat'"), "{err}");
        assert!(err.contains("zig platform"), "{err}");
    }

    /// A `lumen` package the app never declared is a dependency of one it
    /// did; whatever declared it loads it, so nothing here is keyed by its
    /// name.
    #[test]
    fn an_undeclared_lumen_package_is_not_keyed_by_name() {
        let resolved = sort(&[], vec![package("geom", "lumen", Path::new("/tmp"))]).unwrap();
        assert!(resolved.modules.is_empty());
        assert!(resolved.compiler_plugins.is_empty());
    }

    /// An app with no `version` source never looks for lpm, so a machine
    /// without one still runs every app that does not need it.
    #[test]
    fn nothing_runs_without_a_version_source() {
        let resolved = resolve(
            Path::new("/nonexistent"),
            host_target(),
            &[],
            Mode::default(),
        )
        .expect("no requirements, no lpm");
        assert!(resolved.modules.is_empty());
        assert!(resolved.candela_roots.is_empty());
    }

    /// The install path end to end, against a server on this machine: the
    /// archive is downloaded, checked against the checksums published beside
    /// it, unpacked, and left runnable. An archive whose bytes do not match
    /// what the publisher vouched for installs nothing.
    #[cfg(unix)]
    #[test]
    fn a_published_lpm_installs_and_a_tampered_one_does_not() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("lumenc-lpm-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");

        let version = "1.2.3";
        let archive = archive_name(version);
        let tarball = tar_gz_of("lpm", b"#!/bin/sh\necho lpm\n");

        let base = serve(vec![
            (
                format!("/{CHECKSUMS}"),
                format!("{}  {archive}\n", sha256_hex(&tarball)).into_bytes(),
            ),
            (format!("/{archive}"), tarball.clone()),
        ]);
        let into = dir.join("bin").join("lpm");
        install_from(&base, version, &into).expect("the published archive installs");
        assert!(
            into.is_file(),
            "the executable landed at {}",
            into.display()
        );
        let mode = std::fs::metadata(&into).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the copy is runnable: {mode:o}");

        // The same archive, published under a checksum that does not cover it.
        let base = serve(vec![
            (
                format!("/{CHECKSUMS}"),
                format!("{}  {archive}\n", "0".repeat(64)).into_bytes(),
            ),
            (format!("/{archive}"), tarball),
        ]);
        let into = dir.join("tampered").join("lpm");
        let err = install_from(&base, version, &into).expect_err("the checksum does not match");
        assert!(err.contains("does not match the checksum"), "{err}");
        assert!(!into.exists(), "nothing was installed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The version an overriding asset directory publishes is read off the
    /// archive names in its checksum list, because a directory of archives
    /// carries no release tag to take it from.
    #[cfg(unix)]
    #[test]
    fn a_mirror_gives_up_its_version() {
        let base = serve(vec![(
            format!("/{CHECKSUMS}"),
            b"aaaa  lpm_2.5.1_linux_amd64.tar.gz\nbbbb  lpm_2.5.1_darwin_arm64.tar.gz\n".to_vec(),
        )]);
        assert_eq!(probe_version(&base).as_deref(), Ok("2.5.1"));
    }

    /// Serve `files` on a loopback port, one request each, and hand back the
    /// base URL they sit under.
    #[cfg(unix)]
    fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let base = format!("http://{}", listener.local_addr().expect("the bound port"));
        std::thread::spawn(move || {
            for _ in 0..files.len() {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut head = [0u8; 1024];
                let read = socket.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let body = files.iter().find(|(name, _)| name == path);
                let head = match &body {
                    Some((_, bytes)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                             Connection: close\r\n\r\n"
                        .to_string(),
                };
                let _ = socket.write_all(head.as_bytes());
                if let Some((_, bytes)) = body {
                    let _ = socket.write_all(bytes);
                }
                let _ = socket.flush();
            }
        });
        base
    }

    /// A one-member `.tar.gz`, shaped the way a release publishes one.
    #[cfg(unix)]
    fn tar_gz_of(name: &str, body: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_data(&mut header, name, body)
            .expect("append");
        let tar = builder.into_inner().expect("finish the tar");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).expect("compress");
        encoder.finish().expect("finish the gzip")
    }

    #[cfg(unix)]
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }
}
