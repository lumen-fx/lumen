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
        // Development runs one process, with nothing between the developer
        // and the server that answers them.
        if !config.dev {
            return crate::supervisor::run(&config, &log);
        }
    }
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
    if config.dev && !config.bind.is_loopback() {
        log.warn(&format!(
            "--bind {} makes the site reachable from other machines, and --dev shows them \
             what went wrong inside a render. Serve it without --dev, behind a reverse proxy, \
             before anyone else uses it.",
            config.bind
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

/// What a site directory holds.
pub(crate) enum Site {
    /// A build rendered per request: the app every page is rendered from.
    Rendered(Box<SsrSite>),
    /// Any other build, served as the files it is.
    Files,
}

/// The site a server serves, read from the directory a build wrote, and the
/// base path it is served under.
///
/// A directory with a spec file is a rendered site, and its base path is
/// the one it was built for; `base_path` has to agree with it or be left
/// out. Any other directory is served as files, under `base_path`.
pub(crate) fn load_site(dir: &Path, base_path: Option<&str>) -> Result<(Site, String), String> {
    let spec_path = dir.join(SERVER_SPEC_FILE);
    if !spec_path.exists() {
        if !dir.is_dir() {
            return Err(format!(
                "{} is not a directory. Point lumen-server at the directory `lumenc web` wrote.",
                dir.display()
            ));
        }
        return Ok((Site::Files, base_path.unwrap_or("/").to_string()));
    }
    let bytes = std::fs::read(&spec_path).map_err(|e| {
        format!(
            "cannot read {}: {e}. Point lumen-server at the directory `lumenc web` wrote.",
            spec_path.display()
        )
    })?;
    let spec = ServerSpec::from_json(&bytes).map_err(|e| e.to_string())?;
    let built = spec.web.base_path.clone();
    if let Some(asked) = base_path
        && asked.trim_matches('/') != built.trim_matches('/')
    {
        return Err(format!(
            "--base-path {asked} is not the base path {built} this site was built for; a \
             rendered page links under the one it was built with. Drop --base-path, or build \
             again with `lumenc web --base {asked}`."
        ));
    }
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
    Ok((Site::Rendered(Box::new(site)), built))
}

/// What renders run with under `config`, on top of the site's own policy.
pub(crate) fn render_options(site: &SsrSite, config: &Config) -> RenderOptions {
    let mut options = RenderOptions::default().with_policy(site.policy());
    for host in &config.allow_hosts {
        options.fetch = options.fetch.allow_host(host);
    }
    options.queue = config.queue_depth;
    options
}

/// How the render handler runs under `config`.
pub(crate) fn render_settings(config: &Config, log: &Arc<Log>) -> RenderSettings {
    RenderSettings {
        limit: config.render_timeout,
        max_renders: config.max_renders,
        errors: if config.dev {
            ErrorPages::Detailed
        } else {
            ErrorPages::Plain
        },
        log: Arc::clone(log),
    }
}

/// Serve on `listener` until stopped, and say why it stopped.
pub(crate) fn serve_on(
    listener: TcpListener,
    config: &Config,
    log: &Arc<Log>,
    worker: bool,
) -> Exit {
    let dir: PathBuf = config.site.clone().unwrap_or_default();
    let (site, base) = match load_site(&dir, config.base_path.as_deref()) {
        Ok(site) => site,
        Err(message) => {
            log.error(&message);
            std::process::exit(1);
        }
    };
    let mut server = Server::on(listener, &dir, &base)
        .with_limits(config.limits)
        .with_health_path(&config.health_path)
        .with_trust(config.trust.clone())
        .with_access_log(Arc::clone(log));
    let rendered = matches!(site, Site::Rendered(_));
    if let Site::Rendered(site) = site {
        let options = render_options(&site, config);
        let reaches_nothing = options.fetch.hosts.is_empty();
        let handler = match RenderHandler::start(*site, options, render_settings(config, log)) {
            Ok(handler) => handler,
            Err(message) => {
                log.error(&message);
                std::process::exit(1);
            }
        };
        server = server.with_handler(Arc::new(handler));
        if config.dev && reaches_nothing {
            log.info(
                "a render reaches no host; list them in lumen.toml [web.ssr] allow_hosts, or \
                 pass --allow-host, to let the app fetch its data while the page is rendered",
            );
        }
    }
    let stop = server.shutdown_handle();
    on_signal(stop.clone());
    if worker || config.dev {
        crate::parent::watch(stop);
    }
    if worker {
        log.info(&format!("worker {} serving", std::process::id()));
    } else if rendered {
        log.info(&format!(
            "serving {} at {}, rendering every page for the request that asks",
            dir.display(),
            server.url()
        ));
    } else {
        log.info(&format!(
            "serving the files in {} at {} (it has no {SERVER_SPEC_FILE})",
            dir.display(),
            server.url()
        ));
    }
    if config.dev && !worker {
        log.info("press Ctrl-C to stop");
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

#[cfg(test)]
mod tests {
    use lumen_ir::artifact::CompiledApp;
    use lumen_web::WebSpec;

    use super::*;
    use crate::config::{self, Command};

    fn config(args: &[&str]) -> Config {
        match config::parse(args.iter().map(|arg| arg.to_string()), |_| None) {
            Ok(Command::Serve(config)) => config,
            other => panic!("expected a server config, got {other:?}"),
        }
    }

    fn site() -> SsrSite {
        SsrSite::new(CompiledApp::default(), WebSpec::default()).expect("an empty app")
    }

    #[test]
    fn dev_adds_its_hosts_to_the_site_policy_and_lifts_the_limits() {
        let dev = config(&["--dev", "--allow-host", "api.example.com", "site"]);
        let options = render_options(&site(), &dev);
        assert!(options.fetch.hosts.contains("api.example.com"));
        assert_eq!(options.queue, None);
        let log = Arc::new(Log::new(dev.log_format, "test"));
        let settings = render_settings(&dev, &log);
        assert_eq!(settings.errors, ErrorPages::Detailed);
        assert_eq!(settings.limit, None);

        let production = config(&["site"]);
        let options = render_options(&site(), &production);
        assert!(options.fetch.hosts.is_empty());
        assert_eq!(options.queue, Some(2));
        let settings = render_settings(&production, &log);
        assert_eq!(settings.errors, ErrorPages::Plain);
        assert_eq!(settings.limit, Some(config::DEFAULT_RENDER_TIMEOUT));
    }

    #[test]
    fn a_directory_with_no_spec_file_is_files_under_the_base_asked_for() {
        let dir = std::env::temp_dir().join(format!("lumen-server-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the directory");
        let (site, base) = load_site(&dir, Some("/docs")).expect("a site of files");
        assert!(matches!(site, Site::Files));
        assert_eq!(base, "/docs");
        let (_, base) = load_site(&dir, None).expect("a site of files");
        assert_eq!(base, "/");
        let error = load_site(&dir.join("missing"), None)
            .err()
            .expect("no directory there");
        assert!(error.contains("not a directory"), "{error}");
    }
}
