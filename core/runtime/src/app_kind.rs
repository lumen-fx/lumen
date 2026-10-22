//! App-kind detection and build/run dispatch for SDK-authored apps.
//!
//! `lumenc`'s CLI historically assumed every app directory was a pure-markup
//! app (`main.lmn` + `lumen.toml`) driven by the built-in runtime. The Rust,
//! C++, and Python SDKs bypass the CLI entirely today: a Rust app is a cargo
//! bin depending on the `lumenui` crate, a C++ app is a CMake project linking
//! `liblumen`, and a Python app is a script importing `lumen`.
//!
//! This module closes that gap. [`detect`] inspects an app directory and
//! classifies it as [`AppKind::Markup`], [`AppKind::Rust`], [`AppKind::Cpp`],
//! or [`AppKind::Python`]. The CLI (`lumenc run` / `lumenc build`) resolves the
//! kind - an optional `[app] kind` override in `lumen.toml` wins, otherwise
//! auto-detection decides - and reroutes non-markup apps to their native
//! toolchain (`cargo`, `cmake`, the Python interpreter) instead of the markup
//! runtime.
//!
//! # Detection precedence
//!
//! SDK project markers take precedence over the markup fallback, because SDK
//! app directories *also* carry `main.lmn` / `lumen.toml` for the runtime to
//! load at boot. The order is Rust, then C++, then Python, then Markup:
//!
//! 1. **Rust** - `Cargo.toml` whose dependency tables name `lumenui` or
//!    `lumen`.
//! 2. **C++** - a `CMakeLists.txt`, or any `.cpp`/`.cc`/`.cxx` that includes a
//!    `lumen` header.
//! 3. **Python** - any `.py` that imports `lumen`, or a `pyproject.toml`
//!    mentioning `lumen`.
//! 4. **Markup** - the fallback (`main.lmn` + optional `main.css`).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// How a Lumen app directory is authored. Drives the CLI's build/run reroute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    /// Pure-markup app: `main.lmn` (+ optional `main.css` / inline `<script>`)
    /// driven by the built-in `lumenc` runtime. The default.
    #[default]
    Markup,
    /// Rust SDK app: a cargo bin depending on the `lumenui` crate. Built/run
    /// via `cargo`.
    Rust,
    /// C++ SDK app: a CMake project linking `liblumen`. Built via `cmake`.
    Cpp,
    /// Python SDK app: a script importing `lumen` (ctypes over `liblumen`).
    /// Run via the Python interpreter.
    Python,
}

/// Resolve the effective kind for `dir`: an explicit override (from
/// `lumen.toml`'s `[app] kind`) always wins; otherwise [`detect`] decides.
pub fn resolve(dir: &Path, override_kind: Option<AppKind>) -> AppKind {
    override_kind.unwrap_or_else(|| detect(dir))
}

/// Classify the app directory by inspecting its files. See the module docs for
/// the precedence rules and heuristics.
pub fn detect(dir: &Path) -> AppKind {
    if is_rust(dir) {
        AppKind::Rust
    } else if is_cpp(dir) {
        AppKind::Cpp
    } else if is_python(dir) {
        AppKind::Python
    } else {
        AppKind::Markup
    }
}

// -- Per-kind detectors ------------------------------------------------------

/// A `Cargo.toml` whose dependency tables reference `lumenui` / `lumen`.
fn is_rust(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join("Cargo.toml")) {
        Ok(src) => cargo_depends_on_lumen(&src),
        Err(_) => false,
    }
}

/// True when `Cargo.toml` source declares a `lumenui` or `lumen` dependency
/// in any of its dependency tables (including `[target.*]` blocks). Parses the
/// manifest as TOML; falls back to a line scan when the manifest is partial /
/// unparseable.
fn cargo_depends_on_lumen(src: &str) -> bool {
    // `toml::from_str` parses a document; `str::parse::<Value>` parses a single
    // TOML value and would reject every real manifest, silently dropping this
    // function to its line-scan fallback.
    if let Ok(val) = toml::from_str::<toml::Value>(src) {
        if table_has_lumen_dep(&val) {
            return true;
        }
        // Platform-gated deps: [target.'cfg(...)'.dependencies] etc.
        if let Some(targets) = val.get("target").and_then(|v| v.as_table()) {
            for tv in targets.values() {
                if table_has_lumen_dep(tv) {
                    return true;
                }
            }
        }
        return false;
    }
    // Unparseable manifest (edited mid-flight): conservative line scan.
    src.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("lumen ")
            || t.starts_with("lumen=")
            || t.starts_with("lumen.")
            || t.starts_with("lumenui")
            || t.starts_with("\"lumen\"")
            || t.starts_with("\"lumenui\"")
    })
}

/// Inspect the `dependencies` / `dev-dependencies` / `build-dependencies`
/// tables of a manifest (or a `[target.*]` sub-table) for a `lumen` dep key.
fn table_has_lumen_dep(val: &toml::Value) -> bool {
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = val.get(table).and_then(|v| v.as_table())
            && t.keys().any(|k| is_lumen_dep_key(k))
        {
            return true;
        }
    }
    false
}

/// A dependency key that identifies a Lumen SDK app.
fn is_lumen_dep_key(key: &str) -> bool {
    key == "lumen" || key == "lumenui"
}

/// A `CMakeLists.txt`, or a C++ source that includes a `lumen` header.
fn is_cpp(dir: &Path) -> bool {
    if dir.join("CMakeLists.txt").is_file() {
        return true;
    }
    dir_has_matching_file(dir, &["cpp", "cc", "cxx"], |body| {
        body.contains("lumen.hpp") || body.contains("lumen.h") || body.contains("<lumen")
    })
}

/// A `.py` importing `lumen`, or a `pyproject.toml` naming `lumen`.
fn is_python(dir: &Path) -> bool {
    if dir_has_matching_file(dir, &["py"], |body| {
        body.contains("import lumen") || body.contains("from lumen")
    }) {
        return true;
    }
    match std::fs::read_to_string(dir.join("pyproject.toml")) {
        Ok(src) => src.contains("lumen"),
        Err(_) => false,
    }
}

/// True when `dir` holds at least one immediate-child file with one of `exts`
/// whose contents satisfy `pred`. Non-recursive; unreadable files are skipped.
fn dir_has_matching_file(dir: &Path, exts: &[&str], pred: impl Fn(&str) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let has_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.iter().any(|want| e.eq_ignore_ascii_case(want)));
        if has_ext
            && let Ok(body) = std::fs::read_to_string(&path)
            && pred(&body)
        {
            return true;
        }
    }
    false
}

// -- Dispatch: build the external toolchain command sequence per kind ---------

/// Whether the CLI is dispatching a `run` or a `build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `lumenc run <dir>` - launch the app.
    Run,
    /// `lumenc build <dir>` - produce a release binary/artifact.
    Build,
}

/// A single external process to spawn: program, args, working directory, and
/// extra environment. Kept as plain data (rather than a live [`Command`]) so
/// the dispatch tables are cheap to unit-test by asserting on argv / cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Executable name (resolved via `PATH`).
    pub program: String,
    /// Ordered arguments.
    pub args: Vec<String>,
    /// Working directory the child runs in.
    pub cwd: PathBuf,
    /// Extra environment overrides applied on top of the inherited env.
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    fn new(program: &str, cwd: &Path) -> Self {
        Self {
            program: program.to_string(),
            args: Vec::new(),
            cwd: cwd.to_path_buf(),
            env: Vec::new(),
        }
    }

    fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    fn envv(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    /// Materialise into a runnable [`Command`].
    fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).current_dir(&self.cwd);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}

/// Build the external command sequence for `kind` in `mode`, run from `dir`.
///
/// Returns `Err` for [`AppKind::Markup`] (the caller must take the built-in
/// runtime path instead) and for unresolved inputs (e.g. a Python app with no
/// discoverable entry script). An `Ok(vec![])` is a valid no-op (e.g. a Python
/// `build`, which has nothing to compile).
pub fn dispatch(kind: AppKind, dir: &Path, mode: Mode) -> Result<Vec<CommandSpec>, String> {
    match kind {
        AppKind::Markup => {
            Err("markup apps use the built-in runtime, not an external toolchain".into())
        }
        AppKind::Rust => Ok(rust_specs(dir, mode)),
        AppKind::Cpp => Ok(cpp_specs(dir, mode)),
        AppKind::Python => python_specs(dir, mode),
    }
}

/// Rust: `cargo run` (Run) / `cargo build --release` (Build), in the app dir.
///
/// `lumenui` links the release's prebuilt engine statically, so the
/// executable cargo writes carries the whole runtime and needs no library
/// beside it. The app's build script compiles its UI with a `lumenc`
/// ([`LUMENC_ENV`]); this sets the one running now, so a build started from
/// `lumenc` uses the compiler it was started from instead of looking for one.
fn rust_specs(dir: &Path, mode: Mode) -> Vec<CommandSpec> {
    let spec = match mode {
        Mode::Run => CommandSpec::new("cargo", dir).arg("run"),
        Mode::Build => CommandSpec::new("cargo", dir).arg("build").arg("--release"),
    };
    let spec = match std::env::current_exe() {
        Ok(exe) => spec.envv(LUMENC_ENV, exe.to_string_lossy()),
        Err(_) => spec,
    };
    vec![spec]
}

/// The variable naming the `lumenc` a Rust app's build script compiles the
/// app's UI with (`lumenui_build::compile`). Unset, the build script looks
/// for one of its own version.
pub const LUMENC_ENV: &str = "LUMEN_LUMENC";

// -- Headless handoff --------------------------------------------------------

/// The variable that asks an SDK app's own process for a rendered headless
/// run. Set, to any value, it means no window.
pub const HEADLESS_ENV: &str = "LUMEN_HEADLESS";
/// The `--ticks N` of a handed-off headless run.
pub const HEADLESS_TICKS_ENV: &str = "LUMEN_HEADLESS_TICKS";
/// The `--dpr N` of a handed-off headless run.
pub const HEADLESS_DPR_ENV: &str = "LUMEN_HEADLESS_DPR";
/// The `--size WxH` of a handed-off headless run.
pub const HEADLESS_SIZE_ENV: &str = "LUMEN_HEADLESS_SIZE";

/// `lumenc run --headless` for an app that is its own program.
///
/// A markup app runs headless inside lumenc. A Rust SDK app is a separate
/// executable that cargo builds and starts, so lumenc sets these variables on
/// that process instead, and the SDK's `App::run` reads them and takes the
/// rendered headless path with the same options. Both sides go through this
/// type, so the names and the value formats live in one place.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeadlessHandoff {
    /// Run exactly this many ticks, then close.
    pub ticks: Option<u64>,
    /// Device pixel ratio of the offscreen target.
    pub dpr: Option<f32>,
    /// Logical viewport size.
    pub size: Option<(u32, u32)>,
}

impl HeadlessHandoff {
    /// The variables that hand this run to the child process.
    pub fn to_env(self) -> Vec<(String, String)> {
        let mut env = vec![(HEADLESS_ENV.to_string(), "1".to_string())];
        if let Some(n) = self.ticks {
            env.push((HEADLESS_TICKS_ENV.to_string(), n.to_string()));
        }
        if let Some(dpr) = self.dpr {
            env.push((HEADLESS_DPR_ENV.to_string(), dpr.to_string()));
        }
        if let Some((w, h)) = self.size {
            env.push((HEADLESS_SIZE_ENV.to_string(), format!("{w}x{h}")));
        }
        env
    }

    /// The run this process was handed, or `None` when it was started
    /// without one. A malformed value is an error rather than a default, so
    /// a typo in a CI script fails instead of opening a window.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// [`Self::from_env`] over any variable lookup.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        if get(HEADLESS_ENV).is_none() {
            return Ok(None);
        }
        let ticks = match get(HEADLESS_TICKS_ENV) {
            Some(v) => Some(
                v.trim()
                    .parse::<u64>()
                    .map_err(|_| format!("{HEADLESS_TICKS_ENV}={v} is not a tick count"))?,
            ),
            None => None,
        };
        let dpr = match get(HEADLESS_DPR_ENV) {
            Some(v) => match v.trim().parse::<f32>() {
                Ok(d) if d.is_finite() && d > 0.0 => Some(d),
                _ => return Err(format!("{HEADLESS_DPR_ENV}={v} is not a positive number")),
            },
            None => None,
        };
        let size = match get(HEADLESS_SIZE_ENV) {
            Some(v) => Some(
                parse_size(v.trim())
                    .ok_or_else(|| format!("{HEADLESS_SIZE_ENV}={v} is not a WxH size"))?,
            ),
            None => None,
        };
        Ok(Some(Self { ticks, dpr, size }))
    }
}

/// `WxH`, both sides above zero.
fn parse_size(v: &str) -> Option<(u32, u32)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// C++: drive CMake. Configure then build; the Release toggle switches the
/// build type. Launching the resulting binary is left to the caller (its name
/// is project-defined) - see the TODO in [`run_app_external`].
fn cpp_specs(dir: &Path, mode: Mode) -> Vec<CommandSpec> {
    let configure = match mode {
        Mode::Run => CommandSpec::new("cmake", dir).arg("-B").arg("build"),
        Mode::Build => CommandSpec::new("cmake", dir)
            .arg("-B")
            .arg("build")
            .arg("-DCMAKE_BUILD_TYPE=Release"),
    };
    let build = CommandSpec::new("cmake", dir).arg("--build").arg("build");
    vec![configure, build]
}

/// Python: `python3 <entry.py>` for Run; nothing to compile for Build.
/// `LUMEN_LIBRARY_PATH` is seeded from `CARGO_TARGET_DIR` when set so the
/// ctypes loader (`public/lumenui/python/lumen/_ffi.py`) finds `liblumen`;
/// absent that, the loader's own search (cwd `target/`, workspace walk) still
/// applies.
fn python_specs(dir: &Path, mode: Mode) -> Result<Vec<CommandSpec>, String> {
    match mode {
        Mode::Build => Ok(Vec::new()),
        Mode::Run => {
            let entry = python_entry(dir)?;
            let mut spec = CommandSpec::new(python_program(), dir).arg(entry);
            if let Ok(target) = std::env::var("CARGO_TARGET_DIR") {
                let lib_dir = Path::new(&target).join("debug");
                spec = spec.envv("LUMEN_LIBRARY_PATH", lib_dir.to_string_lossy().to_string());
            }
            Ok(vec![spec])
        }
    }
}

/// The Python interpreter name (`python3` everywhere except Windows, where
/// `python` is the conventional launcher).
fn python_program() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// Locate the Python entry script in `dir`. Prefers conventional names, then
/// falls back to the sole `.py` that imports `lumen`.
pub fn python_entry(dir: &Path) -> Result<String, String> {
    for candidate in ["main.py", "app.py", "__main__.py", "run.py"] {
        if dir.join(candidate).is_file() {
            return Ok(candidate.to_string());
        }
    }
    // Fall back to a lone lumen-importing script.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Err(format!("cannot read app directory {}", dir.display()));
    };
    let mut hits: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_py = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("py"));
        if is_py
            && let Ok(body) = std::fs::read_to_string(&path)
            && (body.contains("import lumen") || body.contains("from lumen"))
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            hits.push(name.to_string());
        }
    }
    match hits.len() {
        0 => Err(format!(
            "no Python entry script found in {} (expected main.py / app.py, \
             or a single .py importing lumen)",
            dir.display()
        )),
        1 => Ok(hits.remove(0)),
        _ => Err(format!(
            "multiple Python entry candidates in {} ({}); add a main.py or set \
             [app] entry",
            dir.display(),
            hits.join(", ")
        )),
    }
}

// -- Execution ---------------------------------------------------------------

/// Spawn each spec in order, inheriting stdio. Stops at the first failure and
/// surfaces the child's exit code.
fn execute(specs: &[CommandSpec]) -> ExitCode {
    for spec in specs {
        let shown = if spec.args.is_empty() {
            spec.program.clone()
        } else {
            format!("{} {}", spec.program, spec.args.join(" "))
        };
        eprintln!("lumenc: {} (in {})", shown, spec.cwd.display());
        match spec.to_command().status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status.code().unwrap_or(1);
                return ExitCode::from(code.clamp(0, 255) as u8);
            }
            Err(e) => {
                eprintln!("lumenc: failed to spawn `{}`: {e}", spec.program);
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Reroute `lumenc run <dir>` to the SDK app's native toolchain, with `env`
/// added to every command it runs; that is how a [`HeadlessHandoff`] reaches
/// the app's process.
pub fn run_app_external(kind: AppKind, dir: &Path, env: &[(String, String)]) -> ExitCode {
    let mut specs = match dispatch(kind, dir, Mode::Run) {
        Ok(specs) => specs,
        Err(e) => {
            eprintln!("lumenc run: {e}");
            return ExitCode::from(2);
        }
    };
    for spec in &mut specs {
        spec.env.extend(env.iter().cloned());
    }
    let code = execute(&specs);
    if kind == AppKind::Cpp {
        // TODO(cpp-run): CMake produces one binary per project with a
        // project-defined name; we cannot reliably know it here. For now we
        // configure + build and leave launching to the developer. A follow-up
        // can read the built target from `cmake --build`'s output or a
        // `[app] entry` binary-name override and exec it.
        eprintln!(
            "lumenc run: built the CMake project in {}. Launch the produced \
             binary from {}/build (binary name is project-defined).",
            dir.display(),
            dir.display()
        );
    }
    code
}

/// Reroute `lumenc build <dir>` to the SDK app's native toolchain.
pub fn build_app_external(kind: AppKind, dir: &Path) -> ExitCode {
    let specs = match dispatch(kind, dir, Mode::Build) {
        Ok(specs) => specs,
        Err(e) => {
            eprintln!("lumenc build: {e}");
            return ExitCode::from(2);
        }
    };
    if specs.is_empty() {
        // Python (and any future interpreted kind) has nothing to compile.
        eprintln!(
            "lumenc build: {kind:?} apps have no compile step; run it directly \
             with `lumenc run {}`.",
            dir.display()
        );
        return ExitCode::SUCCESS;
    }
    execute(&specs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique scratch directory per call (no external tempfile dep).
    fn scratch(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lumenc-appkind-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn detects_rust_from_cargo_dep() {
        let dir = scratch("rust");
        write(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"demo\"\n\n[dependencies]\nlumen = { path = \"../..\" }\n",
        );
        // SDK app dirs also carry markup; detection must still say Rust.
        write(&dir, "main.lmn", "<root/>");
        assert_eq!(detect(&dir), AppKind::Rust);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_rust_from_ffi_dep() {
        let dir = scratch("rust-ffi");
        write(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"demo\"\n\n[dependencies]\nlumenui = \"0.4\"\n",
        );
        assert_eq!(detect(&dir), AppKind::Rust);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cargo_without_lumen_is_markup() {
        let dir = scratch("plain-cargo");
        write(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"demo\"\n\n[dependencies]\nserde = \"1\"\n",
        );
        write(&dir, "main.lmn", "<root/>");
        assert_eq!(detect(&dir), AppKind::Markup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_cpp_from_cmake() {
        let dir = scratch("cpp-cmake");
        write(&dir, "CMakeLists.txt", "project(demo)\n");
        assert_eq!(detect(&dir), AppKind::Cpp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_cpp_from_source_include() {
        let dir = scratch("cpp-src");
        write(&dir, "main.cpp", "#include <lumen.hpp>\nint main(){}\n");
        assert_eq!(detect(&dir), AppKind::Cpp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_python_from_import() {
        let dir = scratch("py");
        write(&dir, "app.py", "import lumen\n");
        assert_eq!(detect(&dir), AppKind::Python);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_markup_fallback() {
        let dir = scratch("markup");
        write(&dir, "main.lmn", "<root/>");
        write(&dir, "lumen.toml", "[app]\nentry = \"main.lmn\"\n");
        assert_eq!(detect(&dir), AppKind::Markup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_precedence_over_markup() {
        let dir = scratch("rust-prec");
        write(&dir, "Cargo.toml", "[dependencies]\nlumen = \"0\"\n");
        write(&dir, "main.lmn", "<root/>");
        write(&dir, "CMakeLists.txt", "project(x)\n");
        assert_eq!(detect(&dir), AppKind::Rust);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_override_wins() {
        let dir = scratch("override");
        write(&dir, "main.lmn", "<root/>"); // would auto-detect Markup
        assert_eq!(resolve(&dir, Some(AppKind::Rust)), AppKind::Rust);
        assert_eq!(resolve(&dir, None), AppKind::Markup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_run_command_shape() {
        let dir = scratch("rust-run-cmd");
        let specs = dispatch(AppKind::Rust, &dir, Mode::Run).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].program, "cargo");
        assert_eq!(specs[0].args, vec!["run"]);
        assert_eq!(specs[0].cwd, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_build_command_shape() {
        let dir = scratch("rust-build-cmd");
        let specs = dispatch(AppKind::Rust, &dir, Mode::Build).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].program, "cargo");
        assert_eq!(specs[0].args, vec!["build", "--release"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `lumenui` links the prebuilt engine statically. `-C prefer-dynamic`
    /// would turn the standard library into a shared one the executable then
    /// needs beside it, which no package carries. The one variable a Rust
    /// build gets is the compiler its build script compiles the UI with: the
    /// program running now.
    #[test]
    fn a_rust_build_names_only_the_compiler_for_its_ui() {
        let dir = scratch("rust-env");
        let exe = std::env::current_exe().unwrap();
        for mode in [Mode::Run, Mode::Build] {
            let specs = dispatch(AppKind::Rust, &dir, mode).unwrap();
            assert_eq!(
                specs[0].env,
                [(LUMENC_ENV.to_string(), exe.to_string_lossy().into_owned())],
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn lookup(vars: &[(String, String)]) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn a_headless_handoff_reads_back_what_it_wrote() {
        let handoff = HeadlessHandoff {
            ticks: Some(120),
            dpr: Some(2.0),
            size: Some((800, 600)),
        };
        let env = handoff.to_env();
        assert_eq!(HeadlessHandoff::from_vars(lookup(&env)), Ok(Some(handoff)));

        let bare = HeadlessHandoff::default().to_env();
        assert_eq!(bare, vec![(HEADLESS_ENV.to_string(), "1".to_string())]);
        assert_eq!(
            HeadlessHandoff::from_vars(lookup(&bare)),
            Ok(Some(HeadlessHandoff::default()))
        );
        assert_eq!(HeadlessHandoff::from_vars(|_| None), Ok(None));
    }

    #[test]
    fn a_malformed_headless_handoff_is_an_error() {
        for (name, value) in [
            (HEADLESS_TICKS_ENV, "many"),
            (HEADLESS_DPR_ENV, "0"),
            (HEADLESS_DPR_ENV, "-1"),
            (HEADLESS_SIZE_ENV, "800"),
            (HEADLESS_SIZE_ENV, "0x600"),
        ] {
            let env = vec![
                (HEADLESS_ENV.to_string(), "1".to_string()),
                (name.to_string(), value.to_string()),
            ];
            let err = HeadlessHandoff::from_vars(lookup(&env)).unwrap_err();
            assert!(err.contains(name), "{err}");
        }
    }

    #[test]
    fn cpp_build_command_sequence() {
        let dir = scratch("cpp-build-cmd");
        let specs = dispatch(AppKind::Cpp, &dir, Mode::Build).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].program, "cmake");
        assert_eq!(
            specs[0].args,
            vec!["-B", "build", "-DCMAKE_BUILD_TYPE=Release"]
        );
        assert_eq!(specs[1].args, vec!["--build", "build"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cpp_run_command_sequence() {
        let dir = scratch("cpp-run-cmd");
        let specs = dispatch(AppKind::Cpp, &dir, Mode::Run).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].args, vec!["-B", "build"]);
        assert_eq!(specs[1].args, vec!["--build", "build"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_run_command_shape() {
        let dir = scratch("py-run-cmd");
        write(&dir, "main.py", "import lumen\n");
        let specs = dispatch(AppKind::Python, &dir, Mode::Run).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].program, python_program());
        assert_eq!(specs[0].args, vec!["main.py"]);
        assert_eq!(specs[0].cwd, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_entry_prefers_conventional_name() {
        let dir = scratch("py-entry");
        write(&dir, "helper.py", "import lumen\n");
        write(&dir, "app.py", "import lumen\n");
        assert_eq!(python_entry(&dir).unwrap(), "app.py");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_entry_missing_errors() {
        let dir = scratch("py-empty");
        assert!(python_entry(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_build_is_noop() {
        let dir = scratch("py-build");
        let specs = dispatch(AppKind::Python, &dir, Mode::Build).unwrap();
        assert!(specs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markup_dispatch_errors() {
        let dir = scratch("markup-dispatch");
        assert!(dispatch(AppKind::Markup, &dir, Mode::Run).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
