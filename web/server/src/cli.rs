//! The `lumen-server` command.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use lumen_ssr::{RenderOptions, SERVER_SPEC_FILE, ServerSpec, SsrSite};

use crate::config::{self, Command, Config, USAGE};
use crate::log::Log;
use crate::render::{ErrorPages, RenderHandler, RenderSettings};
use crate::server::{Exit, Server, Shutdown};

/// What a worker whose render never came back exits with, so whatever
/// watches it can tell that from a clean stop.
pub const EXIT_WEDGED: i32 = 70;

/// Run `lumen-server` with `args` (the program name left off) and the
/// process environment.
pub fn main(args: Vec<String>) -> ExitCode {
    let command = match config::parse(args, |name| std::env::var(name).ok()) {
        Ok(command) => command,
        Err(message) => {
            let _ = writeln!(
                std::io::stderr(),
                "lumen-server: {message}\n\nRun `lumen-server --help` for the flags."
            );
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Help => {
            let _ = writeln!(std::io::stdout(), "{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            let _ = writeln!(
                std::io::stdout(),
                "lumen-server {}",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Command::Probe(config) => probe(&config),
        Command::Serve(config) => serve(config),
    }
}

fn serve(config: Config) -> ExitCode {
    let log = Arc::new(Log::new(config.log_format, "lumen-server"));
    #[cfg(unix)]
    {
        if let Some(listener) = crate::supervisor::inherited_listener() {
            let exit = serve_on(listener, &config, &log, true);
            // Straight out, rather than through the drops: a wedged render
            // holds a thread that nothing can join.
            std::process::exit(exit_code(exit));
        }
        crate::supervisor::run(&config, &log)
    }
    #[cfg(not(unix))]
    {
        if config.workers > 1 {
            log.error(
                "--workers above 1 needs the unix supervisor, which Windows does not have. Run \
                 one lumen-server per port behind a balancer instead.",
            );
            return ExitCode::from(2);
        }
        if let Some(renders) = config.max_renders {
            log.warn(&format!(
                "--max-renders ends this process after about {renders} renders, and nothing here \
                 starts another; run it under a service manager that restarts it"
            ));
        }
        let listener = match bind(&config) {
            Ok(listener) => listener,
            Err(message) => {
                log.error(&message);
                return ExitCode::FAILURE;
            }
        };
        let exit = serve_on(listener, &config, &log, false);
        std::process::exit(exit_code(exit));
    }
}

/// The exit status a server that stopped for `exit` reports.
fn exit_code(exit: Exit) -> i32 {
    match exit {
        Exit::Stopped | Exit::Recycled => 0,
        Exit::Wedged => EXIT_WEDGED,
    }
}

/// Take the configured address and port.
pub(crate) fn bind(config: &Config) -> Result<TcpListener, String> {
    TcpListener::bind((config.bind, config.port)).map_err(|e| {
        format!(
            "cannot listen on {} port {}: {e}. Pass --port for another port, or --bind for an \
             address this machine has.",
            config.bind, config.port
        )
    })
}

/// The site a server renders, read from the directory a build wrote, and the
/// base path it is served under.
pub(crate) fn load_site(dir: &Path) -> Result<(SsrSite, String), String> {
    let spec_path = dir.join(SERVER_SPEC_FILE);
    let bytes = std::fs::read(&spec_path).map_err(|e| {
        format!(
            "cannot read {}: {e}. Point lumen-server at the directory `lumenc web --render ssr` \
             wrote.",
            spec_path.display()
        )
    })?;
    let spec = ServerSpec::from_json(&bytes).map_err(|e| e.to_string())?;
    let artifact_path = dir.join(&spec.web.artifact);
    let artifact = std::fs::read(&artifact_path)
        .map_err(|e| format!("cannot read {}: {e}", artifact_path.display()))?;
    let mut catalogues = Vec::new();
    for (tag, path) in &spec.web.catalogues {
        let path = dir.join(path);
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        catalogues.push((tag.clone(), source));
    }
    let site = SsrSite::from_build(&artifact, &spec, catalogues).map_err(|e| e.to_string())?;
    Ok((site, spec.web.base_path.clone()))
}

/// Serve on `listener` until stopped, and say why it stopped.
pub(crate) fn serve_on(
    listener: TcpListener,
    config: &Config,
    log: &Arc<Log>,
    worker: bool,
) -> Exit {
    let dir: PathBuf = config.site.clone().unwrap_or_default();
    let (site, base) = match load_site(&dir) {
        Ok(site) => site,
        Err(message) => {
            log.error(&message);
            std::process::exit(1);
        }
    };
    let mut options = RenderOptions::default().with_policy(site.policy());
    options.queue = Some(config.queue_depth);
    let handler = match RenderHandler::start(
        site,
        options,
        RenderSettings {
            limit: Some(config.render_timeout),
            max_renders: config.max_renders,
            errors: ErrorPages::Plain,
            log: Arc::clone(log),
        },
    ) {
        Ok(handler) => handler,
        Err(message) => {
            log.error(&message);
            std::process::exit(1);
        }
    };
    let server = Server::on(listener, &dir, &base)
        .with_handler(Arc::new(handler))
        .with_limits(config.limits)
        .with_health_path(&config.health_path)
        .with_trust(config.trust.clone())
        .with_access_log(Arc::clone(log));
    let stop = server.shutdown_handle();
    on_signal(stop.clone());
    #[cfg(unix)]
    if worker {
        crate::supervisor::watch_parent(stop);
    }
    #[cfg(not(unix))]
    let _ = worker;
    if worker {
        log.info(&format!("worker {} serving", std::process::id()));
    } else {
        log.info(&format!("serving {} at {}", dir.display(), server.url()));
    }
    let exit = server.run();
    match exit {
        Exit::Stopped => log.info("stopped"),
        Exit::Recycled => log.info("recycled after its renders; exiting for a fresh process"),
        Exit::Wedged => log.error(
            "a render ran past --render-timeout and cannot be stopped; exiting so a fresh \
             process takes over",
        ),
    }
    exit
}

/// Stop the server on SIGTERM or SIGINT.
#[cfg(unix)]
fn on_signal(stop: Shutdown) {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    let Ok(mut signals) = Signals::new([SIGTERM, SIGINT]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("lumen-server-signals".to_string())
        .spawn(move || {
            // A second signal, such as the SIGTERM a supervisor passes on
            // after the terminal's SIGINT reached the whole group, changes
            // nothing: the drain is already under way.
            for _ in signals.forever() {
                stop.shutdown();
            }
        });
}

/// Stop the server on Ctrl-C or Ctrl-Break.
#[cfg(not(unix))]
fn on_signal(stop: Shutdown) {
    let _ = ctrlc::set_handler(move || stop.shutdown());
}

/// Ask the liveness endpoint of the server `config` describes.
fn probe(config: &Config) -> ExitCode {
    // A server listening on every address answers on the loopback one.
    let host = match config.bind {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    let address = SocketAddr::new(host, config.port);
    let path = format!("{}/healthz", config.health_path.trim_end_matches('/'));
    match ask(address, &path) {
        Ok(200) => ExitCode::SUCCESS,
        Ok(status) => {
            let _ = writeln!(
                std::io::stderr(),
                "lumen-server probe: {address}{path} answered {status}"
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "lumen-server probe: cannot reach {address}{path}: {e}"
            );
            ExitCode::FAILURE
        }
    }
}

/// The status a GET of `path` at `address` answers with.
fn ask(address: SocketAddr, path: &str) -> std::io::Result<u16> {
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut head = [0u8; 32];
    let mut read = 0;
    while read < 12 {
        let n = stream.read(&mut head[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    std::str::from_utf8(&head[..read])
        .ok()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| std::io::Error::other("the answer is not HTTP"))
}
