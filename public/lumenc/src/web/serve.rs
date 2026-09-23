//! `lumenc web --serve`: hand the site a build wrote to `lumen-server`.
//!
//! lumenc compiles and nothing else, so it carries no server. `--serve`
//! starts the `lumen-server` installed beside it with `--dev`, pointed at the
//! directory the build wrote, and stays until it exits: its output is the
//! server's own, a Ctrl-C or SIGTERM reaches it so it finishes what it is
//! answering, and its exit status is the command's. The server is told
//! lumenc's process id, and stops by itself once lumenc is gone, so a lumenc
//! killed outright leaves nothing listening.

use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus};

/// The variable that names the `lumen-server` to run, when it is not beside
/// lumenc.
pub const SERVER_VAR: &str = "LUMEN_SERVER";

/// The variable lumen-server reads the id of the process it goes with from.
/// lumen-server declares it by the same name.
pub const SERVER_PARENT_VAR: &str = "LUMEN_SERVER_PARENT";

/// The variables lumen-server reads its settings from, which `--serve` keeps
/// from reaching it: the flags lumenc passes are the development server's
/// whole configuration, and a `LUMEN_WORKERS` or `LUMEN_BASE_PATH` left in
/// the shell for a production server would otherwise refuse or reshape it.
/// lumen-server's tests hold this list to the one it reads.
pub const SERVER_ENV: &[&str] = &[
    "LUMEN_DEV",
    "LUMEN_SITE",
    "LUMEN_BIND",
    "LUMEN_PORT",
    "LUMEN_BASE_PATH",
    "LUMEN_WORKERS",
    "LUMEN_MAX_CONNECTIONS",
    "LUMEN_QUEUE_DEPTH",
    "LUMEN_HEADER_TIMEOUT",
    "LUMEN_BODY_TIMEOUT",
    "LUMEN_WRITE_TIMEOUT",
    "LUMEN_KEEP_ALIVE",
    "LUMEN_RENDER_TIMEOUT",
    "LUMEN_MAX_RENDERS",
    "LUMEN_SHUTDOWN_GRACE",
    "LUMEN_LOG_FORMAT",
    "LUMEN_HEALTH_PATH",
    "LUMEN_TRUSTED_PROXIES",
    "LUMEN_ALLOW_HOSTS",
    "LUMEN_SERVER_LISTEN_FD",
    SERVER_PARENT_VAR,
];

/// The address `--serve` listens on when none is named: this machine, and
/// nobody else.
pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// The file name of the server binary on this OS.
pub fn server_file_name() -> &'static str {
    if cfg!(windows) {
        "lumen-server.exe"
    } else {
        "lumen-server"
    }
}

/// Where the `lumen-server` to run is.
///
/// Looked for in order: in `beside`, the directory the running lumenc is in,
/// where a release archive and every installer put it; at the path `named`,
/// the value of [`SERVER_VAR`]; and on `path`, the value of `PATH`. A
/// variable that names nothing runnable is an error rather than a step to
/// the next place, since whoever set it meant that one.
pub fn find_server(
    beside: Option<&Path>,
    named: Option<OsString>,
    path: Option<OsString>,
) -> Result<PathBuf, String> {
    let file = server_file_name();
    if let Some(dir) = beside {
        let candidate = dir.join(file);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(named) = named.filter(|named| !named.is_empty()) {
        let candidate = PathBuf::from(named);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "{SERVER_VAR} names {}, which is not a file. Point it at the lumen-server binary, \
             or unset it to use the one beside lumenc or on PATH.",
            candidate.display()
        ));
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(file);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let beside = beside
        .map(|dir| format!(" into {}", dir.display()))
        .unwrap_or_default();
    Err(format!(
        "cannot find lumen-server, which --serve runs the site with. It ships in the same \
         release archive as lumenc; put {file}{beside}, on PATH, or name it with \
         {SERVER_VAR}=/path/to/{file}."
    ))
}

/// Find the server the way [`find_server`] says, from this process.
fn locate() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().ok();
    let beside = exe.as_deref().and_then(Path::parent);
    find_server(
        beside,
        std::env::var_os(SERVER_VAR),
        std::env::var_os("PATH"),
    )
}

/// The address to listen on. Nothing named means the loopback address, which
/// is the machine this runs on and nobody else.
pub fn host_address(host: Option<&str>) -> Result<IpAddr, String> {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return Ok(LOOPBACK);
    };
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(LOOPBACK);
    }
    host.parse::<IpAddr>().map_err(|_| {
        format!(
            "--host takes an address this machine has, such as 127.0.0.1 or 0.0.0.0, got `{host}`"
        )
    })
}

/// What `--serve` was asked for, in lumenc's terms.
pub struct Serve<'a> {
    /// The directory the build wrote.
    pub site: &'a Path,
    /// The base path the site was built under.
    pub base: &'a str,
    /// Whether the pages are rendered per request, so the site carries its
    /// own base path and renders take `allow_hosts`.
    pub per_request: bool,
    /// `--host`.
    pub host: IpAddr,
    /// `--port`.
    pub port: u16,
    /// `--allow-host`, each one.
    pub allow_hosts: &'a [String],
}

/// The command line `lumen-server` is started with for `serve`.
pub fn server_args(serve: &Serve<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--dev".into(),
        "--bind".into(),
        serve.host.to_string().into(),
        "--port".into(),
        serve.port.to_string().into(),
    ];
    if serve.per_request {
        for host in serve.allow_hosts {
            args.push("--allow-host".into());
            args.push(host.into());
        }
    } else {
        // A rendered site says its own base path; a site of files does not.
        args.push("--base-path".into());
        args.push(serve.base.into());
    }
    args.push(serve.site.as_os_str().to_owned());
    args
}

/// Run `lumen-server` on the site until it exits, and exit as it did.
pub fn run(serve: &Serve<'_>) -> ExitCode {
    let server = match locate() {
        Ok(server) => server,
        Err(message) => {
            lumen_core::warn_line!(
                "lumenc web: {message}\nThe site is built in {}.",
                serve.site.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let mut command = Command::new(&server);
    command.args(server_args(serve));
    for var in SERVER_ENV {
        command.env_remove(var);
    }
    command.env(SERVER_PARENT_VAR, std::process::id().to_string());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            lumen_core::warn_line!("lumenc web: cannot start {}: {e}", server.display());
            return ExitCode::FAILURE;
        }
    };
    let stops = forward_stops(&child);
    let status = child.wait();
    stops.finished();
    match status {
        Ok(status) => exit_code(status),
        Err(e) => {
            lumen_core::warn_line!("lumenc web: lost track of lumen-server: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The exit status lumenc reports for a server that exited with `status`.
fn exit_code(status: ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitCode::from(u8::try_from(128 + signal).unwrap_or(1));
        }
    }
    ExitCode::FAILURE
}

/// Passes a stop on to the server until the server has exited.
struct Stops {
    #[cfg(unix)]
    exited: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Stops {
    /// The server has exited; its pid may name another process from here on.
    fn finished(&self) {
        #[cfg(unix)]
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Pass SIGINT and SIGTERM on to the server as SIGTERM, which it drains on,
/// rather than letting them end lumenc while the server is still answering.
#[cfg(unix)]
fn forward_stops(child: &Child) -> Stops {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    use rustix::process::{Pid, Signal, kill_process};

    let exited = Arc::new(AtomicBool::new(false));
    let pid = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    if let Some(pid) = pid
        && let Ok(mut signals) = Signals::new([SIGINT, SIGTERM])
    {
        let exited = Arc::clone(&exited);
        let _ = std::thread::Builder::new()
            .name("lumenc-serve-signals".to_string())
            .spawn(move || {
                for _ in signals.forever() {
                    // Once the server is reaped its pid may name another
                    // process, which is sent nothing.
                    if exited.load(Ordering::SeqCst) {
                        return;
                    }
                    let _ = kill_process(pid, Signal::TERM);
                }
            });
    }
    Stops { exited }
}

/// Outlive a Ctrl-C or Ctrl-Break: the console hands the same event to the
/// server, which drains on it, and lumenc waits for it to finish.
#[cfg(windows)]
fn forward_stops(_child: &Child) -> Stops {
    let _ = ctrlc::set_handler(|| {});
    Stops {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own, holding a `lumen-server` file when `with` says.
    fn dir(name: &str, with: bool) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumenc-find-server-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        if with {
            std::fs::write(dir.join(server_file_name()), b"").expect("write the stand-in");
        }
        dir
    }

    fn path_of(dirs: &[&Path]) -> Option<OsString> {
        Some(std::env::join_paths(dirs).expect("a PATH"))
    }

    #[test]
    fn the_server_beside_lumenc_comes_first() {
        let beside = dir("beside-first", true);
        let named = dir("beside-first-named", true).join(server_file_name());
        let on_path = dir("beside-first-path", true);
        let found = find_server(
            Some(&beside),
            Some(named.clone().into()),
            path_of(&[&on_path]),
        );
        assert_eq!(found, Ok(beside.join(server_file_name())));

        // Nothing beside it: the variable, over PATH.
        let empty = dir("beside-first-empty", false);
        let found = find_server(
            Some(&empty),
            Some(named.clone().into()),
            path_of(&[&on_path]),
        );
        assert_eq!(found, Ok(named));

        // No variable: PATH, in its order.
        let first = dir("beside-first-path-a", false);
        let found = find_server(Some(&empty), None, path_of(&[&first, &on_path]));
        assert_eq!(found, Ok(on_path.join(server_file_name())));

        // An empty variable is no variable.
        let found = find_server(Some(&empty), Some(OsString::new()), path_of(&[&on_path]));
        assert_eq!(found, Ok(on_path.join(server_file_name())));
    }

    #[test]
    fn a_variable_naming_nothing_is_an_error_not_a_step_to_path() {
        let empty = dir("named-nothing", false);
        let on_path = dir("named-nothing-path", true);
        let error = find_server(
            Some(&empty),
            Some(empty.join("missing").into()),
            path_of(&[&on_path]),
        )
        .expect_err("the variable names nothing");
        assert!(error.contains(SERVER_VAR), "{error}");
        assert!(error.contains("missing"), "{error}");
    }

    #[test]
    fn no_server_anywhere_says_where_it_ships_and_how_to_name_it() {
        let empty = dir("nowhere", false);
        let error = find_server(Some(&empty), None, path_of(&[&empty])).expect_err("none");
        assert!(error.contains("same release archive"), "{error}");
        assert!(error.contains(SERVER_VAR), "{error}");
        assert!(error.contains("PATH"), "{error}");
        assert!(error.contains(&empty.display().to_string()), "{error}");
    }

    #[test]
    fn a_rendered_site_is_served_in_development_mode_with_the_hosts_added() {
        let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];
        let args = server_args(&Serve {
            site: Path::new("dist/web"),
            base: "/docs/",
            per_request: true,
            host: LOOPBACK,
            port: 8787,
            allow_hosts: &hosts,
        });
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--dev",
                "--bind",
                "127.0.0.1",
                "--port",
                "8787",
                "--allow-host",
                "api.example.com",
                "--allow-host",
                "cdn.example.com",
                "dist/web",
            ]
        );
    }

    #[test]
    fn a_site_of_files_is_served_under_the_base_it_was_built_for() {
        let args = server_args(&Serve {
            site: Path::new("out"),
            base: "/docs/",
            per_request: false,
            host: "0.0.0.0".parse().expect("an address"),
            port: 0,
            allow_hosts: &[],
        });
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--dev",
                "--bind",
                "0.0.0.0",
                "--port",
                "0",
                "--base-path",
                "/docs/",
                "out"
            ]
        );
    }

    #[test]
    fn nothing_named_means_this_machine_and_nobody_else() {
        assert_eq!(host_address(None), Ok(LOOPBACK));
        assert_eq!(host_address(Some("")), Ok(LOOPBACK));
        assert_eq!(host_address(Some(" localhost ")), Ok(LOOPBACK));
        assert!(host_address(Some("127.0.0.1")).is_ok_and(|host| host.is_loopback()));
    }

    #[test]
    fn an_address_that_reaches_further_is_taken_as_written() {
        let any = host_address(Some("0.0.0.0")).expect("an address this machine can have");
        assert!(!any.is_loopback(), "the warning is on this being reachable");
        assert!(host_address(Some("::1")).is_ok_and(|host| host.is_loopback()));
    }

    #[test]
    fn something_that_is_not_an_address_is_named_back() {
        let error = host_address(Some("my-laptop")).expect_err("that is not an address");
        assert!(error.contains("my-laptop"), "{error}");
    }
}
